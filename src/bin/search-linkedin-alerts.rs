use anyhow::{Context, Result};
use job_watcher::{load_environment, source::linkedin::LinkedInAlertSource};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_target(true)
        .with_line_number(true)
        .with_env_filter(EnvFilter::new(
            "debug,headless_chrome::browser::tab=off,h2::codec=off,hyper_util::client::=off,yup_oauth2=off",
        ))
        .init();
    if let Err(error) = run() {
        error!(error = %error, "LinkedIn alert search failed");
        eprintln!("LinkedIn alert search failed: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    load_environment()?;
    info!("starting LinkedIn Job Alert search");
    let source = LinkedInAlertSource::from_env()?
        .context("set LINKEDIN_ENABLED=true and configure Gmail OAuth before searching alerts")?;
    info!("searching Gmail for LinkedIn Job Alert messages");
    let jobs = source.search_alerts()?;
    info!(
        job_count = jobs.len(),
        "LinkedIn Job Alert search completed"
    );
    println!("{} LinkedIn alert jobs found", jobs.len());
    println!("{}", serde_json::to_string_pretty(&jobs)?);
    Ok(())
}
