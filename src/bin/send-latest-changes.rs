use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{FixedOffset, Utc};
use job_watcher::load_environment;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::PathBuf,
    sync::Arc,
};

const TAIPEI_OFFSET: i32 = 8 * 60 * 60;

fn main() {
    if let Err(error) = run() {
        eprintln!("sending latest changes failed: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    load_environment()?;
    let now = Utc::now()
        .with_timezone(&FixedOffset::east_opt(TAIPEI_OFFSET).context("invalid Taipei offset")?);
    let date = now.date_naive();
    let directory = std::env::var_os("JOB_WATCHER_CHANGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("changes"));
    let attachment_path = directory.join(format!("{date}.json"));
    let attachment = fs::read(&attachment_path).with_context(|| {
        format!(
            "failed to read latest change file {}",
            attachment_path.display()
        )
    })?;
    let history: Value = serde_json::from_slice(&attachment).with_context(|| {
        format!(
            "failed to parse latest change file {}",
            attachment_path.display()
        )
    })?;
    let run = history
        .get("runs")
        .and_then(Value::as_array)
        .and_then(|runs| runs.last())
        .context("latest change file contains no runs")?;
    let count = |name: &str| run.get(name).and_then(Value::as_array).map_or(0, Vec::len);
    let mut body = format!(
        "新增：{}\n更新：{}\n刪除：{}",
        count("new"),
        count("updated"),
        count("deleted")
    );
    for (key, label) in [
        ("new", "新增職缺"),
        ("updated", "更新職缺"),
        ("deleted", "刪除職缺"),
    ] {
        let Some(jobs) = run.get(key).and_then(Value::as_array) else {
            continue;
        };
        if jobs.is_empty() {
            continue;
        }
        body.push_str(&format!("\n\n{label}:"));
        for job in jobs {
            let title = job
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("(無標題)");
            let url = job.get("url").and_then(Value::as_str).unwrap_or("(無網址)");
            body.push_str(&format!("\n{title}\n{url}"));
        }
    }
    let username =
        std::env::var("GMAIL_SMTP_USERNAME").context("GMAIL_SMTP_USERNAME is required")?;
    let password =
        std::env::var("GMAIL_SMTP_APP_PASSWORD").context("GMAIL_SMTP_APP_PASSWORD is required")?;
    let recipient =
        std::env::var("JOB_WATCHER_EMAIL_TO").context("JOB_WATCHER_EMAIL_TO is required")?;
    let subject = format!("{} JD更新", date.format("%Y/%m/%d"));
    let filename = format!("{date}.json");
    let message = mime_message(&recipient, &subject, &body, &filename, &attachment);
    send_smtp(&username, &password, &recipient, &message)?;
    println!("sent latest changes from {}", attachment_path.display());
    Ok(())
}

fn send_smtp(username: &str, password: &str, recipient: &str, message: &str) -> Result<()> {
    let stream = TcpStream::connect(("smtp.gmail.com", 587))
        .context("failed to connect to smtp.gmail.com:587")?;
    let mut smtp = Smtp::Plain(BufReader::with_capacity(1, stream));
    smtp.expect(220, "SMTP greeting")?;
    smtp.command("EHLO job-watcher", 250)?;
    smtp.command("STARTTLS", 220)?;
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = ServerName::try_from("smtp.gmail.com").context("invalid SMTP server name")?;
    let tls = ClientConnection::new(Arc::new(config), server_name)
        .context("failed to initialize SMTP TLS")?;
    let stream = match smtp {
        Smtp::Plain(reader) => reader.into_inner(),
        Smtp::Tls(_) => unreachable!(),
    };
    let mut smtp = Smtp::Tls(Box::new(BufReader::with_capacity(
        1,
        StreamOwned::new(tls, stream),
    )));
    smtp.command("EHLO job-watcher", 250)?;
    smtp.command("AUTH LOGIN", 334)?;
    smtp.command(&STANDARD.encode(username.as_bytes()), 334)?;
    smtp.command(&STANDARD.encode(password.as_bytes()), 235)
        .context("SMTP authentication failed; verify Gmail app-password settings")?;
    smtp.command(&format!("MAIL FROM:<{username}>"), 250)?;
    smtp.command(&format!("RCPT TO:<{recipient}>"), 250)?;
    smtp.command("DATA", 354)?;
    smtp.write_message(message)?;
    smtp.command("QUIT", 221).context("SMTP delivery failed")
}

enum Smtp {
    Plain(BufReader<TcpStream>),
    Tls(Box<BufReader<StreamOwned<ClientConnection, TcpStream>>>),
}

impl Smtp {
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
        "To: {recipient}\r\n\
         Subject: {encoded_subject}\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\
         \r\n\
         --{boundary}\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {}\r\n\
         --{boundary}\r\n\
         Content-Type: application/json; name=\"{filename}\"\r\n\
         Content-Disposition: attachment; filename=\"{filename}\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {}\r\n\
         --{boundary}--\r\n",
        STANDARD.encode(body.as_bytes()),
        STANDARD.encode(attachment)
    )
}
