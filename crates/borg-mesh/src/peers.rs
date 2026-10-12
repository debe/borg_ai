//! Where members can be reached: `<store>/mesh/peers.json`, `{"<key>": "<hints>"}`.
//!
//! Hints (addr.rs) are comma-separated direct addresses (`ip:port`, the only form older versions
//! wrote) and relay URLs; an empty string is a member known by key alone, found by address lookup.
//! Local to this node and never synced (addresses say where a colleague's laptop is). Filled by
//! `borg mesh peer <key>[@<hints>]`, by `join`, and by what a member announces when it dials in. A
//! `serve` dials every listed key the membership still lists; the rest are ignored, not deleted, so
//! a re-invited member is reachable again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::addr::{self, Target};
use crate::identity;

pub fn path(home: &Path) -> PathBuf {
    identity::dir(home).join("peers.json")
}

pub fn load(home: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The known members as dial targets (an entry whose hints no longer parse is dialled by key).
pub fn targets(home: &Path) -> Vec<Target> {
    load(home).into_iter().map(|(key, h)| Target { key, hints: addr::hints(&h).unwrap_or_default() }).collect()
}

/// Records `key` at `hints`; false (nothing written) for a hint nobody could dial. No hints keeps
/// what is known and adds the key if it is new.
pub fn remember(home: &Path, key: &str, hints: &str) -> Result<bool, String> {
    identity::check_key(key)?;
    let Ok(parsed) = addr::hints(hints) else { return Ok(false) };
    let value = addr::join_hints(&parsed);
    let mut all = load(home);
    if all.get(key).is_some_and(|v| *v == value || value.is_empty()) {
        return Ok(true);
    }
    all.insert(key.into(), value);
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    let (p, tmp) = (path(home), path(home).with_extension("json.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(&all).unwrap()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(true)
}
