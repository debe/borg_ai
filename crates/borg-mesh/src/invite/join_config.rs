//! `borg mesh join <ticket>`'s change to the joiner's collectives config: the collective, `share: team`,
//! the joiner's repo directories (`--paths`), else a placeholder (`invite::placeholder`) that routes no
//! repo until replaced. Shown as a diff; written, with a backup, only with `--apply`.

use std::path::Path;

use borg::collective::Share;
use borg::install::Change;
use serde_json::{Value, json};

use super::placeholder;
use crate::addr::Relay;

/// The values after `--paths` up to the next flag, made absolute (routing compares path text); `~/`
/// stays as written, collective.rs expands it.
#[must_use]
pub fn paths(args: &[String]) -> Vec<String> {
    let after = args.iter().skip_while(|a| *a != "--paths").skip(1).take_while(|a| !a.starts_with("--"));
    after.map(|p| if p.starts_with("~/") { p.clone() } else { std::path::absolute(p).map_or_else(|_| p.clone(), |a| a.display().to_string()) }).collect()
}

/// The joined collective into the config at `config`: its diff, then (with `apply`) the write. Whether
/// the join goes on: not after a dry run that had something to write.
///
/// # Errors
/// What `config_change` refuses, or a failed write.
pub fn write(config: &Path, name: &str, relay: &Relay, paths: &[String], apply: bool) -> Result<bool, String> {
    let Some(change) = config_change(config, name, relay, paths)? else { return Ok(true) };
    print!("{}", borg::install::diff(&change.path, change.before.as_deref(), change.after.as_deref()));
    if change.before.is_none() {
        println!("note: there is no collectives config yet. Without one every repo belongs to the implicit `default`");
        println!("      team collective; with this file, repos it does not list belong to no collective (off).");
    }
    if !apply {
        println!("(dry run: nothing written, nobody dialled; add --apply to write the config, with a backup, and join)");
        return Ok(false);
    }
    let stamp = format!("{}Z", borg::install::utc(borg::learn::now_ms()));
    if let Some(b) = borg::install::apply(&change, &stamp).map_err(|e| format!("{}: {e}", change.path.display()))? {
        println!("backup: {}", b.display());
    }
    println!("wrote {}", change.path.display());
    Ok(true)
}

/// What `join` would add to the joiner's collectives config: the collective, `share: team`, the
/// joiner's repo directories `paths` (or, without any, a placeholder path that routes no repo until the
/// joiner replaces it), and the admin's relay setting unless it is the default. `None` when the config
/// already lists the collective as team with every one of `paths`.
///
/// # Errors
/// The config lists the collective as local (the joiner decided it never leaves the machine), or is
/// unreadable or broken.
pub fn config_change(config: &Path, name: &str, relay: &Relay, paths: &[String]) -> Result<Option<Change>, String> {
    let before = match std::fs::read_to_string(config) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", config.display())),
    };
    let listed = listed_share(config, name)?;
    let mut doc: Value = match &before {
        Some(t) => serde_json::from_str(t).map_err(|e| format!("{}: {e}", config.display()))?,
        None => json!({ "default": "off", "collectives": [] }),
    };
    let list = collectives_of(&mut doc, config)?;
    let changed = if listed.is_some() {
        extend_paths(list, name, paths)
    } else {
        list.push(new_entry(name, relay, paths));
        true
    };
    let after = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
    Ok(changed.then(|| Change { path: config.to_path_buf(), before, after: Some(after), through_links: true, backup: true }))
}

/// The `collectives` list of the config `doc` (read from `config`), added when absent.
fn collectives_of<'a>(doc: &'a mut Value, config: &Path) -> Result<&'a mut Vec<Value>, String> {
    let list = doc.as_object_mut().ok_or(format!("{}: not a JSON object", config.display()))?.entry("collectives").or_insert(json!([]));
    list.as_array_mut().ok_or(format!("{}: collectives is not a list", config.display()))
}

/// How the (valid) config at `config` already lists `name`: `None` when it does not; an error when it
/// is local, which a join must never turn into a synced collective.
fn listed_share(config: &Path, name: &str) -> Result<Option<Share>, String> {
    let Some(cfg) = borg::collective::load_from(config)? else { return Ok(None) };
    match cfg.collectives.iter().find(|c| c.name == name).map(|c| c.share) {
        Some(Share::Local) => Err(format!("{} lists {name:?} as local (never leaves this machine): change that first if you mean to join", config.display())),
        share => Ok(share),
    }
}

/// The new collective's entry: `paths`, or the placeholder without any.
fn new_entry(name: &str, relay: &Relay, paths: &[String]) -> Value {
    let paths = if paths.is_empty() { vec![placeholder(name)] } else { paths.to_vec() };
    let mut entry = json!({ "name": name, "paths": paths, "share": "team" });
    if *relay != Relay::Default {
        entry["relay"] = relay.setting().into();
    }
    entry
}

/// Adds the missing `paths` to the listed collective `name`, dropping its placeholder once a real path
/// is there; whether anything changed.
fn extend_paths(list: &mut [Value], name: &str, paths: &[String]) -> bool {
    let Some(have) = list.iter_mut().find(|c| c["name"] == name).and_then(|c| c.get_mut("paths")).and_then(|p| p.as_array_mut()) else {
        return false;
    };
    let new: Vec<&String> = paths.iter().filter(|p| !have.iter().any(|h| h == p.as_str())).collect();
    let stale = !paths.is_empty() && have.iter().any(|h| *h == placeholder(name));
    have.retain(|h| paths.is_empty() || *h != placeholder(name));
    have.extend(new.iter().map(|p| Value::from(p.as_str())));
    !new.is_empty() || stale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_adds_the_collective_with_a_placeholder_and_never_routes_a_repo() {
        let dir = std::env::temp_dir().join(format!("borg-mesh-invite-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("collectives.json");
        // No config yet: a new one, default off, the collective with its placeholder path.
        let c = config_change(&cfg, "corp", &Relay::parse("https://relay.corp.example").unwrap(), &[]).unwrap().unwrap();
        assert!(c.before.is_none());
        borg::install::apply(&c, "t").unwrap();
        let loaded = borg::collective::load_from(&cfg).unwrap().unwrap();
        assert_eq!(loaded.default, "off");
        assert_eq!(
            (loaded.collectives[0].paths.clone(), loaded.collectives[0].relay.clone()),
            (vec![placeholder("corp")], Some("https://relay.corp.example/".into()))
        );
        let r = borg::collective::route(Some(&loaded), &dir.join("some/repo").display().to_string(), &dir);
        assert_eq!(r, borg::collective::Route::Off, "no repo routes to it until the joiner names one");
        assert!(config_change(&cfg, "corp", &Relay::Default, &[]).unwrap().is_none(), "already listed: nothing to change");

        // An existing config keeps its entries; the default relay is not written.
        std::fs::write(&cfg, r#"{"default": "off", "collectives": [{"name": "mine", "paths": ["~/p"], "share": "local"}]}"#).unwrap();
        let c = config_change(&cfg, "corp", &Relay::Default, &[]).unwrap().unwrap();
        let after: serde_json::Value = serde_json::from_str(c.after.as_deref().unwrap()).unwrap();
        assert_eq!(after["collectives"][0]["name"], "mine");
        assert_eq!(after["collectives"][1], serde_json::json!({"name": "corp", "paths": [placeholder("corp")], "share": "team"}));
        assert!(config_change(&cfg, "mine", &Relay::Default, &[]).unwrap_err().contains("local"));
        std::fs::write(&cfg, "{broken").unwrap();
        assert!(config_change(&cfg, "corp", &Relay::Default, &[]).is_err());
    }

    #[test]
    fn join_with_paths_routes_them_and_replaces_the_placeholder() {
        let dir = std::env::temp_dir().join(format!("borg-mesh-invite-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("collectives.json");
        let work = dir.join("work").display().to_string();
        let c = config_change(&cfg, "corp", &Relay::Default, std::slice::from_ref(&work)).unwrap().unwrap();
        borg::install::apply(&c, "t").unwrap();
        let loaded = borg::collective::load_from(&cfg).unwrap().unwrap();
        assert_eq!(loaded.collectives[0].paths, vec![work.clone()], "no placeholder once a path is given");
        assert!(config_change(&cfg, "corp", &Relay::Default, std::slice::from_ref(&work)).unwrap().is_none(), "a second join changes nothing");

        // Joined earlier without paths: --paths replaces the placeholder, keeps the rest.
        std::fs::remove_file(&cfg).unwrap();
        borg::install::apply(&config_change(&cfg, "corp", &Relay::Default, &[]).unwrap().unwrap(), "t").unwrap();
        let c = config_change(&cfg, "corp", &Relay::Default, std::slice::from_ref(&work)).unwrap().unwrap();
        let after: serde_json::Value = serde_json::from_str(c.after.as_deref().unwrap()).unwrap();
        assert_eq!(after["collectives"][0]["paths"], serde_json::json!([work]));
    }
}
