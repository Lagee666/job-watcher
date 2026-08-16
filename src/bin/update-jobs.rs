use anyhow::Result;
use job_watcher::{load_environment, watcher};
use tracing::{Level, error, info};

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_target(true)
        .with_line_number(true)
        .init();
    if let Err(error) = run() {
        error!(error = %error, "local incremental job update failed");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    load_environment()?;
    info!(
        "starting local incremental job update; LINE is controlled by JOB_WATCHER_LINE_BOT and cloud upload follows that setting"
    );
    let summary = watcher::run_local_update()?;
    info!(
        new = summary.new.len(),
        updated = summary.updated.len(),
        deleted = summary.deleted.len(),
        export_path = ?summary.export_path,
        "local job update completed"
    );
    Ok(())
}
