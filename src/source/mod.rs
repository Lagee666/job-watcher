use crate::watcher::JobListing;
use anyhow::Result;

pub mod gmail;
pub mod linkedin;

pub struct SourceSnapshot {
    pub source: String,
    pub jobs: Vec<JobListing>,
    pub processed_message_ids: Vec<String>,
    pub allow_deletions: bool,
}

pub trait JobSource: Send + Sync {
    fn source_name(&self) -> &'static str;
    fn acquire(&self) -> Result<SourceSnapshot>;
}
