//! After a session: borg's vector index of the store's lessons, brought up to date with what the
//! session stored, so recall finds a teammate's lesson as soon as it arrives.

use std::path::Path;

use super::Report;
use crate::lines::SYNCED;

/// A session's `result`, once the vector index holds the lessons it left in `home` (borg's
/// `vectors::refresh`: fail-open, nothing without the model). Only a session that `changes_lessons`
/// touches the index. On a blocking thread: embedding blocks, and the index runs its own runtime.
pub async fn reindexed(home: &Path, result: Result<Report, String>) -> Result<Report, String> {
    if result.as_ref().is_ok_and(changes_lessons) {
        let lessons = home.join(SYNCED[0]);
        let _ = tokio::task::spawn_blocking(move || borg::vectors::refresh(&lessons)).await;
    }
    result
}

/// Whether a session changed the lessons the index holds: it stored a lesson (`Report::lessons`), or a
/// quarantine took lines of a revoked member out (`Report::quarantined`), whose vectors would otherwise
/// keep slots in recall's top hits until the next lesson. Usage, outcomes and messages are not embedded:
/// reading the lessons to find nothing new cost each such session a parse of the whole log.
pub(super) fn changes_lessons(r: &Report) -> bool {
    r.lessons > 0 || r.quarantined > 0
}
