//! Command hooks merged into a harness's JSON config (Claude Code's settings.json, and any harness with
//! the same shape: `{"hooks": {<native event>: [{"hooks": [{"type": "command", "command": …}]}]}}`).
//!
//! Someone else's config is precious: merging keeps every other key and hook as it was (and their
//! order), re-running changes nothing, and uninstall removes only borg's own commands,
//! `<…/borg> hook <harness> <event>`, recognised by the binary's name `borg`. A hook an older borg
//! installed and this one does not (`retired`) goes at install and uninstall alike, so an upgrade
//! needs nothing but the reinstall it already does.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{Harness, Wiring};

/// One harness's command hooks: its name (in the command) and its event table.
#[derive(Debug, Clone)]
pub struct Hooks {
    pub harness: &'static str,
    /// (native event, canonical event)
    pub events: &'static [(&'static str, &'static str)],
    /// (canonical event, `timeout` in seconds) for the events given one.
    pub timeouts: &'static [(&'static str, u64)],
    /// (native event, canonical event) an older borg installed and this one removes.
    pub retired: &'static [(&'static str, &'static str)],
    /// The config file's name, for messages ("settings.json").
    pub file: String,
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    match s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        Some(inner) => inner.replace(r"'\''", "'"),
        None => s.to_string(),
    }
}

/// Whether `bin` is named `borg`: that basename is how [`Hooks::recognise`] knows its own hooks.
pub fn named_borg(bin: &Path) -> bool {
    let base = bin.file_name().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
    base == "borg" || base == "borg.exe"
}

/// Serialises like Claude Code does (2-space JSON), keeping the original's trailing newline.
pub fn render(m: Map<String, Value>, original: Option<&str>) -> String {
    let mut s = serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default();
    if original.is_none_or(|o| o.trim().is_empty() || o.ends_with('\n')) {
        s.push('\n');
    }
    s
}

impl Hooks {
    /// `h`'s command hooks; None for a harness wired otherwise.
    pub fn of(h: &dyn Harness) -> Option<Hooks> {
        let Wiring::CommandHooks { events, timeouts, retired, .. } = h.wiring() else { return None };
        let file = h.target(Path::new(""), &|_| None).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        Some(Hooks { harness: h.name(), events, timeouts, retired, file })
    }

    pub fn command(&self, bin: &Path, event: &str) -> String {
        format!("{} hook {} {event}", shell_quote(&bin.display().to_string()), self.harness)
    }

    /// `(binary, canonical event)` if this hook command is one borg installs or once installed:
    /// `<…/borg> hook <harness> <event>`.
    pub fn recognise(&self, cmd: &str) -> Option<(PathBuf, &'static str)> {
        self.events.iter().chain(self.retired).find_map(|(_, ev)| {
            let prog = cmd.trim().strip_suffix(&format!(" hook {} {ev}", self.harness))?;
            let prog = PathBuf::from(unquote(prog));
            named_borg(&prog).then_some((prog, *ev))
        })
    }

    /// borg's hook entry for one canonical event.
    fn entry(&self, bin: &Path, ev: &str) -> Value {
        let mut e = json!({"type": "command", "command": self.command(bin, ev)});
        if let Some((_, s)) = self.timeouts.iter().find(|(x, _)| *x == ev) {
            e["timeout"] = json!(s);
        }
        e
    }

    pub fn parse(&self, text: Option<&str>) -> Result<Map<String, Value>, String> {
        let file = &self.file;
        match text.filter(|t| !t.trim().is_empty()) {
            None => Ok(Map::new()),
            Some(t) => match serde_json::from_str(t) {
                Ok(Value::Object(m)) => Ok(m),
                Ok(_) => Err(format!("{file} is not a JSON object")),
                Err(e) => Err(format!("{file} is not valid JSON ({e}); fix it first, nothing was changed")),
            },
        }
    }

    /// Removes borg's hook entries for `ev` from one event's groups; a group left empty by that goes too.
    fn remove_borg(&self, groups: &mut Vec<Value>, ev: &str) -> usize {
        let mut removed = 0;
        groups.retain_mut(|g| {
            let Some(hooks) = g.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            let n = hooks.len();
            hooks.retain(|h| h.get("command").and_then(Value::as_str).and_then(|c| self.recognise(c)).is_none_or(|(_, e)| e != ev));
            removed += n - hooks.len();
            !(hooks.is_empty() && n > 0)
        });
        removed
    }

    /// borg's (first) hook entry for `ev` among one event's groups.
    fn slot_of<'a>(&self, groups: &'a mut [Value], ev: &str) -> Option<&'a mut Value> {
        groups
            .iter_mut()
            .filter_map(|g| g.get_mut("hooks").and_then(Value::as_array_mut))
            .flatten()
            .find(|h| h.get("command").and_then(Value::as_str).and_then(|c| self.recognise(c)).is_some_and(|(_, e)| e == ev))
    }

    /// The config with borg's hooks merged in (`Ok(None)`: already exactly so).
    ///
    /// `bin` must be named `borg`: that basename is how [`Hooks::recognise`] knows its own hooks, so a
    /// binary named otherwise would be duplicated on every install and never removed by uninstall.
    pub fn install(&self, text: Option<&str>, bin: &Path) -> Result<Option<String>, String> {
        if !named_borg(bin) {
            return Err(format!(
                "{} is not named borg, so its hooks could not be recognised again (duplicates, no uninstall); point BORG_BIN at a binary named borg (a symlink will do)",
                bin.display()
            ));
        }
        let file = &self.file;
        let mut m = self.parse(text)?;
        let orig = m.clone();
        let hooks = m.entry("hooks").or_insert_with(|| json!({}));
        let Some(hooks) = hooks.as_object_mut() else { return Err(format!("{file}: \"hooks\" is not an object")) };
        for (event, ev) in self.events {
            let want = self.entry(bin, ev);
            let groups = hooks.entry(*event).or_insert_with(|| json!([]));
            let Some(groups) = groups.as_array_mut() else { return Err(format!("{file}: hooks.{event} is not an array")) };
            let ours: Vec<Value> = groups
                .iter()
                .flat_map(|g| g.get("hooks").and_then(Value::as_array).cloned().unwrap_or_default())
                .filter(|h| h.get("command").and_then(Value::as_str).and_then(|c| self.recognise(c)).is_some_and(|(_, e)| e == *ev))
                .collect();
            if ours == [want.clone()] {
                continue; // already wired to this binary, with this timeout
            }
            if ours.len() == 1 {
                // A stale binary or timeout: replace it where it stands, so no other hook changes position
                // (Codex keys its trust by a hook's index).
                if let Some(slot) = self.slot_of(groups, ev) {
                    *slot = want;
                    continue;
                }
            }
            self.remove_borg(groups, ev); // none yet, or duplicates: exactly one, after everything else
            groups.push(json!({"hooks": [want]}));
        }
        self.remove_retired(hooks);
        Ok((m != orig).then(|| render(m, text)))
    }

    /// Removes borg's retired hooks; an event list they alone filled goes with them. How many went.
    fn remove_retired(&self, hooks: &mut Map<String, Value>) -> usize {
        let mut removed = 0;
        for (event, ev) in self.retired {
            let Some(groups) = hooks.get_mut(*event).and_then(Value::as_array_mut) else { continue };
            let n = self.remove_borg(groups, ev);
            if n > 0 && groups.is_empty() {
                hooks.shift_remove(*event);
            }
            removed += n;
        }
        removed
    }

    /// The config without borg's hooks (`Ok(None)`: there were none; `Ok(Some(None))`: nothing else
    /// was in it, so the file goes). An event list or `hooks` object that only borg filled goes with them.
    pub fn uninstall(&self, text: Option<&str>) -> Result<Option<Option<String>>, String> {
        if text.is_none() {
            return Ok(None);
        }
        let mut m = self.parse(text)?;
        let Some(hooks) = m.get_mut("hooks").and_then(Value::as_object_mut) else { return Ok(None) };
        let mut removed = 0;
        for (event, ev) in self.events {
            let Some(groups) = hooks.get_mut(*event).and_then(Value::as_array_mut) else { continue };
            let n = self.remove_borg(groups, ev);
            if n > 0 && groups.is_empty() {
                hooks.shift_remove(*event);
            }
            removed += n;
        }
        removed += self.remove_retired(hooks);
        if removed == 0 {
            return Ok(None);
        }
        if hooks.is_empty() {
            m.shift_remove("hooks");
        }
        Ok(Some((!m.is_empty()).then(|| render(m, text))))
    }

    /// Which canonical events the config sends to borg, and to which binary.
    pub fn wired(&self, text: Option<&str>) -> Vec<(&'static str, PathBuf)> {
        let Ok(m) = self.parse(text) else { return Vec::new() };
        let mut out = Vec::new();
        for (event, _) in self.events {
            for g in m.get("hooks").and_then(|h| h.get(*event)).and_then(Value::as_array).into_iter().flatten() {
                for h in g.get("hooks").and_then(Value::as_array).into_iter().flatten() {
                    if let Some((bin, ev)) = h.get("command").and_then(Value::as_str).and_then(|c| self.recognise(c)) {
                        out.push((ev, bin));
                    }
                }
            }
        }
        out
    }

    /// The retired events the config still sends to borg, in table order.
    pub fn leftover(&self, text: Option<&str>) -> Vec<&'static str> {
        let Ok(m) = self.parse(text) else { return Vec::new() };
        let ours = |event: &str, ev: &str| {
            m.get("hooks").and_then(|h| h.get(event)).and_then(Value::as_array).into_iter().flatten().any(|g| {
                g.get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|h| h.get("command").and_then(Value::as_str).and_then(|c| self.recognise(c)).is_some_and(|(_, e)| e == ev))
            })
        };
        self.retired.iter().filter(|(event, ev)| ours(event, ev)).map(|(_, ev)| *ev).collect()
    }

    /// The canonical events missing from `wired`, in table order.
    pub fn missing(&self, wired: &[(&'static str, PathBuf)]) -> Vec<&'static str> {
        self.events.iter().map(|(_, e)| *e).filter(|e| !wired.iter().any(|(x, _)| x == e)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_carries_the_harness_name_and_only_its_own_is_recognised() {
        let sim = Hooks::of(&crate::harness::sim::Sim).unwrap();
        assert_eq!(sim.file, "hooks.json");
        let bin = Path::new("/opt/borg dir/borg");
        assert_eq!(sim.command(bin, "stop"), "'/opt/borg dir/borg' hook sim stop");
        assert_eq!(sim.recognise("'/opt/borg dir/borg' hook sim stop"), Some((bin.to_path_buf(), "stop")));
        assert_eq!(sim.recognise("/b/borg hook claude stop"), None, "another harness's hook is not this one's");
        let text = sim.install(None, Path::new("/b/borg")).unwrap().unwrap();
        assert!(text.contains("\"SimStop\"") && text.contains("/b/borg hook sim stop"), "{text}");
        assert_eq!(sim.missing(&sim.wired(Some(&text))), Vec::<&str>::new());
        assert_eq!(sim.uninstall(Some(&text)).unwrap(), Some(None), "nothing else was in it: the file goes");
    }

    #[test]
    fn each_event_named_in_the_timeouts_gets_its_own() {
        let k = Hooks { timeouts: &[("prompt", 30), ("stop", 90)], ..Hooks::of(&crate::harness::sim::Sim).unwrap() };
        let v: Value = serde_json::from_str(&k.install(None, Path::new("/b/borg")).unwrap().unwrap()).unwrap();
        let timeout = |native: &str| v["hooks"][native][0]["hooks"][0].get("timeout").cloned();
        assert_eq!((timeout("SimPrompt"), timeout("SimStop"), timeout("SimEnd")), (Some(json!(30)), Some(json!(90)), None));
    }

    #[test]
    fn a_reinstall_from_another_path_replaces_borgs_hook_where_it_stands() {
        let sim = Hooks::of(&crate::harness::sim::Sim).unwrap();
        let text = sim.install(None, Path::new("/old/borg")).unwrap().unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["hooks"]["SimStop"].as_array_mut().unwrap().push(json!({"hooks": [{"type": "command", "command": "echo user-after"}]}));
        let text = serde_json::to_string_pretty(&v).unwrap();
        let v: Value = serde_json::from_str(&sim.install(Some(&text), Path::new("/new/borg")).unwrap().unwrap()).unwrap();
        let commands = |native: &str| -> Vec<String> {
            v["hooks"][native].as_array().unwrap().iter().map(|g| g["hooks"][0]["command"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(commands("SimStop"), ["/new/borg hook sim stop", "echo user-after"], "the user's hook keeps its index");
        assert_eq!(commands("SimEnd"), ["/new/borg hook sim end"]);
    }

    #[test]
    fn a_retired_hook_goes_at_install_and_uninstall_and_someone_elses_stays() {
        let k = Hooks { retired: &[("SimStart", "start")], ..Hooks::of(&crate::harness::sim::Sim).unwrap() };
        let mut v: Value = serde_json::from_str(&k.install(None, Path::new("/b/borg")).unwrap().unwrap()).unwrap();
        v["hooks"]["SimStart"] = json!([{"hooks": [{"type": "command", "command": "/old/borg hook sim start"}]}]);
        let old = serde_json::to_string_pretty(&v).unwrap();
        let up: Value = serde_json::from_str(&k.install(Some(&old), Path::new("/b/borg")).unwrap().unwrap()).unwrap();
        assert!(up["hooks"].get("SimStart").is_none(), "{up}");
        assert_eq!(k.install(Some(&render(up.as_object().unwrap().clone(), None)), Path::new("/b/borg")).unwrap(), None, "then nothing to do");
        v["hooks"]["SimStart"][0]["hooks"].as_array_mut().unwrap().push(json!({"type": "command", "command": "echo mine"}));
        let mixed = serde_json::to_string_pretty(&v).unwrap();
        let up: Value = serde_json::from_str(&k.install(Some(&mixed), Path::new("/b/borg")).unwrap().unwrap()).unwrap();
        assert_eq!(up["hooks"]["SimStart"][0]["hooks"], json!([{"type": "command", "command": "echo mine"}]));
        let down: Value = serde_json::from_str(&k.uninstall(Some(&mixed)).unwrap().unwrap().unwrap()).unwrap();
        assert_eq!(down["hooks"].as_object().unwrap().keys().collect::<Vec<_>>(), ["SimStart"], "{down}");
        assert_eq!(k.missing(&k.wired(Some(&old))), Vec::<&str>::new(), "a retired hook is never missing");
        assert_eq!((k.leftover(Some(&old)), k.leftover(Some(&k.install(Some(&old), Path::new("/b/borg")).unwrap().unwrap()))), (vec!["start"], vec![]));
    }
}
