//! JSONL logs: one record per line, read back skipping what does not parse.

use std::path::Path;

/// Every parsable line of a JSONL file, in file order (an incomplete or unknown line is skipped).
pub(crate) fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}
