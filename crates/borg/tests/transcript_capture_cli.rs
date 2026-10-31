//! Transcript capture through the hook (sweep.rs): a Claude Code turn end reads what the turn added to
//! its transcript, the `PostToolUseFailure` capture and the transcript's give one episode, a session's
//! end (`borg learn --session`) reads its whole transcript, and `BORG_TRANSCRIPT_CAPTURE=off` records
//! nothing. No model call (the model CLI on `PATH` is a stub that fails), no network.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const SESSION: &str = "7f3c2a10-5b6d-4e8f-9a01-23456789abcd";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/claude/transcripts/-home-dev-work-relay/{SESSION}.jsonl"))
}

/// A store, a config that routes the fixture's repo to collective `dev`, and a copy of the transcript.
fn setup(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-transcript-capture-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["home", "store", "bin"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("collectives.json"), r#"{ "default": "off", "collectives": [ { "name": "dev", "paths": ["/home/dev/work"] } ] }"#).unwrap();
    std::fs::copy(fixture(), root.join("t.jsonl")).unwrap();
    root
}

fn hook(root: &Path, event: &str, input: &Value, env: &[(&str, &str)]) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["hook", "claude", event])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_PREDICT_MODEL", "claude:")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
}

fn stop(root: &Path, env: &[(&str, &str)]) {
    let input = json!({"session_id": SESSION, "cwd": "/home/dev/work/relay", "transcript_path": root.join("t.jsonl"),
        "last_assistant_message": "Backoff with jitter, budget per connection.", "stop_hook_active": false});
    hook(root, "stop", &input, env);
}

fn episodes(root: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(root.join("store/collectives/dev/episodes.jsonl")).unwrap_or_default();
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn a_turn_end_reads_the_rejection_and_answers_from_the_transcript_once() {
    let root = setup("stop");
    stop(&root, &[]);
    let eps = episodes(&root);
    let ids: Vec<&str> = eps.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["7f3c2a10-Ttoolu_01RelayEdit", "7f3c2a10-Ttoolu_01RelayAsk", "7f3c2a10-Ttoolu_01RelayPlace"]);
    assert!(eps.iter().all(|e| e["harness"] == "claude" && e["harness_version"] == "2.1.312" && e["repo"] == "relay"), "{eps:?}");
    // The next turn end: the transcript did not grow, nothing doubles.
    stop(&root, &[]);
    assert_eq!(episodes(&root).len(), 3);
    assert!(root.join(format!("store/collectives/dev/state/{SESSION}.swept")).exists(), "the offset lives in the collective's state");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_failure_hooks_rejection_and_the_transcripts_are_one_episode() {
    let root = setup("both");
    // A harness that still sends PostToolUseFailure: the hook captures the rejection live.
    let error = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\nuse exponential backoff with jitter, a fixed sleep hammers the broker\n\nNote: The user's next message may contain a correction or preference.";
    let failure = json!({"session_id": SESSION, "cwd": "/home/dev/work/relay", "transcript_path": root.join("t.jsonl"),
        "tool_name": "Edit", "tool_input": {"file_path": "/home/dev/work/relay/src/relay.rs"}, "error": error});
    hook(&root, "post_tool_failure", &failure, &[]);
    let live = episodes(&root);
    assert_eq!(live.len(), 1);
    assert!(live[0].get("harness").is_none(), "the hook's own capture names no transcript");
    stop(&root, &[]);
    let eps = episodes(&root);
    assert_eq!(eps.len(), 3, "the rejection once, the two answers: {eps:?}");
    assert_eq!(eps.iter().filter(|e| e["via"] == "answer").count(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn transcript_capture_off_records_nothing() {
    let root = setup("off");
    stop(&root, &[("BORG_TRANSCRIPT_CAPTURE", "off")]);
    assert!(episodes(&root).is_empty());
    assert!(!root.join(format!("store/collectives/dev/state/{SESSION}.swept")).exists());
    let _ = std::fs::remove_dir_all(root);
}

/// `borg learn --session SESSION` as a session's end runs it (`BORG_HARNESS` from the hook), the model
/// CLI a stub that fails, `PATH` without any other.
fn learn(root: &Path, env: &[(&str, &str)]) {
    let stub = root.join("bin/claude");
    std::fs::write(&stub, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ok = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["learn", "--session", SESSION])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", format!("{}:/usr/bin:/bin", root.join("bin").display()))
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store/collectives/dev"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_HARNESS", "claude")
        .env("BORG_LEARN_MODEL", "claude:")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(ok.success());
}

/// The fixture where Claude Code keeps it: `HOME/.claude/projects/<slug>/<session>.jsonl`.
fn in_projects(root: &Path) {
    let dir = root.join("home/.claude/projects/-home-dev-work-relay");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture(), dir.join(format!("{SESSION}.jsonl"))).unwrap();
}

#[test]
fn a_sessions_end_reads_its_whole_transcript_once_before_learning() {
    let root = setup("end");
    in_projects(&root);
    learn(&root, &[]);
    let eps = episodes(&root);
    let ids: Vec<&str> = eps.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["7f3c2a10-Ttoolu_01RelayEdit", "7f3c2a10-Ttoolu_01RelayAsk", "7f3c2a10-Ttoolu_01RelayPlace"]);
    assert!(root.join(format!("store/collectives/dev/state/{SESSION}.swept")).exists(), "the offset lives beside the store's state");
    // Ended again (a resumed session): nothing doubles.
    learn(&root, &[]);
    assert_eq!(episodes(&root).len(), 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn transcript_capture_off_leaves_a_sessions_end_to_the_hooks() {
    let root = setup("end-off");
    in_projects(&root);
    learn(&root, &[("BORG_TRANSCRIPT_CAPTURE", "off")]);
    assert!(episodes(&root).is_empty());
    let _ = std::fs::remove_dir_all(root);
}
