//! Desktop notifications: one `notify-send` per command or hook run that queued queen questions, so the
//! open decisions do not wait unseen (queen.rs). Detached and best effort: borg never waits for it, and
//! a missing `notify-send`, no display or `BORG_NOTIFY=off` mean no notification. Inert in unit tests.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::queen::{self, Entry};

/// Longest question shown in a notification body, in characters.
const BODY_CHARS: usize = 120;

/// Notifications can be shown: not a unit test, `BORG_NOTIFY` is not "off", a graphical session
/// (`DISPLAY` or `WAYLAND_DISPLAY`) and `notify-send` on PATH.
pub fn enabled() -> bool {
    if cfg!(test) || std::env::var("BORG_NOTIFY").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off")) {
        return false;
    }
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    (set("DISPLAY") || set("WAYLAND_DISPLAY")) && on_path("notify-send")
}

fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| Path::new(&d).join(exe).is_file()))
}

/// (title, body) for `entries` waiting: "Borg: N decision(s) waiting", "<repo>: <first question>".
pub fn message(entries: &[Entry]) -> Option<(String, String)> {
    let first = entries.first()?;
    let n = entries.len();
    let title = format!("Borg: {n} decision{} waiting", if n == 1 { "" } else { "s" });
    Some((title, format!("{}: {}", queen::repo(first), queen::clip(queen::gist(first), BODY_CHARS))))
}

/// Spawn `notify-send` detached; true when it started.
pub fn send(title: &str, body: &str) -> bool {
    Command::new("notify-send").args(["-a", "borg", title, body]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
}

/// One notification for the entries one run queued (none for none). Never fails the caller.
pub fn queued(entries: &[Entry]) -> bool {
    match message(entries) {
        Some((title, body)) if enabled() => send(&title, &body),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queen::Kind;

    fn entry(cwd: &str, q: &str) -> Entry {
        Entry {
            id: "q1".into(),
            decision_key: "k".into(),
            kind: Kind::OpenDecision,
            question: q.into(),
            options: vec![],
            default: "none stated".into(),
            evidence: vec![],
            session: "import".into(),
            cwd: cwd.into(),
            repo: String::new(),
            engineer: String::new(),
            ms: 1,
            answer: None,
            answered_by: None,
            answered_ms: None,
            disagreeing: vec![],
            dismissed: None,
            dismissed_ms: None,
            payload: None,
        }
    }

    #[test]
    fn one_batched_message_names_the_count_and_the_first_question() {
        assert_eq!(message(&[]), None);
        let long = format!("Which retry budget {}?", "x ".repeat(100));
        let (t, b) = message(&[entry("/w/app", &long), entry("/w/lib", "Other?")]).unwrap();
        assert_eq!(t, "Borg: 2 decisions waiting");
        assert!(b.starts_with("app: Which retry budget") && b.ends_with('…') && b.chars().count() <= 5 + BODY_CHARS, "{b}");
        assert_eq!(message(&[entry("/w/app", "Keep it?")]).unwrap(), ("Borg: 1 decision waiting".into(), "app: Keep it?".into()));
        // Never in a unit test: no desktop is ever touched.
        assert!(!enabled() && !queued(&[entry("/w/app", "Keep it?")]));
    }
}
