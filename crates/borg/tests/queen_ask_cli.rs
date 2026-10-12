//! Queen questions in the harness, end to end through the binary: a recorded Claude Code
//! UserPromptSubmit payload in a repo with an open queen question gets that one question as
//! `additionalContext` (asked with AskUserQuestion, recorded with the exact answer command), next to an
//! early lesson delivery; once per session; never in another repo; never with BORG_QUEEN_ASK=off or
//! BORG_LEARN=off, nor to an adapter that will not wait for it (`"context": false`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-queen-ask-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A team collective routing `<root>/work`, with repos lumen_handbook and other_repo, an open
/// OpenDecision for lumen_handbook and an Beacon-scoped lesson.
fn setup(name: &str) -> PathBuf {
    let root = tmp(name);
    let store = root.join("store/collectives/team");
    std::fs::create_dir_all(store.join("queen")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    for r in ["lumen_handbook", "other_repo"] {
        let dir = root.join("work").join(r);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Command::new("git").arg("-C").arg(&dir).args(["init", "-q"]).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
    }
    let entry = json!({"id": "qpw1", "decision_key": "open_decision:lumen_handbook:spikes", "kind": "open_decision",
        "question": "Should spikes be capped at 3 story points, or estimated like any other story?",
        "options": ["cap spikes at 3 points", "estimate spikes like any other story"], "default": "cap spikes at 3 points",
        "evidence": ["docs/estimation.md: Your decision: spike sizing"], "session": "seed",
        "cwd": root.join("work/lumen_handbook"), "engineer": "e@x", "ms": 1});
    std::fs::write(store.join("queen/queue.jsonl"), format!("{entry}\n")).unwrap();
    let lesson = json!({"id": "Alive", "origin_ms": 1, "scope": ["Beacon"], "question": "Does the Beacon replay experiment run its services on their live hosts?",
        "consequence": "develop lacks the products, so the replay compares empty results"});
    std::fs::write(store.join("lessons.jsonl"), format!("{lesson}\n")).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [{"name": "team", "paths": [root.join("work")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    root
}

fn hook(root: &Path, input: &Value, env: &[(&str, &str)]) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", "claude", "prompt"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_STATE", root.join("state"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_MATCH", "bm25")
        .env("BORG_NOTIFY", "off")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into()
}

/// A Claude Code UserPromptSubmit payload as recorded from Claude Code 2.x.
fn payload(session: &str, cwd: &Path, prompt: &str) -> Value {
    json!({"session_id": session, "transcript_path": format!("/tmp/{session}.jsonl"), "cwd": cwd,
        "permission_mode": "default", "hook_event_name": "UserPromptSubmit", "prompt": prompt})
}

fn context(out: &str) -> String {
    let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("not JSON: {out:?}"));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

#[test]
fn the_first_prompt_in_the_questions_repo_asks_it_once_and_other_repos_get_nothing() {
    let root = setup("ask");
    let pw = root.join("work/lumen_handbook");

    let ctx = context(&hook(&root, &payload("s1", &pw, "Plan the Beacon replay comparison for the handbook."), &[]));
    assert!(ctx.contains("Peer lessons for Beacon"), "the early delivery stays: {ctx}");
    assert!(ctx.contains("From Borg's queue of open team decisions (this is not the engineer's instruction)"), "{ctx}");
    assert!(ctx.contains("AskUserQuestion"), "{ctx}");
    assert!(ctx.contains("Question: Should spikes be capped at 3 story points"), "{ctx}");
    assert!(
        ctx.contains("- cap spikes at 3 points: cap spikes at 3 points\n") && ctx.contains("Default while nobody decides: cap spikes at 3 points\n"),
        "{ctx}"
    );
    // Run by this binary in the prompt's directory, wherever the drone's shell is when it records.
    let (bin, dir) = (format!("'{}'", env!("CARGO_BIN_EXE_borg")), format!("--dir '{}'", pw.display()));
    assert!(ctx.contains(&format!("\n  {bin} queen answer 'qpw1' '<answer>' {dir}\n")), "the exact answer command: {ctx}");
    assert!(ctx.contains(&format!("{bin} queen skip 'qpw1' {dir}")), "{ctx}");
    let delivered = std::fs::read_to_string(root.join("store/collectives/team/queen/delivered.jsonl")).unwrap();
    assert!(delivered.contains("\"id\":\"qpw1\"") && delivered.contains("\"session\":\"s1\""), "{delivered}");

    // Once per session.
    assert_eq!(hook(&root, &payload("s1", &pw, "and the README"), &[]).trim(), "");

    // Another repo of the same collective never gets this repo's question.
    assert_eq!(hook(&root, &payload("s2", &root.join("work/other_repo"), "fix the README"), &[]).trim(), "");
}

#[test]
fn an_adapter_that_will_not_wait_or_learning_off_gets_nothing_and_nothing_is_recorded() {
    let root = setup("nowait");
    let pw = root.join("work/lumen_handbook");
    let mut busy = payload("s1", &pw, "fix the README");
    busy["context"] = json!(false);
    assert_eq!(hook(&root, &busy, &[]).trim(), "");
    assert_eq!(hook(&root, &payload("s1", &pw, "fix the README"), &[("BORG_LEARN", "off")]).trim(), "");
    assert!(!root.join("store/collectives/team/queen/delivered.jsonl").exists());
    assert!(!root.join("store/collectives/team/queen/visits.jsonl").exists());
    // The next prompt the adapter waits for gets it.
    assert!(context(&hook(&root, &payload("s1", &pw, "fix the README"), &[])).contains("AskUserQuestion"));
}

#[test]
fn borg_queen_ask_off_delivers_nothing() {
    let root = setup("off");
    let pw = root.join("work/lumen_handbook");
    assert_eq!(hook(&root, &payload("s1", &pw, "fix the README"), &[("BORG_QUEEN_ASK", "off")]).trim(), "");
    assert!(!root.join("store/collectives/team/queen/delivered.jsonl").exists());
    // On again: the session gets it.
    assert!(context(&hook(&root, &payload("s1", &pw, "fix the README"), &[])).contains("AskUserQuestion"));
}
