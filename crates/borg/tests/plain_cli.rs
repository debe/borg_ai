//! A plain interactive session, end to end through the binary and every harness's hooks: its prompt
//! gets its lessons where the harness delivers them early, its stop a verdict, and it leaves outcomes. The model is the fake
//! `claude` of `common`: no test makes a real model call.

mod common;

use std::path::Path;

use common::{borg_as, context, setup};
use serde_json::{Value, json};

/// Learning stays on, as for an engineer, and the stop always asks the model (no free gate), so it has
/// a verdict to compare.
const ASKED: [(&str, &str); 2] = [("BORG_LEARN", "on"), ("BORG_PREFILTER", "off")];

/// The harnesses whose hooks run a plain session.
const HARNESSES: [&str; 4] = ["claude", "omp", "opencode", "codex"];

/// Each event's hook output through `harness` (the root path written as `<root>`) and the outcome kinds
/// the session left.
fn session(root: &Path, harness: &str) -> (Vec<(String, String)>, Vec<String>) {
    let cwd = root.join("harbor_orion");
    let s = "plain1";
    let events = [
        ("prompt", json!({"session_id": s, "cwd": cwd, "prompt": "Tidy the budget handling of the Orion net"})),
        (
            "post_tool",
            json!({"session_id": s, "cwd": cwd, "tool_name": "Edit", "tool_input": {"file_path": cwd.join("net.rs"), "old_string": "reset", "new_string": "drain"}}),
        ),
        (
            "stop",
            json!({"session_id": s, "cwd": cwd, "last_assistant_message": "Done: the budget place is drained, and the websocket handling now covers the websocket edge case and its limits.", "stop_hook_active": false}),
        ),
        ("end", json!({"session_id": s, "cwd": cwd})),
    ];
    // Off Claude Code the model call is pinned to the fake `claude` (common.rs `borg_as`).
    let mut env: Vec<_> = ASKED.into_iter().collect();
    if harness != "claude" {
        env.push(("BORG_PREDICT_MODEL", "claude:sonnet"));
    }
    common::answer(root, &[("G0", 3)]);
    let shown = root.display().to_string();
    let mut outs = Vec::new();
    for (e, input) in &events {
        outs.push((e.to_string(), borg_as(root, harness, e, input, &env).replace(&shown, "<root>")));
    }
    let outcomes = std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl")).unwrap_or_default();
    let mut kinds: Vec<String> = outcomes.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()?["kind"].as_str().map(String::from)).collect();
    kinds.sort();
    (outs, kinds)
}

#[test]
fn a_plain_session_gets_its_lessons_and_a_verdict_in_every_harness() {
    for harness in HARNESSES {
        let root = setup(&format!("plain-{harness}"));
        let (a, kinds) = session(&root, harness);
        // Only Claude Code delivers a scope's lessons at the prompt (harness caps `early_delivery`).
        let early = harness == "claude";
        assert_eq!(!context(&a[0].1).is_empty(), early, "{harness}: the plain session's prompt got its lessons: {a:?}");
        assert!(a.iter().any(|(_, out)| out.contains("[G0]")), "{harness}: and a verdict: {a:?}");
        assert!(!kinds.is_empty(), "{harness}: the session left outcomes");
        let _ = std::fs::remove_dir_all(root);
    }
}
