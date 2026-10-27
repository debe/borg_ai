//! Interactive drones through the binary (phase g, hive/interactive.rs, hive/wait.rs), driven by the
//! Claude Code hook JSON a session sends: a session joins from inside itself (`CLAUDE_CODE_SESSION_ID`
//! in the command's environment), its stop asks it to start `borg hive wait` in the background when
//! nothing is open, the wait claims a task once one is opened and exits with it, and the next turn's
//! hooks take it up and finish it in the drone's own worktree. A `/clear` rebinds the drone; the
//! engineer's turn is never handed work; a claim no hook takes up is released; a drone's model never runs
//! the engineer's commands; a runner and interactive drones never share a goal on one node; and a
//! session that never joined answers byte for byte as before phase g. No model call: the stop runs with
//! word matching only and no prediction model.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use borg::hive::{board, interactive};
use serde_json::{Value, json};

/// Every hook's stop runs without a model.
const NO_MODEL: [(&str, &str); 2] = [("BORG_MATCH", "bm25"), ("BORG_PREDICT_MODEL", "off")];

fn work(root: &Path) -> PathBuf {
    root.join("harbor_orion")
}

fn store(root: &Path) -> PathBuf {
    root.join("store/collectives/corp")
}

fn command_with(bin: &str, root: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut c = Command::new(bin);
    c.args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .env("BORG_LEARN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_STOP_ATTENTION", "off")
        .env("BORG_MODE", "hook")
        .envs(NO_MODEL)
        .envs(env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn command(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    command_with(env!("CARGO_BIN_EXE_borg"), root, args, env)
}

/// `borg hive <args> --dir <work>` with `env`: (stdout, stderr, exit code).
fn hive(root: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let dir = work(root).display().to_string();
    let all: Vec<&str> = ["hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
    let out = command(root, &all, env).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

fn ok(root: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let (out, err, code) = hive(root, args, env);
    assert_eq!(code, 0, "borg hive {args:?}: {out}{err}");
    out
}

/// The environment of a command the model of Claude Code session `s` runs.
fn inside(s: &str) -> [(&str, &str); 1] {
    [("CLAUDE_CODE_SESSION_ID", s)]
}

/// `bin hook claude <event>` with Claude Code's input for session `s`, `extra` merged in: its output.
fn hook_with(bin: &str, root: &Path, event: &str, s: &str, extra: Value) -> String {
    let mut input = json!({"session_id": s, "cwd": work(root), "transcript_path": root.join("none.jsonl")});
    input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let mut c = command_with(bin, root, &["hook", "claude", event], &inside(s));
    c.stdin(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.to_string().as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).trim().to_string()
}

fn hook(root: &Path, event: &str, s: &str, extra: Value) -> Option<Value> {
    let out = hook_with(env!("CARGO_BIN_EXE_borg"), root, event, s, extra);
    (!out.is_empty()).then(|| serde_json::from_str(&out).unwrap())
}

fn prompt(root: &Path, s: &str) -> String {
    hook(root, "prompt", s, json!({"prompt": "carry on"})).map(|v| common::context(&v.to_string())).unwrap_or_default()
}

/// Claude Code's Stop with `background` as its in-flight background tasks.
fn stop(root: &Path, s: &str, again: bool, background: Value) -> Option<Value> {
    hook(
        root,
        "stop",
        s,
        json!({"last_assistant_message": "I made the change.", "stop_hook_active": again, "background_tasks": background, "session_crons": []}),
    )
}

fn reason(v: &Option<Value>) -> String {
    v.as_ref().and_then(|v| v["reason"].as_str()).unwrap_or_default().to_string()
}

/// The background task a model started for `borg hive wait`, as Claude Code's Stop input lists it.
fn wait_task(root: &Path, s: &str) -> Value {
    json!([{"id": "b1", "type": "shell", "status": "running", "description": "wait for hive work",
        "command": format!("borg hive wait --session {s} --dir {}", work(root).display())}])
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

/// A store, a committed git repo `harbor_orion` and goal `g1` whose oracle passes once `ok` exists in the
/// drone's tree, with no task yet.
fn setup(name: &str) -> PathBuf {
    let root = common::setup(name);
    let w = work(&root);
    git(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "orion\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-q", "-m", "start"]);
    ok(&root, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=test -f ok", "--budget", "5"], &[]);
    root
}

fn open_task(root: &Path, id: &str) {
    ok(root, &["open", "g1", "--id", id, "--title", &format!("do {id}")], &[]);
}

fn events(root: &Path) -> Vec<board::Event> {
    board::read(&store(root).join("hive/g1/tasks.jsonl"))
}

fn task_state(root: &Path, id: &str) -> (String, String) {
    let b = board::fold(&events(root));
    b.tasks.get(id).map(|s| (s.status.name().to_string(), s.live(i64::MAX / 4).map_or(String::new(), |l| l.0.to_string()))).unwrap_or_default()
}

/// Whether the board says `id` is claimed by `s` now.
fn held_by(root: &Path, id: &str, s: &str) -> bool {
    let b = board::fold(&events(root));
    b.tasks.get(id).is_some_and(|t| t.live(borg::learn::now_ms()).is_some_and(|l| l.0 == s))
}

fn hs(root: &Path, s: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(store(root).join(format!("state/{s}.hive"))).unwrap_or_else(|_| "null".into())).unwrap()
}

/// A borg command this test started in the background (a `borg hive wait`, a run that should have been
/// refused): never outlives its test. Dropped while it runs it is stopped as an engineer stops it,
/// Ctrl-C, then SIGHUP, each waited for.
struct Waiter(Option<Child>);

fn spawn_wait(root: &Path, s: &str, env: &[(&str, &str)]) -> Waiter {
    let dir = work(root).display().to_string();
    let env: Vec<(&str, &str)> = inside(s).into_iter().chain(env.iter().copied()).collect();
    Waiter(Some(command(root, &["hive", "wait", "--dir", &dir], &env).spawn().unwrap()))
}

impl Waiter {
    fn running(&mut self) -> bool {
        self.0.as_mut().unwrap().try_wait().unwrap().is_none()
    }

    /// Its exit code and output (stdout, then stderr) once it ends, within `secs`.
    fn ended(mut self, secs: u64) -> (i32, String) {
        let t0 = Instant::now();
        while self.running() {
            assert!(t0.elapsed() < Duration::from_secs(secs), "the wait did not end within {secs}s");
            std::thread::sleep(Duration::from_millis(50));
        }
        let out = self.0.take().unwrap().wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else { return };
        for sig in [libc::SIGINT, libc::SIGHUP] {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            // SAFETY: kill(2) on the waiter this test spawned and has not reaped.
            unsafe { libc::kill(child.id() as libc::pid_t, sig) };
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(10) && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Waits (at most `secs`) until `done` holds.
fn until(secs: u64, what: &str, done: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !done() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "{what} within {secs}s");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The whole tracer on one node: join → prompt → a stop with nothing open asks for a background wait
/// (once; a stop that lists the wait passes) → `borg hive wait` returns when a task is opened, holding it
/// → the next turn's prompt takes it up, and its stop runs the oracle in the drone's worktree, publishes
/// the done and asks for the wait again → leave ends the membership with the node's last drone.
#[test]
fn a_joined_session_waits_at_no_cost_and_takes_the_task_that_arrives() {
    let root = setup("hive-i-tracer");
    let s = "sess-tracer-1";
    let out = ok(&root, &["join"], &inside(s));
    assert!(out.contains("joined every open hive goal of this repo (g1)") && out.contains("its own worktree"), "{out}");
    let drone = hs(&root, s)["drone"].as_str().unwrap().to_string();
    let tree = PathBuf::from(hs(&root, s)["worktree"].as_str().unwrap());
    assert!(tree.ends_with(format!(".worktrees/hive-{drone}")) && tree.join("README").exists(), "{tree:?}");
    // The node joined at once, before any claim (phase g correction 1).
    let join = events(&root).iter().position(|e| e.by == "runner" && e.note == board::JOIN).expect("the node's join");
    prompt(&root, s);
    let r = reason(&stop(&root, s, false, json!([])));
    assert!(r.contains("Nothing is open to you now") && r.contains("hive wait --session") && r.contains("run_in_background"), "{r}");
    // Asked once a turn; and a stop that lists the running wait passes without a word.
    assert_eq!(reason(&stop(&root, s, true, json!([]))), "", "asked once a turn");
    assert!(!reason(&stop(&root, s, false, wait_task(&root, s))).contains("hive wait"));
    // The engineer's turn has ended: the wait may hand work.
    let mut w = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "20000")]);
    std::thread::sleep(Duration::from_millis(800));
    assert!(w.running(), "nothing open: it waits");
    open_task(&root, "t1");
    let (code, text) = w.ended(20);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("[borg hive g1] Work arrived.") && text.contains("Your claim: t1 (work) do t1"), "{text}");
    assert!(text.contains("Work in your worktree only: cd") && text.contains(&tree.display().to_string()), "where to work: {text}");
    assert!(held_by(&root, "t1", s), "the wait holds t1 for the session");
    let claim = events(&root).iter().position(|e| e.ev == board::Ev::Claim).unwrap();
    assert!(join < claim, "the join precedes the claim");
    assert!(store(&root).join(format!("state/{s}.hive-takeup")).exists(), "handed, not taken up yet");
    // The wake turn: its first hook takes the claim up.
    let ctx = prompt(&root, s);
    assert!(ctx.contains("Your claim: t1"), "{ctx}");
    assert!(!store(&root).join(format!("state/{s}.hive-takeup")).exists(), "taken up");
    // The oracle runs in the drone's worktree, not the directory the event names.
    std::fs::write(tree.join("ok"), "").unwrap();
    let r = reason(&stop(&root, s, false, json!([])));
    assert!(r.contains("t1 done, verify task t1.v1 opened") || task_state(&root, "t1").0 == "done", "{r}");
    assert_eq!(task_state(&root, "t1").0, "done");
    assert!(r.contains("hive wait --session"), "nothing it may take (its own verify task is barred): wait again: {r}");
    // Leave: a plain session again, and with the node's last drone the node leaves the goal.
    let out = ok(&root, &["leave"], &inside(s));
    assert!(out.contains("plain session again"), "{out}");
    assert!(!store(&root).join(format!("state/{s}.hive")).exists());
    assert!(events(&root).iter().any(|e| e.by == "runner" && e.note == board::LEAVE), "the node left with its last drone");
    let _ = std::fs::remove_dir_all(root);
}

/// `/clear` gives the session a new id: its SessionStart rebinds the drone to it by the harness process
/// that ran the join (here this test, the hooks' and the join's parent), releases what the old id held,
/// and tells the drone to start its wait. A fork or a fresh session of the process is never taken over.
#[test]
fn a_cleared_session_is_the_same_drone() {
    let root = setup("hive-i-clear");
    open_task(&root, "t1");
    let (old, new) = ("sess-clear-old", "sess-clear-new");
    ok(&root, &["join", "g1"], &inside(old));
    let drone = hs(&root, old)["drone"].as_str().unwrap().to_string();
    ok(&root, &["claim"], &inside(old));
    assert!(held_by(&root, "t1", old));
    assert_eq!(hook(&root, "start", "sess-fork", json!({"source": "fork"})), None, "a fork is no drone");
    assert_eq!(hook(&root, "start", "sess-fresh", json!({"source": "startup"})), None, "nor a fresh session");
    hook(&root, "end", old, json!({"reason": "clear"}));
    let out = hook(&root, "start", new, json!({"source": "clear"})).expect("the drone is told");
    let ctx = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains(&format!("drone {drone} of hive goal g1")) && ctx.contains("hive wait --session"), "{ctx}");
    assert_eq!(hs(&root, new)["drone"], drone.as_str(), "the same drone");
    assert!(hs(&root, old).is_null(), "the old id is no drone");
    assert!(!held_by(&root, "t1", old), "the old id's claim is released");
    let live = interactive::live(&store(&root));
    assert_eq!(live.iter().map(|e| (e.drone.as_str(), e.session.as_str(), e.by.as_str())).collect::<Vec<_>>(), [(drone.as_str(), new, "rebind")]);
    // The new id claims and works as the drone.
    assert!(ok(&root, &["claim"], &inside(new)).contains("claimed t1"));
    let _ = std::fs::remove_dir_all(root);
}

/// The engineer's own turn in a drone's session is never handed work (phase g correction 8): from its
/// prompt the wait claims nothing; once the turn's stop ends it, the wait takes the task.
#[test]
fn the_engineers_turn_is_never_handed_work() {
    let root = setup("hive-i-busy");
    let s = "sess-busy-1";
    ok(&root, &["join", "g1"], &inside(s));
    let mut w = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "20000")]);
    // The engineer types: a turn of theirs, a long tool call in it.
    prompt(&root, s);
    hook(&root, "pre_tool", s, json!({"tool_name": "Bash", "tool_input": {"command": "cargo build"}}));
    open_task(&root, "t1");
    std::thread::sleep(Duration::from_millis(2000));
    assert!(w.running() && task_state(&root, "t1").0 == "open", "nothing handed during the engineer's turn");
    hook(&root, "post_tool", s, json!({"tool_name": "Bash", "tool_input": {"command": "cargo build"}, "tool_response": {"stdout": ""}}));
    // A stop that blocks (the conscience's questions) continues the engineer's turn: still nothing handed.
    if stop(&root, s, false, wait_task(&root, s)).is_some_and(|v| v["decision"] == "block") {
        std::thread::sleep(Duration::from_millis(1500));
        assert!(w.running() && task_state(&root, "t1").0 == "open", "a blocked stop goes on with the engineer's turn");
    }
    assert!(stop(&root, s, true, wait_task(&root, s)).is_none_or(|v| v["decision"] != "block"), "the turn ends");
    let (code, text) = w.ended(20);
    assert!(code == 0 && text.contains("Your claim: t1"), "{text}");
    hook(&root, "prompt", s, json!({"prompt": "go"}));
    let _ = std::fs::remove_dir_all(root);
}

/// A claim the wait handed and no hook event of the session took up (the turn never came) is released
/// after the take-up time (phase g correction 10), by the watcher the wait left behind.
#[test]
fn a_claim_no_hook_takes_up_is_released() {
    let root = setup("hive-i-takeup");
    let s = "sess-takeup-1";
    ok(&root, &["join", "g1"], &inside(s));
    open_task(&root, "t1");
    let (code, text) = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "600")]).ended(20);
    assert!(code == 0 && text.contains("Your claim: t1"), "{text}");
    assert!(held_by(&root, "t1", s));
    until(15, "the unclaimed task is released", || task_state(&root, "t1").0 == "open");
    let rel = events(&root).into_iter().find(|e| e.ev == board::Ev::Released).unwrap();
    assert!(rel.note.starts_with("not taken up"), "{}", rel.note);
    let _ = std::fs::remove_dir_all(root);
}

/// A drone's model never runs the engineer's commands (phase g correction 6): approving checks,
/// integrating, scoring are refused inside its session and inside its worktree.
#[test]
fn a_drones_model_is_refused_the_engineers_commands() {
    let root = setup("hive-i-refused");
    let s = "sess-refused-1";
    ok(&root, &["join", "g1"], &inside(s));
    for cmd in [["approve", "g1"], ["integrate", "g1"], ["score", "g1"]] {
        let (out, err, code) = hive(&root, &cmd, &inside(s));
        assert!(code == 1 && err.contains("never by a drone") || err.contains("integrate tasks are run by"), "{cmd:?}: {out}{err}");
    }
    // In its worktree without the session's variable (another shell of the drone's).
    let tree = hs(&root, s)["worktree"].as_str().unwrap().to_string();
    let out = command(&root, &["hive", "approve", "g1", "--dir", &tree], &[]).output().unwrap();
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("never by a drone"), "{}", String::from_utf8_lossy(&out.stderr));
    // The engineer's terminal (no session) approves.
    assert_eq!(hive(&root, &["approve", "g1"], &[]).2, 0);
    let _ = std::fs::remove_dir_all(root);
}

/// A node runs a runner or interactive drones for a goal, never both (phase g correction 3): a run of a
/// goal an interactive drone serves is refused, and a join while a run holds the goal's run lock.
#[test]
fn a_runner_and_interactive_drones_never_share_a_goal_on_one_node() {
    let root = setup("hive-i-mixed");
    let s = "sess-mixed-1";
    ok(&root, &["join", "g1"], &inside(s));
    let dir = work(&root).display().to_string();
    let run = Waiter(Some(command(&root, &["hive", "run", "g1", "--drones", "1", "--harness", "claude", "--yes", "--dir", &dir], &[]).spawn().unwrap()));
    let (code, out) = run.ended(30);
    assert!(code == 2 && out.contains("serves goal g1 on this node"), "refused before anything starts: {out}");
    assert!(!work(&root).join(".worktrees/hive-g1-1").exists() && !events(&root).iter().any(|e| e.ev == board::Ev::Claim), "nothing started");
    ok(&root, &["leave"], &inside(s));
    // A run holds the goal's run lock (run.rs `run_lock`): a join is refused and leaves no drone.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(store(&root).join("hive/g1/run.lock")).unwrap();
    lock.try_lock().unwrap();
    let (out, err, code) = hive(&root, &["join", "g1"], &inside("sess-mixed-2"));
    assert!(code == 1 && err.contains("a `borg hive run g1` runs on this node"), "{out}{err}");
    assert!(hs(&root, "sess-mixed-2").is_null() && interactive::live(&store(&root)).is_empty());
    drop(lock);
    let _ = std::fs::remove_dir_all(root);
}

/// The session's claim reads as the stop's capped one, held and never shown (mod.rs `hand_next` when its
/// claim is not final within `HAND_CAP`, which one node cannot make): a later task marker than its own.
fn unshow(root: &Path, s: &str) {
    let ctx = store(root).join(format!("state/{s}.ctx"));
    let mut text = std::fs::read_to_string(&ctx).unwrap_or_default();
    text.push_str("#borg-hive-task -\n");
    std::fs::write(ctx, text).unwrap();
}

/// A capped claim not shown yet is no hive work of the engineer's turn (phase g G2 (b), (e)): its prompt
/// shows it not, the stop that ends the turn runs no oracle on it, and the wait hands it only once the
/// turn is over. Fails with the stop reading it as in hand (it blocks: t1 is not done) and with the
/// wait's hand of it ungated (handed during the turn).
#[test]
fn an_unshown_claim_waits_out_the_engineers_turn() {
    let root = setup("hive-i-busy-unshown");
    let s = "sess-busy-unshown-1";
    ok(&root, &["join", "g1"], &inside(s));
    open_task(&root, "t1");
    ok(&root, &["claim"], &inside(s));
    unshow(&root, s);
    let ctx = prompt(&root, s);
    assert!(!ctx.contains("Your claim"), "not shown in the engineer's turn: {ctx}");
    hook(&root, "pre_tool", s, json!({"tool_name": "Bash", "tool_input": {"command": "cargo build"}}));
    let mut w = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "20000")]);
    std::thread::sleep(Duration::from_millis(2000));
    assert!(w.running() && !store(&root).join(format!("state/{s}.hive-takeup")).exists(), "nothing handed during the engineer's turn");
    hook(&root, "post_tool", s, json!({"tool_name": "Bash", "tool_input": {"command": "cargo build"}, "tool_response": {"stdout": ""}}));
    // The conscience's questions may block the first stop: never the claim's checks.
    let out = stop(&root, s, false, wait_task(&root, s));
    assert!(!reason(&out).contains("not done") && !reason(&out).contains("t1"), "no oracle on a claim not in hand: {out:?}");
    let out = stop(&root, s, true, wait_task(&root, s));
    assert!(out.as_ref().is_none_or(|v| v["decision"] != "block"), "the turn ends: {out:?}");
    let (code, text) = w.ended(20);
    assert!(code == 0 && text.contains("Work arrived") && text.contains("Your claim: t1"), "{text}");
    assert!(held_by(&root, "t1", s));
    prompt(&root, s);
    let _ = std::fs::remove_dir_all(root);
}

/// A claim's take-up wait counts from its hand, not its stamp (phase g correction 10): the stop's capped
/// claim handed long after it was made still waits `TAKEUP` for the wake's first hook, which keeps it.
/// Fails with the wait counted from the stamp: released at once, before any hook could come.
#[test]
fn a_claim_handed_long_after_its_stamp_gets_its_whole_takeup() {
    let root = setup("hive-i-late-hand");
    let s = "sess-late-hand-1";
    ok(&root, &["join", "g1"], &inside(s));
    open_task(&root, "t1");
    ok(&root, &["claim"], &inside(s));
    unshow(&root, s);
    std::thread::sleep(Duration::from_millis(1500));
    let (code, text) = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "1200")]).ended(20);
    assert!(code == 0 && text.contains("Your claim: t1"), "{text}");
    std::thread::sleep(Duration::from_millis(400));
    assert!(held_by(&root, "t1", s), "still waiting for its take-up");
    prompt(&root, s);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(held_by(&root, "t1", s) && !events(&root).iter().any(|e| e.ev == board::Ev::Released), "taken up: kept");
    let _ = std::fs::remove_dir_all(root);
}

/// Under `claude --continue` the command's `CLAUDE_CODE_SESSION_ID` may be the startup id, which no hook
/// of the session carries (Claude Code's env-vars docs): the join's own PostToolUse moves the drone to the
/// hook's id and says so, and from then on it is that session's drone (spec v2 3.1). Fails with no move:
/// the session stays plain, its stop asks for no wait.
#[test]
fn a_join_under_a_stale_session_id_binds_the_session_its_hooks_name() {
    let root = setup("hive-i-stale-id");
    let (stale, real) = ("sess-startup-id", "sess-continued");
    ok(&root, &["join", "g1"], &inside(stale));
    let drone = hs(&root, stale)["drone"].as_str().unwrap().to_string();
    let dir = work(&root).display().to_string();
    let out = hook(
        &root,
        "post_tool",
        real,
        json!({"tool_name": "Bash", "tool_input": {"command": format!("borg hive join g1 --dir {dir}")}, "tool_response": {"stdout": ""}}),
    );
    let ctx = out.map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(ctx.contains(&format!("This session is drone {drone}")), "{ctx}");
    assert_eq!(hs(&root, real)["drone"], drone.as_str());
    assert!(hs(&root, stale).is_null(), "the stale id is no drone");
    let live = interactive::live(&store(&root));
    assert_eq!(live.iter().map(|e| (e.session.as_str(), e.by.as_str())).collect::<Vec<_>>(), [(real, "rebind")]);
    let r = reason(&stop(&root, real, false, json!([])));
    assert!(r.contains("hive wait --session") && r.contains(real), "{r}");
    // Another session's command (no join) moves nothing.
    assert_eq!(hook(&root, "post_tool", "sess-other", json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_response": {"stdout": ""}})), None);
    assert!(hs(&root, "sess-other").is_null());
    let _ = std::fs::remove_dir_all(root);
}

/// A drone's edits stay in its own worktree (spec v2 3.1): an edit of its repo outside the tree (the
/// engineer's checkout) is denied and names the path to use; one in its tree or outside the repo runs.
#[test]
fn a_drones_edit_of_the_engineers_checkout_is_refused() {
    let root = setup("hive-i-outside");
    let s = "sess-outside-1";
    ok(&root, &["join", "g1"], &inside(s));
    let tree = PathBuf::from(hs(&root, s)["worktree"].as_str().unwrap());
    let edit = |file: PathBuf| json!({"tool_name": "Edit", "tool_input": {"file_path": file, "old_string": "orion", "new_string": "orion 2"}});
    let out = hook(&root, "pre_tool", s, edit(work(&root).join("README"))).expect("denied");
    let why = out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap_or_default();
    assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(why.contains("outside your worktree") && why.contains(&tree.join("README").display().to_string()), "{why}");
    let relative = json!({"tool_name": "Write", "cwd": work(&root), "tool_input": {"file_path": "../harbor_orion/src/x.rs", "content": "x"}});
    assert!(hook(&root, "pre_tool", s, relative).is_some(), "a relative path into the checkout too");
    assert_eq!(hook(&root, "pre_tool", s, edit(tree.join("README"))), None, "its own tree");
    assert_eq!(hook(&root, "pre_tool", s, edit(root.join("notes.md"))), None, "outside the repo");
    let _ = std::fs::remove_dir_all(root);
}

/// The events of a plain session that never joined, through `bin`'s hooks: (event, output with the
/// root as `<root>`).
fn plain_session(bin: &str, root: &Path) -> Vec<(String, String)> {
    let s = "plain-unjoined";
    let edit = json!({"tool_name": "Edit", "tool_input": {"file_path": work(root).join("src/net.rs"), "old_string": "reset", "new_string": "drain the Orion budget place"}});
    let steps = [
        ("start", json!({"source": "startup"})),
        ("prompt", json!({"prompt": "fix the Orion net budget drain and the websocket edge case"})),
        ("pre_tool", edit.clone()),
        ("post_tool", edit),
        (
            "stop",
            json!({"last_assistant_message": "Done: the budget place is drained, and the websocket handling covers the websocket edge case and its limits.", "stop_hook_active": false, "background_tasks": [], "session_crons": []}),
        ),
        ("stop", json!({"last_assistant_message": "Done.", "stop_hook_active": true, "background_tasks": [], "session_crons": []})),
        ("end", json!({"reason": "prompt_input_exit"})),
    ];
    let shown = root.display().to_string();
    steps.into_iter().map(|(e, extra)| (e.to_string(), hook_with(bin, root, e, s, extra).replace(&shown, "<root>"))).collect()
}

fn pin_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/unjoined-session.json")
}

/// Phase g correction 18: a session that never joined answers exactly as at HEAD f1b77ee, before phase
/// g, while another session of its repo is an interactive drone with a wait running. The pin was taken
/// with that commit's binary (`BORG_PIN_BIN=<it> cargo test --test hive_interactive_cli unjoined`, which
/// rewrites it, the drone left out: that binary has none).
#[test]
fn an_unjoined_session_answers_byte_for_byte_as_before_phase_g() {
    let root = setup("hive-i-unjoined");
    open_task(&root, "t1");
    if let Ok(bin) = std::env::var("BORG_PIN_BIN") {
        let outs = plain_session(&bin, &root);
        std::fs::write(pin_path(), serde_json::to_string_pretty(&outs).unwrap() + "\n").unwrap();
        let _ = std::fs::remove_dir_all(root);
        return;
    }
    ok(&root, &["join"], &inside("sess-drone-beside"));
    let _w = spawn_wait(&root, "sess-drone-beside", &[("BORG_HIVE_TAKEUP_MS", "20000")]);
    let got = plain_session(env!("CARGO_BIN_EXE_borg"), &root);
    let pinned: Vec<(String, String)> = serde_json::from_str(&std::fs::read_to_string(pin_path()).unwrap()).unwrap();
    assert!(pinned.iter().any(|(e, out)| e == "stop" && !out.is_empty()), "the pin covers a stop that answered");
    assert_eq!(got, pinned);
    let state: Vec<String> = std::fs::read_dir(store(&root).join("state"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("plain-unjoined.hive"))
        .collect();
    assert!(state.is_empty(), "no hive state for it: {state:?}");
    let _ = std::fs::remove_dir_all(root);
}
