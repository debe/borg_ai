//! `borg move`, `borg correct` and their doctor checks through the binary: collectives never mix on
//! their own, and crossing one is the engineer's explicit act. A lesson moves as a dry run first, then
//! with --apply; a correction lands in the collective it names and nowhere else; `borg doctor` names
//! the evidence a re-routed repo left behind with the command that moves it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-moves-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `borg <args>` in `dir`: (stdout, stderr, exit code).
fn borg(root: &Path, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = command(root, dir, args).stdin(Stdio::null()).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code())
}

/// `borg <args>` in `dir`, isolated in `root`; no model (a test that learns puts a fake `claude` first
/// on PATH and names it, `fake_model`).
fn command(root: &Path, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("BORG_PREDICT_MODEL", "off")
        .env("BORG_LEARN_MODEL", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_REVIEW_FIX", "off")
        .env("BORG_TRANSCRIPT_CAPTURE", "off")
        .env("BORG_HOME", root.join("base"))
        .env("BORG_STATE", root.join("state"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// A `claude` that answers every learning prompt with one lesson and keeps each prompt in
/// `<root>/prompts.txt`.
fn fake_model(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let answer = json!({"structured_output": {"is_lesson": true, "question": "Q", "situations": [], "consequence": "c", "same_as": "",
        "area": "", "supersedes": [], "reach": "team"}, "total_cost_usd": 0});
    let script = format!("#!/bin/sh\nprintf '%s\\n---\\n' \"$2\" >> '{}'\necho '{answer}'\n", root.join("prompts.txt").display());
    std::fs::write(root.join("bin/claude"), script).unwrap();
    std::fs::set_permissions(root.join("bin/claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// One hook event of a Claude Code session.
fn hook(root: &Path, dir: &Path, event: &str, input: &Value) {
    use std::io::Write;
    let mut child = command(root, dir, &["hook", "claude", event]).stdin(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    child.wait_with_output().unwrap();
}

/// Two team collectives: `corp` (work/app) and `oss` (oss/lib), each repo with this engineer's email.
fn setup(name: &str) -> PathBuf {
    let root = tmp(name);
    std::fs::create_dir_all(root.join("home")).unwrap();
    for repo in ["work/app", "oss/lib"] {
        let dir = root.join(repo);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
        git(&["init", "-q"]);
        git(&["config", "user.email", "me@x"]);
    }
    let cfg = json!({"default": "off", "collectives": [
        {"name": "corp", "paths": [root.join("work")], "share": "team"},
        {"name": "oss", "paths": [root.join("oss")], "share": "team"}]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    root
}

fn store(root: &Path, name: &str) -> PathBuf {
    root.join("base/collectives").join(name)
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn append(path: &Path, v: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    text.push_str(&format!("{v}\n"));
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_lesson_moves_as_a_dry_run_first_then_on_apply() {
    let root = setup("lesson");
    let app = root.join("work/app");
    let corp = store(&root, "corp").join("lessons.jsonl");
    append(
        &corp,
        &json!({"id": "L1", "origin_ms": 1, "repo": "lib", "question": "Does the parser keep upstream's grammar?", "situations": [], "consequence": "forks drift",
        "support": 1, "episodes": ["e1"], "history": [], "version": 1, "engineers": ["me@x"]}),
    );
    let before = std::fs::read_to_string(&corp).unwrap();
    let (out, err, code) = borg(&root, &app, &["move", "L1", "--to", "oss"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("oss: append v2 to lessons.jsonl, reach team") && out.contains("dry run: nothing changed"), "{out}");
    assert_eq!(std::fs::read_to_string(&corp).unwrap(), before, "a dry run writes nothing");
    assert!(!store(&root, "oss").join("lessons.jsonl").exists());

    let (out, err, code) = borg(&root, &app, &["move", "L1", "--to", "oss", "--apply"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.trim_end().ends_with("moved"), "{out}");
    let oss = lines(&store(&root, "oss").join("lessons.jsonl"));
    assert_eq!((oss[0]["id"].as_str(), oss[0]["version"].as_u64()), (Some("L1"), Some(2)));
    assert!(oss[0].get("moved_from").is_none(), "the source's name stays off the target's synced lessons");
    assert_eq!(lines(&store(&root, "oss").join("placed.jsonl"))[0]["from"], "corp", "noted locally instead");
    let tomb = lines(&corp).pop().unwrap();
    assert_eq!((tomb["question"].as_str(), tomb["episodes"].clone()), (Some("(moved to another collective)"), json!(["e1"])));

    let (_, err, code) = borg(&root, &app, &["move", "L1", "--to", "nope"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("the collectives are: corp, oss"), "{err}");
}

#[test]
fn a_correction_lands_in_the_named_collective_only() {
    let root = setup("correct");
    let app = root.join("work/app");
    let (out, err, code) = borg(&root, &app, &["correct", "--collective", "oss", "--session", "sess-1", "keep", "the", "upstream", "grammar"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.starts_with("recorded in oss: episode correct-sess-1-"), "{out}");
    let eps = lines(&store(&root, "oss").join("episodes.jsonl"));
    assert_eq!((eps.len(), eps[0]["correction"].as_str(), eps[0]["via"].as_str()), (1, Some("keep the upstream grammar"), Some("correct")));
    assert_eq!(eps[0]["engineer"], "", "the session repo's email stays in its collective; oss knows none without --repo");
    assert!(!store(&root, "corp").join("episodes.jsonl").exists(), "nothing in the session's own collective");

    // One session, two collectives: one command each.
    let (_, err, code) = borg(&root, &app, &["correct", "--collective", "corp", "--session", "sess-1", "name", "the", "ticket", "in", "commits"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(lines(&store(&root, "corp").join("episodes.jsonl")).len(), 1);
    assert_eq!(lines(&store(&root, "oss").join("episodes.jsonl")).len(), 1);

    for bad in ["nope", "off"] {
        let (_, err, code) = borg(&root, &app, &["correct", "--collective", bad, "--session", "sess-1", "words"]);
        assert_eq!(code, Some(2), "{bad}: {err}");
        assert!(err.contains("the collectives are: corp, oss"), "{err}");
    }
    let (_, err, code) = borg(&root, &app, &["correct", "--session", "sess-1", "words"]);
    assert_eq!(code, Some(2));
    assert!(err.starts_with("usage: borg correct"), "{err}");
}

#[test]
fn doctor_names_held_evidence_and_the_move_takes_it() {
    let root = setup("evidence");
    let app = root.join("work/app");
    let corp = store(&root, "corp");
    for id in ["h1", "h2"] {
        append(
            &corp.join("episodes.jsonl"),
            &json!({"id": id, "ms": 1, "repo": "lib", "session": format!("s-{id}"), "task": "", "agent": "",
            "rejected_tool": null, "correction": format!("words of {id}")}),
        );
    }
    let lib = root.join("oss/lib");
    let (out, _, _) = borg(&root, &app, &["doctor"]);
    let want = format!("WARN evidence: 2 waiting episode(s) of lib are in corp, but lib routes to oss now: borg move {} --from corp --to oss", lib.display());
    assert!(out.lines().any(|l| l == want), "{out}");

    // Evidence goes only where its repo routes now: anywhere else it would wait unlearned again.
    let (_, err, code) = borg(&root, &app, &["move", &lib.display().to_string(), "--to", "corp", "--apply"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("routes to oss, not corp"), "{err}");
    let (out, err, code) = borg(&root, &app, &["move", &lib.display().to_string(), "--to", "oss", "--apply"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("corp: 2 episode(s) to oss's episodes.jsonl"), "{out}");
    assert_eq!(lines(&store(&root, "oss").join("episodes.jsonl")).len(), 2);
    assert!(lines(&corp.join("episodes.jsonl")).is_empty());
    let (out, _, _) = borg(&root, &app, &["doctor"]);
    assert!(!out.contains("WARN evidence"), "{out}");
}

/// One session in corp, two corrections said in one prompt: the agent records each in its own collective.
/// The open-source one carries only the engineer's words (no task, proposal or work email); the raw
/// prompt the hook captured in corp is set aside, so corp learns only what was said for it; and the
/// session's end learns both stores.
#[test]
fn one_sessions_correction_goes_to_each_collective_it_concerns_and_nothing_else_crosses() {
    let root = setup("two");
    fake_model(&root);
    let app = root.join("work/app");
    let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&app).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
    git(&["config", "user.email", "ann@work.example"]);
    let cwd = app.display().to_string();
    let prompt = |p: &str| hook(&root, &app, "prompt", &json!({"session_id": "sess-9", "cwd": cwd, "prompt": p}));
    prompt("add the export endpoint to the billing service");
    prompt("no, don't do that: the ticket key goes in the commit subject, and in the parser library keep upstream's grammar");
    let corp = store(&root, "corp");
    let raw = lines(&corp.join("episodes.jsonl"));
    assert_eq!(raw.len(), 1, "the hook captured the correcting prompt in corp");

    let (out, err, code) = borg(&root, &app, &["correct", "--collective", "oss", "--session", "sess-9", "--repo", "lib", "keep", "upstream's", "grammar"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("set aside in corp"), "{out}");
    let oss = lines(&store(&root, "oss").join("episodes.jsonl"));
    assert_eq!((oss[0]["task"].as_str(), oss[0]["agent"].as_str()), (Some(""), Some("")), "nothing of corp's session crosses");
    assert_eq!(oss[0]["engineer"], "me@x", "the email the open-source repo knows, not the work one");
    let (_, err, code) = borg(&root, &app, &["correct", "--collective", "corp", "--session", "sess-9", "the", "ticket", "key", "goes", "in", "the", "subject"]);
    assert_eq!(code, Some(0), "{err}");
    let corp_eps = lines(&corp.join("episodes.jsonl"));
    assert_eq!(corp_eps.len(), 2);
    assert_eq!(corp_eps[1]["task"], "add the export endpoint to the billing service", "its own collective keeps the task");
    assert_eq!(corp_eps[1]["engineer"], "ann@work.example");

    // The session's end learns corp and oss; corp's model never sees the raw prompt.
    let mut learn = command(&root, &app, &["learn", "--session", "sess-9"]);
    let out = learn.env("BORG_LEARN_MODEL", "claude:fake").env("BORG_PREDICT_MODEL", "claude:").stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(lines(&store(&root, "oss").join("lessons.jsonl")).len(), 1, "learned in oss at the session's end");
    assert_eq!(lines(&corp.join("lessons.jsonl")).len(), 1, "corp learns its restatement only");
    let asked = std::fs::read_to_string(root.join("prompts.txt")).unwrap_or_default();
    assert!(!asked.contains("in the parser library keep upstream's grammar"), "the raw prompt is never learned: {asked}");
}
