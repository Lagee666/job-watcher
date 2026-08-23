use anyhow::{Context, Result};
use job_watcher::{load_environment, source::gmail::GMAIL_READONLY_SCOPE};
use std::path::PathBuf;
use yup_oauth2::{InstalledFlowAuthenticator, InstalledFlowReturnMethod};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Gmail authorization failed: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    load_environment()?;
    let client_file = std::env::var("GMAIL_OAUTH_CLIENT_FILE")
        .context("GMAIL_OAUTH_CLIENT_FILE is required for Gmail authorization")?;
    let token_file = std::env::var("GMAIL_OAUTH_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/job-watcher/gmail-oauth-token.json"));
    prepare_token_cache_directory(&token_file)?;

    let secret = yup_oauth2::read_application_secret(&client_file)
        .await
        .context("failed to read Google OAuth client secret")?;
    let auth = InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
        .persist_tokens_to_disk(&token_file)
        .build()
        .await
        .context("failed to initialize Gmail OAuth authenticator")?;
    let _token = auth
        .token(&[GMAIL_READONLY_SCOPE])
        .await
        .context("failed to obtain Gmail read-only access token")?;

    println!(
        "Gmail authorization succeeded; token cache saved to {}",
        token_file.display()
    );
    Ok(())
}

fn prepare_token_cache_directory(token_file: &std::path::Path) -> Result<()> {
    if let Some(parent) = token_file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Gmail OAuth token directory {}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::prepare_token_cache_directory;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn creates_missing_token_parent_directory() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("job-watcher-gmail-auth-{suffix}"));
        let token_file = directory.join("nested/gmail-oauth-token.json");

        prepare_token_cache_directory(&token_file).expect("token directory should be created");

        assert!(
            PathBuf::from(&token_file)
                .parent()
                .is_some_and(|path| path.is_dir())
        );
        fs::remove_dir_all(directory).expect("test directory should be removable");
    }
}
