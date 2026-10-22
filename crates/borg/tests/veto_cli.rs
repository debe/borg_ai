//! `borg no` through the binary: a terminal veto finds the repo's drone (or asks which), credits the
//! lessons it was shown, records the reason as a veto episode and counts it; an off repo is left without
//! a file. In the session, a prompt starting "borg no" is the same veto, and the drone is told to stop.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-veto-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `borg <args>` in `dir`: (stdout, stderr, exit code).
fn borg(root: &Path, dir: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> (String, String, Option<i32>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_MATCH", "bm25") // word matching only: no model call from a test
        .env("BORG_PREDICT_MODEL", "off")
        .env("BORG_LEARN_MODEL", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_MODE", "hook")
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .envs(env.iter().copied())
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code())
}

fn prompt(root: &Path, work: &Path, session: &str, text: &str, env: &[(&str, &str)]) -> String {
    let input = json!({"session_id": session, "cwd": work.display().to_string(), "prompt": text});
    let (out, _, code) = borg(root, root, &["hook", "claude", "prompt"], &input.to_string(), env);
    assert_eq!(code, Some(0));
    serde_json::from_str::<Value>(out.trim()).ok().and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)).unwrap_or_default()
}

fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = tmp(name);
    let (work, secret) = (root.join("work/app"), root.join("secret/x"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&secret).unwrap();
    let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&work).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["config", "user.email", "alice@x"]);
    let cfg = json!({"default": "off", "off": [root.join("secret")], "collectives": [{"name": "corp", "paths": [root.join("work")], "share": "local"}]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    (root, work, secret)
}

fn store(root: &Path) -> PathBuf {
    root.join("base/collectives/corp")
}

fn jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn of_kind(path: &Path, kind: &str) -> Vec<Value> {
    jsonl(path).into_iter().filter(|v| v["kind"] == kind).collect()
}

#[test]
fn a_terminal_veto_credits_the_shown_lessons_and_records_the_reason() {
    let (root, work, _) = setup("terminal");
    prompt(&root, &work, "aaaa-1111", "fix the merge lookup", &[]);
    let asked = json!({"kind": "asked", "session": "aaaa-1111", "lessons": ["Lmerge"], "ms": 1, "repo": "app"});
    std::fs::write(store(&root).join("outcomes.jsonl"), format!("{asked}\n")).unwrap();

    let (out, err, code) = borg(&root, &work, &["no", "never", "read", "the", "commit", "log", "for", "merges"], "", &[]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("vetoed One of One (aaaa-111)") && out.contains("Lmerge") && out.contains("borg steer aaaa-111 --pause"), "{out}");
    let vetoed = of_kind(&store(&root).join("outcomes.jsonl"), "vetoed");
    assert_eq!((vetoed.len(), vetoed[0]["lessons"].clone(), vetoed[0]["session"].clone()), (1, json!(["Lmerge"]), json!("aaaa-1111")));
    let eps = of_kind(&store(&root).join("episodes.jsonl"), "veto");
    assert_eq!(eps.len(), 1);
    assert_eq!(
        (eps[0]["correction"].as_str(), eps[0]["task"].as_str(), eps[0]["engineer"].as_str()),
        (Some("never read the commit log for merges"), Some("fix the merge lookup"), Some("alice@x"))
    );
    let ev = of_kind(&store(&root).join("stats/events.jsonl"), "veto");
    assert_eq!((ev.len(), ev[0]["why"].as_str(), ev[0]["lessons"].clone()), (1, Some("cli"), json!(["Lmerge"])));
    assert!(of_kind(&store(&root).join("stats/events.jsonl"), "correction").is_empty(), "a veto is no correction");

    // --json for scripts; a second veto credits nothing new.
    let (out, _, code) = borg(&root, &work, &["no", "--json", "and not the reflog either"], "", &[]);
    assert_eq!(code, Some(0));
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!((v["session"].as_str(), v["credited"].clone()), (Some("aaaa-1111"), json!([])));
    assert_eq!(of_kind(&store(&root).join("outcomes.jsonl"), "vetoed").len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_drones_in_the_repo_need_a_session_and_bad_input_records_nothing() {
    let (root, work, _) = setup("ambiguous");
    prompt(&root, &work, "aaaa-1111", "fix the merge lookup", &[]);
    prompt(&root, &work, "bbbb-2222", "write the docs", &[]);
    let (out, err, code) = borg(&root, &work, &["no", "never read the commit log"], "", &[]);
    assert_eq!(code, Some(1));
    assert!(err.contains("2 drones work in app") && err.contains("aaaa-111") && err.contains("bbbb-222"), "{out}{err}");
    assert!(of_kind(&store(&root).join("episodes.jsonl"), "veto").is_empty());
    let (out, err, code) = borg(&root, &work, &["no", "--session", "bbbb", "never read the commit log"], "", &[]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(of_kind(&store(&root).join("episodes.jsonl"), "veto")[0]["session"], "bbbb-2222");
    // --dir from elsewhere names the repo the same way.
    let (_, _, code) = borg(&root, &root, &["no", "--dir", &work.display().to_string(), "--session", "aaaa", "not this way"], "", &[]);
    assert_eq!(code, Some(0));
    // No reason: usage.
    let (_, err, code) = borg(&root, &work, &["no", "--session", "bbbb"], "", &[]);
    assert_eq!(code, Some(2), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_off_repo_records_nothing() {
    let (root, _, secret) = setup("off");
    let (out, err, code) = borg(&root, &secret, &["no", "never read the commit log"], "", &[]);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(err.starts_with("off:"), "{err}");
    assert!(!root.join("base").exists(), "an off repo leaves no file");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_veto_said_in_the_session_tells_the_drone_and_keeps_the_task() {
    let (root, work, _) = setup("prompt");
    prompt(&root, &work, "aaaa-1111", "fix the merge lookup", &[]);
    let ctx = prompt(&root, &work, "aaaa-1111", "borg no don't touch the public schema", &[]);
    assert!(ctx.contains("said no to this work") && ctx.contains("don't touch the public schema"), "{ctx}");
    let eps = of_kind(&store(&root).join("episodes.jsonl"), "veto");
    assert_eq!((eps.len(), eps[0]["correction"].as_str()), (1, Some("don't touch the public schema")));
    assert_eq!(std::fs::read_to_string(store(&root).join("state/aaaa-1111.task")).unwrap(), "fix the merge lookup");
    assert_eq!(of_kind(&store(&root).join("stats/events.jsonl"), "veto")[0]["why"], "prompt");

    // BORG_VETO=off: an ordinary prompt.
    let ctx = prompt(&root, &work, "bbbb-2222", "borg no don't touch the public schema", &[("BORG_VETO", "off")]);
    assert!(!ctx.contains("said no"), "{ctx}");
    assert!(of_kind(&store(&root).join("episodes.jsonl"), "veto").iter().all(|e| e["session"] != "bbbb-2222"));
    let _ = std::fs::remove_dir_all(&root);
}
