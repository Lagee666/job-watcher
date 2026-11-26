use super::{AlertJob, alert_metadata, alerts::normalized};
use crate::watcher::JobListing;
use anyhow::{Context, Result, bail};
use reqwest::{Url, blocking::Client};
use scraper::{Html, Selector};
use serde_json::Value;
use std::{collections::HashSet, io::Read, time::Duration};

const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
enum Format {
    Greenhouse(String),
    Lever(String),
    JsonLd,
}

struct Request {
    original: Url,
    endpoint: Url,
    format: Format,
}

#[derive(Default)]
struct Posting {
    title: String,
    company: Option<String>,
    locations: Vec<String>,
    description: String,
    published_at: Option<String>,
}

pub(super) struct CareerFetcher {
    http: Client,
    allowed_hosts: HashSet<String>,
}

impl CareerFetcher {
    pub fn from_env() -> Result<Self> {
        let allowed_hosts = std::env::var("JOB_WATCHER_CAREER_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        Ok(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("failed to initialize company-career client")?,
            allowed_hosts,
        })
    }

    fn request(&self, raw: &str) -> Option<Request> {
        let original = Url::parse(raw).ok()?;
        if original.scheme() != "https"
            || !original.username().is_empty()
            || original.password().is_some()
            || original.port_or_known_default() != Some(443)
        {
            return None;
        }
        let host = original.host_str()?;
        let parts: Vec<_> = original
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let (endpoint, format) = match host {
            "boards.greenhouse.io" | "job-boards.greenhouse.io"
                if parts.len() == 3
                    && parts[1] == "jobs"
                    && parts[2].bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                let endpoint = Url::parse(&format!(
                    "https://boards-api.greenhouse.io/v1/boards/{}/jobs/{}",
                    parts[0], parts[2]
                ))
                .ok()?;
                (endpoint, Format::Greenhouse(parts[2].into()))
            }
            "jobs.lever.co" | "jobs.eu.lever.co" if parts.len() == 2 => {
                let api = if host == "jobs.eu.lever.co" {
                    "api.eu.lever.co"
                } else {
                    "api.lever.co"
                };
                (
                    Url::parse(&format!(
                        "https://{api}/v0/postings/{}/{}?mode=json",
                        parts[0], parts[1]
                    ))
                    .ok()?,
                    Format::Lever(parts[1].into()),
                )
            }
            _ if self.allowed_hosts.contains(host)
                && host != "localhost"
                && !host.ends_with(".localhost")
                && !host.ends_with(".local")
                && host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_err()
                && !host.ends_with(".linkedin.com")
                && host != "linkedin.com" =>
            {
                (original.clone(), Format::JsonLd)
            }
            _ => return None,
        };
        Some(Request {
            original,
            endpoint,
            format,
        })
    }

    pub fn enrich(&self, alert: &AlertJob) -> Result<Option<JobListing>> {
        self.enrich_with(alert, |url| {
            let response = self
                .http
                .get(url.clone())
                .send()
                .context("company JD request failed")?;
            let status = response.status();
            if !status.is_success() {
                bail!("company JD returned HTTP {status}; url={url}");
            }
            let mut bytes = Vec::new();
            response
                .take(MAX_BODY_BYTES + 1)
                .read_to_end(&mut bytes)
                .context("failed to read company JD")?;
            if bytes.len() as u64 > MAX_BODY_BYTES {
                bail!("company JD exceeded the 2 MiB response limit");
            }
            String::from_utf8(bytes).context("company JD response was not UTF-8")
        })
    }

    pub fn supports(&self, alert: &AlertJob) -> bool {
        !super::alerts::is_placeholder_title(&alert.title)
            && alert.company != "Unknown company"
            && alert
                .career_url
                .as_deref()
                .and_then(|url| self.request(url))
                .is_some()
    }

    pub(super) fn enrich_with(
        &self,
        alert: &AlertJob,
        mut fetch: impl FnMut(&Url) -> Result<String>,
    ) -> Result<Option<JobListing>> {
        let Some(request) = alert
            .career_url
            .as_deref()
            .and_then(|url| self.request(url))
        else {
            return Ok(None);
        };
        if super::alerts::is_placeholder_title(&alert.title) || alert.company == "Unknown company" {
            return Ok(None);
        }
        let body = fetch(&request.endpoint)?;
        let posting = match &request.format {
            Format::Greenhouse(id) => parse_greenhouse(&body, id)?,
            Format::Lever(id) => parse_lever(&body, id)?,
            Format::JsonLd => parse_json_ld(&body, alert)?,
        };
        validate_posting(&posting, alert)?;
        let mut job = alert_metadata(alert.clone());
        job.description = Some(posting.description);
        job.description_url = Some(request.original.to_string());
        job.location = job.location.or_else(|| posting.locations.first().cloned());
        job.published_at = posting.published_at;
        job.fetch_state = "Complete".into();
        Ok(Some(job))
    }
}

fn html_text(value: &str) -> String {
    let fragment = Html::parse_fragment(value);
    let text = fragment
        .root_element()
        .text()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.contains("<p>") || text.contains("<div>") || text.contains("<li>") {
        Html::parse_fragment(&text)
            .root_element()
            .text()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text
    }
}

fn field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn parse_greenhouse(body: &str, expected_id: &str) -> Result<Posting> {
    let value: Value = serde_json::from_str(body).context("invalid Greenhouse job JSON")?;
    if value.get("id").map(Value::to_string).as_deref() != Some(expected_id) {
        bail!("Greenhouse job ID did not match the email link");
    }
    let company = field(&value, "company_name");
    if company.is_empty() {
        bail!("Greenhouse response did not identify its company");
    }
    Ok(Posting {
        title: field(&value, "title"),
        company: Some(company),
        locations: value
            .get("location")
            .map(|location| field(location, "name"))
            .filter(|location| !location.is_empty())
            .into_iter()
            .collect(),
        description: html_text(&field(&value, "content")),
        published_at: value
            .get("first_published")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn parse_lever(body: &str, expected_id: &str) -> Result<Posting> {
    let value: Value = serde_json::from_str(body).context("invalid Lever job JSON")?;
    if field(&value, "id") != expected_id {
        bail!("Lever job ID did not match the email link");
    }
    let mut description = field(&value, "descriptionPlain");
    if description.is_empty() {
        description = html_text(&field(&value, "description"));
    }
    if let Some(sections) = value.get("lists").and_then(Value::as_array) {
        for section in sections {
            description.push_str(&format!(
                "\n\n{}\n{}",
                field(section, "text"),
                html_text(&field(section, "content"))
            ));
        }
    }
    let closing = field(&value, "additionalPlain");
    description.push_str(&format!(
        "\n\n{}",
        if closing.is_empty() {
            html_text(&field(&value, "additional"))
        } else {
            closing
        }
    ));
    Ok(Posting {
        title: field(&value, "text"),
        locations: value
            .get("categories")
            .map(|value| field(value, "location"))
            .filter(|location| !location.is_empty())
            .into_iter()
            .collect(),
        description: description.trim().into(),
        ..Default::default()
    })
}

fn job_postings<'a>(value: &'a Value, postings: &mut Vec<&'a Value>) {
    match value {
        Value::Array(values) => {
            for value in values {
                job_postings(value, postings);
            }
        }
        Value::Object(object) => {
            let is_job = value.get("@type").is_some_and(|kind| {
                kind.as_str() == Some("JobPosting")
                    || kind.as_array().is_some_and(|kinds| {
                        kinds.iter().any(|kind| kind.as_str() == Some("JobPosting"))
                    })
            });
            if is_job {
                postings.push(value);
            }
            if let Some(graph) = object.get("@graph") {
                job_postings(graph, postings);
            }
        }
        _ => {}
    }
}

fn posting_locations(value: &Value) -> Vec<String> {
    let Some(locations) = value.get("jobLocation") else {
        return if value.get("jobLocationType").and_then(Value::as_str) == Some("TELECOMMUTE") {
            vec!["Remote".into()]
        } else {
            Vec::new()
        };
    };
    let locations: Vec<_> = locations
        .as_array()
        .map(|values| values.iter().collect())
        .unwrap_or_else(|| vec![locations]);
    locations
        .into_iter()
        .filter_map(|location| {
            let address = location.get("address")?;
            let parts: Vec<_> = ["addressLocality", "addressRegion", "addressCountry"]
                .into_iter()
                .filter_map(|name| address.get(name).and_then(Value::as_str))
                .filter(|part| !part.is_empty())
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        })
        .collect()
}

fn parse_json_ld(body: &str, alert: &AlertJob) -> Result<Posting> {
    let document = Html::parse_document(body);
    let selector = Selector::parse("script[type='application/ld+json']")
        .map_err(|_| anyhow::anyhow!("invalid JSON-LD selector"))?;
    let mut matches = Vec::new();
    for script in document.select(&selector) {
        let Ok(value) = serde_json::from_str::<Value>(&script.text().collect::<String>()) else {
            continue;
        };
        let mut values = Vec::new();
        job_postings(&value, &mut values);
        for value in values {
            let Some(company) = value
                .get("hiringOrganization")
                .map(|company| field(company, "name"))
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let posting = Posting {
                title: field(value, "title"),
                company: Some(company),
                description: html_text(&field(value, "description")),
                published_at: value
                    .get("datePosted")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                locations: posting_locations(value),
            };
            if validate_posting(&posting, alert).is_ok() {
                matches.push(posting);
            }
        }
    }
    if matches.len() != 1 {
        bail!(
            "company page did not contain exactly one matching JobPosting (found {})",
            matches.len()
        );
    }
    matches
        .pop()
        .context("matching company JobPosting was missing")
}

fn validate_posting(posting: &Posting, alert: &AlertJob) -> Result<()> {
    if normalized(&posting.title) != normalized(&alert.title) {
        bail!("company JD title did not match the email job title");
    }
    if posting
        .company
        .as_ref()
        .is_some_and(|company| normalized(company) != normalized(&alert.company))
    {
        bail!("company JD employer did not match the email company");
    }
    if let Some(location) = alert.location.as_ref()
        && !posting.locations.is_empty()
        && !posting
            .locations
            .iter()
            .any(|candidate| normalized(candidate) == normalized(location))
    {
        bail!("company JD location did not match the email location");
    }
    if posting.description.chars().count() < 40 {
        bail!("company JD description was missing or too short");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetcher() -> CareerFetcher {
        let _ = rustls::crypto::ring::default_provider().install_default();
        CareerFetcher {
            http: Client::builder().no_proxy().build().unwrap(),
            allowed_hosts: HashSet::from(["careers.acme.test".into()]),
        }
    }

    fn alert() -> AlertJob {
        AlertJob {
            id: "4454432978".into(),
            url: "https://www.linkedin.com/jobs/view/4454432978/".into(),
            title: "Rust & Linux Engineer".into(),
            company: "Acme Systems".into(),
            location: Some("Taipei, Taiwan".into()),
            career_url: Some("https://boards.greenhouse.io/acme/jobs/1234".into()),
        }
    }

    #[test]
    fn greenhouse_exact_email_link_provides_full_jd_and_provenance() {
        let job = fetcher()
            .enrich_with(&alert(), |url| {
                assert_eq!(
                    url.as_str(),
                    "https://boards-api.greenhouse.io/v1/boards/acme/jobs/1234"
                );
                Ok(include_str!("../fixture/greenhouse_job.json").into())
            })
            .unwrap()
            .unwrap();
        assert_eq!(job.source, "linkedin");
        assert_eq!(job.external_id, "4454432978");
        assert_eq!(job.fetch_state, "Complete");
        assert_eq!(
            job.description_url.as_deref(),
            Some("https://boards.greenhouse.io/acme/jobs/1234")
        );
        assert!(
            job.description
                .as_deref()
                .unwrap()
                .contains("Qualifications")
        );
        assert!(job.description.as_deref().unwrap().contains("Rust & Linux"));
        assert!(!job.description.as_deref().unwrap().contains("<p>"));
    }

    #[test]
    fn lever_preserves_plain_text_requirements_and_closing_content() {
        let mut alert = alert();
        alert.title = "Senior C++ Engineer".into();
        alert.company = "Beta Devices".into();
        alert.location = Some("Hsinchu, Taiwan".into());
        alert.career_url = Some("https://jobs.lever.co/beta/abc-123".into());
        let job = fetcher()
            .enrich_with(&alert, |url| {
                assert_eq!(
                    url.as_str(),
                    "https://api.lever.co/v0/postings/beta/abc-123?mode=json"
                );
                Ok(include_str!("../fixture/lever_job.json").into())
            })
            .unwrap()
            .unwrap();
        let text = job.description.unwrap();
        for expected in ["Vec<T>", "Qualifications", "SocketCAN", "We welcome"] {
            assert!(text.contains(expected));
        }
    }

    #[test]
    fn company_employer_title_location_and_id_mismatches_are_rejected() {
        let body = include_str!("../fixture/greenhouse_job.json");
        for invalid in [
            body.replace("Acme Systems", "Another Company"),
            body.replace("Rust & Linux Engineer", "Senior Rust & Linux Engineer"),
            body.replace("Taipei, Taiwan", "Hsinchu, Taiwan"),
            body.replace("1234", "9999"),
        ] {
            assert!(
                fetcher()
                    .enrich_with(&alert(), |_| Ok(invalid.clone()))
                    .is_err()
            );
        }
    }

    #[test]
    fn allowlisted_json_ld_requires_one_matching_posting() {
        let mut alert = alert();
        alert.career_url = Some("https://careers.acme.test/jobs/1234".into());
        let body = include_str!("../fixture/company_job.html");
        let job = fetcher()
            .enrich_with(&alert, |_| Ok(body.into()))
            .unwrap()
            .unwrap();
        assert!(job.description.unwrap().contains("Qualifications"));
        assert!(
            fetcher()
                .enrich_with(&alert, |_| Ok(body.replace("Taipei", "Hsinchu")))
                .is_err()
        );
        assert!(
            fetcher()
                .enrich_with(&alert, |_| Ok(format!("{body}{body}")))
                .is_err()
        );
    }

    #[test]
    fn unknown_private_insecure_and_linkedin_hosts_are_not_requested() {
        for url in [
            "http://boards.greenhouse.io/acme/jobs/1234",
            "https://127.0.0.1/jobs/1234",
            "https://localhost/jobs/1234",
            "https://jobs.lever.co.evil.test/beta/abc",
            "https://www.linkedin.com/jobs/view/1234/",
            "https://unconfigured.test/jobs/1234",
            "https://user:secret@boards.greenhouse.io/acme/jobs/1234",
        ] {
            let mut alert = alert();
            alert.career_url = Some(url.into());
            assert!(
                fetcher()
                    .enrich_with(&alert, |_| panic!("unsupported host must not be requested"))
                    .unwrap()
                    .is_none()
            );
        }
    }
}
