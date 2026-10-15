//! After a session: borg's vector index of the store's lessons, brought up to date with what the
//! session stored, so recall finds a teammate's lesson as soon as it arrives.

use std::path::Path;

use super::Report;
use crate::lines::SYNCED;

/// A session's `result`, once the vector index holds the lessons it stored in `home` (borg's
/// `vectors::refresh`: fail-open, nothing without the model or new lessons). On a blocking thread:
/// embedding blocks, and the index runs its own runtime.
pub async fn reindexed(home: &Path, result: Result<Report, String>) -> Result<Report, String> {
    if result.as_ref().is_ok_and(|r| r.received > 0) {
        let lessons = home.join(SYNCED[0]);
        let _ = tokio::task::spawn_blocking(move || borg::vectors::refresh(&lessons)).await;
    }
    result
}
