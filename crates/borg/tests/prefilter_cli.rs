//! The Stop gate end to end through the binary: a turn that touches no lesson skips the model call
//! (and logs `#borg-skip`), a turn that does reaches it, `BORG_PREFILTER=off` and `BORG_MATCH=bm25`
//! behave as before, and `borg status` counts both. The model is a fake `claude` on PATH that logs
//! each call: no test makes a real model call.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-prefilter-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A store with one lesson about a websocket send lock among 30 unrelated ones, a config routing
/// `<root>/corp` to it, and a fake `claude` that predicts that lesson at $0.05 and logs its calls.
fn setup(name: &str) -> PathBuf {
    let root = tmp(name);
    let store = root.join("store/collectives/corp");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(root.join("corp")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let mut lessons = String::new();
    for i in 0..30 {
        lessons.push_str(&serde_json::json!({"id": format!("L{i}"), "origin_ms": 1, "question": format!("Did the gardening schedule{i} water the tomatoes{i} before noon, and did I check the compost pile, the greenhouse vents, the irrigation timer, the seedling trays and the mulch layer for slugs?")}).to_string());
        lessons.push('\n');
    }
    lessons.push_str(&serde_json::json!({"id": "Llock", "origin_ms": 1,
        "question": "Before holding the websocket send lock across an await on the delivery queue, did I check that a concurrent sender cannot deadlock the realtime session, and would a lock-free ConcurrentLinkedQueue drained by one writer thread avoid synchronized blocks entirely?",
        "situations": ["holding a lock across an await"], "consequence": "deadlock"}).to_string());
    lessons.push('\n');
    std::fs::write(store.join("lessons.jsonl"), lessons).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        serde_json::json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("corp")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    let fake = root.join("bin/claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho call >> {}\necho '{{\"structured_output\":{{\"moves\":[],\"applies\":[{{\"id\":\"Llock\",\"why\":\"w\",\"grade\":3}}]}},\"total_cost_usd\":0.05}}'\n",
            root.join("calls.log").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

fn borg(root: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> String {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        // The fixed rule's delivery (select.rs `Policy::Fixed`), as tests/common pins it.
        .env("BORG_SELECT", "fixed")
        .env("BORG_STOP_ATTENTION", "off")
        .env("PATH", path)
        // The fake `claude` alone answers: a failed answer would otherwise fall through to the next model
        // CLI on PATH (runner.rs `pick`), a real call.
        .env("BORG_PREDICT_MODEL", "claude:")
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into()
}

/// One turn: the engineer's prompt, then the Stop with the drone's final message. Returns the Stop's output.
fn turn(root: &Path, session: &str, prompt: &str, last: &str, env: &[(&str, &str)]) -> String {
    let cwd = root.join("corp");
    borg(root, &["hook", "claude", "prompt"], &serde_json::json!({"session_id": session, "cwd": cwd, "prompt": prompt}).to_string(), env);
    let stop = serde_json::json!({"session_id": session, "cwd": cwd, "last_assistant_message": last, "stop_hook_active": false});
    borg(root, &["hook", "claude", "stop"], &stop.to_string(), env)
}

fn calls(root: &Path) -> usize {
    std::fs::read_to_string(root.join("calls.log")).unwrap_or_default().lines().count()
}

fn state(root: &Path, session: &str) -> String {
    std::fs::read_to_string(root.join(format!("store/collectives/corp/state/{session}.ctx"))).unwrap_or_default()
}

const CHAT: &str = "thanks, looks good";
const WORK: &str = "Plan: keep the websocket send lock but hold it across the await on the delivery queue; \
    a concurrent sender in the realtime session then waits; alternatively a ConcurrentLinkedQueue drained by one writer thread \
    replaces the synchronized blocks, lock-free.";

#[test]
fn a_chat_turn_skips_the_model_and_a_matching_turn_reaches_it() {
    let root = setup("gate");
    let out = turn(&root, "s1", "ok", CHAT, &[]);
    assert!(out.trim().is_empty(), "nothing asked: {out}");
    assert_eq!(calls(&root), 0, "no model call for a chat turn");
    assert!(state(&root, "s1").lines().any(|l| l.starts_with("#borg-skip ")), "{}", state(&root, "s1"));

    // The next turn is about the lesson: only this turn counts, and it passes the gate.
    let out = turn(&root, "s1", "how should the realtime session send?", WORK, &[]);
    assert_eq!(calls(&root), 1, "the matching turn asks the model once");
    assert!(out.contains("block") && out.contains("websocket send lock"), "{out}");
    let s = state(&root, "s1");
    assert!(s.contains("#borg-predict-cost 0.0500") && s.contains("#borg-asked Llock"), "{s}");

    // And the chat turn after it is judged alone again.
    turn(&root, "s1", "fine", CHAT, &[]);
    assert_eq!(calls(&root), 1);
    assert_eq!(state(&root, "s1").lines().filter(|l| l.starts_with("#borg-skip ")).count(), 2);

    let status = borg(&root, &["status"], "", &[]);
    assert!(status.contains("stop gate    2 turn ends skipped at 0 tokens, 1 asked the model ($0.05 so far)"), "{status}");
}

#[test]
fn prefilter_off_and_bm25_mode_are_unchanged() {
    let root = setup("modes");
    turn(&root, "s2", "ok", CHAT, &[("BORG_PREFILTER", "off")]);
    assert_eq!(calls(&root), 1, "BORG_PREFILTER=off asks the model at every turn end");
    assert!(!state(&root, "s2").contains("#borg-skip"));

    // A threshold of 0 never skips either; a huge one skips even the matching turn.
    turn(&root, "s3", "ok", CHAT, &[("BORG_PREFILTER", "0")]);
    assert_eq!(calls(&root), 2);
    turn(&root, "s4", "how should the realtime session send?", WORK, &[("BORG_PREFILTER", "1000")]);
    assert_eq!(calls(&root), 2);
    assert!(state(&root, "s4").contains("#borg-skip"));

    // BORG_MATCH=bm25: word matching alone, no gate and no model.
    let out = turn(&root, "s5", "ok", CHAT, &[("BORG_MATCH", "bm25")]);
    assert_eq!(calls(&root), 2);
    let s = state(&root, "s5");
    assert!(!s.contains("#borg-skip") && s.contains("#borg-asked"), "{s}; {out}");
}
