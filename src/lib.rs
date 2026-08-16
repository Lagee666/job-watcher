pub mod watcher;

use anyhow::{Context, Result};
use std::path::Path;

pub fn load_environment() -> Result<()> {
    let system_config = Path::new("/etc/job-watcher/job-watcher.env");
    if system_config.is_file() {
        dotenvy::from_path(system_config)
            .context("failed to load /etc/job-watcher/job-watcher.env")?;
    } else {
        dotenvy::dotenv().ok();
    }
    Ok(())
}
