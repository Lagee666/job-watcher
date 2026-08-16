use anyhow::Result;
use job_watcher::{load_environment, watcher};
use tracing::info;

fn main() -> Result<()> {
    load_environment()?;
    tracing_subscriber::fmt().with_target(false).init();
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
