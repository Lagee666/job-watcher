use anyhow::Result;
use job_watcher::{load_environment, watcher};
use tracing::info;

fn main() -> Result<()> {
    load_environment()?;
    tracing_subscriber::fmt().with_target(false).init();
    info!(
        "starting local full job update; LINE is controlled by JOB_WATCHER_LINE_BOT and cloud upload follows that setting"
    );
    let summary = watcher::run_local_update_all()?;
    info!(
        new = summary.new.len(),
        updated = summary.updated.len(),
        deleted = summary.deleted.len(),
        export_path = ?summary.export_path,
        "local full job update completed"
    );
    Ok(())
}
