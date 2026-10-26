//! A plain interactive session, no hive, end to end through the binary and every harness's hooks, in
//! both delivery modes: its prompt, edit, stop and end answer exactly the same in a store that also holds a hive goal synced from another node (with that
//! node's runner joined to it and an open task in this repo), the session joins nothing and learns the
//! same; and a store without playbooks shows none. The model is the fake `claude` of `common`: no test
//! makes a real model call.

mod common;

use std::path::Path;

use common::{borg, borg_as, context, setup, state};
use serde_json::{Value, json};

/// Learning stays on, as for an engineer.
const ON: [(&str, &str); 1] = [("BORG_LEARN", "on")];
/// And the stop always asks the model (no free gate), so it has a verdict to compare.
const ASKED: [(&str, &str); 2] = [("BORG_LEARN", "on"), ("BORG_PREFILTER", "off")];

/// A goal another node made and synced here: its projection (no goal.json), its board with an open task
/// in this session's repo, and that node's runner joined to it.
fn add_foreign_goal(root: &Path) {
    let dir = root.join("store/collectives/corp/hive/peerg");
    std::fs::create_dir_all(&dir).unwrap();
    let goal = json!({"v": 1, "id": "peerg", "spec": "Tidy the Orion net's budget handling", "oracle": [], "budget_usd": 5.0,
        "repos": [], "author": "peer@x", "created_ms": 1});
    std::fs::write(dir.join("goal.jsonl"), format!("{goal}\n")).unwrap();
    let open = json!({"v": 1, "ev": "open", "task": "t1", "ms": 2, "by": "engineer:peer@x", "node": "peer-node",
        "def": {"id": "t1", "role": "work", "title": "drain the Orion budget place", "repo": "harbor_orion"}});
    let join = json!({"v": 1, "ev": "heartbeat", "task": "", "ms": 3, "by": "runner", "node": "peer-node", "note": "join"});
    std::fs::write(dir.join("tasks.jsonl"), format!("{open}\n{join}\n")).unwrap();
    // A goal the hive itself takes: it loads, its task is open and the peer's node is in it.
    let store = root.join("store/collectives/corp");
    assert!(borg::hive::goal::load(&store, "peerg").is_ok());
    let b = borg::hive::board::fold(&borg::hive::board::read(&borg::hive::tasks_path(&store, "peerg")));
    assert!(b.tasks.contains_key("t1") && b.members["peer-node"].0 == 3, "{b:?}");
}

/// The harnesses whose hooks run a plain session, and the delivery modes: `hook` judges at the turn's
/// end, `live` (the default, live.rs) also checks during the turn, through `borg check`.
const HARNESSES: [&str; 4] = ["claude", "omp", "opencode", "codex"];
const MODES: [&str; 2] = ["hook", "live"];

/// Waits (at most 20 s) until the session's live check has covered its edit.
fn checked(root: &Path, session: &str) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while state(root, session, "live.mark").is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!state(root, session, "live.mark").is_empty(), "the live check ran");
}

/// Each event's hook output through `harness` in `mode` (the root path written as `<root>`) and the
/// outcome kinds the session left.
fn session(root: &Path, harness: &str, mode: &str) -> (Vec<(String, String)>, Vec<String>) {
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
    let mut env: Vec<_> = ASKED.into_iter().chain([("BORG_MODE", mode)]).collect();
    if harness != "claude" {
        env.push(("BORG_PREDICT_MODEL", "claude:sonnet"));
    }
    common::answer(root, &[("G0", 3)]);
    let shown = root.display().to_string();
    let mut outs = Vec::new();
    for (e, input) in &events {
        outs.push((e.to_string(), borg_as(root, harness, e, input, &env).replace(&shown, "<root>")));
        if mode == "live" && *e == "post_tool" {
            checked(root, s);
        }
    }
    let outcomes = std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl")).unwrap_or_default();
    let mut kinds: Vec<String> = outcomes.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()?["kind"].as_str().map(String::from)).collect();
    kinds.sort();
    (outs, kinds)
}

#[test]
fn a_synced_foreign_goal_changes_nothing_in_a_plain_session() {
    for harness in HARNESSES {
        for mode in MODES {
            let plain = setup(&format!("plain-alone-{harness}-{mode}"));
            let hived = setup(&format!("plain-hived-{harness}-{mode}"));
            add_foreign_goal(&hived);
            let (a, a_kinds) = session(&plain, harness, mode);
            let (b, b_kinds) = session(&hived, harness, mode);
            // Only Claude Code delivers a scope's lessons at the prompt (harness caps `early_delivery`).
            let early = harness == "claude";
            assert_eq!(!context(&a[0].1).is_empty(), early, "{harness} {mode}: the plain session's prompt got its lessons: {a:?}");
            assert!(a.iter().any(|(_, out)| out.contains("[G0]")), "{harness} {mode}: and a verdict to compare: {a:?}");
            for ((event, alone), (_, beside)) in a.iter().zip(&b) {
                assert_eq!(alone, beside, "{harness} {mode} {event}: the foreign goal changed the hook's answer");
            }
            assert!(!a_kinds.is_empty(), "{harness} {mode}: the session left outcomes");
            assert_eq!(a_kinds, b_kinds, "{harness} {mode}: the same outcomes");
            for root in [&plain, &hived] {
                let hive_files: Vec<_> = std::fs::read_dir(root.join("store/collectives/corp/state"))
                    .unwrap()
                    .flatten()
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| x.starts_with("hive")))
                    .collect();
                assert!(hive_files.is_empty(), "{harness} {mode}: the session joined no goal: {hive_files:?}");
                let ctx = state(root, "plain1", "ctx");
                assert!(!ctx.contains("#borg-hive"), "{harness} {mode}: {ctx}");
            }
            for root in [plain, hived] {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }
}

#[test]
fn a_store_without_playbooks_shows_no_playbook() {
    let root = setup("plain-playbooks");
    assert!(!root.join("store/collectives/corp/playbooks.jsonl").exists());
    let cwd = root.join("harbor_orion");
    let out = borg(&root, "prompt", &json!({"session_id": "pb1", "cwd": cwd, "prompt": "Follow the playbook to tidy the Orion net's budget handling"}), &ON);
    let ctx = context(&out);
    assert!(!ctx.is_empty(), "the prompt got its lessons: {out}");
    assert!(!ctx.to_lowercase().contains("playbook for this kind of task"), "{ctx}");
    assert!(!state(&root, "pb1", "ctx").lines().any(|l| l.starts_with("#borg-playbook")), "{}", state(&root, "pb1", "ctx"));
    let _ = std::fs::remove_dir_all(root);
}
