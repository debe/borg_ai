//! `borg setup`'s collectives config: what the engineer chose ([`Want`]) merged into `collectives.json`
//! as one [`Change`]. An existing config is extended, never replaced: its keys, entries and order stay,
//! a chosen collective it already lists only gains the missing paths, and a broken one is refused (setup
//! never rewrites a file it cannot read). New entries are a `collective::Entry` written as JSON, and the
//! result must load as a `collective::Config` that passes `check` before it is offered.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::collective::{self, Entry, Share};
use crate::install::Change;

/// The collectives the engineer chose.
#[derive(Debug, Default)]
pub struct Want {
    pub entries: Vec<Entry>,
    /// Repos no collective may ever see.
    pub off: Vec<String>,
    /// The local collective for personal lessons (reach.rs); set only where the config names none.
    pub personal: Option<String>,
    /// `default` of a new config (an existing one keeps its own).
    pub default: String,
}

/// The change that writes `want` into the config at `path`; `None` when it already says all of it.
///
/// # Errors
/// An unreadable or invalid existing config, a collective listed with the other `share`, or a result
/// that would not load.
pub fn change(path: &Path, want: &Want) -> Result<Option<Change>, String> {
    let before = crate::install::change::read(path).map_err(|e| format!("{}: cannot read ({e})", path.display()))?;
    let mut doc = existing(path, before.as_deref(), &want.default)?;
    let obj = doc.as_object_mut().ok_or(format!("{}: not a JSON object", path.display()))?;
    // A new config is always written: without one every repo would join the implicit `default` team
    // collective (collective.rs).
    if !(add(obj, want)? || before.is_none()) {
        return Ok(None);
    }
    let after = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
    validate(&after).map_err(|e| format!("the new {} would be invalid: {e}", path.display()))?;
    Ok(Some(Change { path: path.to_path_buf(), before, after: Some(after), through_links: true, backup: true }))
}

/// The config as it is (it must load: setup never rewrites a broken one), or a new one.
fn existing(path: &Path, before: Option<&str>, default: &str) -> Result<Value, String> {
    let Some(text) = before else { return Ok(json!({ "default": default, "collectives": [] })) };
    collective::load_from(path).map_err(|e| format!("{e}: fix it first (`borg doctor`); setup never rewrites a broken config"))?;
    serde_json::from_str(text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Adds `want` to the config's object; whether anything was missing.
fn add(obj: &mut Map<String, Value>, want: &Want) -> Result<bool, String> {
    let mut changed = false;
    for e in &want.entries {
        changed |= merge(obj, e)?;
    }
    changed |= add_all(obj, "off", &want.off);
    if let Some(p) = want.personal.as_ref().filter(|_| !obj.contains_key("personal")) {
        obj.insert("personal".into(), p.as_str().into());
        changed = true;
    }
    Ok(changed)
}

/// Whether `text` loads as a config every hook accepts.
fn validate(text: &str) -> Result<(), String> {
    let cfg: collective::Config = serde_json::from_str(text).map_err(|e| e.to_string())?;
    cfg.check()
}

/// `e` added to the config's collectives, or its missing paths to the entry of that name ([`agrees`]).
fn merge(obj: &mut Map<String, Value>, e: &Entry) -> Result<bool, String> {
    let list = obj.entry("collectives").or_insert(json!([])).as_array_mut().ok_or("collectives is not a list")?;
    let Some(have) = list.iter_mut().find(|c| c["name"] == e.name.as_str()) else {
        list.push(to_json(e));
        return Ok(true);
    };
    agrees(have, e)?;
    Ok(have.as_object_mut().is_some_and(|c| add_all(c, "paths", &e.paths)))
}

/// `e` as the config writes it: the optional fields only when set (setup sets no experts).
fn to_json(e: &Entry) -> Value {
    let mut v = json!({ "name": e.name, "paths": e.paths, "share": if e.share == Share::Local { "local" } else { "team" } });
    for (key, value) in [("relay", &e.relay), ("mode", &e.mode)] {
        if let Some(x) = value {
            v[key] = x.as_str().into();
        }
    }
    v
}

/// An error when the listed entry `have` says otherwise than `e`: the other `share` (setup never turns
/// a local collective into a synced one), or another relay or mode than the one asked for (setup never
/// rewrites one; the default asked for keeps what is listed).
fn agrees(have: &Value, e: &Entry) -> Result<(), String> {
    let share: Share = have.get("share").and_then(|s| serde_json::from_value(s.clone()).ok()).unwrap_or_default();
    if share != e.share {
        return Err(format!("collective {:?} is already listed with \"share\": {share:?}: choose another name", e.name));
    }
    for (key, asked) in [("relay", &e.relay), ("mode", &e.mode)] {
        let listed = have.get(key).and_then(Value::as_str);
        if let Some(a) = asked.as_deref().filter(|a| listed != Some(*a)) {
            let listed = listed.unwrap_or("the default");
            return Err(format!("collective {:?} is already listed with {key} {listed}, not {a}: change it in collectives.json first", e.name));
        }
    }
    Ok(())
}

/// Adds the `items` not yet in the list `key`; whether any were.
fn add_all(obj: &mut Map<String, Value>, key: &str, items: &[String]) -> bool {
    if items.is_empty() {
        return false;
    }
    let Some(list) = obj.entry(key).or_insert(json!([])).as_array_mut() else { return false };
    let new: Vec<&String> = items.iter().filter(|i| !list.iter().any(|v| v == i.as_str())).collect();
    list.extend(new.iter().map(|i| Value::from(i.as_str())));
    !new.is_empty()
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// `p` as the config writes it: `~/…` under HOME, else absolute.
///
/// # Errors
/// When `p` is not a directory.
pub fn dir(p: &str) -> Result<String, String> {
    let full = std::path::absolute(collective::expand(p)).map_err(|e| format!("{p}: {e}"))?;
    if !full.is_dir() {
        return Err(format!("{p}: not a directory"));
    }
    Ok(match full.strip_prefix(home()) {
        Ok(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => full.display().to_string(),
    })
}

/// Where repos probably live: the current directory's parent (inside HOME), else the usual roots that
/// exist.
#[must_use]
pub fn default_roots() -> Vec<String> {
    let home = home();
    let parent = std::env::current_dir().ok().and_then(|d| d.parent().map(Path::to_path_buf)).filter(|p| p.starts_with(&home) && *p != home);
    if let Some(p) = parent.and_then(|p| dir(&p.display().to_string()).ok()) {
        return vec![p];
    }
    ["src", "code", "projects", "repos", "dev", "git", "work"].iter().filter_map(|d| dir(&home.join(d).display().to_string()).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, paths: &[&str], share: Share) -> Entry {
        Entry { name: name.into(), paths: paths.iter().map(|p| (*p).to_string()).collect(), share, experts: Vec::new(), relay: None, mode: None }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-setup-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("collectives.json")
    }

    #[test]
    fn a_new_config_is_written_through_the_types_and_a_second_run_changes_nothing() {
        let path = tmp("new");
        let want = Want { entries: vec![entry("private", &["/p"], Share::Local)], personal: Some("private".into()), default: "off".into(), ..Want::default() };
        let c = change(&path, &want).unwrap().unwrap();
        let v: Value = serde_json::from_str(c.after.as_deref().unwrap()).unwrap();
        assert_eq!(v, json!({"default": "off", "collectives": [{"name": "private", "paths": ["/p"], "share": "local"}], "personal": "private"}));
        crate::install::apply(&c, "t").unwrap();
        assert!(change(&path, &want).unwrap().is_none());
    }

    #[test]
    fn an_existing_config_is_extended_never_clobbered() {
        let path = tmp("extend");
        std::fs::write(&path, r#"{"default": "work", "engineers": [], "collectives": [{"name": "work", "paths": ["/w"], "mode": "hook"}]}"#).unwrap();
        let want = Want { entries: vec![entry("work", &["/w", "/w2"], Share::Team)], off: vec!["/secret".into()], default: "off".into(), ..Want::default() };
        let v: Value = serde_json::from_str(change(&path, &want).unwrap().unwrap().after.as_deref().unwrap()).unwrap();
        assert_eq!(v["default"], "work", "an existing default stays");
        assert_eq!(v["collectives"][0], json!({"name": "work", "paths": ["/w", "/w2"], "mode": "hook"}));
        assert_eq!(v["off"], json!(["/secret"]));

        let mut relay = entry("work", &["/w"], Share::Team);
        relay.relay = Some("disabled".into());
        let asked = Want { entries: vec![relay], ..Want::default() };
        assert!(change(&path, &asked).unwrap_err().contains("relay the default, not disabled"), "never dropped silently");
        let local = Want { entries: vec![entry("work", &["/x"], Share::Local)], ..Want::default() };
        assert!(change(&path, &local).unwrap_err().contains("already listed"), "a team collective never turns local, nor back");
        std::fs::write(&path, "{broken").unwrap();
        assert!(change(&path, &want).unwrap_err().contains("fix it first"));
    }

    #[test]
    fn an_invalid_result_is_refused() {
        let path = tmp("invalid");
        let want = Want { entries: vec![entry("t", &["/t"], Share::Team)], personal: Some("t".into()), default: "off".into(), ..Want::default() };
        assert!(change(&path, &want).unwrap_err().contains("would be invalid"));
    }
}
