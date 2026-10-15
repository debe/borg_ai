//! The coding agents borg runs in ("harnesses"), each one module behind one trait.
//!
//! Every harness already speaks one canonical, Claude-shaped hook event and gets a Claude-shaped answer
//! back (hook.rs `dispatch`): a TS adapter translates for omp and opencode, Claude Code's command hooks need none.
//! So the core differs per harness only in data: how `borg install` wires it ([`Wiring`], `target`),
//! what reaches its model and when ([`Caps`]), where the turn's work and the drone's last message come
//! from ([`Transcript`]), and the stop's time budget. The hook (hook.rs), install (install.rs) and
//! every other module ask these, never the harness's name.
//!
//! Adding a harness: one module here, a line in [`ALL`], and an adapter where the harness has no command
//! hooks (docs/harnesses.md has the checklist; the conformance suites prove the basics).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

pub mod claude;
pub mod omp;
pub mod opencode;
pub mod sim;
pub mod wiring;

/// One coding agent borg can run in.
pub trait Harness: Sync {
    /// The CLI argument (`borg hook <name>`, `borg install <name>`), `BORG_HARNESS` and presence's `harness`.
    fn name(&self) -> &'static str;
    fn caps(&self) -> Caps;
    /// What `borg install` writes.
    fn wiring(&self) -> Wiring;
    /// The config file `borg install` writes, under `home`; `var` reads the harness's own config-dir
    /// variables (`--home` gives none, so nothing outside `home` is reached).
    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf;
    /// What `borg doctor` asks of the harness's CLI.
    fn probe(&self) -> Probe;
    /// Left out of help, completions, `all`, status and doctor (the simulated harness).
    fn hidden(&self) -> bool {
        false
    }
    /// The native hook payload as the canonical event. Default: it is one already.
    fn normalise(&self, raw: Value) -> Value {
        raw
    }
    /// What `borg doctor` checks beyond the CLI's version line ([`Probe`]), e.g. that the harness trusts
    /// borg's hooks; `target` is the config `borg install` writes. Default: nothing more.
    fn checks(&self, _target: &Path) -> Vec<(Level, String)> {
        Vec::new()
    }
    /// What `borg install` tells the engineer to do in the harness itself (e.g. trust the new hooks).
    fn install_notes(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What a harness can do with borg's answers, and what it sends. Each flag gates one behaviour of the
/// core; a flag changes only with an evaluation behind it (docs/harnesses.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Context returned at a prompt reaches the model.
    pub prompt_context: bool,
    /// Context returned after a tool call reaches the model before its next step (live mode's asks).
    pub tool_context: bool,
    /// Early delivery: a prompt or touch naming a lesson's scope gets that scope's lessons at once (hook.rs `early`).
    pub early_delivery: bool,
    /// Steering is shown at the next prompt or tool call; without it, at the next stop (steer.rs `around`).
    pub steer_in_context: bool,
    /// A queen question is asked with a dialog (AskUserQuestion), else inline (queen.rs `render_delivery`).
    pub question_dialog: bool,
    pub transcript: Transcript,
    /// `post_tool_failure` carries the engineer's denials, which become episodes (hook.rs
    /// `capture_rejection`); without it the event is ignored.
    pub deny_capture: bool,
    /// Events carry the harness's pid, so a gone process reads as ended (presence.rs `state`); without
    /// it an event's `pid` is dropped (hook.rs `main`).
    pub sends_pid: bool,
    /// The stop's budget when the event names none (`BORG_STOP_BUDGET_MS`, which an adapter sets): the
    /// live wait, the stop's model call and the expert drone keep inside it (expert_route.rs
    /// `stop_budget`). None: the expert drone keeps inside [`DEFAULT_STOP_BUDGET`], the model call
    /// inside the model's own limit.
    pub stop_budget: Option<Duration>,
}

/// Where the turn's work and the drone's last message come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transcript {
    /// The event's `transcript_path` is a Claude Code JSONL transcript.
    Claude,
    /// None: the event's own `last_assistant_message`, and the tool calls the hook logs (`.turn`).
    None,
}

/// How `borg install` wires a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wiring {
    /// Command hooks merged into a JSON file: one hook group per native event (native name, canonical
    /// event), each running `<borg> hook <name> <event>`; `timeouts` gives a canonical event's hook a
    /// `timeout` in seconds (the others get the harness's own default). `through_links`: the file is
    /// often a symlink into a dotfiles repo, written through.
    CommandHooks { events: &'static [(&'static str, &'static str)], timeouts: &'static [(&'static str, u64)], through_links: bool },
    /// A compiled-in adapter copied whole; a symlink is replaced, never written through, and uninstall
    /// removes only a file with exactly this content.
    AdapterFile { source: &'static str },
}

impl Wiring {
    /// The adapter `borg install` copies whole ("" for command hooks).
    pub fn adapter(self) -> &'static str {
        match self {
            Wiring::AdapterFile { source } => source,
            Wiring::CommandHooks { .. } => "",
        }
    }
}

/// What `borg doctor` asks: the CLI whose `--version` it prints, and the versions borg was verified on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub cli: &'static str,
    /// Appended to the version line, e.g. " (adapter tested with …)"; empty for none.
    pub tested: &'static str,
}

/// How bad a `borg doctor` finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

/// The stop's budget assumed where a harness names none: Claude Code's default command-hook timeout.
pub const DEFAULT_STOP_BUDGET: Duration = Duration::from_secs(60);

/// Set (to anything) by the conformance suites: only then are the hidden harnesses found by name, so
/// `borg install sim` and `borg hook sim …` are unknown to everyone else.
pub const TEST_ENV: &str = "BORG_TEST_HARNESS";

/// Every harness, in the order help, status and `all` list them. Adding one is a line here.
pub static ALL: &[&dyn Harness] = &[&claude::Claude, &omp::Omp, &opencode::Opencode, &sim::Sim];

/// The harness called `name`; a hidden one only under [`TEST_ENV`].
pub fn by_name(name: &str) -> Option<&'static dyn Harness> {
    let hidden_too = std::env::var_os(TEST_ENV).is_some();
    ALL.iter().copied().find(|h| h.name() == name && (hidden_too || !h.hidden()))
}

/// The harnesses people see (help, completions, `all`, status, doctor).
pub fn visible() -> impl Iterator<Item = &'static dyn Harness> {
    ALL.iter().copied().filter(|h| !h.hidden())
}

/// `claude|omp|opencode`: the visible names, for usage lines.
pub fn names(sep: &str) -> String {
    visible().map(|h| h.name()).collect::<Vec<_>>().join(sep)
}

/// The drone's harness in a process its hook started (`BORG_HARNESS`, `borg check`), else the first.
pub fn current() -> &'static dyn Harness {
    std::env::var("BORG_HARNESS").ok().and_then(|n| by_name(&n)).unwrap_or(ALL[0])
}

/// The event's transcript, when the harness writes one and the event names it.
pub fn transcript(caps: Caps, ev: &Value) -> Option<&str> {
    match caps.transcript {
        Transcript::Claude => ev.get("transcript_path").and_then(Value::as_str).filter(|t| !t.is_empty()),
        Transcript::None => None,
    }
}

/// Live mode: this turn's work so far, its last `cap` characters: from the transcript where the harness
/// writes one ([`Transcript`]), else the turn's tool calls the hook logged in `turn_log`.
pub fn work_so_far(caps: Caps, ev: &Value, turn_log: &Path, cap: usize) -> String {
    if let Some(t) = transcript(caps, ev).filter(|t| std::fs::metadata(t).is_ok()) {
        return claude::turn_so_far(t.as_ref(), cap);
    }
    let text = std::fs::read_to_string(turn_log).unwrap_or_default();
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(cap)).collect()
}

/// The drone's last message, whole: the event's `last_assistant_message`, else the transcript's last
/// assistant text.
pub fn last_message(caps: Caps, ev: &Value) -> String {
    match given(ev) {
        Some(t) => t.to_string(),
        None => claude::last_assistant_full(transcript(caps, ev).unwrap_or_default().as_ref()),
    }
}

/// The drone's plan, as the queen and the experts read it: [`last_message`], but a transcript's text
/// only its last 2000 characters (`claude::last_assistant_text`).
pub fn plan(caps: Caps, ev: &Value) -> String {
    match given(ev) {
        Some(t) => t.to_string(),
        None => claude::last_assistant_text(transcript(caps, ev).unwrap_or_default().as_ref()),
    }
}

/// The event's own non-blank `last_assistant_message`.
fn given(ev: &Value) -> Option<&str> {
    ev.get("last_assistant_message").and_then(Value::as_str).filter(|t| !t.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Today's flags, pinned: changing one is a product decision with an evaluation behind it, never a
    /// side effect of a refactor.
    #[test]
    fn caps_are_todays() {
        let row = |n: &str| {
            let c = by_name(n).unwrap().caps();
            (c.prompt_context, c.tool_context, c.early_delivery, c.steer_in_context, c.question_dialog, c.transcript, c.deny_capture, c.sends_pid)
        };
        assert_eq!(row("claude"), (true, true, true, true, true, Transcript::Claude, true, false));
        for n in ["omp", "opencode"] {
            assert_eq!(row(n), (true, true, false, false, false, Transcript::None, true, true), "{n}");
        }
        for h in ALL {
            assert_eq!(h.caps().stop_budget, None, "{}: the expert drone keeps inside Claude Code's 60 s, the model call its own limit", h.name());
        }
        let Wiring::CommandHooks { timeouts, through_links, .. } = by_name("claude").unwrap().wiring() else { panic!("claude has command hooks") };
        assert_eq!((timeouts, through_links), (&[("stop", 90)][..], true), "a timeout on the stop only");
    }

    #[test]
    fn names_are_unique_and_found_and_sim_is_hidden() {
        for h in visible() {
            assert_eq!(by_name(h.name()).map(|x| x.name()), Some(h.name()));
        }
        assert_eq!(ALL.iter().filter(|h| ALL.iter().filter(|x| x.name() == h.name()).count() > 1).count(), 0);
        assert_eq!(names("|"), "claude|omp|opencode");
        assert!(ALL.iter().any(|h| h.name() == "sim" && h.hidden()));
        // Known by name only under TEST_ENV (conformance_cli.rs runs `borg` with and without it).
        assert_eq!(by_name("sim").is_some(), std::env::var_os(TEST_ENV).is_some());
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn the_transcript_is_read_only_where_the_harness_writes_one() {
        let ev = serde_json::json!({"transcript_path": "/nonexistent/t.jsonl", "last_assistant_message": ""});
        assert_eq!(transcript(by_name("claude").unwrap().caps(), &ev), Some("/nonexistent/t.jsonl"));
        assert_eq!(transcript(by_name("omp").unwrap().caps(), &ev), None);
        let msg = serde_json::json!({"last_assistant_message": "done"});
        assert_eq!(last_message(by_name("opencode").unwrap().caps(), &msg), "done");
    }

    #[test]
    fn a_plan_from_a_transcript_is_its_tail_and_a_given_message_is_whole() {
        let dir = std::env::temp_dir().join(format!("borg-harness-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("t.jsonl");
        let long = "x".repeat(2500);
        let line = serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": long}]}});
        std::fs::write(&t, format!("{line}\n")).unwrap();
        let caps = by_name("claude").unwrap().caps();
        let ev = serde_json::json!({"transcript_path": t, "last_assistant_message": ""});
        assert_eq!((last_message(caps, &ev).len(), plan(caps, &ev).len()), (2500, 2000));
        let given = serde_json::json!({"transcript_path": t, "last_assistant_message": long});
        assert_eq!(plan(caps, &given).len(), 2500, "the event's own message is the plan, whole");
        let _ = std::fs::remove_dir_all(dir);
    }
}
