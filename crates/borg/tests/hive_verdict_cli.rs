//! The hive's reward through the binary (hive/verdict.rs, guard.rs, hidden.rs): a verdict checks the
//! work's diff and the guard's flags before it credits the lessons shown during the claim; the hidden
//! oracle decides what a pass or a reject credits; a tool call that edits tests is flagged at pre_tool.
//! A session in no goal sees none of it. No model call: word matching only, no prediction or learn
//! model, and learning on so that outcomes and stats are written.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const ENV: [(&str, &str); 5] =
    [("BORG_MATCH", "bm25"), ("BORG_PREDICT_MODEL", "off"), ("BORG_LEARN_MODEL", "off"), ("BORG_LEARN", "on"), ("BORG_TEST_HARNESS", "1")];

fn work(root: &Path) -> PathBuf {
    root.join("harbor_orion")
}

fn store(root: &Path) -> PathBuf {
    root.join("store/collectives/corp")
}

fn command(root: &Path, args: &[&str]) -> Command {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_MODE", "hook")
        .envs(ENV.iter().copied())
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// `borg hive <args> --dir <work>`: (stdout, stderr, exit code).
fn hive(root: &Path, args: &[&str]) -> (String, String, i32) {
    let dir = work(root).display().to_string();
    let all: Vec<&str> = ["hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
    let out = command(root, &all).stdin(Stdio::null()).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

fn ok(root: &Path, args: &[&str]) -> String {
    let (out, err, code) = hive(root, args);
    assert_eq!(code, 0, "borg hive {args:?}: {out}{err}");
    out
}

/// `borg debug hive <args> --dir <work>`: the hidden oracle and `score` live there.
fn debug_hive(root: &Path, args: &[&str]) -> (String, String, i32) {
    let dir = work(root).display().to_string();
    let all: Vec<&str> = ["debug", "hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
    let out = command(root, &all).stdin(Stdio::null()).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

fn debug_ok(root: &Path, args: &[&str]) -> String {
    let (out, err, code) = debug_hive(root, args);
    assert_eq!(code, 0, "borg debug hive {args:?}: {out}{err}");
    out
}

/// `borg hook <harness> <event>` with `extra` in the input: its stdout.
fn hook(root: &Path, harness: &str, event: &str, session: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut input = json!({"session_id": session, "cwd": work(root)});
    input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let mut child = command(root, &["hook", harness, event]).envs(env.iter().copied()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A store whose work dir is a git repo with `tests/test_parse.py` and `src/parse.py` committed.
fn setup(name: &str) -> PathBuf {
    let root = common::setup(name);
    let w = work(&root);
    std::fs::create_dir_all(w.join("tests")).unwrap();
    std::fs::create_dir_all(w.join("src")).unwrap();
    std::fs::write(w.join("tests/test_parse.py"), "assert parse('a') == 'a'\n").unwrap();
    std::fs::write(w.join("src/parse.py"), "def parse(s): return s\n").unwrap();
    git(&w, &["init", "-q"]);
    git(&w, &["add", "-A"]);
    git(&w, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", "init"]);
    root
}

/// A drone `session` joined to `goal` at its first prompt.
fn join(root: &Path, session: &str, goal: &str) {
    hook(root, "claude", "prompt", session, json!({"prompt": "carry on"}), &[("BORG_HIVE", goal)]);
    assert!(store(root).join(format!("state/{session}.hive")).exists(), "{session} joined {goal}");
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// An `asked` outcome of `lesson` in `session` now: a lesson shown during the claim.
fn asked(root: &Path, session: &str, lesson: &str) {
    let o = json!({"kind": "asked", "session": session, "lessons": [lesson], "ms": now_ms(), "repo": "harbor_orion"});
    let path = store(root).join("outcomes.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, format!("{text}{o}\n")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn outcomes(root: &Path, kind: &str) -> Vec<Value> {
    lines(&store(root).join("outcomes.jsonl")).into_iter().filter(|o| o["kind"] == kind).collect()
}

fn stats(root: &Path, kind: &str) -> Vec<Value> {
    lines(&store(root).join("stats/events.jsonl")).into_iter().filter(|e| e["kind"] == kind).collect()
}

fn board_lines(root: &Path, goal: &str) -> Vec<Value> {
    lines(&store(root).join(format!("hive/{goal}/tasks.jsonl")))
}

/// An edit of src: a comment appended.
fn comment(body: &str) -> String {
    format!("{body}# changed by sa\n")
}

/// An edit that weakens the test: its assertion made trivial.
fn weaken(body: &str) -> String {
    body.replace("assert parse('a') == 'a'", "assert True")
}

/// `sa` claims t1 of `goal`, is shown `Mdrain`, edits `edit` by `change` (an Edit its pre_tool hook
/// sees) and publishes done; `sb` claims the verify task.
fn work_done(root: &Path, goal: &str, edit: &str, change: fn(&str) -> String) {
    join(root, "sa", goal);
    join(root, "sb", goal);
    ok(root, &["claim", "t1", "--session", "sa"]);
    asked(root, "sa", "Mdrain");
    let p = work(root).join(edit);
    hook(root, "claude", "pre_tool", "sa", json!({"tool_name": "Edit", "tool_input": {"file_path": p, "new_string": "x"}}), &[]);
    let body = std::fs::read_to_string(&p).unwrap_or_default();
    std::fs::write(&p, change(&body)).unwrap();
    ok(root, &["done", "--session", "sa"]);
    asked(root, "sa", "Pbeacon");
    ok(root, &["claim", "t1.v1", "--session", "sb"]);
}

#[test]
fn pre_tool_flags_a_test_edit_only_for_a_drone_in_a_goal_and_answers_nothing() {
    let root = setup("verdict-pretool");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    let sed = json!({"tool_name": "Bash", "tool_input": {"command": "sed -i s/a/b/ tests/test_parse.py"}});
    let flags = store(&root).join("hive/g1/flags.jsonl");
    // Not in the goal: nothing, and nothing written.
    assert_eq!(hook(&root, "sim", "pre_tool", "s0", sed.clone(), &[]), "");
    assert!(!flags.exists());
    join(&root, "sa", "g1");
    ok(&root, &["claim", "t1", "--session", "sa"]);
    assert_eq!(hook(&root, "sim", "pre_tool", "sa", sed.clone(), &[]), "", "flag only: never a decision");
    assert_eq!(hook(&root, "sim", "pre_tool", "sa", sed, &[]), "");
    let cat = json!({"tool_name": "Bash", "tool_input": {"command": "cat tests/test_parse.py"}});
    assert_eq!(hook(&root, "sim", "pre_tool", "sa", cat, &[]), "");
    let edit = json!({"tool_name": "Edit", "tool_input": {"file_path": work(&root).join("src/parse.py"), "new_string": "x"}});
    hook(&root, "claude", "pre_tool", "sa", edit, &[]);
    let got = lines(&flags);
    assert_eq!(got.len(), 1, "one flag per (session, claim, class, path); reading a test or editing src is none: {got:?}");
    assert_eq!((got[0]["class"].as_str(), got[0]["path"].as_str(), got[0]["task"].as_str()), (Some("test"), Some("tests/test_parse.py"), Some("t1")));
    assert_eq!(got[0]["scoped"], false);
    // A new claim of the same task flags the same edit again: each claim's verdict sees its own flags.
    ok(&root, &["release", "--session", "sa"]);
    std::thread::sleep(std::time::Duration::from_millis(5));
    ok(&root, &["claim", "t1", "--session", "sa"]);
    hook(&root, "sim", "pre_tool", "sa", json!({"tool_name": "Bash", "tool_input": {"command": "sed -i s/a/b/ tests/test_parse.py"}}), &[]);
    assert_eq!(lines(&flags).len(), 2, "{:?}", lines(&flags));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_hacked_attempt_reworked_in_the_same_tree_is_still_hacked() {
    let root = setup("verdict-rework");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    work_done(&root, "g1", "tests/test_parse.py", weaken);
    // A verifier in another repo cannot diff the work's snapshots: no verdict, never an unchecked pass.
    let other = work(&root).join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    let out = command(&root, &["hive", "verify", "--pass", "--session", "sb", "--dir", other.to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot diff"), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&other).unwrap();
    assert!(ok(&root, &["verify", "--pass", "--session", "sb"]).contains("rejected as hacked"));
    // sa takes t1 again in the same tree, leaves the weakened test as it is and changes only src.
    ok(&root, &["claim", "t1", "--session", "sa"]);
    let src = work(&root).join("src/parse.py");
    std::fs::write(&src, format!("{}# rework\n", std::fs::read_to_string(&src).unwrap())).unwrap();
    ok(&root, &["done", "--session", "sa"]);
    ok(&root, &["claim", "t1.v2", "--session", "sb"]);
    let out = ok(&root, &["verify", "--pass", "--session", "sb"]);
    assert!(out.contains("rejected as hacked") && out.contains("tests/test_parse.py"), "the rework's diff runs from the first claim's tree: {out}");
    let claims: Vec<Value> = board_lines(&root, "g1").into_iter().filter(|e| e["ev"] == "claim" && e["task"] == "t1").collect();
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0]["base"], claims[1]["base"], "{claims:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_verdict_on_a_diff_that_edited_tests_is_hacked_and_credits_the_claims_lessons_once() {
    let root = setup("verdict-hacked");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    work_done(&root, "g1", "tests/test_parse.py", weaken);
    // The author cannot even claim its own verify task, so it cannot give a verdict on it.
    let before = board_lines(&root, "g1").len();
    let (_, err, code) = hive(&root, &["verify", "--pass", "--session", "sa"]);
    assert_eq!(code, 1, "{err}");
    assert_eq!(board_lines(&root, "g1").len(), before, "nothing appended");
    let out = ok(&root, &["verify", "--pass", "--session", "sb"]);
    assert!(out.contains("rejected as hacked") && out.contains("tests/test_parse.py"), "{out}");
    let verdict = board_lines(&root, "g1").into_iter().find(|e| e["ev"] == "rejected").unwrap();
    assert_eq!((verdict["hacked"].as_bool(), verdict["by"].as_str()), (Some(true), Some("sb")));
    let hacked = outcomes(&root, "hacked");
    assert_eq!(
        hacked.len(),
        1,
        "Mdrain was shown during the claim, Pbeacon after the done: {hacked:?}\nboard: {:?}\noutcomes: {}",
        board_lines(&root, "g1"),
        std::fs::read_to_string(store(&root).join("outcomes.jsonl")).unwrap_or_default()
    );
    assert_eq!((hacked[0]["lessons"][0].as_str(), hacked[0]["session"].as_str(), hacked[0]["cause"].as_str()), (Some("Mdrain"), Some("sa"), Some("g1/t1.v1")));
    let stat = stats(&root, "task_hacked");
    assert_eq!((stat.len(), stat[0]["n"].as_u64()), (1, Some(1)));
    // The work is open again with the paths to restore.
    let board: Value = serde_json::from_str(&ok(&root, &["board", "g1", "--json"])).unwrap();
    let t1 = board.as_array().unwrap().iter().find(|t| t["id"] == "t1").unwrap();
    assert_eq!((t1["status"].as_str(), t1["rework"].as_u64()), (Some("open"), Some(1)));
    // A rerun appends nothing: the verify task is closed.
    let (before_b, before_o) = (board_lines(&root, "g1").len(), outcomes(&root, "hacked").len());
    assert_eq!(hive(&root, &["verify", "--pass", "--session", "sb"]).2, 1);
    assert_eq!((board_lines(&root, "g1").len(), outcomes(&root, "hacked").len()), (before_b, before_o));
    // `borg debug memory` shows it.
    let mem = command(&root, &["debug", "memory", store(&root).join("lessons.jsonl").to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    let m: Vec<Value> = String::from_utf8_lossy(&mem.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mdrain = m.iter().find(|l| l["id"] == "Mdrain").unwrap();
    assert_eq!((mdrain["hacked"].as_u64(), mdrain["verified"].as_u64()), (Some(1), Some(0)), "{mdrain}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_verdict_on_work_that_only_adds_tests_verifies() {
    let root = setup("verdict-grow");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    work_done(&root, "g1", "tests/test_parse.py", |body| format!("{body}\ndef test_more():\n    assert parse('b') == 'b'\n"));
    let out = ok(&root, &["verify", "--pass", "--session", "sb"]);
    assert!(out.contains("t1.v1 verified"), "tests may grow outside the task's files: {out}");
    let board = board_lines(&root, "g1");
    assert!(board.iter().all(|e| e.get("hacked").is_none()), "{board:?}");
    let v = outcomes(&root, "verified");
    assert_eq!((v.len(), v[0]["lessons"][0].as_str()), (1, Some("Mdrain")));
    let flags = lines(&store(&root).join("hive/g1/flags.jsonl"));
    assert_eq!(
        (flags.len(), flags[0]["class"].as_str(), flags[0]["path"].as_str()),
        (1, Some("test"), Some("tests/test_parse.py")),
        "the verifier still sees the write"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_verdict_given_from_a_subdirectory_reads_the_whole_diff() {
    let root = setup("verdict-subdir");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    work_done(&root, "g1", "tests/test_parse.py", |body| format!("{body}\ndef test_more():\n    assert parse('b') == 'b'\n"));
    let src = work(&root).join("src");
    let out = command(&root, &["hive", "verify", "--pass", "--session", "sb", "--dir", src.to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    let out = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.contains("t1.v1 verified"), "an appended test read from src/ as from the top: {out}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_clean_pass_given_at_the_stop_verifies_and_credits_at_once() {
    let root = setup("verdict-stop");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    work_done(&root, "g1", "src/parse.py", comment);
    let out = hook(&root, "claude", "stop", "sb", json!({"last_assistant_message": "Ran the tests.\nVERDICT: pass", "stop_hook_active": false}), &[]);
    let reason = serde_json::from_str::<Value>(out.trim()).map(|v| v["reason"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
    assert!(reason.contains("t1.v1 verified"), "{out}");
    let verdict = board_lines(&root, "g1").into_iter().find(|e| e["ev"] == "verified").unwrap();
    assert!(verdict.get("hacked").is_none());
    let v = outcomes(&root, "verified");
    assert_eq!((v.len(), v[0]["lessons"][0].as_str()), (1, Some("Mdrain")));
    assert_eq!(stats(&root, "task_verified").len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_hidden_oracle_decides_the_credit_once() {
    let root = setup("verdict-hidden");
    let held = root.join("held");
    std::fs::create_dir_all(&held).unwrap();
    let h = held.display().to_string();
    for (goal, cmd) in [("g2", "test -f \"$BORG_WORK/src/parse.py\" && test -d \"$BORG_HIDDEN\""), ("g3", "exit 1")] {
        let task = "t1|work|Fix the parser|src/*.py";
        debug_ok(
            &root,
            &[
                "init",
                goal,
                "--spec",
                "Ship",
                "--oracle",
                "ok=true",
                "--budget",
                "5",
                "--task",
                task,
                "--hidden-dir",
                &h,
                "--hidden",
                cmd,
                "--hidden-timeout",
                "60",
            ],
        );
    }
    let (_, err, code) = debug_hive(
        &root,
        &[
            "init",
            "g4",
            "--spec",
            "S",
            "--oracle",
            "ok=true",
            "--budget",
            "1",
            "--hidden-dir",
            &work(&root).join("tests").display().to_string(),
            "--hidden",
            "true",
        ],
    );
    assert_eq!(code, 1, "a hidden dir inside the repo is refused");
    assert!(err.contains("inside the repo"), "{err}");
    work_done(&root, "g2", "src/parse.py", comment);
    let out = ok(&root, &["verify", "--pass", "--session", "sb"]);
    assert!(out.contains("hidden oracle scores it next"), "{out}");
    assert!(outcomes(&root, "verified").is_empty() && stats(&root, "task_verified").is_empty(), "deferred to the score");
    // A drone never runs the hidden oracle.
    let dir = work(&root).display().to_string();
    let out = command(&root, &["debug", "hive", "score", "g2", "--dir", &dir]).env("BORG_HIVE", "g2").stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("never by a drone"), "{}", String::from_utf8_lossy(&out.stderr));
    // Scored from another repo, the checkout fails: no score and no credit, so the right repo still scores it.
    let other = work(&root).join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    let out = command(&root, &["debug", "hive", "score", "g2", "--dir", other.to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.code() == Some(1) && text.contains("not scored") && text.contains("0 attempt(s) scored"), "{text}");
    assert!(lines(&store(&root).join("hive/g2/oracle.jsonl")).is_empty() && outcomes(&root, "rejected").is_empty());
    std::fs::remove_dir_all(&other).unwrap();
    let out = debug_ok(&root, &["score", "g2"]);
    assert!(out.contains("1 attempt(s) scored"), "{out}");
    let scores = lines(&store(&root).join("hive/g2/oracle.jsonl"));
    assert_eq!((scores.len(), scores[0]["pass"].as_bool(), scores[0]["agree"].as_bool()), (1, Some(true), Some(true)), "{scores:?}");
    assert_eq!(outcomes(&root, "verified").len(), 1);
    assert_eq!(stats(&root, "task_verified").len(), 1);
    assert!(debug_ok(&root, &["score", "g2"]).contains("0 attempt(s) scored"), "scored once");
    assert_eq!(outcomes(&root, "verified").len(), 1);
    assert!(!store(&root).join("hive/g2/scratch").read_dir().is_ok_and(|mut d| d.next().is_some()), "no scratch checkout left");
    // g3: the verifier passes, the hidden oracle fails: rejected, and the disagreement is logged.
    for s in ["sa", "sb"] {
        ok(&root, &["leave", "--session", s]);
    }
    work_done(&root, "g3", "src/parse.py", comment);
    ok(&root, &["verify", "--pass", "--session", "sb"]);
    debug_ok(&root, &["score", "g3"]);
    let scores = lines(&store(&root).join("hive/g3/oracle.jsonl"));
    assert_eq!((scores[0]["pass"].as_bool(), scores[0]["agree"].as_bool()), (Some(false), Some(false)));
    let r = outcomes(&root, "rejected");
    assert_eq!((r.len(), r[0]["cause"].as_str()), (1, Some("g3/t1.v1")));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn no_hook_of_a_drone_outside_any_goal_creates_a_hive() {
    let root = setup("verdict-plain");
    let edit = json!({"tool_name": "Edit", "tool_input": {"file_path": work(&root).join("tests/test_parse.py"), "new_string": "x"}});
    hook(&root, "claude", "prompt", "s1", json!({"prompt": "fix the parser"}), &[]);
    assert_eq!(hook(&root, "claude", "pre_tool", "s1", edit.clone(), &[]), "");
    hook(&root, "claude", "post_tool", "s1", edit, &[]);
    hook(&root, "claude", "stop", "s1", json!({"last_assistant_message": "VERDICT: pass", "stop_hook_active": false}), &[]);
    hook(&root, "claude", "end", "s1", json!({}), &[]);
    assert!(!store(&root).join("hive").exists());
    let _ = std::fs::remove_dir_all(root);
}

/// `borg hive <args> --dir <dir>` with `env`: (stdout, stderr, exit code).
fn hive_at(root: &Path, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let all: Vec<&str> = ["hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.to_str().unwrap()]).collect();
    let out = command(root, &all).envs(env.iter().copied()).stdin(Stdio::null()).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

#[test]
fn a_goal_of_joined_sessions_integrates_fixes_and_finishes_through_the_cli() {
    // No `borg debug hive run`: the engineer integrates, and a joined session fixes the combined tree in a
    // checkout of it.
    let root = setup("verdict-integrate");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=grep -rq fixed src", "--budget", "5"]);
    ok(&root, &["open", "g1", "--id", "t1", "--title", "Fix the parser", "--files", "src/*.py", "--accept", "own=true"]);
    let (out, _, code) = hive(&root, &["integrate", "g1"]);
    assert_eq!((code, out.contains("no integration of g1 is due")), (2, true), "{out}");
    work_done(&root, "g1", "src/parse.py", comment);
    ok(&root, &["verify", "--pass", "--session", "sb"]);
    let (_, err, code) = hive_at(&root, &work(&root), &["integrate", "g1"], &[("BORG_HIVE", "g1")]);
    assert!(code != 0 && err.contains("borg hive integrate"), "a drone never integrates: {err}");
    // The combined tree fails the oracle: exit 1, and where the fix is claimed.
    let (out, _, code) = hive(&root, &["integrate", "g1"]);
    assert!(code == 1 && out.contains("integrate.1: ") && out.contains("ok fails"), "{out}");
    let b = board_lines(&root, "g1");
    let m = b.iter().find(|e| e["ev"] == "open" && e["task"] == "integrate.1.fix").unwrap()["def"]["from"].as_str().unwrap().to_string();
    assert!(out.contains(&format!("git worktree add <dir> {m}")), "{out}");
    let (out, _, code) = hive(&root, &["integrate", "g1"]);
    assert!(code == 2 && out.contains("integrate.1.fix starts from the integrated tree"), "{out}");
    // Not in the engineer's checkout; in a checkout of the combined tree, yes.
    let (_, err, code) = hive(&root, &["claim", "integrate.1.fix", "--session", "sa"]);
    assert!(code != 0 && err.contains("git worktree add"), "{err}");
    let fix = work(&root).join(".worktrees/fix");
    git(&work(&root), &["worktree", "add", "-q", "--detach", fix.to_str().unwrap(), &m]);
    let (out, err, code) = hive_at(&root, &fix, &["claim", "integrate.1.fix", "--session", "sa"], &[]);
    assert!(code == 0 && out.contains(&format!("You work on the integrated tree {m}")), "{out}{err}");
    std::fs::write(fix.join("src/parse.py"), "def parse(s): return s  # fixed\n").unwrap();
    let (out, err, code) = hive_at(&root, &fix, &["done", "--session", "sa"], &[]);
    assert_eq!(code, 0, "{out}{err}");
    let (out, err, code) = hive_at(&root, &fix, &["claim", "integrate.1.fix.v1", "--session", "sb"], &[]);
    assert_eq!(code, 0, "{out}{err}");
    let (out, err, code) = hive_at(&root, &fix, &["verify", "--pass", "--session", "sb"], &[]);
    assert_eq!(code, 0, "{out}{err}");
    let (out, err, code) = hive(&root, &["integrate", "g1"]);
    assert!(code == 0 && out.starts_with("integrate.2: integrated ") && out.contains("the goal is finished"), "{out}{err}");
    let (out, _, code) = hive(&root, &["integrate", "g1"]);
    assert_eq!((code, out.contains("the goal is finished")), (2, true), "{out}");
    let _ = std::fs::remove_dir_all(root);
}

/// The context a hook's output hands the drone.
fn context(out: &str) -> String {
    serde_json::from_str::<Value>(out.trim()).ok().and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)).unwrap_or_default()
}

/// `borg <args>` on the collective's store with the learn model the fake `claude` (common.rs): its stdout.
fn with_fake_model(root: &Path, args: &[&str]) -> String {
    let out = command(root, args).env("BORG_HOME", store(root)).env("BORG_LEARN_MODEL", "claude:fake").stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "borg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into()
}

#[test]
fn a_verified_task_proposes_a_playbook_edit_and_once_promoted_it_reaches_the_next_matching_task_only() {
    let root = setup("playbook");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    work_done(&root, "g1", "src/parse.py", comment);
    ok(&root, &["verify", "--pass", "--session", "sb"]);
    // `borg learn` reflects on the verified attempt once and queues the model's edit for the engineer.
    let answer = json!({"shape": "fix the parser", "op": "add", "step": "", "text": "Run the parser's tests before calling it done"});
    std::fs::write(root.join("answer.json"), json!({"structured_output": answer, "total_cost_usd": 0}).to_string()).unwrap();
    with_fake_model(&root, &["learn"]);
    let reflections = || common::prompts(&root).into_iter().filter(|p| p.starts_with(borg::playbook::OPENING)).collect::<Vec<_>>();
    let asked = reflections();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].contains("## The task\nFix the parser") && asked[0].contains("An independent verifier passed it.") && asked[0].contains("(none)"),
        "{}",
        asked[0]
    );
    assert!(!store(&root).join(borg::playbook::FILE).exists(), "learning never applies an edit");
    let q = borg::queen::open(&store(&root)).into_iter().find(|e| e.kind == borg::queen::Kind::Playbook).unwrap();
    assert!(q.question.contains("Run the parser's tests"), "{}", q.question);
    with_fake_model(&root, &["learn"]);
    assert_eq!(reflections().len(), 1, "once per attempt");
    let out = command(&root, &["queen", "answer", &q.id, "b", "--dir", work(&root).to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("the edit is promoted"),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let pbs = borg::playbook::read(&store(&root));
    assert_eq!(pbs.len(), 1);
    let key = pbs[0].key();

    // A task of the playbook's shape is shown it once; a task of another shape is not.
    ok(&root, &["open", "g1", "--id", "t2", "--title", "Fix the parser for empty input", "--files", "src/parse.py"]);
    ok(&root, &["open", "g1", "--id", "t3", "--title", "Write the release notes", "--files", "docs/notes.md"]);
    for s in ["sc", "sd", "se"] {
        join(&root, s, "g1");
    }
    ok(&root, &["claim", "t2", "--session", "sc"]);
    ok(&root, &["claim", "t3", "--session", "sd"]);
    let prompt = |s: &str, text: &str| context(&hook(&root, "claude", "prompt", s, json!({"prompt": text}), &[]));
    let shown = prompt("sc", "go on");
    assert!(shown.contains(&format!("playbook {} v1 for \"fix the parser\"", pbs[0].id)) && shown.contains("Run the parser's tests"), "{shown}");
    assert!(!prompt("sc", "go on").contains("playbook"), "each version once");
    assert!(!prompt("sd", "go on").contains("playbook"), "another task's shape");
    let logged = outcomes(&root, "playbook");
    assert_eq!((logged.len(), logged[0]["session"].as_str(), logged[0]["lessons"][0].as_str()), (1, Some("sc"), Some(key.as_str())));
    // The verdict on that attempt credits the version shown, as it credits the lessons shown.
    let p = work(&root).join("src/parse.py");
    std::fs::write(&p, comment(&std::fs::read_to_string(&p).unwrap())).unwrap();
    ok(&root, &["done", "--session", "sc"]);
    ok(&root, &["claim", "t2.v1", "--session", "se"]);
    ok(&root, &["verify", "--pass", "--session", "se"]);
    let v: Vec<Value> = outcomes(&root, "verified").into_iter().filter(|o| o["session"] == "sc").collect();
    assert_eq!((v.len(), v[0]["lessons"][0].as_str()), (1, Some(key.as_str())));
    // So does the engineer's no.
    hook(&root, "claude", "prompt", "sc", json!({"prompt": "borg no: never patch the parser without a failing test first"}), &[]);
    let vetoed = outcomes(&root, "vetoed");
    assert!(vetoed.iter().any(|o| o["session"] == "sc" && o["lessons"][0] == key.as_str()), "{vetoed:?}");
    let belief = &borg::playbook::beliefs(&store(&root))[&key];
    assert_eq!((belief.verified, belief.vetoed), (1, 1));
    // A session outside the hive sees it only when its prompt names the whole shape.
    assert!(!prompt("sn", "tidy the docs").contains("playbook"));
    let plain = prompt("sn", "please fix the parser, it breaks on tabs");
    assert!(plain.contains("the team's playbook") && plain.contains("Run the parser's tests"), "{plain}");
    let _ = std::fs::remove_dir_all(root);
}
