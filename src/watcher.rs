use crate::source::{JobSource, SourceSnapshot, linkedin::LinkedInAlertSource};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{Days, FixedOffset, LocalResult, NaiveDate, TimeZone, Utc};
use headless_chrome::{Browser, browser::tab::Tab};
use rusqlite::{Connection, params};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};
use tracing::{debug, error, info};

const SEARCH_URL: &str =
    "https://www.104.com.tw/jobs/search/?jobsource=index_s&keyword=Rust&mode=s&order=16";
const SOURCE: &str = "104";
const TAIPEI_OFFSET: i32 = 8 * 60 * 60;
const JD_BATCH_SIZE: usize = 10;

#[derive(Clone, Copy, Debug)]
enum JobUpdateMode {
    Changed,
    All,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChromeVersion {
    web_socket_debugger_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct JobListing {
    #[serde(default = "default_source")]
    pub source: String,
    pub external_id: String,
    pub title: String,
    pub company: String,
    pub location: Option<String>,
    pub salary: Option<String>,
    pub description: Option<String>,
    pub url: String,
    pub published_at: Option<String>,
    pub platform_updated_at: Option<String>,
    #[serde(default = "default_fetch_state")]
    pub fetch_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub seen_count: i64,
}

fn default_source() -> String {
    SOURCE.into()
}

fn default_fetch_state() -> String {
    "Complete".into()
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChangeRecord {
    pub change_type: String,
    pub source: String,
    pub external_id: String,
    pub title: String,
    pub company: String,
    pub location: Option<String>,
    pub salary: Option<String>,
    pub published_at: Option<String>,
    pub url: String,
    #[serde(default = "default_fetch_state")]
    pub fetch_state: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub changed_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    pub first_seen_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub seen_count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ChangeHistory {
    pub date: String,
    pub runs: Vec<HistoryRun>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryRun {
    pub generated_at: String,
    pub trigger: String,
    pub new: Vec<ChangeRecord>,
    pub updated: Vec<ChangeRecord>,
    pub deleted: Vec<ChangeRecord>,
}

#[derive(Clone, Debug, Default)]
pub struct SyncSummary {
    pub generated_at: String,
    pub date: String,
    pub trigger: String,
    pub new: Vec<ChangeRecord>,
    pub updated: Vec<ChangeRecord>,
    pub deleted: Vec<ChangeRecord>,
    pub export_path: Option<String>,
    pub export_error: Option<String>,
}

impl SyncSummary {
    fn has_changes(&self) -> bool {
        !(self.new.is_empty() && self.updated.is_empty() && self.deleted.is_empty())
    }
}

struct LineNotifier {
    channel_access_token: String,
    user_id: String,
}

pub struct EmailReporter {
    username: String,
    app_password: String,
    recipient: String,
}

impl EmailReporter {
    pub fn from_env() -> Result<Option<Self>> {
        let username = std::env::var("GMAIL_SMTP_USERNAME").ok();
        let app_password = std::env::var("GMAIL_SMTP_APP_PASSWORD").ok();
        let recipient = std::env::var("JOB_WATCHER_EMAIL_TO").ok();
        match (username, app_password, recipient) {
            (None, None, None) => Ok(None),
            (Some(username), Some(app_password), Some(recipient)) => Ok(Some(Self {
                username,
                app_password,
                recipient,
            })),
            _ => {
                anyhow::bail!(
                    "GMAIL_SMTP_USERNAME, GMAIL_SMTP_APP_PASSWORD, and JOB_WATCHER_EMAIL_TO must be set together"
                )
            }
        }
    }

    fn send_change_email(&self, summary: &SyncSummary) -> Result<()> {
        let attachment_path = summary
            .export_path
            .as_deref()
            .context("cannot send Gmail notification without the daily JSON export")?;
        let attachment = fs::read(attachment_path)
            .with_context(|| format!("failed to read Gmail attachment {attachment_path}"))?;
        let date = NaiveDate::parse_from_str(&summary.date, "%Y-%m-%d")?;
        self.send_message(
            &date,
            gmail_subject(summary)?,
            &gmail_body(summary),
            &attachment,
        )
    }

    pub fn send_test_email(&self, date: NaiveDate) -> Result<()> {
        let attachment = br#"{"test":true,"source":"job-watcher"}"#;
        let subject = format!("{} JD更新", date.format("%Y/%m/%d"));
        self.send_message(&date, subject, "新增：1\n更新：11\n刪除：0", attachment)
    }

    fn send_message(
        &self,
        date: &NaiveDate,
        subject: String,
        body: &str,
        attachment: &[u8],
    ) -> Result<()> {
        let filename = format!("{date}.json");
        let mime = mime_message(&self.recipient, &subject, body, &filename, attachment);
        let stream = TcpStream::connect(("smtp.gmail.com", 587))
            .context("failed to connect to smtp.gmail.com:587")?;
        let mut smtp = SmtpConnection::plain(stream)?;
        smtp.expect(220, "SMTP greeting")?;
        smtp.command("EHLO job-watcher", 250)?;
        smtp.command("STARTTLS", 220)?;
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name =
            ServerName::try_from("smtp.gmail.com").context("invalid SMTP server name")?;
        let tls_connection = ClientConnection::new(Arc::new(config), server_name)
            .context("failed to initialize SMTP TLS")?;
        let tls_stream = StreamOwned::new(tls_connection, smtp.into_stream());
        let mut smtp = SmtpConnection::tls(tls_stream)?;
        smtp.command("EHLO job-watcher", 250)?;
        smtp.command("AUTH LOGIN", 334)
            .context("SMTP authentication negotiation failed")?;
        smtp.command(&STANDARD.encode(self.username.as_bytes()), 334)
            .context("SMTP username authentication failed")?;
        smtp.command(&STANDARD.encode(self.app_password.as_bytes()), 235)
            .context("SMTP authentication failed; verify GMAIL_SMTP_USERNAME and app password")?;
        smtp.command(&format!("MAIL FROM:<{}>", self.username), 250)?;
        smtp.command(&format!("RCPT TO:<{}>", self.recipient), 250)?;
        smtp.command("DATA", 354)?;
        smtp.write_message(&mime)?;
        smtp.command("QUIT", 221).context("SMTP delivery failed")?;
        Ok(())
    }
}

enum SmtpConnection {
    Plain(BufReader<TcpStream>),
    Tls(Box<BufReader<StreamOwned<ClientConnection, TcpStream>>>),
}

impl SmtpConnection {
    fn plain(stream: TcpStream) -> Result<Self> {
        Ok(Self::Plain(BufReader::with_capacity(1, stream)))
    }
    fn tls(stream: StreamOwned<ClientConnection, TcpStream>) -> Result<Self> {
        Ok(Self::Tls(Box::new(BufReader::with_capacity(1, stream))))
    }
    fn into_stream(self) -> TcpStream {
        match self {
            Self::Plain(reader) => reader.into_inner(),
            Self::Tls(_) => unreachable!("STARTTLS is only used on a plain connection"),
        }
    }
    fn response(&mut self) -> Result<(u16, String)> {
        let reader: &mut dyn BufRead = match self {
            Self::Plain(reader) => reader,
            Self::Tls(reader) => reader,
        };
        let mut response = String::new();
        let code = loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .context("failed to read SMTP response")?;
            if line.len() < 3 {
                anyhow::bail!("invalid SMTP response")
            }
            let code: u16 = line[..3].parse().context("invalid SMTP response code")?;
            response.push_str(line.trim_end());
            if line.as_bytes().get(3) == Some(&b' ') {
                break code;
            }
        };
        Ok((code, response))
    }
    fn expect(&mut self, expected: u16, context: &str) -> Result<()> {
        let (code, response) = self.response()?;
        if code != expected {
            anyhow::bail!("{context} failed with SMTP {code}: {response}")
        }
        Ok(())
    }
    fn command(&mut self, command: &str, expected: u16) -> Result<()> {
        self.write_line(command)?;
        self.expect(expected, command)
    }
    fn write_line(&mut self, line: &str) -> Result<()> {
        match self {
            Self::Plain(reader) => reader.get_mut().write_all(format!("{line}\r\n").as_bytes()),
            Self::Tls(reader) => reader.get_mut().write_all(format!("{line}\r\n").as_bytes()),
        }
        .context("failed to write SMTP command")
    }
    fn write_message(&mut self, message: &str) -> Result<()> {
        let mut data = String::with_capacity(message.len() + 8);
        for line in message.split_inclusive('\n') {
            if line.starts_with('.') {
                data.push('.');
            }
            data.push_str(line);
        }
        if !data.ends_with("\r\n") {
            data.push_str("\r\n");
        }
        data.push_str(".\r\n");
        match self {
            Self::Plain(reader) => reader.get_mut().write_all(data.as_bytes()),
            Self::Tls(reader) => reader.get_mut().write_all(data.as_bytes()),
        }
        .context("failed to write SMTP message")?;
        self.expect(250, "SMTP message delivery")
    }
}

#[derive(Serialize)]
struct LinePushRequest {
    to: String,
    messages: Vec<LineMessage>,
}
#[derive(Serialize)]
struct LineReplyRequest {
    #[serde(rename = "replyToken")]
    reply_token: String,
    messages: Vec<LineMessage>,
}
#[derive(Serialize)]
struct LineMessage {
    #[serde(rename = "type")]
    message_type: &'static str,
    text: String,
}

impl LineNotifier {
    fn from_env() -> Result<Option<Self>> {
        let token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN").ok();
        let user_id = std::env::var("LINE_USER_ID").ok();
        match (token, user_id) {
            (None, None) => Ok(None),
            (Some(_), None) | (None, Some(_)) => {
                anyhow::bail!("LINE_CHANNEL_ACCESS_TOKEN and LINE_USER_ID must be set together")
            }
            (Some(channel_access_token), Some(user_id)) => Ok(Some(Self {
                channel_access_token,
                user_id,
            })),
        }
    }
    fn send_text(&self, text: &str) -> Result<()> {
        reqwest::blocking::Client::new()
            .post("https://api.line.me/v2/bot/message/push")
            .bearer_auth(&self.channel_access_token)
            .json(&LinePushRequest {
                to: self.user_id.clone(),
                messages: vec![LineMessage {
                    message_type: "text",
                    text: text.to_owned(),
                }],
            })
            .send()
            .context("failed to send LINE notification")?
            .error_for_status()
            .context("LINE Messaging API rejected the notification")?;
        Ok(())
    }
    fn reply_text(&self, reply_token: &str, text: &str) -> Result<()> {
        reqwest::blocking::Client::new()
            .post("https://api.line.me/v2/bot/message/reply")
            .bearer_auth(&self.channel_access_token)
            .json(&LineReplyRequest {
                reply_token: reply_token.to_owned(),
                messages: vec![LineMessage {
                    message_type: "text",
                    text: text.to_owned(),
                }],
            })
            .send()
            .context("failed to reply to LINE webhook")?
            .error_for_status()
            .context("LINE Messaging API rejected the webhook reply")?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct Service {
    lock: Arc<Mutex<bool>>,
    notifier: Arc<Option<LineNotifier>>,
}

impl Service {
    pub fn new() -> Result<Self> {
        Ok(Self {
            lock: Arc::new(Mutex::new(false)),
            notifier: Arc::new(LineNotifier::from_env()?),
        })
    }
    pub fn try_synchronize(&self, trigger: &str) -> Result<Option<SyncSummary>> {
        let mut running = self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("synchronization guard poisoned"))?;
        if *running {
            info!(
                "synchronization request ignored because another synchronization is already running"
            );
            return Ok(None);
        }
        *running = true;
        drop(running);
        info!(%trigger, "synchronization started");
        let result = synchronize(
            trigger,
            self.notifier.as_ref().as_ref(),
            JobUpdateMode::Changed,
        );
        match &result {
            Ok(summary) => info!(
                %trigger,
                new = summary.new.len(),
                updated = summary.updated.len(),
                deleted = summary.deleted.len(),
                "synchronization completed"
            ),
            Err(error) => error!(%trigger, error = %error, "synchronization failed"),
        }
        *self
            .lock
            .lock()
            .map_err(|_| anyhow::anyhow!("synchronization guard poisoned"))? = false;
        result.map(Some)
    }
    pub fn reply_text(&self, text: &str) -> Result<()> {
        if let Some(notifier) = self.notifier.as_ref() {
            notifier.send_text(text)
        } else {
            debug!(%text, "LINE disabled; reply not sent");
            Ok(())
        }
    }
    pub fn reply_to_line_event(&self, reply_token: &str, text: &str) -> Result<()> {
        if let Some(notifier) = self.notifier.as_ref() {
            notifier.reply_text(reply_token, text)
        } else {
            debug!(%text, "LINE disabled; webhook reply not sent");
            Ok(())
        }
    }
}

pub fn handle_webhook(payload: Value, service: Service) {
    let Some(events) = payload.get("events").and_then(Value::as_array) else {
        return;
    };
    for event in events {
        let Some(text) = event
            .pointer("/message/text")
            .and_then(Value::as_str)
            .map(str::trim)
        else {
            continue;
        };
        let Some(command) = command_for(text) else {
            continue;
        };
        let reply_token = event
            .get("replyToken")
            .and_then(Value::as_str)
            .map(str::to_owned);
        info!(%command, "LINE command received");
        let service = service.clone();
        thread::spawn(move || {
            let message = match command {
                "today" => today_digest(),
                _ => {
                    let execute_time = current_execution_time();
                    let started =
                        format!("104 Job Watcher\n更新JD 已開始\n預計執行時間：{execute_time}");
                    let start_result = if let Some(token) = reply_token.as_deref() {
                        service.reply_to_line_event(token, &started)
                    } else {
                        service.reply_text(&started)
                    };
                    if let Err(error) = start_result {
                        error!(error = %error, "failed to send update-start notification");
                    }
                    match service.try_synchronize("manual") {
                        Ok(Some(summary)) => summary_digest(&summary, "104 Job Watcher\n更新完成"),
                        Ok(None) => format!(
                            "104 Job Watcher\n\n執行時間：{execute_time}\n目前正在更新 JD，請稍後再試。"
                        ),
                        Err(error) => format!(
                            "104 Job Watcher\n\n執行時間：{execute_time}\n更新失敗：{error:#}"
                        ),
                    }
                }
            };
            let result = if command == "today" {
                if let Some(token) = reply_token.as_deref() {
                    match service.reply_to_line_event(token, &message) {
                        Ok(()) => Ok(()),
                        Err(reply_error) => {
                            error!(
                                error = %reply_error,
                                "LINE reply failed; attempting push-message fallback"
                            );
                            service.reply_text(&message)
                        }
                    }
                } else {
                    service.reply_text(&message)
                }
            } else {
                // The webhook reply token was consumed by the immediate
                // acknowledgement; completion is delivered asynchronously.
                service.reply_text(&message)
            };
            if let Err(error) = result {
                error!(error = %error, "LINE command response failed");
            }
        });
    }
}

fn command_for(text: &str) -> Option<&'static str> {
    match text.trim() {
        "今日履歷" => Some("today"),
        "更新JD" => Some("update"),
        _ => None,
    }
}

fn current_execution_time() -> String {
    taipei_now()
        .map(|time| time.format("%Y/%m/%d %H:%M:%S").to_string())
        .unwrap_or_else(|_| "時間無法取得".to_owned())
}

fn parse_job_list(value: &str) -> Result<Vec<JobListing>> {
    serde_json::from_str(value).context("failed to parse extracted 104 job list")
}

fn is_challenge_page(title: &str, html: &str) -> bool {
    let cards = html.contains("data-job-no");
    title.trim().eq_ignore_ascii_case("Just a moment...")
        || (!cards
            && (html.contains("challenge-platform")
                || html.contains("cf-chl-")
                || html.contains("Verify you are human")))
}

fn extract_job_list(tab: &Tab) -> Result<Vec<JobListing>> {
    let result = tab.evaluate(r#"(async () => {
        const jobs = new Map(); const text = (root, selector) => { const v = root.querySelector(selector)?.innerText?.trim(); return v || null; };
        const collect = () => { for (const card of document.querySelectorAll('div[data-job-no]')) {
            const id = card.getAttribute('data-job-no'); const anchor = card.querySelector('a.info-job'); if (!id || !anchor) continue;
            let url = anchor.href; try { url = new URL(url).searchParams.get('url') || url; } catch (_) {}
            jobs.set(id, { external_id:id, title:text(card,'.info-name') || '', company:text(card,'.info-company > a') || '', location:text(card,'[data-gtm-joblist^="職缺-地區"]'), salary:text(card,'[data-gtm-joblist^="職缺-薪資"]'), description:text(card,'.info-content'), url, published_at:text(card,'.job-mobile__date') });
        }};
        const scroller = document.querySelector('.vue-recycle-scroller'); for (let i=0;i<120;i++) { collect(); if (!scroller) break; const old=scroller.scrollTop; scroller.scrollTop += Math.max(window.innerHeight,600); scroller.dispatchEvent(new Event('scroll',{bubbles:true})); await new Promise(r=>setTimeout(r,100)); if (scroller.scrollTop === old) break; } collect(); return JSON.stringify([...jobs.values()]);
    })()"#, true)?;
    let value = result
        .value
        .context("104 job extraction returned no value")?;
    let json = value
        .as_str()
        .context("104 job extraction returned a non-string value")?;
    parse_job_list(json)
}

fn extract_total_pages(tab: &Tab) -> Result<usize> {
    let result = tab.evaluate(r#"Math.max(1, ...Array.from(document.querySelectorAll('a[href*="page="]')).map(x=>Number(new URL(x.href).searchParams.get('page'))).filter(x=>Number.isInteger(x)&&x>0))"#, false)?;
    let pages = usize::try_from(
        result
            .value
            .context("104 pagination returned no value")?
            .as_u64()
            .context("104 pagination returned a non-integer value")?,
    )
    .context("104 page count does not fit in usize")?;
    if pages > 150 {
        anyhow::bail!("104 reported an unsafe page count: {pages}");
    }
    Ok(pages)
}

struct TabCleanup {
    tab: Arc<Tab>,
}

impl Drop for TabCleanup {
    fn drop(&mut self) {
        if let Err(error) = self.tab.close_target() {
            debug!(error = %error, "failed to close Chromium page");
        }
    }
}

fn extract_detail(browser: &Browser, job: &JobListing) -> Result<String> {
    // Reuse one independent CDP connection, but close each detail target so
    // Chromium does not retain one browser target per job.
    let tab = browser
        .new_tab()
        .context("failed to create Chromium JD detail tab")?;
    let result = (|| {
        tab.navigate_to(&job.url)
            .with_context(|| format!("failed to navigate to JD {}", job.external_id))?
            .wait_until_navigated()
            .context("JD page did not finish navigating")?;
        thread::sleep(Duration::from_secs(2));
        let title = tab.get_title()?;
        let html = tab.get_content()?;
        if is_challenge_page(&title, &html) {
            anyhow::bail!("104 returned a challenge for JD {}", job.external_id);
        }
        let result = tab.evaluate(
            r#"(() => {
            const selectors = [
                '.job-description',
                '.job-description__content',
                '[id*="job-description"]',
                '[class*="job-description"]',
                '[class*="description"]',
                'article'
            ];
            const candidates = selectors
                .flatMap(selector => [...document.querySelectorAll(selector)])
                .map(element => element.innerText?.trim() || '')
                .filter(text => text.length > 0);
            return candidates.sort((left, right) => right.length - left.length)[0]
                || document.body?.innerText?.trim()
                || '';
        })()"#,
            true,
        )?;
        Ok(result
            .value
            .context("JD extraction returned no value")?
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned())
    })();
    if let Err(error) = tab.close_target() {
        debug!(external_id = %job.external_id, error = %error, "failed to close JD detail tab");
    }
    result
}

fn ensure_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS jobs (source TEXT NOT NULL, external_id TEXT NOT NULL, title TEXT NOT NULL, company TEXT NOT NULL, location TEXT, salary TEXT, description TEXT, url TEXT NOT NULL, published_at TEXT, platform_updated_at TEXT, fetch_state TEXT NOT NULL DEFAULT 'Complete', work_site TEXT, annual_salary TEXT, last_updated TEXT, first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, seen_count INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(source, external_id));").context("failed to initialize SQLite schema")?;
    for (name, definition) in [
        ("work_site", "TEXT"),
        ("annual_salary", "TEXT"),
        ("last_updated", "TEXT"),
        ("description", "TEXT"),
        ("platform_updated_at", "TEXT"),
        ("fetch_state", "TEXT NOT NULL DEFAULT 'Complete'"),
        ("first_seen_at", "TEXT"),
        ("last_seen_at", "TEXT"),
        ("seen_count", "INTEGER NOT NULL DEFAULT 1"),
    ] {
        let exists: bool = connection
            .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name=?1")?
            .exists([name])?;
        if !exists {
            connection.execute(
                &format!("ALTER TABLE jobs ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    connection.execute(
        "UPDATE jobs SET first_seen_at=COALESCE(NULLIF(first_seen_at,''),CURRENT_TIMESTAMP), last_seen_at=COALESCE(NULLIF(last_seen_at,''),CURRENT_TIMESTAMP), seen_count=COALESCE(NULLIF(seen_count,0),1)",
        [],
    )?;
    Ok(())
}

fn load_known_jobs(connection: &Connection) -> Result<HashMap<String, JobListing>> {
    let mut statement = connection.prepare("SELECT source,external_id,title,company,COALESCE(work_site,location),COALESCE(annual_salary,salary),description,url,published_at,platform_updated_at,fetch_state,first_seen_at,last_seen_at,seen_count FROM jobs WHERE source='104'")?;
    let rows = statement.query_map([], |row| {
        Ok(JobListing {
            source: row.get(0)?,
            external_id: row.get(1)?,
            title: row.get(2)?,
            company: row.get(3)?,
            location: row.get(4)?,
            salary: row.get(5)?,
            description: row.get(6)?,
            url: row.get(7)?,
            published_at: row.get(8)?,
            platform_updated_at: row.get(9)?,
            fetch_state: row.get(10)?,
            first_seen_at: row.get(11)?,
            last_seen_at: row.get(12)?,
            seen_count: row.get(13)?,
        })
    })?;
    rows.map(|row| row.map(|job| (job.external_id.clone(), job)))
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .context("failed to read known 104 jobs")
}

fn load_all_known_jobs(connection: &Connection) -> Result<HashMap<(String, String), JobListing>> {
    let mut statement = connection.prepare("SELECT source,external_id,title,company,COALESCE(work_site,location),COALESCE(annual_salary,salary),description,url,published_at,platform_updated_at,fetch_state,first_seen_at,last_seen_at,seen_count FROM jobs")?;
    let rows = statement.query_map([], |row| {
        Ok(JobListing {
            source: row.get(0)?,
            external_id: row.get(1)?,
            title: row.get(2)?,
            company: row.get(3)?,
            location: row.get(4)?,
            salary: row.get(5)?,
            description: row.get(6)?,
            url: row.get(7)?,
            published_at: row.get(8)?,
            platform_updated_at: row.get(9)?,
            fetch_state: row.get(10)?,
            first_seen_at: row.get(11)?,
            last_seen_at: row.get(12)?,
            seen_count: row.get(13)?,
        })
    })?;
    rows.map(|row| row.map(|job| ((job.source.clone(), job.external_id.clone()), job)))
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .context("failed to read known jobs")
}

fn normalize_text(value: Option<&str>) -> String {
    value
        .unwrap_or_default()
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn changed_fields(old: &JobListing, new: &JobListing) -> Vec<String> {
    let mut fields = Vec::new();
    if old.title != new.title {
        fields.push("title".into());
    }
    if old.company != new.company {
        fields.push("company".into());
    }
    if normalize_text(old.location.as_deref()) != normalize_text(new.location.as_deref()) {
        fields.push("location".into());
    }
    if normalize_text(old.salary.as_deref()) != normalize_text(new.salary.as_deref()) {
        fields.push("salary".into());
    }
    if normalize_text(old.description.as_deref()) != normalize_text(new.description.as_deref()) {
        fields.push("description".into());
    }
    if old.url != new.url {
        fields.push("url".into());
    }
    if old.published_at != new.published_at {
        fields.push("published_at".into());
    }
    if old.platform_updated_at != new.platform_updated_at {
        fields.push("platform_updated_at".into());
    }
    if old.fetch_state != new.fetch_state {
        fields.push("fetch_state".into());
    }
    fields
}

fn listing_fields_changed(old: &JobListing, new: &JobListing) -> bool {
    changed_fields(old, new)
        .into_iter()
        .any(|field| field != "description")
}

fn copy_tracking(target: &mut JobListing, persisted: &JobListing) {
    target.first_seen_at = persisted.first_seen_at.clone();
    target.last_seen_at = persisted.last_seen_at.clone();
    target.seen_count = persisted.seen_count;
}

fn apply_run_tracking(job: &mut JobListing, old: Option<&JobListing>, run_at: &str) {
    job.first_seen_at = old
        .and_then(|previous| previous.first_seen_at.clone())
        .or_else(|| Some(run_at.to_owned()));
    job.last_seen_at = Some(run_at.to_owned());
    job.seen_count = old
        .map(|previous| previous.seen_count.max(1) + 1)
        .unwrap_or(1);
}

fn change_record(
    kind: &str,
    job: &JobListing,
    fields: Vec<String>,
    deleted_at: Option<String>,
) -> ChangeRecord {
    ChangeRecord {
        change_type: kind.into(),
        source: job.source.clone(),
        external_id: job.external_id.clone(),
        title: job.title.clone(),
        company: job.company.clone(),
        location: job.location.clone(),
        salary: job.salary.clone(),
        published_at: job.published_at.clone(),
        url: job.url.clone(),
        fetch_state: job.fetch_state.clone(),
        changed_fields: fields,
        description: job.description.clone(),
        deleted_at,
        first_seen_at: job.first_seen_at.clone(),
        last_seen_at: job.last_seen_at.clone(),
        seen_count: job.seen_count,
    }
}

fn persist_state(
    connection: &mut Connection,
    source: &str,
    jobs: &[JobListing],
    ids: &HashSet<String>,
    allow_deletions: bool,
    run_at: &str,
) -> Result<()> {
    ensure_schema(connection)?;
    let tx = connection
        .transaction()
        .context("failed to begin SQLite state transaction")?;
    {
        let mut statement = tx.prepare("INSERT INTO jobs(source,external_id,title,company,location,salary,description,url,published_at,platform_updated_at,fetch_state,work_site,annual_salary,last_updated,first_seen_at,last_seen_at,seen_count) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?5,?6,COALESCE(?10,?9),?12,?12,1) ON CONFLICT(source,external_id) DO UPDATE SET title=excluded.title,company=excluded.company,location=excluded.location,salary=excluded.salary,description=COALESCE(excluded.description,jobs.description),url=excluded.url,published_at=excluded.published_at,platform_updated_at=excluded.platform_updated_at,fetch_state=excluded.fetch_state,work_site=excluded.work_site,annual_salary=excluded.annual_salary,last_updated=excluded.last_updated,first_seen_at=COALESCE(jobs.first_seen_at,excluded.first_seen_at),last_seen_at=excluded.last_seen_at,seen_count=COALESCE(jobs.seen_count,0)+1")?;
        let mut persisted_ids = HashSet::new();
        for job in jobs {
            if !persisted_ids.insert(job.external_id.clone()) {
                continue;
            }
            statement
                .execute(params![
                    source,
                    job.external_id,
                    job.title,
                    job.company,
                    job.location,
                    job.salary,
                    job.description,
                    job.url,
                    job.published_at,
                    job.platform_updated_at,
                    job.fetch_state,
                    run_at
                ])
                .with_context(|| format!("failed to persist job {}", job.external_id))?;
        }
    }
    if !allow_deletions {
        return tx
            .commit()
            .context("failed to commit SQLite state transaction");
    }
    if ids.is_empty() {
        tx.execute("DELETE FROM jobs WHERE source=?", [source])?;
    } else {
        let marks = vec!["?"; ids.len()].join(",");
        tx.execute(
            &format!("DELETE FROM jobs WHERE source=? AND external_id NOT IN ({marks})"),
            rusqlite::params_from_iter(
                std::iter::once(source).chain(ids.iter().map(String::as_str)),
            ),
        )?;
    }
    tx.commit()
        .context("failed to commit SQLite state transaction")
}

fn taipei_now() -> Result<chrono::DateTime<FixedOffset>> {
    let offset = FixedOffset::east_opt(TAIPEI_OFFSET).context("invalid Asia/Taipei offset")?;
    Ok(Utc::now().with_timezone(&offset))
}
pub fn taipei_date() -> Result<NaiveDate> {
    Ok(taipei_now()?.date_naive())
}
fn history_dir() -> PathBuf {
    std::env::var_os("JOB_WATCHER_CHANGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("changes"))
}
fn history_path(date: NaiveDate) -> PathBuf {
    history_dir().join(format!("{date}.json"))
}

fn job_file_dir() -> PathBuf {
    std::env::var_os("JOB_WATCHER_JD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("changes"))
}

struct JobFileWriter {
    root: PathBuf,
    directory: PathBuf,
    stamp: String,
    next_batch: usize,
    jobs: Vec<JobListing>,
}

impl JobFileWriter {
    fn new(date: NaiveDate, generated_at: &str) -> Result<Self> {
        let root = job_file_dir();
        let directory = root.join(date.format("%m-%d").to_string());
        fs::create_dir_all(&directory).context("failed to create local JD directory")?;
        let stamp: String = generated_at
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .collect();
        Ok(Self {
            root,
            directory,
            stamp,
            next_batch: 1,
            jobs: Vec::with_capacity(JD_BATCH_SIZE),
        })
    }

    fn push(&mut self, job: JobListing) -> Result<()> {
        self.jobs.push(job);
        if self.jobs.len() >= JD_BATCH_SIZE {
            self.flush()?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.flush()?;
        rotate_job_files(&self.root)
    }

    fn flush(&mut self) -> Result<()> {
        if self.jobs.is_empty() {
            return Ok(());
        }
        let path = self
            .directory
            .join(format!("jd-{}-{:03}.json", self.stamp, self.next_batch));
        fs::write(&path, serde_json::to_vec_pretty(&self.jobs)?)
            .with_context(|| format!("failed to write full JD batch {}", path.display()))?;
        info!(path = %path.display(), jobs = self.jobs.len(), "full JD batch file written");
        self.jobs.clear();
        self.next_batch += 1;
        Ok(())
    }
}

fn rotate_job_files(root: &Path) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let retention = Duration::from_secs(7 * 24 * 60 * 60);
    let now = SystemTime::now();
    for date_entry in fs::read_dir(root)? {
        let date_dir = date_entry?.path();
        if !date_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&date_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let modified = fs::metadata(&path)?.modified()?;
            if now.duration_since(modified).unwrap_or_default() > retention {
                fs::remove_file(&path).with_context(|| {
                    format!("failed to remove expired JD file {}", path.display())
                })?;
                info!(path = %path.display(), "removed expired JD file");
            }
        }
        if fs::read_dir(&date_dir)?.next().is_none() {
            fs::remove_dir(&date_dir).with_context(|| {
                format!("failed to remove empty JD directory {}", date_dir.display())
            })?;
            info!(path = %date_dir.display(), "removed empty JD directory");
        }
    }
    Ok(())
}

fn append_history(summary: &SyncSummary) -> Result<PathBuf> {
    let dir = history_dir();
    fs::create_dir_all(&dir).with_context(|| {
        format!(
            "failed to create change history directory {}",
            dir.display()
        )
    })?;
    let path = history_path(NaiveDate::parse_from_str(&summary.date, "%Y-%m-%d")?);
    let mut history = if path.is_file() {
        serde_json::from_slice::<ChangeHistory>(
            &fs::read(&path).context("failed to read change history")?,
        )
        .context("failed to parse change history")?
    } else {
        ChangeHistory {
            date: summary.date.clone(),
            runs: Vec::new(),
        }
    };
    history.runs.push(HistoryRun {
        generated_at: summary.generated_at.clone(),
        trigger: summary.trigger.clone(),
        new: summary.new.clone(),
        updated: summary.updated.clone(),
        deleted: summary.deleted.clone(),
    });
    fs::write(&path, serde_json::to_vec_pretty(&history)?)
        .context("failed to write change history")?;
    Ok(path)
}

fn rotate_history(today: NaiveDate) -> Result<()> {
    let cutoff = today
        .checked_sub_days(Days::new(6))
        .context("failed to calculate retention cutoff")?;
    let dir = history_dir();
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff {
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove expired change export {}", path.display())
            })?;
            info!(path = %path.display(), "removed expired change export");
        }
    }
    Ok(())
}

fn synchronize(
    trigger: &str,
    notifier: Option<&LineNotifier>,
    update_mode: JobUpdateMode,
) -> Result<SyncSummary> {
    match synchronize_104_and_linkedin(trigger, notifier, update_mode) {
        Ok(summary) => Ok(summary),
        Err(error) => {
            let Ok(Some(linkedin)) = LinkedInAlertSource::from_env() else {
                return Err(error);
            };
            let snapshot = match linkedin.acquire() {
                Ok(snapshot) => snapshot,
                Err(linkedin_error) => {
                    error!(
                        error = %linkedin_error,
                        original_error = %error,
                        "104 and LinkedIn synchronization failed"
                    );
                    return Err(error);
                }
            };
            info!(error = %error, "104 synchronization failed; continuing with LinkedIn snapshot");
            synchronize_linkedin_only(trigger, notifier, snapshot)
        }
    }
}

fn synchronize_linkedin_only(
    trigger: &str,
    notifier: Option<&LineNotifier>,
    mut snapshot: SourceSnapshot,
) -> Result<SyncSummary> {
    let now = taipei_now()?;
    let generated_at = now.to_rfc3339();
    let mut connection = Connection::open("jobs.sqlite3").context("failed to open jobs.sqlite3")?;
    ensure_schema(&connection)?;
    let known = load_all_known_jobs(&connection)?;
    let mut summary = SyncSummary {
        generated_at: generated_at.clone(),
        date: now.date_naive().to_string(),
        trigger: trigger.into(),
        ..Default::default()
    };
    let ids = merge_snapshot(&mut summary, &mut snapshot, &known, &generated_at);
    persist_state(
        &mut connection,
        &snapshot.source,
        &snapshot.jobs,
        &ids,
        snapshot.allow_deletions,
        &generated_at,
    )?;
    finish_summary(summary, notifier)
}

fn merge_snapshot(
    summary: &mut SyncSummary,
    snapshot: &mut SourceSnapshot,
    known: &HashMap<(String, String), JobListing>,
    generated_at: &str,
) -> HashSet<String> {
    let mut ids = HashSet::new();
    for job in &mut snapshot.jobs {
        let key = (snapshot.source.clone(), job.external_id.clone());
        apply_run_tracking(job, known.get(&key), generated_at);
        ids.insert(job.external_id.clone());
        if let Some(old) = known.get(&key) {
            let fields = changed_fields(old, job);
            if !fields.is_empty() {
                summary
                    .updated
                    .push(change_record("updated", job, fields, None));
            }
        } else {
            summary
                .new
                .push(change_record("new", job, Vec::new(), None));
        }
    }
    for ((source, id), old) in known {
        if snapshot.allow_deletions && source == &snapshot.source && !ids.contains(id) {
            summary.deleted.push(change_record(
                "deleted",
                old,
                Vec::new(),
                Some(generated_at.to_owned()),
            ));
        }
    }
    ids
}

fn finish_summary(
    mut summary: SyncSummary,
    notifier: Option<&LineNotifier>,
) -> Result<SyncSummary> {
    let date = NaiveDate::parse_from_str(&summary.date, "%Y-%m-%d")?;
    match append_history(&summary) {
        Ok(path) => {
            summary.export_path = Some(path.display().to_string());
            if let Err(error) = rotate_history(date) {
                summary.export_error = Some(format!("{error:#}"));
                error!(error = %error, "change-history rotation failed");
            }
        }
        Err(error) => {
            summary.export_error = Some(format!("{error:#}"));
            error!(error = %error, "change-history export failed");
        }
    }
    if let Some(email) = EmailReporter::from_env()?
        && let Err(error) = email.send_change_email(&summary)
    {
        error!(error = %error, "Gmail notification failed");
    }
    if let Some(notifier) = notifier
        && let Err(error) = notifier.send_text(&summary_digest(&summary, "Job Watcher\n更新完成"))
    {
        error!(error = %error, "LINE notification failed");
    }
    Ok(summary)
}

fn synchronize_104_and_linkedin(
    trigger: &str,
    notifier: Option<&LineNotifier>,
    update_mode: JobUpdateMode,
) -> Result<SyncSummary> {
    let now = taipei_now()?;
    let date = now.date_naive();
    let generated_at = now.to_rfc3339();
    info!(
        %trigger,
        url = SEARCH_URL,
        started_at = %generated_at,
        "opening 104 job-list URL"
    );
    let version: ChromeVersion = reqwest::blocking::get("http://127.0.0.1:9222/json/version")?
        .error_for_status()?
        .json()?;
    let web_socket_debugger_url = version.web_socket_debugger_url.clone();
    let browser = Browser::connect(version.web_socket_debugger_url)
        .context("failed to connect to Chromium")?;

    let detail_browser = Browser::connect(web_socket_debugger_url)
        .context("failed to connect to Chromium for JD details")?;
    let tab = browser.new_tab().context("failed to create Chromium tab")?;
    let _tab_cleanup = TabCleanup {
        tab: Arc::clone(&tab),
    };
    tab.navigate_to(SEARCH_URL)?.wait_until_navigated()?;
    let title = tab.get_title()?;
    let html = tab.get_content()?;
    fs::write("job-list.html", &html)?;
    if is_challenge_page(&title, &html) {
        anyhow::bail!("104 returned a Cloudflare challenge; refusing to persist an empty result");
    }
    thread::sleep(Duration::from_secs(2));
    let total_pages = extract_total_pages(&tab)?;
    let mut connection = Connection::open("jobs.sqlite3").context("failed to open jobs.sqlite3")?;
    ensure_schema(&connection)?;
    let known = load_known_jobs(&connection)?;
    info!(
        pages = total_pages,
        known_jobs = known.len(),
        ?update_mode,
        "104 search ready"
    );
    let mut ids = HashSet::new();
    let mut search_jobs = Vec::new();
    for page in 1..=total_pages {
        if page > 1 {
            tab.navigate_to(&format!("{SEARCH_URL}&page={page}"))?
                .wait_until_navigated()?;
            thread::sleep(Duration::from_secs(2));
        }
        let page_jobs =
            extract_job_list(&tab).with_context(|| format!("failed to extract page {page}"))?;
        if page_jobs.is_empty() {
            anyhow::bail!("104 page {page} returned no cards; refusing an incomplete result set");
        }
        info!(
            page,
            total_pages,
            cards = page_jobs.len(),
            "extracted 104 search page"
        );
        for job in page_jobs {
            if !ids.insert(job.external_id.clone()) {
                continue;
            }
            search_jobs.push(job);
        }
    }
    if search_jobs.is_empty() {
        anyhow::bail!("104 returned no job cards; refusing to replace the saved job list");
    }
    let total_jobs = search_jobs.len();
    info!(total_jobs, "104 search result set collected");
    let mut job_writer = match JobFileWriter::new(date, &generated_at) {
        Ok(writer) => Some(writer),
        Err(error) => {
            error!(error = %error, "local JD batch writer could not be initialized");
            None
        }
    };
    let mut current = Vec::with_capacity(total_jobs);
    for (index, mut job) in search_jobs.into_iter().enumerate() {
        let external_id = job.external_id.clone();
        apply_run_tracking(&mut job, known.get(&external_id), &generated_at);
        let remaining = total_jobs.saturating_sub(index + 1);
        info!(
            job_number = index + 1,
            total_jobs,
            remaining,
            external_id = %job.external_id,
            title = %job.title,
            "executing JD fetch"
        );
        let write_file = !known.contains_key(&job.external_id)
            || matches!(update_mode, JobUpdateMode::All)
            || known
                .get(&job.external_id)
                .is_some_and(|old| listing_fields_changed(old, &job));
        if let Some(old) = known.get(&job.external_id) {
            let fetch_detail = write_file;
            if fetch_detail {
                debug!(external_id = %job.external_id, title = %job.title, "fetching complete JD");
                match extract_detail(&detail_browser, &job) {
                    Ok(description) if !description.is_empty() => {
                        info!(
                            external_id = %job.external_id,
                            description_chars = description.chars().count(),
                            "full JD extracted"
                        );
                        job.description = Some(description);
                    }
                    Ok(_) => job.description = old.description.clone(),
                    Err(error) => {
                        error!(external_id = %job.external_id, error = %error, "JD fetch failed; preserving previous JD");
                        job.description = old.description.clone();
                    }
                }
            } else {
                debug!(external_id = %job.external_id, title = %job.title, "JD unchanged; reusing persisted JD");
                job.description = old.description.clone();
            }
        } else {
            debug!(external_id = %job.external_id, title = %job.title, "fetching complete JD for new job");
            job.description = None;
            if let Err(error) = extract_detail(&detail_browser, &job).map(|description| {
                if !description.is_empty() {
                    info!(
                        external_id = %job.external_id,
                        description_chars = description.chars().count(),
                        "full JD extracted for new job"
                    );
                    job.description = Some(description);
                }
            }) {
                error!(external_id = %job.external_id, error = %error, "JD fetch failed for new job");
            }
        }
        if write_file {
            let write_result = if let Some(writer) = job_writer.as_mut() {
                writer.push(job.clone())
            } else {
                Ok(())
            };
            if let Err(error) = write_result {
                error!(error = %error, "local JD batch write failed");
                job_writer = None;
            }
        }
        current.push(job);
    }
    if current.is_empty() {
        anyhow::bail!("104 returned no job cards; refusing to replace the saved job list");
    }
    let mut summary = SyncSummary {
        generated_at: generated_at.clone(),
        date: date.to_string(),
        trigger: trigger.into(),
        ..Default::default()
    };
    info!(
        current_jobs = current.len(),
        "comparing current jobs with SQLite"
    );
    for job in &current {
        if let Some(old) = known.get(&job.external_id) {
            let fields = changed_fields(old, job);
            if !fields.is_empty() {
                summary
                    .updated
                    .push(change_record("updated", job, fields, None));
            }
        } else {
            summary
                .new
                .push(change_record("new", job, Vec::new(), None));
        }
    }
    for (id, old) in &known {
        if !ids.contains(id) {
            summary.deleted.push(change_record(
                "deleted",
                old,
                Vec::new(),
                Some(generated_at.clone()),
            ));
        }
    }
    match LinkedInAlertSource::from_env() {
        Ok(Some(linkedin)) => match linkedin.acquire() {
            Ok(mut snapshot) => {
                let known_all = load_all_known_jobs(&connection)?;
                let mut linkedin_summary = summary.clone();
                let linkedin_ids = merge_snapshot(
                    &mut linkedin_summary,
                    &mut snapshot,
                    &known_all,
                    &generated_at,
                );
                if let Err(error) = persist_state(
                    &mut connection,
                    linkedin.source_name(),
                    &snapshot.jobs,
                    &linkedin_ids,
                    snapshot.allow_deletions,
                    &generated_at,
                ) {
                    error!(error = %error, "LinkedIn state persistence failed; preserving previous LinkedIn state");
                } else {
                    summary = linkedin_summary;
                }
            }
            Err(error) => {
                error!(error = %error, "LinkedIn acquisition failed; preserving existing LinkedIn jobs");
            }
        },
        Ok(None) => {}
        Err(error) => {
            error!(error = %error, "LinkedIn configuration invalid; preserving existing LinkedIn jobs");
        }
    }
    let unchanged = current
        .len()
        .saturating_sub(summary.new.len() + summary.updated.len());
    info!(
        new = summary.new.len(),
        updated = summary.updated.len(),
        deleted = summary.deleted.len(),
        unchanged,
        "change comparison complete"
    );
    persist_state(&mut connection, SOURCE, &current, &ids, true, &generated_at)?;
    let persisted_jobs = load_all_known_jobs(&connection)?;
    for job in &mut current {
        if let Some(persisted) = persisted_jobs.get(&(job.source.clone(), job.external_id.clone()))
        {
            copy_tracking(job, persisted);
        }
    }
    fs::write("job-list.json", serde_json::to_vec_pretty(&current)?)
        .context("failed to save extracted job list")?;
    info!(current_jobs = current.len(), "SQLite state committed");
    if let Some(writer) = job_writer
        && let Err(error) = writer.finish()
    {
        error!(error = %error, "local JD batch rotation failed");
    }
    match append_history(&summary) {
        Ok(path) => {
            summary.export_path = Some(path.display().to_string());
            info!(path = %path.display(), "change history appended");
            if let Err(error) = rotate_history(date) {
                summary.export_error = Some(format!("{error:#}"));
                error!(error = %error, "change-history rotation failed");
            }
        }
        Err(error) => {
            error!(error = %error, error_chain = ?error, "change-history export failed");
            summary.export_error = Some(format!("{error:#}"));
        }
    }
    if let Some(email) = EmailReporter::from_env()?
        && let Err(error) = email.send_change_email(&summary)
    {
        error!(error = %error, "Gmail notification failed");
    }
    if matches!(
        trigger,
        "startup" | "scheduled" | "local-cli" | "local-cli-all"
    ) && let Some(notifier) = notifier
        && let Err(error) = notifier.send_text(&summary_digest(
            &summary,
            match trigger {
                "scheduled" => "每日履歷更新\n同步完成",
                "startup" => "Job Watcher\n啟動更新完成",
                _ => "Job Watcher\n更新完成",
            },
        ))
    {
        error!(error = %error, "LINE notification failed");
    }
    Ok(summary)
}

pub fn run_local_update() -> Result<SyncSummary> {
    let line_bot = line_bot_enabled();
    let notifier = if line_bot {
        Some(
            LineNotifier::from_env()?
                .context("JOB_WATCHER_LINE_BOT=true requires LINE credentials")?,
        )
    } else {
        None
    };
    synchronize("local-cli", notifier.as_ref(), JobUpdateMode::Changed)
}

pub fn run_local_update_all() -> Result<SyncSummary> {
    let line_bot = line_bot_enabled();
    let notifier = if line_bot {
        Some(
            LineNotifier::from_env()?
                .context("JOB_WATCHER_LINE_BOT=true requires LINE credentials")?,
        )
    } else {
        None
    };
    synchronize("local-cli-all", notifier.as_ref(), JobUpdateMode::All)
}

fn line_bot_enabled() -> bool {
    matches!(
        std::env::var("JOB_WATCHER_LINE_BOT")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn summary_digest(summary: &SyncSummary, heading: &str) -> String {
    let mut out = format!(
        "{heading}\n{}\n\n新增：{}\n更新：{}\n刪除：{}",
        summary.generated_at,
        summary.new.len(),
        summary.updated.len(),
        summary.deleted.len()
    );
    if summary.has_changes() {
        for (label, items) in [
            ("New", &summary.new),
            ("Updated", &summary.updated),
            ("Deleted", &summary.deleted),
        ] {
            if !items.is_empty() {
                out.push_str(&format!("\n\n[{label}]"));
                for job in items.iter().take(10) {
                    out.push_str(&format!(
                        "\n{}｜{}｜{}\n{}",
                        job.title,
                        job.company,
                        job.location.as_deref().unwrap_or("未提供"),
                        job.url
                    ));
                }
            }
        }
    }
    if summary.export_error.is_some() {
        out.push_str("\n\nSQLite 已更新，但完整 JD 匯出失敗。\n");
    }
    if !summary.has_changes() && summary.export_error.is_none() {
        out.push_str("\n\n目前沒有新的職缺異動。\n");
    }
    out
}

fn gmail_subject(summary: &SyncSummary) -> Result<String> {
    let date = NaiveDate::parse_from_str(&summary.date, "%Y-%m-%d")?;
    Ok(format!("{} JD更新", date.format("%Y/%m/%d")))
}

fn gmail_body(summary: &SyncSummary) -> String {
    format!(
        "新增：{}\n更新：{}\n刪除：{}",
        summary.new.len(),
        summary.updated.len(),
        summary.deleted.len()
    )
}

fn mime_message(
    recipient: &str,
    subject: &str,
    body: &str,
    filename: &str,
    attachment: &[u8],
) -> String {
    let boundary = "job-watcher-attachment";
    let encoded_subject = format!("=?UTF-8?B?{}?=", STANDARD.encode(subject.as_bytes()));
    format!(
        "To: {recipient}\r\nSubject: {encoded_subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n--{boundary}\r\nContent-Type: application/json; name=\"{filename}\"\r\nContent-Disposition: attachment; filename=\"{filename}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n",
        STANDARD.encode(attachment)
    )
}

fn today_digest() -> String {
    let Ok(now) = taipei_now() else {
        error!("cannot determine Asia/Taipei time while reading today's history");
        return "今日履歷資料無法讀取。".into();
    };
    let date = now.date_naive();
    let path = history_path(date);
    let Ok(bytes) = fs::read(&path) else {
        debug!(path = %path.display(), "today's change history does not exist");
        return "今日尚無職缺異動。".into();
    };
    let history = match serde_json::from_slice::<ChangeHistory>(&bytes) {
        Ok(history) => history,
        Err(error) => {
            error!(path = %path.display(), error = %error, "today's change history is invalid");
            return "今日履歷資料無法讀取。".into();
        }
    };
    let mut by_id: HashMap<(String, String), ChangeRecord> = HashMap::new();
    for run in history.runs {
        for record in run.new.into_iter().chain(run.updated).chain(run.deleted) {
            by_id.insert((record.source.clone(), record.external_id.clone()), record);
        }
    }
    if by_id.is_empty() {
        return "今日尚無職缺異動。".into();
    }
    let mut summary = SyncSummary {
        date: date.to_string(),
        generated_at: now.to_rfc3339(),
        trigger: "read-only".into(),
        ..Default::default()
    };
    for record in by_id.into_values() {
        match record.change_type.as_str() {
            "new" => summary.new.push(record),
            "updated" => summary.updated.push(record),
            "deleted" => summary.deleted.push(record),
            _ => {}
        }
    }
    summary_digest(&summary, "今日履歷")
}

fn next_scheduled_run() -> Result<Duration> {
    let offset = FixedOffset::east_opt(TAIPEI_OFFSET).context("invalid Taipei timezone")?;
    let now = Utc::now().with_timezone(&offset);
    let today = now.date_naive();
    let candidate = |date: NaiveDate| match offset
        .from_local_datetime(&date.and_hms_opt(7, 0, 0).context("invalid 07:00")?)
    {
        LocalResult::Single(value) => Ok(value),
        _ => anyhow::bail!("ambiguous Taipei scheduled time"),
    };
    let next = if let Some(c) = candidate(today)?
        .checked_sub_signed(chrono::TimeDelta::zero())
        .filter(|c| *c > now)
    {
        c
    } else {
        candidate(
            today
                .checked_add_days(Days::new(1))
                .context("failed to calculate tomorrow")?,
        )?
    };
    (next - now)
        .to_std()
        .context("invalid scheduled run duration")
}

pub fn run_service(service: Service) -> Result<()> {
    info!("running startup synchronization; automatic schedule is 07:00 Asia/Taipei");
    if let Err(error) = service.try_synchronize("startup") {
        error!(error = %error, "startup synchronization failed");
    }
    loop {
        let delay = next_scheduled_run()?;
        info!(
            delay_seconds = delay.as_secs(),
            "waiting for next scheduled synchronization"
        );
        thread::sleep(delay);
        if let Err(error) = service.try_synchronize("scheduled") {
            error!(error = %error, "scheduled synchronization failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whitespace_only_description_is_unchanged() {
        let mut a = sample();
        let mut b = a.clone();
        a.description = Some("a\n b".into());
        b.description = Some(" a  b ".into());
        assert!(changed_fields(&a, &b).is_empty());
    }
    #[test]
    fn changed_salary_is_reported() {
        let a = sample();
        let mut b = a.clone();
        b.salary = Some("200".into());
        assert_eq!(changed_fields(&a, &b), vec!["salary"]);
    }
    #[test]
    fn gmail_format_uses_taipei_date_and_counts_only() {
        let summary = SyncSummary {
            date: "2026-08-19".into(),
            new: vec![change_record("new", &sample(), Vec::new(), None)],
            updated: vec![change_record(
                "updated",
                &sample(),
                vec!["salary".into()],
                None,
            )],
            ..Default::default()
        };
        assert_eq!(gmail_subject(&summary).unwrap(), "2026/08/19 JD更新");
        assert_eq!(gmail_body(&summary), "新增：1\n更新：1\n刪除：0");
    }
    #[test]
    fn persistence_and_delete_are_transactional() {
        let mut c = Connection::open_in_memory().unwrap();
        let a = sample();
        persist_state(
            &mut c,
            SOURCE,
            std::slice::from_ref(&a),
            &HashSet::from([a.external_id.clone()]),
            true,
            "2026-08-23T07:00:00+08:00",
        )
        .unwrap();
        assert_eq!(load_known_jobs(&c).unwrap().len(), 1);
        let mut linkedin = a.clone();
        linkedin.source = "linkedin".into();
        persist_state(
            &mut c,
            "linkedin",
            std::slice::from_ref(&linkedin),
            &HashSet::from([linkedin.external_id.clone()]),
            false,
            "2026-08-23T07:00:00+08:00",
        )
        .unwrap();
        assert_eq!(load_all_known_jobs(&c).unwrap().len(), 2);
        persist_state(
            &mut c,
            SOURCE,
            &[],
            &HashSet::new(),
            true,
            "2026-08-24T07:00:00+08:00",
        )
        .unwrap();
        assert!(load_known_jobs(&c).unwrap().is_empty());
        assert_eq!(load_all_known_jobs(&c).unwrap().len(), 1);
    }

    fn tracking(c: &Connection, source: &str, external_id: &str) -> (String, String, i64) {
        c.query_row(
            "SELECT first_seen_at,last_seen_at,seen_count FROM jobs WHERE source=?1 AND external_id=?2",
            rusqlite::params![source, external_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn seen_tracking_uses_first_observation_time_and_counts_distinct_runs() {
        let mut c = Connection::open_in_memory().unwrap();
        let mut job = sample();
        job.published_at = Some("2026-08-20T00:00:00+08:00".into());
        let ids = HashSet::from([job.external_id.clone()]);

        persist_state(
            &mut c,
            SOURCE,
            &[job.clone(), job.clone()],
            &ids,
            false,
            "2026-08-23T07:00:00+08:00",
        )
        .unwrap();
        assert_eq!(
            tracking(&c, SOURCE, &job.external_id),
            (
                "2026-08-23T07:00:00+08:00".into(),
                "2026-08-23T07:00:00+08:00".into(),
                1
            )
        );

        persist_state(
            &mut c,
            SOURCE,
            std::slice::from_ref(&job),
            &ids,
            false,
            "2026-08-25T07:00:00+08:00",
        )
        .unwrap();
        assert_eq!(
            tracking(&c, SOURCE, &job.external_id),
            (
                "2026-08-23T07:00:00+08:00".into(),
                "2026-08-25T07:00:00+08:00".into(),
                2
            )
        );
    }

    #[test]
    fn seen_tracking_without_published_time_uses_first_run_time_and_survives_reload() {
        let path =
            std::env::temp_dir().join(format!("job-watcher-seen-{}.sqlite3", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut job = sample();
        job.external_id = "no-published-date".into();
        job.published_at = None;
        {
            let mut c = Connection::open(&path).unwrap();
            persist_state(
                &mut c,
                SOURCE,
                std::slice::from_ref(&job),
                &HashSet::from([job.external_id.clone()]),
                false,
                "2026-08-23T07:00:00+08:00",
            )
            .unwrap();
        }
        {
            let c = Connection::open(&path).unwrap();
            assert_eq!(
                tracking(&c, SOURCE, &job.external_id),
                (
                    "2026-08-23T07:00:00+08:00".into(),
                    "2026-08-23T07:00:00+08:00".into(),
                    1
                )
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_jobs_receive_tracking_defaults_without_resetting_published_history() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE jobs (source TEXT NOT NULL, external_id TEXT NOT NULL, title TEXT NOT NULL, company TEXT NOT NULL, location TEXT, salary TEXT, description TEXT, url TEXT NOT NULL, published_at TEXT, platform_updated_at TEXT, fetch_state TEXT NOT NULL DEFAULT 'Complete', work_site TEXT, annual_salary TEXT, last_updated TEXT, PRIMARY KEY(source, external_id)); INSERT INTO jobs(source,external_id,title,company,url,published_at) VALUES('linkedin','legacy','Old','Company','https://www.linkedin.com/jobs/view/legacy/','2026-08-20');")
            .unwrap();
        ensure_schema(&c).unwrap();
        let state = tracking(&c, "linkedin", "legacy");
        assert!(!state.0.is_empty());
        assert!(!state.1.is_empty());
        assert_eq!(state.2, 1);
    }

    #[test]
    fn challenge_pages_are_rejected() {
        assert!(is_challenge_page("Just a moment...", "challenge-platform"));
        assert!(!is_challenge_page("104", "data-job-no=abc"));
    }

    #[test]
    fn line_commands_are_exact_and_unknown_text_is_ignored() {
        assert_eq!(command_for("更新JD"), Some("update"));
        assert_eq!(command_for(" 今日履歷 "), Some("today"));
        assert_eq!(command_for("url"), None);
        assert_eq!(command_for("更新JD now"), None);
        assert_eq!(command_for("hello"), None);
    }

    #[test]
    fn history_schema_preserves_multiple_runs_and_full_description() {
        let mut listed = sample();
        listed.first_seen_at = Some("2026-08-20T00:00:00+08:00".into());
        listed.last_seen_at = Some("2026-08-25T07:00:00+08:00".into());
        listed.seen_count = 2;
        let record = change_record("updated", &listed, vec!["description".into()], None);
        let history = ChangeHistory {
            date: "2026-08-16".into(),
            runs: vec![HistoryRun {
                generated_at: "2026-08-16T14:30:00+08:00".into(),
                trigger: "manual".into(),
                new: Vec::new(),
                updated: vec![record],
                deleted: Vec::new(),
            }],
        };
        let json = serde_json::to_string(&history).unwrap();
        assert!(json.contains("description"));
        assert!(json.contains("2026-08-16T14:30:00+08:00"));
        assert!(json.contains("2026-08-20T00:00:00+08:00"));
        assert!(json.contains("\"published_at\":\"8/16\""));
        assert!(json.contains("\"seen_count\":2"));
        assert_eq!(
            serde_json::from_str::<ChangeHistory>(&json)
                .unwrap()
                .runs
                .len(),
            1
        );
    }
    fn sample() -> JobListing {
        JobListing {
            source: SOURCE.into(),
            external_id: "abc".into(),
            title: "Rust".into(),
            company: "Co".into(),
            location: Some("Taipei".into()),
            salary: Some("100".into()),
            description: Some("JD".into()),
            url: "https://www.104.com.tw/job/abc".into(),
            published_at: Some("8/16".into()),
            platform_updated_at: None,
            fetch_state: "Complete".into(),
            first_seen_at: None,
            last_seen_at: None,
            seen_count: 0,
        }
    }
}
