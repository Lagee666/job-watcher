use anyhow::{Context, Result};
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::{fs, path::PathBuf, time::Duration};
use tracing::debug;

const GMAIL_API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const DEFAULT_TOKEN_FILE: &str = "/var/lib/job-watcher/gmail-oauth-token.json";
pub const GMAIL_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";

#[derive(Clone, Debug)]
pub struct GmailClient {
    http: Client,
    client_file: PathBuf,
    token_file: PathBuf,
}

#[derive(Debug, Deserialize)]
struct MessageList {
    #[serde(default)]
    messages: Vec<MessageRef>,
}

#[derive(Debug, Deserialize)]
struct MessageRef {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GmailMessage {
    id: String,
    payload: MimePart,
}

#[derive(Debug, Deserialize)]
struct MimePart {
    #[serde(rename = "mimeType", default)]
    mime_type: String,
    #[serde(default)]
    headers: Vec<MimeHeader>,
    #[serde(default)]
    body: MimeBody,
    #[serde(default)]
    parts: Vec<MimePart>,
}

#[derive(Debug, Deserialize)]
struct MimeHeader {
    name: String,
    value: String,
}

#[derive(Debug, Default, Deserialize)]
struct MimeBody {
    data: Option<String>,
}

impl GmailClient {
    pub fn from_env() -> Result<Self> {
        let client_file = std::env::var("GMAIL_OAUTH_CLIENT_FILE")
            .context("GMAIL_OAUTH_CLIENT_FILE is required for LinkedIn Job Alerts")?;
        let token_file = std::env::var("GMAIL_OAUTH_TOKEN_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_TOKEN_FILE));
        fs::metadata(&client_file)
            .with_context(|| format!("failed to read Gmail OAuth client file {client_file}"))?;
        fs::metadata(&token_file).with_context(|| {
            format!(
                "Gmail OAuth token cache {} is missing; run `cargo run --bin gmail-auth` first",
                token_file.display()
            )
        })?;
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to initialize Gmail HTTP client")?;
        Ok(Self {
            http,
            client_file: PathBuf::from(client_file),
            token_file,
        })
    }

    pub fn search_html_messages(&mut self, query: &str) -> Result<Vec<(String, String)>> {
        let access_token = self.access_token()?;
        let list: MessageList = self
            .http
            .get(format!("{GMAIL_API}/messages"))
            .bearer_auth(&access_token)
            .query(&[("q", query), ("maxResults", "100")])
            .send()
            .context("Gmail message search failed")?
            .error_for_status()
            .context("Gmail message search returned an error")?
            .json()
            .context("Gmail message search response was invalid")?;
        debug!(
            query,
            message_count = list.messages.len(),
            "Gmail messages found for LinkedIn alert search"
        );
        let mut messages = Vec::new();
        for message in list.messages {
            let full: GmailMessage = self
                .http
                .get(format!("{GMAIL_API}/messages/{}", message.id))
                .bearer_auth(&access_token)
                .query(&[("format", "full")])
                .send()
                .with_context(|| format!("Gmail message fetch failed for {}", message.id))?
                .error_for_status()
                .with_context(|| {
                    format!("Gmail message fetch returned an error for {}", message.id)
                })?
                .json()
                .with_context(|| format!("Gmail message {} was invalid", message.id))?;
            if let Some(html) = html_body(&full.payload) {
                let subject = full
                    .payload
                    .headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case("Subject"))
                    .map_or("(no subject)", |header| header.value.as_str());
                debug!(
                    message_id = %full.id,
                    subject,
                    "checking Gmail message"
                );
                messages.push((full.id, html));
            }
        }
        debug!(
            message_count = messages.len(),
            "Gmail messages with HTML bodies ready for LinkedIn alert parsing"
        );
        Ok(messages)
    }

    fn access_token(&self) -> Result<String> {
        let client_file = self.client_file.clone();
        let token_file = self.token_file.clone();
        let runtime =
            tokio::runtime::Runtime::new().context("failed to initialize Gmail OAuth runtime")?;
        runtime.block_on(async move {
            let secret = yup_oauth2::read_application_secret(&client_file)
                .await
                .context("failed to read Gmail OAuth client secret")?;
            let auth = yup_oauth2::InstalledFlowAuthenticator::builder(
                secret,
                yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
            )
            .persist_tokens_to_disk(&token_file)
            .build()
            .await
            .context("failed to initialize Gmail OAuth authenticator")?;
            let token = auth
                .token(&[GMAIL_READONLY_SCOPE])
                .await
                .context("failed to obtain Gmail read-only access token")?;
            token
                .token()
                .map(str::to_owned)
                .context("Gmail OAuth response has no access token")
        })
    }
}

fn html_body(part: &MimePart) -> Option<String> {
    if part.mime_type.eq_ignore_ascii_case("text/html")
        && let Some(data) = &part.body.data
    {
        return decode_body(data);
    }
    part.parts.iter().find_map(html_body)
}

fn decode_body(data: &str) -> Option<String> {
    URL_SAFE_NO_PAD
        .decode(data.as_bytes())
        .or_else(|_| URL_SAFE.decode(data.as_bytes()))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::{MimeBody, MimePart, decode_body, html_body};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    #[test]
    fn html_mime_part_is_decoded() {
        let encoded = URL_SAFE_NO_PAD.encode(
            b"<a href=\"https://www.linkedin.com/jobs/view/4454432978/?trackingId=x\">Role</a>",
        );
        let part = MimePart {
            mime_type: "multipart/alternative".into(),
            headers: Vec::new(),
            body: MimeBody::default(),
            parts: vec![MimePart {
                mime_type: "text/html".into(),
                headers: Vec::new(),
                body: MimeBody {
                    data: Some(encoded),
                },
                parts: Vec::new(),
            }],
        };
        assert!(html_body(&part).unwrap().contains("4454432978"));
        assert!(decode_body("not-base64").is_none());
    }
}
