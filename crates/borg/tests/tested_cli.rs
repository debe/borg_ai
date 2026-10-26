//! A plain session's own test runs reward the lessons asked before them, through every harness's hooks:
//! a failed run after the stop asked a lesson and a passing one before the session ends give that lesson
//! one `tested` outcome at session end (`borg learn`), once, however often learning runs again; a pass
//! that came before the ask gives none. The model is the fake `claude` of `common`: no test makes a real
//! model call.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{borg_as, setup};
use serde_json::{Value, json};

const HARNESSES: [&str; 4] = ["claude", "omp", "opencode", "codex"];

fn outcomes(root: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl")).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn of(root: &Path, kind: &str, session: &str) -> Vec<Value> {
    outcomes(root).into_iter().filter(|o| o["kind"] == kind && o["session"] == session).collect()
}

/// Waits (at most 20 s) until the session's asks are settled: `borg learn` ran to its end.
fn settled(root: &Path, session: &str) {
    let until = Instant::now() + Duration::from_secs(20);
    while of(root, "asked_not_corrected", session).is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!of(root, "asked_not_corrected", session).is_empty(), "{session}: learning settled the session: {:?}", outcomes(root));
}

/// A test run as `harness` reports it: the hook event and its payload's result fields. Claude Code and
/// omp send a failed call as `post_tool_failure` with its text as `error`; opencode's adapter sends the
/// end of the output, after Codex's `Exit code: N` line on a non-zero exit (opencode's shell keeps such
/// a call out of the error state); Codex sends its own string.
fn test_run(harness: &str, pass: bool) -> (&'static str, Value) {
    let out = if pass { "test result: ok. 3 passed; 0 failed" } else { "test result: FAILED. 2 passed; 1 failed" };
    match (harness, pass) {
        ("claude", true) => ("post_tool", json!({"tool_response": {"stdout": out, "stderr": "", "interrupted": false}})),
        ("claude", false) => ("post_tool_failure", json!({"error": format!("Exit code 101\n{out}")})),
        ("omp", true) | ("opencode", true) => ("post_tool", json!({"tool_response": out})),
        ("omp", false) => ("post_tool_failure", json!({"error": format!("{out}\n\nCommand exited with code 101")})),
        ("opencode", false) => ("post_tool", json!({"tool_response": format!("Exit code: 101\nOutput:\n{out}")})),
        (_, true) => ("post_tool", json!({"tool_response": format!("Exit code: 0\nOutput:\n{out}")})),
        (_, false) => ("post_tool", json!({"tool_response": format!("Exit code: 101\nOutput:\n{out}")})),
    }
}

/// A session through `harness`: a prompt, then `before` test runs, the stop that asks Mdrain, `after`
/// test runs, and the end. Each run passed or failed, sent as `harness` reports it (`test_run`), or is
/// `None`: sent without its result, as an adapter that forwards no output does.
fn session_with(root: &Path, harness: &str, s: &str, before: &[Option<bool>], after: &[Option<bool>]) {
    let cwd = root.join("harbor_orion");
    let mut env = vec![("BORG_LEARN", "on"), ("BORG_PREFILTER", "off")];
    if harness != "claude" {
        env.push(("BORG_PREDICT_MODEL", "claude:sonnet"));
    }
    let run = |pass: &Option<bool>| {
        let (event, result) = pass.map_or(("post_tool", json!({})), |p| test_run(harness, p));
        let mut input = json!({"session_id": s, "cwd": cwd, "tool_name": "Bash", "tool_input": {"command": "cargo test -p orion"}});
        input.as_object_mut().unwrap().extend(result.as_object().unwrap().clone());
        borg_as(root, harness, event, &input, &env);
    };
    borg_as(root, harness, "prompt", &json!({"session_id": s, "cwd": cwd, "prompt": "Tidy the budget handling of the Orion net"}), &env);
    before.iter().for_each(run);
    let stop = json!({"session_id": s, "cwd": cwd, "last_assistant_message": "Done: the Orion net's budget place is reset.", "stop_hook_active": false});
    let out = borg_as(root, harness, "stop", &stop, &env);
    assert!(out.contains("[Mdrain]"), "{harness} {s}: the stop asked the lesson: {out}");
    after.iter().for_each(run);
    borg_as(root, harness, "end", &json!({"session_id": s, "cwd": cwd}), &env);
    settled(root, s);
}

fn session(root: &Path, harness: &str, s: &str, before: &[bool], after: &[bool]) {
    let known = |runs: &[bool]| runs.iter().map(|p| Some(*p)).collect::<Vec<_>>();
    session_with(root, harness, s, &known(before), &known(after));
}

/// `borg learn --session s` once more, as the end hook runs it.
fn learn_again(root: &Path, s: &str) {
    let store = root.join("store/collectives/corp");
    let status = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["learn", "--session", s])
        .env_clear()
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        .env("HOME", root.join("home"))
        .env("BORG_HOME", &store)
        .env("BORG_STATE", store.join("state"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_VECTORS", "off")
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn a_session_whose_tests_go_green_after_an_ask_credits_the_lesson_once() {
    for harness in HARNESSES {
        let root = setup(&format!("tested-{harness}"));
        session(&root, harness, "red1", &[], &[false, true]);
        let got = of(&root, "tested", "red1");
        assert_eq!(got.len(), 1, "{harness}: one credit: {:?}", outcomes(&root));
        assert_eq!(got[0]["lessons"], json!(["Mdrain"]), "{harness}");
        learn_again(&root, "red1");
        assert_eq!(of(&root, "tested", "red1").len(), 1, "{harness}: learning again credits nothing more");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn a_pass_before_the_ask_or_no_test_run_credits_nothing() {
    let root = setup("tested-none");
    session(&root, "claude", "early1", &[false, true], &[]);
    session(&root, "claude", "quiet1", &[], &[]);
    // A run whose result the harness does not show is no run: red, then a run of unknown result, is no
    // green, so no credit.
    session_with(&root, "opencode", "bare1", &[], &[Some(false), None]);
    assert!(outcomes(&root).iter().all(|o| o["kind"] != "tested"), "{:?}", outcomes(&root));
    let _ = std::fs::remove_dir_all(root);
}
