//! `borg hive run` through the binary (hive/run.rs): drones of the simulated harness (a scripted drone,
//! hive/sim_drone.rs) in worktrees of a real git repo, against the real board and the real hooks. No
//! model call: the stop runs with word matching only and no prediction model.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use borg::hive::{Status, board};
use serde_json::{Value, json};

fn work(root: &Path) -> PathBuf {
    root.join("harbor_orion")
}

fn store(root: &Path) -> PathBuf {
    root.join("store/collectives/corp")
}

fn goal_dir(root: &Path) -> PathBuf {
    store(root).join("hive/g")
}

fn command(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
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
        .env("BORG_LEARN", "off")
        .env("BORG_LEARN_MODEL", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_MATCH", "bm25")
        .env("BORG_PREDICT_MODEL", "off")
        .env("BORG_STOP_ATTENTION", "off")
        .env("BORG_MODE", "hook")
        .env("BORG_TEST_HARNESS", "1")
        .env("BORG_SIM_CAPS", "prompt,tool,deny,headless")
        .env("BORG_SIM_SCRIPT", root.join("script.json"))
        .envs(env.iter().copied())
        .current_dir(work(root))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// `borg hive <args> --dir <work>`: (stdout, stderr, exit code).
fn hive(root: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let dir = work(root).display().to_string();
    let all: Vec<&str> = ["hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
    let out = command(root, &all, env).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

fn ok(root: &Path, args: &[&str]) -> String {
    let (out, err, code) = hive(root, args, &[]);
    assert_eq!(code, 0, "borg hive {args:?}: {out}{err}");
    out
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=e@x", "-c", "user.name=E"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A committed git repo in the collective, a goal `g` with `budget` and one work task per file
/// (t1..tn, each with an oracle that wants `done tN` in fN.txt), and the sim drones' `script`.
fn setup(name: &str, tasks: usize, budget: &str, script: Value) -> PathBuf {
    let root = common::setup(&format!("hive-run-{name}"));
    let w = work(&root);
    git(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "orion\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-q", "-m", "start"]);
    std::fs::write(root.join("script.json"), script.to_string()).unwrap();
    ok(&root, &["init", "g", "--spec", "Write the files", "--oracle", "ok=true", "--budget", budget]);
    for i in 1..=tasks {
        let (id, file) = (format!("t{i}"), format!("f{i}.txt"));
        let accept = format!("ok=grep -q 'done {id}' {file}");
        ok(&root, &["open", "g", "--id", &id, "--title", &format!("write {file}"), "--files", &file, "--accept", &accept]);
    }
    root
}

fn run(root: &Path, args: &[&str]) -> (String, String, i32) {
    let all: Vec<&str> = ["run", "g"].iter().copied().chain(args.iter().copied()).collect();
    hive(root, &all, &[])
}

fn fold(root: &Path) -> board::Board {
    board::fold(&board::read(&goal_dir(root).join("tasks.jsonl")))
}

fn spend(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(goal_dir(root).join("spend.jsonl")).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn events(root: &Path) -> Vec<board::Event> {
    board::read(&goal_dir(root).join("tasks.jsonl"))
}

#[test]
fn three_sim_drones_finish_a_board() {
    let root = setup("three", 3, "3", json!({"usd_per_iter": 0.10}));
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    for n in 1..=3 {
        let wt = work(&root).join(format!(".worktrees/hive-g-{n}"));
        assert!(wt.is_dir(), "{}", wt.display());
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), format!("hive/g/{n}"));
    }
    let b = fold(&root);
    assert!(b.finished(), "{out}");
    // Never two holders at once: each task's tenures follow one another.
    for t in b.tasks.keys() {
        let mut spans: Vec<&board::Span> = b.spans.iter().filter(|s| &s.task == t).collect();
        spans.sort_by_key(|s| s.from_ms);
        for w in spans.windows(2) {
            assert!(w[0].to_ms.is_some_and(|end| end <= w[1].from_ms), "{t}: overlapping claims {w:?}");
        }
    }
    // Every work task's verdict came from another drone than its author's.
    let drone = |session: &str| b.drones.get(session).cloned().unwrap_or_default();
    for i in 1..=3 {
        let author = b.tasks[&format!("t{i}")].author.clone().unwrap();
        let Status::Verified { by, .. } = &b.tasks[&format!("t{i}.v1")].status else { panic!("t{i}.v1 verified") };
        assert!(!drone(&author).is_empty() && drone(&author) != drone(by), "t{i}: {} verified its own work", drone(by));
    }
    let lines = spend(&root);
    let total: f64 = lines.iter().map(|l| l["usd"].as_f64().unwrap()).sum();
    assert!(total <= 3.0 + 1e-9, "{total}");
    for d in ["d1", "d2", "d3"] {
        let mine: f64 = lines.iter().filter(|l| l["drone"] == d).map(|l| l["usd"].as_f64().unwrap()).sum();
        assert!(mine <= 1.0 + 0.10 + 1e-9, "{d} spent {mine}");
    }
    let sim_sessions = std::fs::read_dir(store(&root).join("presence"))
        .unwrap()
        .flatten()
        .filter(|e| std::fs::read_to_string(e.path()).unwrap_or_default().contains("\"harness\":\"sim\""))
        .count();
    assert!(sim_sessions >= 3, "every iteration ran the real hooks: {sim_sessions}");
    let exclude = std::fs::read_to_string(work(&root).join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.lines().filter(|l| *l == "/.worktrees/").count(), 1);
    assert!(git(&work(&root), &["status", "--porcelain"]).is_empty(), "the engineer's tree stays clean");
    assert!(out.contains("end: goal finished") && out.contains("the goal is finished"), "{out}");
    // borg top shows the hive below the collective's drones, and in its JSON.
    let w = work(&root).display().to_string();
    let top = command(&root, &["top", "--dir", &w], &[]).output().unwrap();
    let top = String::from_utf8_lossy(&top.stdout);
    assert!(top.contains("hive g: $0.") && top.contains("· finished") && top.contains("DRONE      SPENT/SHARE"), "{top}");
    let top = command(&root, &["top", "--dir", &w, "--json"], &[]).output().unwrap();
    let v: Value = serde_json::from_slice(&top.stdout).unwrap();
    assert_eq!((v[0]["hive"][0]["goal"].as_str(), v[0]["hive"][0]["finished"].as_bool()), (Some("g"), Some(true)));
    // A second run resumes: the board is finished, nothing more is spent.
    let (_, _, again) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!((again, spend(&root).len()), (0, lines.len()));
    // The run left its worktrees: a session started there later (an engineer's review) joins nothing.
    let wt = work(&root).join(".worktrees/hive-g-1");
    assert!(!prompt_hook(&root, "reviewer", &wt).contains("[borg hive g]"));
    assert!(!store(&root).join("state/reviewer.hive").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_drone_out_of_budget_releases_its_claim() {
    // Every drone's first work fails at its oracle, so each ends its iteration holding its claim; one
    // iteration ($0.10) spends its whole share ($0.05).
    let first = json!({"fail_first": true});
    let root = setup("budget", 3, "0.15", json!({"usd_per_iter": 0.10, "drones": {"d1": first, "d2": first, "d3": first}}));
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 1, "the board is not finished: {out}{err}");
    let ev = events(&root);
    let b = fold(&root);
    let claimed: Vec<&str> =
        ev.iter().filter(|e| e.ev == board::Ev::Claim && !b.lost.iter().any(|l| l.0 == e.by && l.1 == e.task && l.2 == e.ms)).map(|e| e.by.as_str()).collect();
    assert!(!claimed.is_empty(), "{ev:?}");
    for by in claimed {
        assert!(ev.iter().any(|e| e.ev == board::Ev::Released && e.note == "budget" && e.by == by), "{by} released with the reason: {ev:?}");
    }
    assert_eq!(out.lines().filter(|l| l.contains("end: budget")).count(), 3, "{out}");
    assert!(b.tasks.values().all(|t| !matches!(t.status, Status::Claimed { .. })), "no claim is left held");
    assert_eq!(spend(&root).len(), 3, "one iteration each");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn fail_first_is_blocked_at_the_stop_until_the_oracle_passes() {
    // Both drones fail their first work, so whichever claims first is blocked once.
    let first = json!({"fail_first": true});
    let root = setup("fail-first", 2, "2", json!({"usd_per_iter": 0.10, "drones": {"d1": first, "d2": first}}));
    let (out, err, code) = run(&root, &["--drones", "2", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    let logs = std::fs::read_dir(goal_dir(&root).join("oracle")).map(|d| d.count()).unwrap_or(0);
    assert!(logs >= 1, "the failing oracle blocked the stop once");
    let b = fold(&root);
    assert!(b.finished());
    assert!(events(&root).iter().any(|e| e.ev == board::Ev::Released && e.note == "iteration ended"), "the ended session's claim was released");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_harness_without_headless_is_refused() {
    let root = setup("refused", 1, "1", json!({}));
    let (out, err, code) = hive(&root, &["run", "g", "--drones", "2", "--harness", "sim"], &[("BORG_SIM_CAPS", "prompt,tool")]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("sim cannot run headless drones (Harness::headless); use one of: claude"), "{err}");
    assert!(!work(&root).join(".worktrees").exists(), "nothing made");
    let (_, err, code) = run(&root, &["--drones", "21", "--harness", "sim"]);
    assert!(code == 2 && err.contains("--drones is 1 to 20"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_hanging_drone_is_cut_off() {
    let root = setup("hang", 1, "1", json!({"drones": {"d1": {"hang": true}}}));
    let t0 = Instant::now();
    let (out, err, code) = run(&root, &["--drones", "1", "--harness", "sim", "--iter-timeout", "2"]);
    assert!(t0.elapsed() < Duration::from_secs(15), "{:?}", t0.elapsed());
    assert_eq!(code, 1, "{out}{err}");
    let lines = spend(&root);
    // Cut off without naming its cost, it is charged the rest of the share (no dearer iteration yet).
    assert_eq!(lines.iter().map(|l| (l["exit"].as_str().unwrap(), l["est"].as_f64())).collect::<Vec<_>>(), [("timeout", Some(1.0))]);
    assert!(out.contains("end: budget"), "{out}");
    let notes: Vec<String> = events(&root).iter().filter(|e| e.ev == board::Ev::Released).map(|e| e.note.clone()).collect();
    assert_eq!(notes, ["budget"], "the cut-off session's claim is released");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_harness_that_reports_no_cost_gets_one_iteration() {
    // $0 per iteration: a subscription the budget cannot bound.
    let root = setup("zero", 2, "1", json!({"usd_per_iter": 0.0}));
    let (out, err, code) = run(&root, &["--drones", "1", "--harness", "sim"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(out.contains("end: cost not reported"), "{out}");
    assert_eq!(spend(&root).len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_rerun_shares_what_is_left_of_the_goals_budget_and_keeps_the_drones_branches() {
    let first = json!({"fail_first": true});
    let root = setup("rerun", 3, "0.10", json!({"usd_per_iter": 0.10, "drones": {"d1": first, "d2": first, "d3": first}}));
    let (out, err, _) = run(&root, &["--drones", "1", "--harness", "sim"]);
    assert!(out.contains("end: budget"), "{out}{err}");
    assert_eq!(spend(&root).len(), 1);
    // The engineer removes d1's worktree but keeps its branch, which holds a commit of the drone's.
    let wt = work(&root).join(".worktrees/hive-g-1");
    std::fs::write(wt.join("kept.txt"), "x\n").unwrap();
    git(&wt, &["add", "kept.txt"]);
    git(&wt, &["commit", "-q", "-m", "drone work"]);
    let tip = git(&wt, &["rev-parse", "HEAD"]);
    git(&work(&root), &["worktree", "remove", "--force", wt.to_str().unwrap()]);
    // More drones spend nothing more: the goal's budget is spent, whatever the drone count.
    let (out, err, _) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(out.lines().filter(|l| l.contains("end: budget")).count(), 3, "{out}{err}");
    assert_eq!(spend(&root).len(), 1, "{out}");
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), tip, "the branch was checked out, not reset");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dry_run_writes_nothing() {
    let root = setup("dry", 1, "1", json!({}));
    let before: Vec<_> = std::fs::read_dir(goal_dir(&root)).unwrap().flatten().map(|e| e.file_name()).collect();
    let (out, err, code) = run(&root, &["--drones", "2", "--harness", "sim", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(".worktrees/hive-g-2") && out.contains("$0.50 of $1.00") && out.contains("drone d1 of hive goal g"), "{out}");
    assert!(out.contains("\"hive\" \"sim-drone\""), "the command: {out}");
    assert!(!work(&root).join(".worktrees").exists());
    assert!(!std::fs::read_to_string(work(&root).join(".git/info/exclude")).unwrap_or_default().contains(".worktrees"));
    let after: Vec<_> = std::fs::read_dir(goal_dir(&root)).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(before, after);
    let _ = std::fs::remove_dir_all(root);
}

/// A prompt hook of the sim harness from `cwd`, without any hive env.
fn prompt_hook(root: &Path, session: &str, cwd: &Path) -> String {
    let input = json!({"session_id": session, "cwd": cwd, "prompt": "carry on"});
    common::borg_as(root, "sim", "prompt", &input, &[("BORG_TEST_HARNESS", "1"), ("BORG_MATCH", "bm25"), ("BORG_PREDICT_MODEL", "off")])
}

#[test]
fn join_routes_a_session_without_env() {
    let root = setup("join", 1, "1", json!({}));
    let sub = work(&root).join("src");
    std::fs::create_dir_all(&sub).unwrap();
    assert!(!prompt_hook(&root, "before-join", &sub).contains("[borg hive g]"));
    let out = ok(&root, &["join", "g", "--here"]);
    assert!(out.contains("joined g as j-"), "{out}");
    let ctx = prompt_hook(&root, "after-join", &sub);
    assert!(ctx.contains("[borg hive g]") && ctx.contains("Next open: t1"), "{ctx}");
    ok(&root, &["leave", "--here"]);
    assert!(!prompt_hook(&root, "after-leave", &sub).contains("[borg hive g]"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn non_hive_hook_output_is_unchanged() {
    let plain = common::setup("hive-run-plain");
    let a = prompt_hook(&plain, "s1", &work(&plain));
    assert!(!store(&plain).join("hive").exists(), "a prompt makes no hive");
    // A store with a goal no one joined: the same answer.
    let with_goal = setup("untouched", 1, "1", json!({}));
    let b = prompt_hook(&with_goal, "s1", &work(&with_goal));
    assert_eq!(a, b);
    let _ = std::fs::remove_dir_all(plain);
    let _ = std::fs::remove_dir_all(with_goal);
}
