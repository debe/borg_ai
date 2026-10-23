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

/// `sa` claims t1 of `goal`, is shown `Mdrain`, edits `edit` and publishes done; `sb` claims the verify task.
fn work_done(root: &Path, goal: &str, edit: &str) {
    join(root, "sa", goal);
    join(root, "sb", goal);
    ok(root, &["claim", "t1", "--session", "sa"]);
    asked(root, "sa", "Mdrain");
    let p = work(root).join(edit);
    let body = std::fs::read_to_string(&p).unwrap_or_default();
    std::fs::write(&p, format!("{body}# changed by sa\n")).unwrap();
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
    work_done(&root, "g1", "tests/test_parse.py");
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
    work_done(&root, "g1", "tests/test_parse.py");
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
    assert_eq!(hacked.len(), 1, "Mdrain was shown during the claim, Pbeacon after the done: {hacked:?}\nboard: {:?}\noutcomes: {}", board_lines(&root, "g1"), std::fs::read_to_string(store(&root).join("outcomes.jsonl")).unwrap_or_default());
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
    // `borg memory` shows it.
    let mem = command(&root, &["memory", store(&root).join("lessons.jsonl").to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    let m: Vec<Value> = String::from_utf8_lossy(&mem.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mdrain = m.iter().find(|l| l["id"] == "Mdrain").unwrap();
    assert_eq!((mdrain["hacked"].as_u64(), mdrain["verified"].as_u64()), (Some(1), Some(0)), "{mdrain}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_clean_pass_given_at_the_stop_verifies_and_credits_at_once() {
    let root = setup("verdict-stop");
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=true", "--budget", "5", "--task", "t1|work|Fix the parser|src/*.py"]);
    work_done(&root, "g1", "src/parse.py");
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
        ok(
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
    let (_, err, code) = hive(
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
    work_done(&root, "g2", "src/parse.py");
    let out = ok(&root, &["verify", "--pass", "--session", "sb"]);
    assert!(out.contains("hidden oracle scores it next"), "{out}");
    assert!(outcomes(&root, "verified").is_empty() && stats(&root, "task_verified").is_empty(), "deferred to the score");
    // A drone never runs the hidden oracle.
    let dir = work(&root).display().to_string();
    let out = command(&root, &["hive", "score", "g2", "--dir", &dir]).env("BORG_HIVE", "g2").stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("never by a drone"), "{}", String::from_utf8_lossy(&out.stderr));
    // Scored from another repo, the checkout fails: no score and no credit, so the right repo still scores it.
    let other = work(&root).join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    let out = command(&root, &["hive", "score", "g2", "--dir", other.to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.code() == Some(1) && text.contains("not scored") && text.contains("0 attempt(s) scored"), "{text}");
    assert!(lines(&store(&root).join("hive/g2/oracle.jsonl")).is_empty() && outcomes(&root, "rejected").is_empty());
    std::fs::remove_dir_all(&other).unwrap();
    let out = ok(&root, &["score", "g2"]);
    assert!(out.contains("1 attempt(s) scored"), "{out}");
    let scores = lines(&store(&root).join("hive/g2/oracle.jsonl"));
    assert_eq!((scores.len(), scores[0]["pass"].as_bool(), scores[0]["agree"].as_bool()), (1, Some(true), Some(true)), "{scores:?}");
    assert_eq!(outcomes(&root, "verified").len(), 1);
    assert_eq!(stats(&root, "task_verified").len(), 1);
    assert!(ok(&root, &["score", "g2"]).contains("0 attempt(s) scored"), "scored once");
    assert_eq!(outcomes(&root, "verified").len(), 1);
    assert!(!store(&root).join("hive/g2/scratch").read_dir().is_ok_and(|mut d| d.next().is_some()), "no scratch checkout left");
    // g3: the verifier passes, the hidden oracle fails: rejected, and the disagreement is logged.
    for s in ["sa", "sb"] {
        ok(&root, &["leave", "--session", s]);
    }
    work_done(&root, "g3", "src/parse.py");
    ok(&root, &["verify", "--pass", "--session", "sb"]);
    ok(&root, &["score", "g3"]);
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
