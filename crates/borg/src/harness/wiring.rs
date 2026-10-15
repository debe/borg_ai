//! Command hooks merged into a harness's JSON config (Claude Code's settings.json, and any harness with
//! the same shape: `{"hooks": {<native event>: [{"hooks": [{"type": "command", "command": …}]}]}}`).
//!
//! Someone else's config is precious: merging keeps every other key and hook as it was (and their
//! order), re-running changes nothing, and uninstall removes only borg's own commands,
//! `<…/borg> hook <harness> <event>`, recognised by the binary's name `borg`.

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
        let Wiring::CommandHooks { events, timeouts, .. } = h.wiring() else { return None };
        let file = h.target(Path::new(""), &|_| None).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        Some(Hooks { harness: h.name(), events, timeouts, file })
    }

    pub fn command(&self, bin: &Path, event: &str) -> String {
        format!("{} hook {} {event}", shell_quote(&bin.display().to_string()), self.harness)
    }

    /// `(binary, canonical event)` if this hook command is one borg installs: `<…/borg> hook <harness> <event>`.
    pub fn recognise(&self, cmd: &str) -> Option<(PathBuf, &'static str)> {
        self.events.iter().find_map(|(_, ev)| {
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
            self.remove_borg(groups, ev); // stale binary, timeout or duplicates: replace with exactly one
            groups.push(json!({"hooks": [want]}));
        }
        Ok((m != orig).then(|| render(m, text)))
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
}
