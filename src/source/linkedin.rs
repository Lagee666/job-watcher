use super::{JobSource, SourceSnapshot, gmail::GmailClient};
use crate::watcher::JobListing;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{collections::HashSet, env, fs, thread, time::Duration};
use tracing::{debug, info};

const SOURCE: &str = "linkedin";
const DEFAULT_QUERY: &str = "from:(linkedin.com) newer_than:30d";

#[derive(Clone, Debug)]
pub struct LinkedInAlertSource {
    gmail_query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AlertJob {
    id: String,
    url: String,
    title: String,
    company: String,
    location: Option<String>,
}

#[derive(Default)]
struct LinkedInHeader {
    job_id: Option<String>,
    url: Option<String>,
    title: Option<String>,
    company: Option<String>,
    location: Option<String>,
    published_raw: Option<String>,
    work_mode: Option<String>,
    employment_type: Option<String>,
    apply_url: Option<String>,
}

impl LinkedInAlertSource {
    pub fn from_env() -> Result<Option<Self>> {
        let enabled = env::var("LINKEDIN_ENABLED")
            .ok()
            .map(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if !enabled {
            return Ok(None);
        }
        Ok(Some(Self {
            gmail_query: env::var("LINKEDIN_GMAIL_QUERY").unwrap_or_else(|_| DEFAULT_QUERY.into()),
        }))
    }

    fn discover(&self, skip_processed: bool) -> Result<(Vec<AlertJob>, Vec<String>)> {
        let mut gmail = GmailClient::from_env()?;
        let messages = gmail.search_html_messages(&self.gmail_query)?;
        let processed_file = processed_message_file();
        let processed: HashSet<String> = fs::read(&processed_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut jobs = Vec::new();
        let mut message_ids = Vec::new();
        for (message_id, html) in messages {
            if skip_processed && processed.contains(&message_id) {
                continue;
            }
            let alert_jobs = extract_alert_jobs(&html);
            debug!(
                message_id = %message_id,
                job_count = alert_jobs.len(),
                "LinkedIn job links extracted from Gmail body"
            );
            for job in &alert_jobs {
                info!(
                    message_id = %message_id,
                    external_id = %job.id,
                    url = %job.url,
                    "LinkedIn job URL found in Gmail body"
                );
            }
            jobs.extend(alert_jobs);
            message_ids.push(message_id);
        }
        let mut ids = HashSet::new();
        jobs.retain(|job| ids.insert(job.id.clone()));
        Ok((jobs, message_ids))
    }

    pub fn search_alerts(&self) -> Result<Vec<JobListing>> {
        let (alerts, _) = self.discover(false)?;
        Ok(alerts.into_iter().map(alert_metadata).collect())
    }

    pub fn acquire_with_known(
        &self,
        known: &std::collections::HashMap<String, JobListing>,
    ) -> Result<SourceSnapshot> {
        // Re-read matching alerts on each synchronization. SQL job identity,
        // rather than Gmail message state, determines whether detail fetching
        // is needed and lets known jobs update their seen counters.
        let (alerts, processed_message_ids) = self.discover(false)?;
        let unknown_alerts = alerts
            .iter()
            .filter(|alert| !known.contains_key(&alert.id))
            .count();
        let client = if unknown_alerts > 0 {
            Some(
                Client::builder()
                    .timeout(Duration::from_secs(30))
                    .user_agent("job-watcher/1.0 (+public LinkedIn job page fetch)")
                    .build()
                    .context("failed to initialize LinkedIn public-page client")?,
            )
        } else {
            None
        };
        let mut jobs = Vec::with_capacity(alerts.len());
        let mut fetched = 0;
        for alert in alerts {
            if let Some(existing) = known.get(&alert.id) {
                info!(
                    job_id = %alert.id,
                    url = %alert.url,
                    "LinkedIn job already exists in SQLite; updating seen tracking without HTTP fetch"
                );
                jobs.push(existing.clone());
                continue;
            }
            if fetched > 0 {
                thread::sleep(Duration::from_secs(10));
            }
            fetched += 1;
            jobs.push(fetch_job(
                client.as_ref().expect("client exists for unknown alert"),
                alert,
            ));
        }
        if jobs.iter().all(|job| job.fetch_state == "Complete") {
            record_processed_messages(&processed_message_file(), &processed_message_ids)?;
        }
        Ok(SourceSnapshot {
            source: SOURCE.into(),
            jobs,
            processed_message_ids,
            allow_deletions: false,
        })
    }
}

impl JobSource for LinkedInAlertSource {
    fn source_name(&self) -> &'static str {
        SOURCE
    }

    fn acquire(&self) -> Result<SourceSnapshot> {
        self.acquire_with_known(&std::collections::HashMap::new())
    }
}

fn processed_message_file() -> std::path::PathBuf {
    env::var("LINKEDIN_PROCESSED_MESSAGES_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("linkedin-processed-gmail-messages.json"))
}

fn record_processed_messages(path: &std::path::Path, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut processed: HashSet<String> = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    processed.extend(ids.iter().cloned());
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(&processed)?).with_context(|| {
        format!(
            "failed to persist processed Gmail IDs in {}",
            path.display()
        )
    })?;
    Ok(())
}

fn fetch_job(client: &Client, alert: AlertJob) -> JobListing {
    debug!(
        job_id = %alert.id,
        url = %alert.url,
        "Fetching LinkedIn job detail"
    );
    match client.get(&alert.url).send() {
        Ok(response) if response.status().is_success() => {
            let status = response.status();
            let final_url = response.url().to_string();
            match response.text() {
                Ok(html) => {
                    debug!(
                        job_id = %alert.id,
                        status = %status,
                        html_len = html.len(),
                        has_company_semantic_dom = html.contains("aria-label=\"Company,"),
                        has_expandable_description = html.contains("data-testid=\"expandable-text-box\""),
                        has_jobposting_jsonld = html.contains("\"JobPosting\""),
                        final_url = %final_url,
                        "LinkedIn job detail fetched"
                    );
                    enrich_from_html_at(alert, &html, Utc::now())
                }
                Err(error) => metadata_only(alert, "FetchFailed", &error.to_string()),
            }
        }
        Ok(response) => {
            let state = if response.status().as_u16() == 404 {
                "Removed"
            } else {
                "FetchFailed"
            };
            metadata_only(alert, state, &format!("HTTP {}", response.status()))
        }
        Err(error) => metadata_only(alert, "FetchFailed", &error.to_string()),
    }
}

fn alert_metadata(alert: AlertJob) -> JobListing {
    JobListing {
        source: SOURCE.into(),
        external_id: alert.id,
        title: alert.title,
        company: alert.company,
        location: alert.location,
        salary: None,
        description: None,
        url: alert.url,
        published_at: None,
        platform_updated_at: None,
        fetch_state: "MetadataOnly".into(),
        first_seen_at: None,
        last_seen_at: None,
        seen_count: 0,
    }
}

#[cfg(test)]
fn enrich_from_html(alert: AlertJob, html: &str) -> JobListing {
    enrich_from_html_at(alert, html, Utc::now())
}

fn enrich_from_html_at(alert: AlertJob, html: &str, scraped_at: DateTime<Utc>) -> JobListing {
    let json_ld = extract_json_ld(html);
    let header = parse_linkedin_header(html, &alert);
    let external_id = header.job_id.clone().unwrap_or_else(|| alert.id.clone());
    let title = normalize_linkedin_title(
        &header
            .title
            .clone()
            .or_else(|| {
                json_ld
                    .as_ref()
                    .and_then(|value| value.get("title").and_then(Value::as_str))
                    .map(str::to_owned)
            })
            .or_else(|| extract_meta(html, "og:title"))
            .or_else(|| extract_page_title(html))
            .unwrap_or(alert.title),
    );
    let company_source = if json_ld
        .as_ref()
        .and_then(|value| value.get("hiringOrganization"))
        .and_then(|value| value.get("name").and_then(Value::as_str))
        .is_some()
    {
        "JSON-LD"
    } else if header.company.is_some() {
        "job header"
    } else if alert.company != "Unknown company" {
        "Gmail alert"
    } else if extract_company_from_description(html).is_some() {
        "description Company section"
    } else {
        "unavailable"
    };
    let company = json_ld
        .as_ref()
        .and_then(|value| value.get("hiringOrganization"))
        .and_then(|value| value.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| header.company.clone())
        .or_else(|| (alert.company != "Unknown company").then_some(alert.company.clone()))
        .or_else(|| extract_company_from_description(html))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let location = header
        .location
        .clone()
        .or_else(|| json_ld.as_ref().and_then(json_ld_location))
        .or(alert.location)
        .or_else(|| extract_location_from_title(html));
    let description_source;
    let description = if let Some(value) = extract_expandable_description(html) {
        description_source = "expandable-text-box";
        Some(value)
    } else if let Some(value) = json_ld
        .as_ref()
        .and_then(|value| value.get("description").and_then(Value::as_str))
        .map(strip_html)
        .filter(|value| value.len() > 40)
    {
        description_source = "JSON-LD description";
        Some(value)
    } else if let Some(value) = extract_description(html) {
        description_source = "description DOM fallback";
        Some(value)
    } else {
        description_source = "unavailable";
        None
    };
    let (published_at, published_source) = json_ld
        .as_ref()
        .and_then(|value| value.get("datePosted").and_then(Value::as_str))
        .map(|value| (Some(value.to_owned()), "JSON-LD datePosted"))
        .or_else(|| {
            header.published_raw.as_deref().and_then(|raw| {
                parse_relative_posted_at(raw, scraped_at)
                    .map(|value| (Some(value), "header relative posting text (inferred)"))
            })
        })
        .or_else(|| extract_time_datetime(html).map(|value| (Some(value), "time datetime")))
        .or_else(|| {
            extract_relative_posted_at(html, scraped_at)
                .map(|value| (Some(value), "relative posting text (inferred)"))
        })
        .unwrap_or((None, "unavailable"));
    let (salary, salary_source) = json_ld
        .as_ref()
        .and_then(json_ld_salary)
        .map(|value| (Some(value), "JSON-LD baseSalary"))
        .or_else(|| {
            extract_explicit_salary(html).map(|value| (Some(value), "explicit compensation text"))
        })
        .unwrap_or((None, "not provided"));
    debug!(
        job_id = %external_id,
        title = if title.is_empty() { "missing" } else { "found" },
        company = company_source,
        location = if location.is_some() { "found" } else { "missing" },
        published_at = published_source,
        salary = salary_source,
        description = description_source,
        description_chars = description.as_ref().map_or(0, |value| value.chars().count()),
        published_at_raw = header.published_raw.as_deref().unwrap_or("missing"),
        work_mode = header.work_mode.as_deref().unwrap_or("not provided"),
        employment_type = header.employment_type.as_deref().unwrap_or("not provided"),
        apply_url = header.apply_url.as_deref().unwrap_or("not provided"),
        "LinkedIn job fields parsed"
    );
    let state = if description.is_some() {
        "Complete"
    } else {
        "ParseFailed"
    };
    JobListing {
        source: SOURCE.into(),
        external_id,
        title,
        company,
        location,
        salary,
        description,
        url: header.url.unwrap_or(alert.url),
        published_at,
        platform_updated_at: None,
        fetch_state: state.into(),
        first_seen_at: None,
        last_seen_at: None,
        seen_count: 0,
    }
}

fn metadata_only(alert: AlertJob, state: &str, detail: &str) -> JobListing {
    tracing::warn!(source = SOURCE, job_id = %alert.id, state, detail, "LinkedIn JD fetch did not produce a complete description");
    JobListing {
        source: SOURCE.into(),
        external_id: alert.id,
        title: alert.title,
        company: alert.company,
        location: alert.location,
        salary: None,
        description: None,
        url: alert.url,
        published_at: None,
        platform_updated_at: None,
        fetch_state: state.into(),
        first_seen_at: None,
        last_seen_at: None,
        seen_count: 0,
    }
}

fn extract_alert_jobs(html: &str) -> Vec<AlertJob> {
    let mut jobs = Vec::new();
    let mut search_from = 0;
    while let Some(relative) = html[search_from..].find("https://www.linkedin.com/") {
        let start = search_from + relative;
        let end = html[start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>')
            })
            .map_or(html.len(), |offset| start + offset);
        let raw_url = html[start..end].replace("&amp;", "&");
        if let Some((id, url)) = canonical_linkedin_url(&raw_url) {
            let title =
                alert_anchor_text(html, start).unwrap_or_else(|| "LinkedIn job alert".into());
            jobs.push(AlertJob {
                id,
                url,
                title,
                company: "Unknown company".into(),
                location: None,
            });
        }
        search_from = end.max(start + 1);
    }
    let mut ids = HashSet::new();
    jobs.into_iter()
        .filter(|job| ids.insert(job.id.clone()))
        .collect()
}

fn alert_anchor_text(html: &str, url_start: usize) -> Option<String> {
    let anchor_start = html[..url_start]
        .rfind("<a ")
        .or_else(|| html[..url_start].rfind("<a>"))?;
    let content_start = html[anchor_start..].find('>')? + anchor_start + 1;
    let content_end = html[content_start..].find("</a>")? + content_start;
    let text = strip_html(&html[content_start..content_end]);
    (!text.is_empty()).then_some(text)
}

fn canonical_linkedin_url(value: &str) -> Option<(String, String)> {
    let marker = "/jobs/view/";
    let start = value.find(marker)? + marker.len();
    let id: String = value[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!id.is_empty()).then(|| {
        (
            id.clone(),
            format!("https://www.linkedin.com/jobs/view/{id}/"),
        )
    })
}

fn extract_json_ld(html: &str) -> Option<Value> {
    let start = html.find("application/ld+json")?;
    let content_start = html[start..].find('>')? + start + 1;
    let content_end = html[content_start..].find("</script>")? + content_start;
    let value: Value = serde_json::from_str(html[content_start..content_end].trim()).ok()?;
    if value.is_array() {
        value
            .as_array()?
            .iter()
            .find(|item| item.get("@type").and_then(Value::as_str) == Some("JobPosting"))
            .cloned()
    } else {
        Some(value)
    }
}

fn json_ld_location(value: &Value) -> Option<String> {
    value
        .get("jobLocation")?
        .as_object()?
        .get("address")?
        .as_object()?
        .get("addressLocality")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("jobLocation")?
                .get("address")?
                .get("name")
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn json_ld_salary(value: &Value) -> Option<String> {
    let salary = value.get("baseSalary")?;
    if let Some(text) = salary.as_str() {
        return non_empty(text);
    }
    let object = salary.as_object()?;
    let currency = object.get("currency").and_then(Value::as_str).unwrap_or("");
    let value = object.get("value")?;
    if let Some(text) = value.as_str() {
        return non_empty(&format!("{currency} {text}"));
    }
    let value_object = value.as_object()?;
    let min = value_object.get("minValue").and_then(value_to_string);
    let max = value_object.get("maxValue").and_then(value_to_string);
    match (min, max) {
        (Some(min), Some(max)) => Some(format!("{currency} {min}-{max}").trim().into()),
        (Some(min), None) => Some(format!("{currency} {min}").trim().into()),
        (None, Some(max)) => Some(format!("{currency} {max}").trim().into()),
        (None, None) => None,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_f64().map(|number| number.to_string()))
}

fn extract_expandable_description(html: &str) -> Option<String> {
    let content = extract_tag_by_attribute(html, "span", "data-testid", "expandable-text-box")?;
    let text = strip_ui_suffix(&strip_html(content));
    (text.len() > 40).then_some(text)
}

fn parse_linkedin_header(html: &str, alert: &AlertJob) -> LinkedInHeader {
    let mut header = LinkedInHeader {
        job_id: Some(alert.id.clone()),
        url: Some(alert.url.clone()),
        ..Default::default()
    };
    let company_marker = "aria-label=\"Company,";
    let company_position = html
        .find(company_marker)
        .or_else(|| html.find("data-testid=\"job-details-company-name\""))
        .or_else(|| html.find("https://www.linkedin.com/jobs/view/"));
    if let Some(position) =
        company_position.map(|position| html[..position].rfind('<').unwrap_or(position))
    {
        let label = html[position..]
            .strip_prefix(company_marker)
            .and_then(|value| value.split_once('"').map(|(label, _)| label));
        header.company = label
            .and_then(|value| value.strip_prefix("Company,"))
            .map(|value| value.trim_end_matches('.').trim().to_owned())
            .filter(|value| !value.is_empty());

        let header_end = (position + 5000).min(html.len());
        let header_html = &html[position..header_end];
        header.company = header
            .company
            .or_else(|| extract_company_from_dom(header_html))
            .or_else(|| extract_link_text_containing(header_html, "/company/"));
        if let Some((id, url)) = first_canonical_url(header_html) {
            header.job_id = Some(id);
            header.url = Some(url);
        }
        if let Some((title, metadata)) = header_title_and_metadata(header_html) {
            header.title = Some(title);
            let values = tag_texts(metadata, "span");
            header.location = values.iter().find(|value| is_location(value)).cloned();
            header.published_raw = values.iter().find(|value| is_posted_text(value)).cloned();
            header.work_mode = values.iter().find(|value| is_work_mode(value)).cloned();
            header.employment_type = values
                .iter()
                .find(|value| is_employment_type(value))
                .cloned();
        }
        header.apply_url = extract_apply_url(header_html);
    }
    header
}

fn header_title_and_metadata(html: &str) -> Option<(String, &str)> {
    let mut search_from = 0;
    let (title_start, title_end) = loop {
        let relative = html[search_from..].find("<p")?;
        let start = search_from + relative;
        let end = html[start..].find("</p>")? + start;
        if !html[start..end].contains("/company/") {
            break (start, end);
        }
        search_from = end + 4;
    };
    let title_html = &html[title_start..title_end + 4];
    let title_html = title_html
        .split_once("<a href=\"#\">")
        .map_or(title_html, |(title, _)| title);
    let title = non_empty(&strip_html(title_html))?;
    let metadata_start = title_end + 4;
    let metadata_relative = html[metadata_start..].find("<p")?;
    let metadata_start = metadata_start + metadata_relative;
    let metadata_end = html[metadata_start..].find("</p>")? + metadata_start;
    Some((title, &html[metadata_start..metadata_end + 4]))
}

fn tag_texts(html: &str, tag: &str) -> Vec<String> {
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    let mut values = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = html[cursor..].find(&opening) {
        let start = cursor + relative;
        let content_start = match html[start..].find('>') {
            Some(value) => start + value + 1,
            None => break,
        };
        let content_end = match html[content_start..].find(&closing) {
            Some(value) => content_start + value,
            None => break,
        };
        if let Some(value) = non_empty(&strip_html(&html[content_start..content_end])) {
            values.push(value);
        }
        cursor = content_end + closing.len();
    }
    values
}

fn first_canonical_url(html: &str) -> Option<(String, String)> {
    let marker = "https://www.linkedin.com/jobs/view/";
    let start = html.find(marker)?;
    let end = html[start..]
        .find(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>')
        })
        .map_or(html.len(), |offset| start + offset);
    canonical_linkedin_url(&html[start..end].replace("&amp;", "&"))
}

fn extract_apply_url(html: &str) -> Option<String> {
    let marker = "aria-label=\"Apply on company website\"";
    let position = html.find(marker)?;
    let start = html[..position].rfind("<a ")?;
    let end = html[start..].find('>')? + start;
    let href_start = html[start..end].find("href=\"")? + start + 6;
    let href_end = html[href_start..].find('"')? + href_start;
    let href = &html[href_start..href_end];
    let encoded = href.split_once("url=")?.1.split('&').next()?;
    Some(percent_decode(encoded))
}

fn is_location(value: &str) -> bool {
    value.contains(',') && !is_posted_text(value) && !value.contains("people clicked apply")
}

fn is_posted_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("posted ") || lower.starts_with("reposted ")
}

fn is_work_mode(value: &str) -> bool {
    matches!(value, "On-site" | "Hybrid" | "Remote")
}

fn is_employment_type(value: &str) -> bool {
    matches!(
        value,
        "Full-time" | "Part-time" | "Contract" | "Internship" | "Temporary"
    )
}

fn parse_relative_posted_at(raw: &str, scraped_at: DateTime<Utc>) -> Option<String> {
    let text = raw
        .trim()
        .trim_start_matches("Reposted ")
        .trim_start_matches("Posted ");
    let mut words = text.split_whitespace();
    let amount = words.next()?.parse::<i64>().ok()?;
    let unit = words.next()?.to_ascii_lowercase();
    let days = if unit.starts_with("hour") {
        return Some((scraped_at - ChronoDuration::hours(amount)).to_rfc3339());
    } else if unit.starts_with("day") {
        amount
    } else if unit.starts_with("week") {
        amount * 7
    } else if unit.starts_with("month") {
        amount * 30
    } else {
        return None;
    };
    Some((scraped_at - ChronoDuration::days(days)).to_rfc3339())
}

fn percent_decode(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            output.push(byte);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn extract_company_from_dom(html: &str) -> Option<String> {
    [
        "job-details-company-name",
        "company-name",
        "job-company-name",
    ]
    .iter()
    .find_map(|test_id| {
        extract_tag_by_attribute(html, "a", "data-testid", test_id)
            .or_else(|| extract_tag_by_attribute(html, "span", "data-testid", test_id))
            .map(strip_html)
            .and_then(|value| non_empty(&value))
    })
    .or_else(|| extract_link_text_containing(html, "/company/"))
}

fn extract_company_from_description(html: &str) -> Option<String> {
    extract_labeled_section(html, "Company")
}

fn extract_labeled_section(html: &str, label: &str) -> Option<String> {
    let marker = format!(">{label}");
    let start = html.find(&marker)? + marker.len();
    strip_html(&html[start..]).lines().find_map(non_empty)
}

fn extract_time_datetime(html: &str) -> Option<String> {
    let marker = "<time";
    let start = html.find(marker)?;
    let datetime = html[start..].find("datetime=\"")? + start + 10;
    let end = html[datetime..].find('"')? + datetime;
    non_empty(&html[datetime..end])
}

fn extract_relative_posted_at(html: &str, scraped_at: DateTime<Utc>) -> Option<String> {
    let text = strip_html(html).to_ascii_lowercase();
    let (amount, unit) = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(3)
        .find_map(|words| {
            let amount = words[0].parse::<i64>().ok()?;
            if words[1].starts_with("day")
                || words[1].starts_with("week")
                || words[1].starts_with("month")
            {
                Some((amount, words[1]))
            } else {
                None
            }
        })?;
    let days = if unit.starts_with("week") {
        amount * 7
    } else if unit.starts_with("month") {
        amount * 30
    } else {
        amount
    };
    Some((scraped_at - ChronoDuration::days(days)).to_rfc3339())
}

fn extract_explicit_salary(html: &str) -> Option<String> {
    let html = remove_non_content_blocks(html);
    let body = html
        .split_once("<body")
        .and_then(|(_, value)| value.split_once('>').map(|(_, body)| body))
        .unwrap_or(html.as_str());
    let text = strip_html(body);
    text.lines()
        .map(str::trim)
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            (lower.contains("salary") || lower.contains("compensation"))
                && (line.contains('$')
                    || line.contains("USD")
                    || line.contains("TWD")
                    || line.contains("NT"))
        })
        .and_then(non_empty)
}

fn remove_non_content_blocks(html: &str) -> String {
    let mut result = html.to_owned();
    for tag in ["script", "style"] {
        let lower = result.to_ascii_lowercase();
        let mut cleaned = String::with_capacity(result.len());
        let mut cursor = 0;
        while let Some(relative) = lower[cursor..].find(&format!("<{tag}")) {
            let start = cursor + relative;
            cleaned.push_str(&result[cursor..start]);
            let Some(close_relative) = lower[start..].find(&format!("</{tag}>")) else {
                cursor = result.len();
                break;
            };
            cursor = start + close_relative + tag.len() + 3;
        }
        cleaned.push_str(&result[cursor..]);
        result = cleaned;
    }
    result
}

fn extract_link_text_containing(html: &str, href_fragment: &str) -> Option<String> {
    let mut search_from = 0;
    while let Some(relative) = html[search_from..].find("<a") {
        let start = search_from + relative;
        let end = html[start..].find("</a>")? + start + 4;
        let anchor = &html[start..end];
        if anchor.contains(href_fragment) {
            let text_start = anchor.find('>')? + 1;
            return non_empty(&strip_html(&anchor[text_start..anchor.len() - 4]));
        }
        search_from = end;
    }
    None
}

fn extract_tag_by_attribute<'a>(
    html: &'a str,
    tag: &str,
    attribute: &str,
    expected: &str,
) -> Option<&'a str> {
    let opening_marker = format!("<{tag}");
    let attribute_marker = format!("{attribute}=\"{expected}\"");
    let mut search_from = 0;
    let opening_end = loop {
        let relative = html[search_from..].find(&opening_marker)?;
        let candidate = search_from + relative;
        let end = html[candidate..].find('>')? + candidate + 1;
        if html[candidate..end].contains(&attribute_marker) {
            break end;
        }
        search_from = end;
    };
    let closing_marker = format!("</{tag}>");
    let mut cursor = opening_end;
    let mut depth = 1;
    while cursor < html.len() {
        let next_open = html[cursor..]
            .find(&opening_marker)
            .map(|offset| cursor + offset);
        let next_close = html[cursor..]
            .find(&closing_marker)
            .map(|offset| cursor + offset);
        match (next_open, next_close) {
            (_, Some(close)) if next_open.is_none_or(|open| close < open) => {
                depth -= 1;
                if depth == 0 {
                    return Some(&html[opening_end..close]);
                }
                cursor = close + closing_marker.len();
            }
            (Some(open), _) => {
                let end = html[open..].find('>')? + open;
                if !html[open..=end].ends_with("/>") {
                    depth += 1;
                }
                cursor = end + 1;
            }
            _ => return None,
        }
    }
    None
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then_some(value.to_owned())
}

fn extract_meta(html: &str, property: &str) -> Option<String> {
    let start = html.find(&format!("property=\"{property}\""))?;
    let content = html[start..].find("content=\"")? + start + 9;
    let end = html[content..].find('"')? + content;
    Some(html[content..end].to_owned())
}

fn extract_page_title(html: &str) -> Option<String> {
    let start = html.find("<title>")? + "<title>".len();
    let end = html[start..].find("</title>")? + start;
    let title = strip_html(&html[start..end]);
    non_empty(&normalize_linkedin_title(&title))
}

fn extract_location_from_title(html: &str) -> Option<String> {
    let start = html.find("<title>")? + "<title>".len();
    let end = html[start..].find("</title>")? + start;
    let title = strip_html(&html[start..end]);
    let location = title.split_once(" in ")?.1.split(" | LinkedIn").next()?;
    non_empty(location)
}

fn normalize_linkedin_title(title: &str) -> String {
    let title = title.split(" | LinkedIn").next().unwrap_or(title).trim();
    let title = title
        .split_once(" hiring ")
        .map_or(title, |(_, value)| value)
        .split_once(" in ")
        .map_or(title, |(value, _)| value)
        .trim();
    title.to_owned()
}

fn extract_description(html: &str) -> Option<String> {
    if let Some(content) = extract_tag_by_attribute(
        html,
        "div",
        "class",
        "description__text description__text--rich",
    ) {
        let text = strip_html(content);
        if text.len() > 80 {
            return Some(text);
        }
    }
    [
        "show-more-less-html__markup",
        "description__text",
        "job-description",
    ]
    .iter()
    .filter_map(|class| {
        let start = html.find(&format!("class=\"{class}"))?;
        let content_start = html[start..].find('>')? + start + 1;
        let content_end = html[content_start..].find("</")? + content_start;
        let text = strip_html(&html[content_start..content_end]);
        (text.len() > 80).then_some(text)
    })
    .max_by_key(String::len)
}

fn strip_html(value: &str) -> String {
    let value = value
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</li>", "\n")
        .replace("</p>", "\n")
        .replace("</div>", "\n")
        .replace("</section>", "\n");
    let mut output = String::new();
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&gt;", ">")
        .lines()
        .map(str::split_whitespace)
        .map(|line| line.collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_ui_suffix(value: &str) -> String {
    value
        .trim_end_matches("... more")
        .trim_end_matches("… more")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use tokio::fs;

    use super::{
        AlertJob, canonical_linkedin_url, enrich_from_html, enrich_from_html_at,
        extract_alert_jobs, parse_linkedin_header,
    };

    #[test]
    fn alert_urls_are_canonical_and_deduplicated() {
        let html = r#"<a href="https://www.linkedin.com/jobs/view/4454432978/?refId=abc&trackingId=xyz">Role</a><a href="https://www.linkedin.com/jobs/view/4454432978/?trackingId=other">Again</a><a href="https://www.linkedin.com/jobs/view/1234567890/">Other</a>"#;
        let jobs = extract_alert_jobs(html);
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            jobs[0].url,
            "https://www.linkedin.com/jobs/view/4454432978/"
        );
    }

    #[test]
    fn public_job_fixture_preserves_complete_description() {
        let html = std::fs::read_to_string("src/source/fixture/linkedin_public.html")
            .expect("LinkedIn public fixture should be readable");
        let job = enrich_from_html(
            AlertJob {
                id: "4454432978".into(),
                url: "https://www.linkedin.com/jobs/view/4454432978/".into(),
                title: "Alert title".into(),
                company: "Alert company".into(),
                location: None,
            },
            &html,
        );
        assert_eq!(job.fetch_state, "Complete");
        assert!(
            job.description
                .as_deref()
                .is_some_and(|description| description.contains("Qualifications"))
        );
        assert_eq!(job.company, "Example Corp");
        assert_eq!(job.salary.as_deref(), Some("TWD 100000-150000"));
        assert_eq!(job.published_at.as_deref(), Some("2026-08-20"));
        assert!(
            !job.description
                .as_deref()
                .is_some_and(|description| description.ends_with("... more"))
        );
    }

    #[test]
    fn dom_fallback_extracts_full_expandable_description_and_metadata() {
        let job = enrich_from_html(
            AlertJob {
                id: "1".into(),
                url: "https://www.linkedin.com/jobs/view/1/".into(),
                title: "Alert title".into(),
                company: "Alert company".into(),
                location: Some("Taipei".into()),
            },
            r#"<h1>Page title</h1><span data-testid="job-details-company-name">DOM Company</span><time datetime="2026-08-21"></time><span data-testid="expandable-text-box"><strong>Responsibilities</strong> Build reliable backend systems for production services and maintain Linux infrastructure.<button data-testid="expandable-text-button">... more</button></span>"#,
        );
        assert_eq!(job.company, "DOM Company");
        assert_eq!(job.published_at.as_deref(), Some("2026-08-21"));
        assert!(job.description.as_deref().is_some_and(|text| {
            text.contains("Build reliable backend systems") && !text.ends_with("... more")
        }));
        assert_eq!(job.salary, None);
    }

    #[test]
    fn relative_posted_time_is_inferred_from_scrape_time() {
        let job = enrich_from_html_at(
            AlertJob {
                id: "2".into(),
                url: "https://www.linkedin.com/jobs/view/2/".into(),
                title: "Alert title".into(),
                company: "Alert company".into(),
                location: None,
            },
            r#"Reposted 3 days ago <span data-testid="expandable-text-box">A complete job description with enough meaningful content to pass extraction.</span>"#,
            chrono::DateTime::parse_from_rfc3339("2026-08-23T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert_eq!(
            job.published_at.as_deref(),
            Some("2026-08-20T00:00:00+00:00")
        );
    }

    #[test]
    fn current_header_dom_extracts_primary_job_metadata() {
        let html = r##"
            <div aria-label="Company, Microsoft.">
                <a href="https://www.linkedin.com/company/microsoft/life/">Microsoft</a>
            </div>
            <p>Software Engineer 2 - Performance <a href="#"><span aria-label="Verified job">verified</span></a></p>
            <p>
                <span>Taipei, Taipei City, Taiwan</span>
                · <span>Reposted 1 day ago</span>
                · <span>91 people clicked apply</span>
                · <span>Hybrid</span>
                · <span>Full-time</span>
            </p>
            <a href="https://www.linkedin.com/jobs/view/4447522099/">View job</a>
            <a aria-label="Apply on company website" href="https://www.linkedin.com/safety/go/?url=https%3A%2F%2Fapply.careers.microsoft.com%2Fjob%2F1">Apply</a>
            <span data-testid="expandable-text-box">Complete About the job content with responsibilities and qualifications.<button data-testid="expandable-text-button">... more</button></span>
        "##;
        let header = parse_linkedin_header(
            html,
            &AlertJob {
                id: "4418261323".into(),
                url: "https://www.linkedin.com/jobs/view/4418261323/".into(),
                title: "Alert title".into(),
                company: "Unknown company".into(),
                location: None,
            },
        );
        assert_eq!(header.job_id.as_deref(), Some("4447522099"));
        assert_eq!(
            header.title.as_deref(),
            Some("Software Engineer 2 - Performance")
        );
        assert_eq!(header.company.as_deref(), Some("Microsoft"));
        assert_eq!(
            header.location.as_deref(),
            Some("Taipei, Taipei City, Taiwan")
        );
        assert_eq!(header.published_raw.as_deref(), Some("Reposted 1 day ago"));
        assert_eq!(header.work_mode.as_deref(), Some("Hybrid"));
        assert_eq!(header.employment_type.as_deref(), Some("Full-time"));
        assert_eq!(
            header.apply_url.as_deref(),
            Some("https://apply.careers.microsoft.com/job/1")
        );
    }

    #[tokio::test]
    #[ignore]
    async fn example_data_fixture_extracts_complete_linkedin_description() {
        let html = fs::read_to_string("example_data/linkedin_job_detail.html")
            .await
            .expect("LinkedIn detail fixture should be readable");
        let job = enrich_from_html_at(
            AlertJob {
                id: "4418261323".into(),
                url: "https://www.linkedin.com/jobs/view/4418261323/".into(),
                title: "LinkedIn job alert".into(),
                company: "Unknown company".into(),
                location: None,
            },
            &html,
            chrono::DateTime::parse_from_rfc3339("2026-08-20T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let description = job
            .description
            .expect("LinkedIn example body should contain a complete JD");
        assert_eq!(job.fetch_state, "Complete");
        assert_eq!(job.external_id, "4418261323");
        assert_eq!(
            job.title,
            "Customer Engineering – Embedded Software Boot and Stability Engineer"
        );
        assert_eq!(job.company, "Qualcomm Communication Technologies Ltd.");
        assert_eq!(job.location.as_deref(), Some("Taipei, Taipei City, Taiwan"));
        assert_eq!(job.published_at.as_deref(), Some("2026-08-19"));
        assert_eq!(job.salary, None);
        assert_eq!(job.url, "https://www.linkedin.com/jobs/view/4418261323/");

        let expect_description = fs::read_to_string("example_data/description_example.txt")
            .await
            .unwrap();
        let normalize = |value: &str| value.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalize(&description).contains(&normalize(&expect_description)),
            "the example description should be contained in the parsed LinkedIn JD"
        );
        assert!(!description.contains("... more"));
    }

    #[tokio::test]
    #[ignore]
    async fn example_data_fixture_extracts_complete_linkedin_description_1() {
        let html = fs::read_to_string("example_data/Applied Materials Taiwan.html")
            .await
            .expect("Applied Materials fixture should be readable");
        let job = enrich_from_html_at(
            AlertJob {
                id: "4441813838".into(),
                url: "https://www.linkedin.com/jobs/view/4441813838/".into(),
                title: "LinkedIn job alert".into(),
                company: "Unknown company".into(),
                location: None,
            },
            &html,
            chrono::DateTime::parse_from_rfc3339("2026-08-20T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let description = job
            .description
            .expect("LinkedIn example body should contain a complete JD");
        assert_eq!(job.fetch_state, "Complete");
        assert_eq!(job.external_id, "4441813838");
        assert_eq!(job.title, "Software Engineer");
        assert_eq!(job.company, "Applied Materials Taiwan");
        assert_eq!(
            job.location.as_deref(),
            Some("Hsinchu City, Taiwan, Taiwan")
        );
        assert_eq!(
            job.published_at.as_deref(),
            Some("2026-08-19T09:00:00+00:00")
        );
        assert_eq!(job.salary, None);
        assert_eq!(job.url, "https://www.linkedin.com/jobs/view/4441813838/");

        for section in [
            "Who We Are",
            "Role Responsibilities",
            "Minimum Qualifications",
            "Additional Information",
        ] {
            assert!(
                description.contains(section),
                "parsed LinkedIn JD should contain section: {section}"
            );
        }
        assert!(!description.contains("... more"));
    }

    #[test]
    fn url_parser_ignores_tracking_parameters() {
        assert_eq!(
            canonical_linkedin_url(
                "https://www.linkedin.com/jobs/view/4454432978/?refId=abc&trackingId=xyz"
            ),
            Some((
                "4454432978".into(),
                "https://www.linkedin.com/jobs/view/4454432978/".into()
            ))
        );
    }
}
