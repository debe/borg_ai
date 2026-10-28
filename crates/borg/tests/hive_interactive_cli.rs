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

/// `borg debug hive <args> --dir <work>` with `env`: `score` lives there.
fn debug_hive(root: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let dir = work(root).display().to_string();
    let all: Vec<&str> = ["debug", "hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
    let out = command(root, &all, env).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
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

/// What a stop says to the model and goes on with: a block's reason, or a drone's Stop feedback (its
/// `additionalContext`, interactive.rs `feedback`).
fn reason(v: &Option<Value>) -> String {
    v.as_ref().and_then(|v| v["reason"].as_str().or_else(|| v["hookSpecificOutput"]["additionalContext"].as_str())).unwrap_or_default().to_string()
}

/// Whether a stop continues the turn (a block, or Stop feedback with text).
fn continues(v: &Option<Value>) -> bool {
    !reason(v).is_empty()
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

/// The engineer opens task `id` from a terminal and approves it there (`borg hive ratify`, phase g G5):
/// interactive drones take only approved tasks.
fn open_task(root: &Path, id: &str) {
    open_unapproved(root, id);
    ok(root, &["ratify", "g1"], &[]);
}

/// The engineer opens task `id` from a terminal, its plan not approved yet.
fn open_unapproved(root: &Path, id: &str) {
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
    assert!(r.starts_with("[borg hive] t1 done: its checks passed (ok); verify task t1.v1 is open for another drone."), "the result first: {r}");
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
    if continues(&stop(&root, s, false, wait_task(&root, s))) {
        std::thread::sleep(Duration::from_millis(1500));
        assert!(w.running() && task_state(&root, "t1").0 == "open", "a blocked stop goes on with the engineer's turn");
    }
    assert!(!continues(&stop(&root, s, true, wait_task(&root, s))), "the turn ends");
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

/// A drone's model never runs the engineer's commands (phase g correction 6): approving checks or the
/// plan (`ratify`, phase g G5), integrating, scoring are refused inside its session and inside its
/// worktree.
#[test]
fn a_drones_model_is_refused_the_engineers_commands() {
    let root = setup("hive-i-refused");
    let s = "sess-refused-1";
    ok(&root, &["join", "g1"], &inside(s));
    for cmd in [["approve", "g1"], ["integrate", "g1"], ["score", "g1"], ["ratify", "g1"]] {
        let (out, err, code) = if cmd[0] == "score" { debug_hive(&root, &cmd, &inside(s)) } else { hive(&root, &cmd, &inside(s)) };
        assert!(code == 1 && err.contains("never by a drone") || err.contains("integrate tasks are run by"), "{cmd:?}: {out}{err}");
    }
    // In its worktree without the session's variable (another shell of the drone's).
    let tree = hs(&root, s)["worktree"].as_str().unwrap().to_string();
    for cmd in ["approve", "ratify"] {
        let out = command(&root, &["hive", cmd, "g1", "--dir", &tree], &[]).output().unwrap();
        assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("never by a drone"), "{cmd}: {}", String::from_utf8_lossy(&out.stderr));
    }
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
    let run =
        Waiter(Some(command(&root, &["debug", "hive", "run", "g1", "--drones", "1", "--harness", "claude", "--yes", "--dir", &dir], &[]).spawn().unwrap()));
    let (code, out) = run.ended(30);
    assert!(code == 2 && out.contains("serves goal g1 on this node"), "refused before anything starts: {out}");
    assert!(!work(&root).join(".worktrees/hive-g1-1").exists() && !events(&root).iter().any(|e| e.ev == board::Ev::Claim), "nothing started");
    ok(&root, &["leave"], &inside(s));
    // A run holds the goal's run lock (run.rs `run_lock`): a join is refused and leaves no drone.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(store(&root).join("hive/g1/run.lock")).unwrap();
    lock.try_lock().unwrap();
    let (out, err, code) = hive(&root, &["join", "g1"], &inside("sess-mixed-2"));
    assert!(code == 1 && err.contains("a `borg debug hive run g1` runs on this node"), "{out}{err}");
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
    assert!(!continues(&out), "the turn ends: {out:?}");
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

/// A drone's stop with its task done says the result before anything else, as Stop feedback, not an
/// error (live probe gaps 1 and 3): the model wrote its last message before the checks ran ("I didn't
/// tell borg the task is done"), so the done's note on the board is the stop's own summary, the checks,
/// the files and the commits, and never that message. Its idle line says the work comes by itself.
/// Fails with the stop's done unsaid (only the wait asked for), with the model's message as the note,
/// and with a `decision: block` (shown as "Stop hook error").
#[test]
fn a_done_is_said_first_and_its_note_is_the_stops_own() {
    let root = setup("hive-i-done-said");
    let s = "sess-done-said-1";
    ok(&root, &["join", "g1"], &inside(s));
    open_task(&root, "t1");
    let (code, text) = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "20000")]).ended(20);
    assert!(code == 0 && text.contains("Your claim: t1"), "{text}");
    prompt(&root, s);
    let tree = PathBuf::from(hs(&root, s)["worktree"].as_str().unwrap());
    std::fs::write(tree.join("ok"), "hello hive\n").unwrap();
    git(&tree, &["add", "ok"]);
    git(&tree, &["commit", "-q", "-m", "Add the ok marker"]);
    let wrong = "I finished task t1. I didn't tell borg the task is done, because the instructions didn't say how.";
    let out = hook(&root, "stop", s, json!({"last_assistant_message": wrong, "stop_hook_active": false, "background_tasks": [], "session_crons": []}));
    let v = out.clone().expect("the stop goes on");
    assert!(v.get("decision").is_none() && v["hookSpecificOutput"]["hookEventName"] == "Stop", "Stop feedback, not a block: {v}");
    let r = reason(&out);
    assert!(r.starts_with("[borg hive] t1 done: its checks passed (ok); verify task t1.v1 is open for another drone."), "{r}");
    let (done_at, wait_at) = (r.find("t1 done").unwrap(), r.find("hive wait --session").expect("then the wait"));
    assert!(done_at < wait_at, "{r}");
    assert!(r.contains(interactive::IDLE_LINE) && r.contains("only on the engineer's request"), "{r}");
    let done = events(&root).into_iter().find(|e| e.ev == board::Ev::Done).unwrap();
    assert!(!done.note.contains("didn't tell borg"), "not the model's message: {}", done.note);
    assert!(done.note.starts_with("its checks passed (ok)") && done.note.contains("it changed ok") && done.note.contains("Add the ok marker"), "{}", done.note);
    assert!(!done.note.contains("borg hive snapshot"), "{}", done.note);
    let _ = std::fs::remove_dir_all(root);
}

/// A drone whose session ends with no SessionEnd (a closed terminal, Claude Code's "Move to background
/// and exit", a crash) leaves no orphan waiter (live probe gap 2): the waiter sees its harness process
/// gone, leaves (the registry, the node's leave with its last drone) and exits. The harness here is a
/// perl process the join ran under, stopped with SIGTERM. Fails with the waiter blind to its harness:
/// it waits on.
#[test]
fn a_waiter_whose_session_is_gone_leaves_and_ends() {
    let root = setup("hive-i-gone");
    let s = "sess-gone-1";
    let dir = work(&root).display().to_string();
    let args = ["-e", "system(@ARGV); sleep 600", env!("CARGO_BIN_EXE_borg"), "hive", "join", "g1", "--dir", &dir];
    let mut harness = Waiter(Some(command_with("perl", &root, &args, &inside(s)).spawn().unwrap()));
    let pid = harness.0.as_ref().unwrap().id();
    until(20, "the join", || interactive::live(&store(&root)).iter().any(|e| e.session == s));
    assert_eq!(interactive::live(&store(&root))[0].pid, pid, "the join's harness process");
    let mut w = spawn_wait(&root, s, &[]);
    std::thread::sleep(Duration::from_millis(800));
    assert!(w.running(), "its session lives: it waits");
    // SAFETY: kill(2) on the perl process this test spawned and has not reaped.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    let _ = harness.0.take().unwrap().wait();
    let (code, out) = w.ended(15);
    assert!(code == 0 && out.contains("session is gone"), "{out}");
    assert!(hs(&root, s).is_null() && interactive::live(&store(&root)).is_empty(), "a plain session no more bound");
    assert!(events(&root).iter().any(|e| e.by == "runner" && e.note == board::LEAVE), "the node left with its last drone");
    let _ = std::fs::remove_dir_all(root);
}

/// A joined session's end (SessionEnd: `/exit` with "Exit and stop tasks", Ctrl-C or Ctrl-D, logout)
/// leaves cleanly (live probe gap 2): its waiter stops, a claim it held is released and its node leaves
/// with its last drone. Fails with the end leaving the drone bound (its waiter waits on).
#[test]
fn a_session_end_stops_the_waiter_releases_the_claim_and_leaves() {
    let root = setup("hive-i-end");
    let s = "sess-end-1";
    ok(&root, &["join", "g1"], &inside(s));
    let w = spawn_wait(&root, s, &[]);
    std::thread::sleep(Duration::from_millis(800));
    hook(&root, "end", s, json!({"reason": "prompt_input_exit"}));
    let (code, _) = w.ended(15);
    assert_eq!(code, 128 + libc::SIGTERM, "stopped by the leave");
    assert!(hs(&root, s).is_null() && interactive::live(&store(&root)).is_empty());
    assert!(events(&root).iter().any(|e| e.by == "runner" && e.note == board::LEAVE));
    // Holding a task: the end releases it.
    let s2 = "sess-end-2";
    ok(&root, &["join", "g1"], &inside(s2));
    open_task(&root, "t1");
    ok(&root, &["claim"], &inside(s2));
    assert!(held_by(&root, "t1", s2));
    hook(&root, "end", s2, json!({"reason": "other"}));
    assert_eq!(task_state(&root, "t1").0, "open", "released");
    assert!(hs(&root, s2).is_null());
    let _ = std::fs::remove_dir_all(root);
}

/// A repo in no collective: `borg hive join` and `init` say so on stderr with the one step that fixes it
/// (live probe gap 6: a fresh `borg setup` that took the defaults left the repo off, and the hive refused
/// with a line naming no fix); an `off` path says the hive does not run there. Nothing is changed for
/// the engineer.
#[test]
fn a_repo_in_no_collective_is_told_how_to_join_one() {
    let root = setup("hive-i-off");
    let config = std::fs::read_to_string(root.join("collectives.json")).unwrap();
    std::fs::write(root.join("collectives.json"), json!({"default": "off", "collectives": []}).to_string()).unwrap();
    for args in [&["join"][..], &["init", "g2", "--spec", "x", "--oracle", "ok=true"][..]] {
        let (out, err, code) = hive(&root, args, &inside("sess-off-1"));
        assert_eq!(code, 1, "{out}{err}");
        assert!(err.contains("belongs to no collective") && err.contains("borg setup --private") && err.contains("borg setup --team"), "{err}");
    }
    let w = work(&root).display().to_string();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "off": [w], "collectives": [{"name": "corp", "paths": [root.join("harbor_orion")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    let (_, err, code) = hive(&root, &["join"], &inside("sess-off-1"));
    assert!(code == 1 && err.contains("is an `off` path") && !err.contains("borg setup"), "{err}");
    // An `off` path no collective path or default covers is still said to be one: a setup would not lift it.
    std::fs::write(root.join("collectives.json"), json!({"default": "off", "off": [w], "collectives": []}).to_string()).unwrap();
    let (_, err, code) = hive(&root, &["join"], &inside("sess-off-1"));
    assert!(code == 1 && err.contains("is an `off` path") && !err.contains("borg setup"), "{err}");
    std::fs::write(root.join("collectives.json"), config).unwrap();
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

/// `borg hive <args> --dir <dir>` with `env`: a drone's command in its own tree.
fn ok_in(root: &Path, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let d = dir.display().to_string();
    let all: Vec<&str> = ["hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", d.as_str()]).collect();
    let out = command(root, &all, env).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "borg hive {args:?} in {d}: {text}");
    text
}

fn tree_of(root: &Path, s: &str) -> PathBuf {
    PathBuf::from(hs(root, s)["worktree"].as_str().unwrap())
}

/// Phase g G4, the waiter's fallback: with no mesh daemon on the node, the waiter of one of its drones
/// does the node's goal work while it holds the node lock (hive/node.rs): t1 done by one drone and
/// verified by the other, every task verified and no drone working, the goal is integrated by the
/// node's `borg hive integrate` child and said finished, and the single-goal drone ends on it. While
/// another process holds the node lock (a daemon) the waiter does none of it: no beat, no integration.
/// Fails with the waiter's node work removed: nobody integrates and the wait waits on.
#[test]
fn with_no_daemon_a_drones_wait_does_the_nodes_work_and_integrates() {
    let root = setup("hive-i-node-wait");
    open_task(&root, "t1");
    let (sa, sb) = ("sess-node-wait-a", "sess-node-wait-b");
    ok(&root, &["join", "g1"], &inside(sa));
    let ta = tree_of(&root, sa);
    ok_in(&root, &ta, &["claim"], &inside(sa));
    std::fs::write(ta.join("ok"), "").unwrap();
    ok_in(&root, &ta, &["done"], &inside(sa));
    ok(&root, &["join", "g1"], &inside(sb));
    let tb = tree_of(&root, sb);
    assert!(ok_in(&root, &tb, &["claim"], &inside(sb)).contains("t1.v1"));
    ok_in(&root, &tb, &["verify", "--pass"], &inside(sb));
    let b = board::fold(&events(&root));
    assert!(board::integration_due(&b), "every task verified, nothing integrated yet");
    // A daemon holds the node lock: the wait waits and does none of the node's work.
    let lock = borg::hive::node::take(&store(&root)).expect("the node lock");
    let mut w = spawn_wait(&root, sa, &[]);
    std::thread::sleep(Duration::from_millis(2500));
    assert!(w.running(), "it waits");
    assert!(!events(&root).iter().any(board::is_beat), "no beat of the waiter's while a daemon does the node's work");
    assert!(!board::fold(&events(&root)).tasks.contains_key("integrate.1"), "nor an integration");
    // No daemon: the waiter takes the lock, beats, integrates (its child) and the goal ends.
    drop(lock);
    let (code, out) = w.ended(60);
    assert!(code == 0 && out.contains("the goal is finished"), "{out}");
    let evs = events(&root);
    let b = board::fold(&evs);
    assert!(b.finished() && matches!(b.tasks["integrate.1"].status, borg::hive::Status::Verified { .. }), "integrated");
    assert!(evs.iter().any(board::is_beat), "the waiter beat for its node");
    assert!(evs.iter().any(|e| e.by == "runner" && e.note == board::finished_note("integrate.1")), "the node said it finished");
    assert!(evs.iter().any(|e| e.by == "runner" && e.note.starts_with("drones ")), "the roster of its takers");
    ok(&root, &["leave"], &inside(sb));
    let _ = std::fs::remove_dir_all(root);
}

/// Phase g G4, the reaper: a drone whose session dies mid-task with no wait of its own running (its
/// harness process gone, no SessionEnd) is reaped by its node's worker, here the other drone's waiter:
/// its claim released `its session is gone`, it is no drone any more, and the node stays in the goal for
/// the drone that lives on, whose wait then takes the task. The harness is a perl process the join and
/// the claim ran under, stopped with SIGTERM. Fails with no reaper: the claim stays held for its lease.
#[test]
fn a_drone_whose_session_died_mid_task_is_reaped_by_its_nodes_worker() {
    let root = setup("hive-i-node-reap");
    open_task(&root, "t1");
    let (sa, sb) = ("sess-node-reap-a", "sess-node-reap-b");
    let dir = work(&root).display().to_string();
    let bin = env!("CARGO_BIN_EXE_borg");
    let script = format!("{bin} hive join g1 --dir {dir} && {bin} hive claim --dir {dir}");
    let args = ["-e", "system('sh', '-c', $ARGV[0]); sleep 600", script.as_str()];
    let mut harness = Waiter(Some(command_with("perl", &root, &args, &inside(sa)).spawn().unwrap()));
    let pid = harness.0.as_ref().unwrap().id();
    until(20, "the claim", || held_by(&root, "t1", sa));
    ok(&root, &["join", "g1"], &inside(sb));
    let w = spawn_wait(&root, sb, &[("BORG_HIVE_TAKEUP_MS", "20000")]);
    std::thread::sleep(Duration::from_millis(800));
    assert!(held_by(&root, "t1", sa), "its session lives: it keeps its claim");
    // SAFETY: kill(2) on the perl process this test spawned and has not reaped.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    let _ = harness.0.take().unwrap().wait();
    until(15, "the reap", || events(&root).iter().any(|e| e.ev == board::Ev::Released && e.by == sa && e.note == "its session is gone"));
    assert!(hs(&root, sa).is_null() && interactive::live(&store(&root)).iter().all(|e| e.session == sb), "no drone any more");
    assert!(!events(&root).iter().any(|e| e.by == "runner" && e.note == board::LEAVE), "the node stays for the drone that lives on");
    let (code, out) = w.ended(20);
    assert!(code == 0 && out.contains("Your claim: t1"), "the living drone takes it: {out}");
    ok(&root, &["leave"], &inside(sb));
    let _ = std::fs::remove_dir_all(root);
}

// ---------------------------------------------------------------- omp and opencode (phase g G3)

/// The hook JSON the omp or opencode adapter sends for a drone's turn, recorded from the adapter itself
/// with a recording borg (fixtures/<harness>/hive-drone.json: prompt, the join's pre_tool and post_tool,
/// a read's pre_tool, stop, end), with session `s`, the repo and the harness pid `pid` put in.
fn adapter_events(harness: &str, root: &Path, s: &str, pid: u32) -> Vec<(String, Value)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{harness}/hive-drone.json"));
    let text =
        std::fs::read_to_string(path).unwrap().replace("SESSION", s).replace("WORK", &work(root).display().to_string()).replace("\"PID\"", &pid.to_string());
    serde_json::from_str(&text).unwrap()
}

/// The recorded `event` input of `harness` (the `nth` of that event), `extra` merged in.
fn adapter_event(harness: &str, root: &Path, s: &str, pid: u32, event: &str, nth: usize, extra: Value) -> Value {
    let mut v = adapter_events(harness, root, s, pid).into_iter().filter(|(e, _)| e == event).nth(nth).unwrap().1;
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    v
}

/// `borg hook <harness> <event>` with `input`: its output.
fn hook_as(root: &Path, harness: &str, event: &str, input: &Value) -> Option<Value> {
    let mut c = command(root, &["hook", harness, event], &[]);
    c.stdin(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.to_string().as_bytes()).unwrap();
    let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).trim().to_string();
    (!out.is_empty()).then(|| serde_json::from_str(&out).unwrap())
}

/// A `borg hive wait --stream` as a plugin keeps it: its stdout's lines as they come, and the process,
/// stopped as an engineer stops it when dropped (`Waiter`).
struct Stream {
    waiter: Waiter,
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

fn spawn_stream(root: &Path, s: &str) -> Stream {
    let dir = work(root).display().to_string();
    stream_of(command(root, &["hive", "wait", "--stream", "--session", s, "--dir", &dir], &[("BORG_HIVE_TAKEUP_MS", "20000")]).spawn().unwrap())
}

fn stream_of(mut child: Child) -> Stream {
    let out = child.stdout.take().unwrap();
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = lines.clone();
    std::thread::spawn(move || {
        for l in std::io::BufRead::lines(std::io::BufReader::new(out)).map_while(Result::ok) {
            sink.lock().unwrap().push(l);
        }
    });
    Stream { waiter: Waiter(Some(child)), lines }
}

impl Stream {
    fn wakes(&self) -> Vec<Value> {
        self.lines.lock().unwrap().iter().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v.get("wake").is_some()).collect()
    }
}

/// A copy of perl at `to`, a process named by its file: a harness process to join under. A copy, not a
/// link: macOS names a process by the file a link resolves to (`perl`), Linux by the link.
fn perl_as(to: &Path) {
    if !to.exists() {
        let perl = String::from_utf8(Command::new("sh").args(["-c", "command -v perl"]).output().unwrap().stdout).unwrap();
        let part = to.with_extension("part");
        std::fs::copy(perl.trim(), &part).unwrap();
        std::fs::rename(&part, to).unwrap();
    }
}

/// `c` started, again while the copy it runs is busy (ETXTBSY): another test thread's fork can hold
/// the copy's write handle open for a moment, until that child's exec closes it.
fn spawn_copy(c: &mut Command) -> std::process::Child {
    for _ in 0..50 {
        match c.spawn() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => std::thread::sleep(Duration::from_millis(20)),
            r => return r.unwrap(),
        }
    }
    c.spawn().unwrap()
}

/// A process named `name` (a copy of perl) that runs `args` and sleeps: a harness process to join
/// under, stopped as an engineer stops it when dropped.
fn harness_named(root: &Path, name: &str, args: &[&str], env: &[(&str, &str)]) -> (Waiter, u32) {
    let link = root.join("bin").join(name);
    perl_as(&link);
    let all: Vec<&str> = ["-e", "system(@ARGV); sleep 600"].into_iter().chain(args.iter().copied()).collect();
    let w = Waiter(Some(spawn_copy(&mut command_with(link.to_str().unwrap(), root, &all, env))));
    let pid = w.0.as_ref().unwrap().id();
    (w, pid)
}

/// omp (phase g G3): a session joins by the `PI_SESSION_ID` omp gives its shell; the waiter its
/// extension keeps from the join (`--stream`) hands the task that opens only once the engineer's turn
/// has stopped, as one JSON line, and waits on while the drone works it; the stop passes with no ask to
/// start a wait, and says the done first as a block (the `decision`/`reason` the adapter reads, never
/// Claude's Stop feedback); the pid the extension sends is the drone's harness process from its first
/// event on; the session's end leaves and stops the waiter. All with the hook JSON the adapter sends.
/// Fails with the stop asking for a background wait (Claude's arm), with the done turned into Stop
/// feedback (the adapter would drop it), with a waiter that exits at its first hand, and with the
/// registry's pid left at the join's ancestor.
#[test]
fn an_omp_drone_is_woken_by_its_extensions_waiter_and_its_stop_speaks_omps_block() {
    let root = setup("hive-i-omp");
    let s = "omp-sess-1";
    let out = ok(&root, &["join"], &[("PI_SESSION_ID", s)]);
    assert!(out.contains("joined every open hive goal of this repo (g1)") && out.contains("plugin waits for work"), "{out}");
    let entry = interactive::live(&store(&root)).into_iter().next().unwrap();
    assert_eq!((entry.session.as_str(), entry.harness.as_str()), (s, "omp"));
    // omp's own process, as its extension names it (`process.pid`), not the join's ancestor.
    let (_omp, omp_pid) = harness_named(&root, "omp", &[], &[]);
    let ev = |event: &str, nth: usize, extra: Value| adapter_event("omp", &root, s, omp_pid, event, nth, extra);
    let w = spawn_stream(&root, s);
    // The join's turn is the engineer's: its join, then a task opens.
    assert_eq!(hook_as(&root, "omp", "pre_tool", &ev("pre_tool", 0, json!({}))), None);
    hook_as(&root, "omp", "post_tool", &ev("post_tool", 0, json!({})));
    let e = interactive::live(&store(&root)).into_iter().next().unwrap();
    assert_eq!((e.pid, e.by.as_str()), (omp_pid, "rebind"), "the extension's pid is the drone's harness");
    open_unapproved(&root, "t1");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(w.wakes().is_empty() && task_state(&root, "t1").0 == "open", "nothing handed in the engineer's turn");
    // The conscience may ask its questions at the turn's first stop; never for a wait to start.
    let out = hook_as(&root, "omp", "stop", &ev("stop", 0, json!({})));
    assert!(!reason(&out).contains("hive wait") && out.as_ref().is_none_or(|o| o.get("hookSpecificOutput").is_none()), "the extension's waiter runs: {out:?}");
    // Questions alone: no hive mark and no idle line, so the adapter's rules for questions hold.
    let out = out.expect("the conscience asks");
    assert!(out.get("hive").is_none() && !out["reason"].as_str().unwrap().contains(interactive::IDLE_LINE), "{out}");
    std::thread::sleep(Duration::from_millis(800));
    assert!(w.wakes().is_empty(), "a blocked stop goes on with the engineer's turn");
    assert_eq!(hook_as(&root, "omp", "stop", &ev("stop", 0, json!({"stop_hook_active": true}))), None, "the turn ends");
    // Its plan is not approved (phase g G5): the idle drone's waiter hands nothing; the engineer's
    // approval wakes it.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(w.wakes().is_empty() && task_state(&root, "t1").0 == "open", "an unapproved task is never handed");
    ok(&root, &["ratify", "g1"], &[]);
    until(15, "the wake", || !w.wakes().is_empty());
    let wake = w.wakes()[0].clone();
    assert_eq!(wake["left"], false);
    assert!(wake["wake"].as_str().unwrap().starts_with("[borg hive g1] Work arrived.") && wake["wake"].as_str().unwrap().contains("Your claim: t1"), "{wake}");
    assert!(held_by(&root, "t1", s));
    // The woken turn's first tool call takes it up; the waiter waits on while the drone works.
    hook_as(&root, "omp", "pre_tool", &ev("pre_tool", 1, json!({})));
    assert!(!store(&root).join(format!("state/{s}.hive-takeup")).exists(), "taken up");
    let mut w = w;
    std::thread::sleep(Duration::from_millis(800));
    assert!(w.waiter.running() && w.wakes().len() == 1, "one waiter for the session's life");
    std::fs::write(tree_of(&root, s).join("ok"), "").unwrap();
    let done = hook_as(&root, "omp", "stop", &ev("stop", 0, json!({}))).expect("the done is said");
    assert_eq!(done["decision"], "block", "omp reads a block: {done}");
    let r = done["reason"].as_str().unwrap();
    assert!(r.starts_with("[borg hive] t1 done: its checks passed (ok); verify task t1.v1 is open for another drone."), "{r}");
    assert!(r.contains(interactive::IDLE_LINE) && !r.contains("hive wait"), "{r}");
    assert!(done.get("hookSpecificOutput").is_none(), "not Claude's Stop feedback: {done}");
    assert_eq!(done["hive"], true, "marked the hive's own, apart from a conscience's questions: {done}");
    // omp's events carry no usage: the claim's spend line says it is unmetered (phase g G5).
    let spend = std::fs::read_to_string(store(&root).join("hive/g1/spend.jsonl")).unwrap_or_default();
    let line: Value = serde_json::from_str(spend.lines().next().expect("a spend line for the claim")).unwrap();
    assert_eq!((line["exit"].as_str(), line["harness"].as_str(), line["usd"].is_null()), (Some("unmetered"), Some("omp"), true), "{line}");
    assert!(line["claim"].as_str().unwrap().starts_with("t1@"), "{line}");
    assert_eq!(hook_as(&root, "omp", "stop", &ev("stop", 0, json!({"stop_hook_active": true}))), None);
    // The session's end: the drone leaves, its waiter is stopped by the leave.
    hook_as(&root, "omp", "end", &ev("end", 0, json!({})));
    let (code, _) = w.waiter.ended(15);
    assert_eq!(code, 128 + libc::SIGTERM);
    assert!(hs(&root, s).is_null() && interactive::live(&store(&root)).is_empty());
    assert!(events(&root).iter().any(|e| e.by == "runner" && e.note == board::LEAVE), "the node left with its last drone");
    let _ = std::fs::remove_dir_all(root);
}

/// opencode (phase g G3): its shell has no session id, so a `borg hive join` its model runs binds
/// nobody, not even a session whose variable it inherited from an outer harness (the nearest harness
/// process is opencode): its plugin binds the session that ran it (`--bind --session`, under opencode's
/// own process, the harness `opencode`). Another session of the same opencode process running a join
/// never moves the drone (`adopt` is Claude Code's). A stream waiter whose harness process is gone
/// leaves for its session. Fails with the inherited id bound, with the drone moved to the other
/// session, and with the plugin's harness pid unrecorded.
#[test]
fn an_opencode_session_is_bound_by_its_plugin_never_by_its_command() {
    let root = setup("hive-i-opencode");
    let s = "ses-opencode-1";
    let dir = work(&root).display().to_string();
    let bin = env!("CARGO_BIN_EXE_borg");
    let binder = [("BORG_HIVE_BINDER", "opencode"), ("CLAUDE_CODE_SESSION_ID", "outer-claude-session")];
    // The model's own join, in opencode's shell (a process named opencode), with an outer Claude Code
    // session's variable inherited.
    let (_cmd, _) = harness_named(&root, "opencode", &["sh", "-c", &format!("{bin} hive join --dir {dir} > {}", root.join("said").display())], &binder);
    until(20, "the model's join", || std::fs::read_to_string(root.join("said")).is_ok_and(|t| !t.is_empty()));
    let said = std::fs::read_to_string(root.join("said")).unwrap();
    assert!(said.contains("harness plugin binds it"), "{said}");
    assert!(interactive::live(&store(&root)).is_empty() && hs(&root, "outer-claude-session").is_null(), "nobody bound");
    // The plugin's bind from opencode's process, then (once `go` exists) the post_tool of another
    // session of that process whose model ran a join: both children of opencode, as the plugin spawns them.
    let other = adapter_event("opencode", &root, "ses-opencode-2", 0, "post_tool", 0, json!({}));
    std::fs::write(root.join("other.json"), other.to_string()).unwrap();
    let script = format!(
        "{bin} hive join --bind --session {s} --dir {dir} && while [ ! -f {go} ]; do sleep 0.05; done; {bin} hook opencode post_tool < {o} > {out}.part; mv {out}.part {out}",
        go = root.join("go").display(),
        o = root.join("other.json").display(),
        out = root.join("other.out").display()
    );
    let (mut oc, oc_pid) = harness_named(&root, "opencode", &["sh", "-c", &script], &[("BORG_HARNESS", "opencode"), ("BORG_HIVE_BINDER", "opencode")]);
    until(20, "the bind", || interactive::live(&store(&root)).iter().any(|e| e.session == s));
    let e = interactive::live(&store(&root)).into_iter().find(|e| e.session == s).unwrap();
    assert_eq!((e.harness.as_str(), e.pid), ("opencode", oc_pid));
    let drone = e.drone.clone();
    // Another session of the process runs a join: its post_tool moves nothing.
    std::fs::write(root.join("go"), "").unwrap();
    until(20, "the other session's post_tool", || root.join("other.out").exists());
    assert_eq!(hs(&root, s)["drone"], drone.as_str(), "the drone stays");
    assert!(hs(&root, "ses-opencode-2").is_null());
    // Its stop passes (the plugin waits), and its waiter leaves once opencode's process is gone.
    hook_as(&root, "opencode", "prompt", &adapter_event("opencode", &root, s, oc_pid, "prompt", 0, json!({})));
    let out = hook_as(&root, "opencode", "stop", &adapter_event("opencode", &root, s, oc_pid, "stop", 0, json!({})));
    assert!(!reason(&out).contains("hive wait"), "no ask to start a wait: {out:?}");
    assert_eq!(hook_as(&root, "opencode", "stop", &adapter_event("opencode", &root, s, oc_pid, "stop", 0, json!({"stop_hook_active": true}))), None);
    let w = spawn_stream(&root, s);
    std::thread::sleep(Duration::from_millis(800));
    // SAFETY: kill(2) on the process this test spawned and has not reaped.
    unsafe { libc::kill(oc_pid as libc::pid_t, libc::SIGTERM) };
    let _ = oc.0.take().unwrap().wait();
    let lines = w.lines.clone();
    let (code, _) = w.waiter.ended(15);
    assert_eq!(code, 0);
    let last: Value = serde_json::from_str(lines.lock().unwrap().last().unwrap()).unwrap();
    assert!(last["left"] == true && last["wake"].as_str().unwrap().contains("session is gone"), "{last}");
    assert!(hs(&root, s).is_null() && interactive::live(&store(&root)).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

/// The events of a plain omp or opencode session that never joined, as its adapter sends them, through
/// `bin`'s hooks: (event, output with the root as `<root>`).
fn plain_adapter_session(bin: &str, root: &Path, harness: &str) -> Vec<(String, String)> {
    let s = "plain-unjoined";
    let edit = json!({"tool_name": "Edit", "tool_input": {"file_path": work(root).join("src/net.rs"), "new_string": "drain the Orion budget place"}});
    let steps = [
        ("prompt", json!({"prompt": "fix the Orion net budget drain and the websocket edge case"})),
        ("pre_tool", edit.clone()),
        ("post_tool", edit),
        ("pre_tool", json!({"tool_name": "Bash", "tool_input": {"command": "borg hive join"}})),
        ("post_tool", json!({"tool_name": "Bash", "tool_input": {"command": "borg hive join"}, "tool_response": "nothing joined"})),
        (
            "stop",
            json!({"last_assistant_message": "Done: the budget place is drained, and the websocket handling covers the websocket edge case and its limits.", "stop_hook_active": false}),
        ),
        ("stop", json!({"last_assistant_message": "Done.", "stop_hook_active": true})),
        ("end", json!({})),
    ];
    let shown = root.display().to_string();
    steps
        .into_iter()
        .map(|(e, extra)| {
            let mut input = json!({"session_id": s, "cwd": work(root), "pid": std::process::id()});
            input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let mut c = command_with(bin, root, &["hook", harness, e], &[]);
            c.stdin(Stdio::piped()).stderr(Stdio::null());
            let mut child = c.spawn().unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.to_string().as_bytes()).unwrap();
            let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).trim().to_string();
            (e.to_string(), out.replace(&shown, "<root>"))
        })
        .collect()
}

/// Phase g correction 18 for omp and opencode: a session that never joined answers exactly as at HEAD
/// 2e8fd8e, before their drones, while another session of its repo is an omp drone with a stream waiter
/// running (its join command in the plain session included). The pins were taken with that commit's
/// binary (`BORG_PIN_BIN=<it> cargo test --test hive_interactive_cli unjoined_omp`, which rewrites them).
#[test]
fn an_unjoined_omp_or_opencode_session_answers_byte_for_byte_as_before() {
    for harness in ["omp", "opencode"] {
        let root = setup(&format!("hive-i-unjoined-{harness}"));
        open_task(&root, "t1");
        let pin = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{harness}/unjoined-session.json"));
        if let Ok(bin) = std::env::var("BORG_PIN_BIN") {
            let outs = plain_adapter_session(&bin, &root, harness);
            std::fs::write(&pin, serde_json::to_string_pretty(&outs).unwrap() + "\n").unwrap();
            let _ = std::fs::remove_dir_all(root);
            continue;
        }
        ok(&root, &["join"], &[("PI_SESSION_ID", "omp-drone-beside")]);
        let _w = spawn_stream(&root, "omp-drone-beside");
        let got = plain_adapter_session(env!("CARGO_BIN_EXE_borg"), &root, harness);
        let pinned: Vec<(String, String)> = serde_json::from_str(&std::fs::read_to_string(&pin).unwrap()).unwrap();
        assert!(pinned.iter().any(|(e, out)| e == "stop" && !out.is_empty()), "{harness}: the pin covers a stop that answered");
        assert_eq!(got, pinned, "{harness}");
        assert!(interactive::live(&store(&root)).iter().all(|e| e.session == "omp-drone-beside"), "{harness}: no drone made of it");
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The done at a plugin drone's stop while its stream waiter runs, with a second task open: the board's
/// done never makes the waiter claim beside the stop's handover (it holds off while the stop runs,
/// interactive.rs `Stopping`; and a picked claim of a session that holds a task is never written, mod.rs
/// `claim`), so t2 gets one claim line and reaches the drone once, by the stop's `Next`. Fails, as
/// reproduced, with the waiter free to claim during the stop: two `Claim t2` lines, and t2 both in the
/// stop's block and in a second wake.
#[test]
fn a_done_at_a_plugin_drones_stop_hands_its_next_task_once() {
    let root = setup("hive-i-omp-race");
    let s = "omp-race-1";
    ok(&root, &["join"], &[("PI_SESSION_ID", s)]);
    let (_omp, omp_pid) = harness_named(&root, "omp", &[], &[]);
    let ev = |event: &str, nth: usize, extra: Value| adapter_event("omp", &root, s, omp_pid, event, nth, extra);
    let w = spawn_stream(&root, s);
    open_task(&root, "t1");
    hook_as(&root, "omp", "stop", &ev("stop", 0, json!({})));
    hook_as(&root, "omp", "stop", &ev("stop", 0, json!({"stop_hook_active": true})));
    until(15, "t1's wake", || w.wakes().len() == 1);
    hook_as(&root, "omp", "pre_tool", &ev("pre_tool", 1, json!({})));
    open_task(&root, "t2");
    std::fs::write(tree_of(&root, s).join("ok"), "").unwrap();
    let done = hook_as(&root, "omp", "stop", &ev("stop", 0, json!({}))).expect("the done is said");
    let r = done["reason"].as_str().unwrap().to_string();
    assert!(r.starts_with("[borg hive] t1 done"), "{r}");
    std::thread::sleep(Duration::from_millis(1500));
    let claims = events(&root).iter().filter(|e| e.ev == board::Ev::Claim && e.task == "t2").count();
    let by_stop = usize::from(r.contains("Your claim: t2"));
    let by_wake = w.wakes().iter().filter(|v| v["wake"].as_str().unwrap_or_default().contains("Your claim: t2")).count();
    assert_eq!((claims, by_stop + by_wake), (1, 1), "one claim, one delivery: stop {r:?}, wakes {:?}", w.wakes());
    assert!(held_by(&root, "t2", s));
    hook_as(&root, "omp", "end", &ev("end", 0, json!({})));
    let _ = w.waiter.ended(15);
    let _ = std::fs::remove_dir_all(root);
}

/// opencode (phase g correction 12): the engineer's turn halted with no stop (an Esc) is over on borg's
/// side too once the plugin sends `halted`, so the drone's stream waiter hands the task that opens within
/// seconds, not `BUSY_IDLE_MS` later; a plain session's `halted` writes nothing. Fails with the busy flag
/// the prompt wrote left until its 5 minutes run out.
#[test]
fn an_opencode_drones_halted_turn_lets_its_waiter_hand_work_at_once() {
    let root = setup("hive-i-opencode-halted");
    let s = "ses-opencode-halt";
    let dir = work(&root).display().to_string();
    let (_oc, oc_pid) = harness_named(&root, "opencode", &[], &[]);
    ok(&root, &["join", "--bind", "--session", s, "--dir", &dir], &[("BORG_HARNESS", "opencode")]);
    let ev = |event: &str, extra: Value| adapter_event("opencode", &root, s, oc_pid, event, 0, extra);
    let w = spawn_stream(&root, s);
    hook_as(&root, "opencode", "prompt", &ev("prompt", json!({})));
    open_task(&root, "t1");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(w.wakes().is_empty(), "nothing in the engineer's turn");
    // Esc: no stop, the plugin's `halted` (the session id alone, as it sends it).
    let halted = json!({"session_id": s, "cwd": work(&root), "pid": oc_pid});
    assert_eq!(hook_as(&root, "opencode", "halted", &halted), None);
    until(10, "the wake after the halted turn", || !w.wakes().is_empty());
    assert!(w.wakes()[0]["wake"].as_str().unwrap().contains("Your claim: t1"));
    // A plain session's `halted`: nothing written for it.
    let plain = json!({"session_id": "ses-plain-halt", "cwd": work(&root), "pid": oc_pid});
    assert_eq!(hook_as(&root, "opencode", "halted", &plain), None);
    assert!(!store(&root).join("state/ses-plain-halt.hive-busy").exists() && hs(&root, "ses-plain-halt").is_null());
    hook_as(&root, "opencode", "end", &ev("end", json!({})));
    let _ = w.waiter.ended(15);
    let _ = std::fs::remove_dir_all(root);
}

// ---------------------------------------------------------------- codex (phase g G3)

/// codex 0.153.2's hook input `name` (tests/fixtures/codex: recorded from codex itself, against a local
/// mock model), for session `s` in the repo, `said` as a tool's output, `extra` merged in.
fn codex_event(root: &Path, name: &str, s: &str, said: &str, extra: Value) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/codex/{name}.json"));
    let text = std::fs::read_to_string(path).unwrap();
    let mut v: Value = serde_json::from_str(&text).unwrap();
    for (k, with) in
        [("session_id", s.to_string()), ("cwd", work(root).display().to_string()), ("transcript_path", root.join("none.jsonl").display().to_string())]
    {
        v[k] = Value::String(with);
    }
    if v["tool_response"] == "{{said}}" {
        v["tool_response"] = Value::String(said.into());
    }
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    v
}

/// A process named `codex` (a copy of perl) that runs each line it is sent with `sh -c`, as codex runs
/// its hooks: their nearest harness ancestor is it. Stopped as an engineer stops it when dropped.
struct Codex {
    proc: Waiter,
    to: std::process::ChildStdin,
    from: std::io::BufReader<std::process::ChildStdout>,
    pid: u32,
}

/// The fake `codex` CLI on the tests' PATH (`root/bin/codex`): each call's arguments, NUL-separated, in
/// `root/queue/<n>`; exit 1 while `root/queue.fail` exists; while `root/queue.linger` exists, a process
/// of its own session (`setsid`) holds its output for 20 s.
fn fake_codex_cli(root: &Path) {
    let q = root.join("queue");
    std::fs::create_dir_all(&q).unwrap();
    let script = format!(
        "#!/bin/sh\nn=$(ls {q} | wc -l)\nprintf '%s\\0' \"$@\" > {q}/$((n + 1000))\nif [ -f {fail} ]; then echo 'Error: failed to queue session message' >&2; exit 1; fi\nif [ -f {linger} ]; then setsid sleep 20 & fi\necho \"Queued message m1 for thread $3.\"\n",
        q = q.display(),
        fail = root.join("queue.fail").display(),
        linger = root.join("queue.linger").display()
    );
    let bin = root.join("bin/codex");
    std::fs::write(&bin, script).unwrap();
    let mut perm = std::fs::metadata(&bin).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&bin, perm).unwrap();
}

/// The `codex queue` calls so far: each one's arguments.
fn queued(root: &Path) -> Vec<Vec<String>> {
    let mut names: Vec<_> = std::fs::read_dir(root.join("queue")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    names.sort();
    names.iter().map(|p| std::fs::read_to_string(p).unwrap_or_default().split('\0').filter(|a| !a.is_empty()).map(String::from).collect()).collect()
}

impl Codex {
    fn start(root: &Path) -> Self {
        std::fs::create_dir_all(root.join("proc")).unwrap();
        let link = root.join("proc/codex");
        perl_as(&link);
        let script = "$| = 1; while (my $l = <STDIN>) { chomp $l; system('sh', '-c', $l); print \"done\\n\"; }";
        let mut c = command_with(link.to_str().unwrap(), root, &["-e", script], &[("BORG_HIVE_TAKEUP_MS", "20000")]);
        c.stdin(Stdio::piped()).stderr(Stdio::null());
        let mut child = spawn_copy(&mut c);
        let (to, from) = (child.stdin.take().unwrap(), std::io::BufReader::new(child.stdout.take().unwrap()));
        let pid = child.id();
        Codex { proc: Waiter(Some(child)), to, from, pid }
    }

    /// `borg hook codex <event>` with `input`, run by codex's process: its output.
    fn hook(&mut self, root: &Path, event: &str, input: &Value) -> Option<Value> {
        let (i, o) = (root.join("codex-in.json"), root.join("codex-out.json"));
        std::fs::write(&i, input.to_string()).unwrap();
        let line = format!("{} hook codex {event} < {} > {}\n", env!("CARGO_BIN_EXE_borg"), i.display(), o.display());
        std::io::Write::write_all(&mut self.to, line.as_bytes()).unwrap();
        let mut done = String::new();
        std::io::BufRead::read_line(&mut self.from, &mut done).unwrap();
        assert_eq!(done.trim(), "done");
        let out = std::fs::read_to_string(&o).unwrap().trim().to_string();
        (!out.is_empty()).then(|| serde_json::from_str(&out).unwrap())
    }
}

/// Whether `drone`'s waiter runs (its lock is held).
fn waiter_runs(root: &Path, drone: &str) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(store(root).join(format!("state/{drone}.hive-waiter"))) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// codex (phase g G3): a session joins by the `CODEX_THREAD_ID` codex gives its shell (here outside the
/// harness, as from codex's sandbox: the join finds no codex process); the join's own PostToolUse,
/// whose `session_id` is that thread, ties the drone to codex's process (its hooks run as its children);
/// the stop that ends the engineer's turn starts the waiter detached and passes at once, never asking
/// the model for a wait; the waiter wakes the idle session with `codex queue --thread <thread>
/// --message <task>`, which the woken turn's prompt takes up; the done is said first as a block with
/// the idle line; one waiter for the session's life; a queue that exits nonzero releases its claim at
/// once and is tried again; SessionEnd leaves and stops the waiter. All with codex's own hook JSON.
/// Fails with the stop asking for a background wait (Claude's arm), with no waiter started by the
/// stop, with a failed queue leaving the claim held, and without `CODEX_THREAD_ID` in `SESSION_ENV`.
#[test]
fn a_codex_drone_is_woken_by_codex_queue_from_the_waiter_its_stop_starts() {
    let root = setup("hive-i-codex");
    fake_codex_cli(&root);
    let s = "01a17d2b-1640-7d00-83a5-32e2d9613929";
    let said = ok(&root, &["join"], &[("CODEX_THREAD_ID", s)]);
    assert!(said.contains("joined every open hive goal of this repo (g1)") && said.contains("codex queue") && !said.contains("/clear"), "{said}");
    let entry = interactive::live(&store(&root)).into_iter().next().unwrap();
    assert_eq!((entry.session.as_str(), entry.harness.as_str()), (s, "codex"));
    let drone = entry.drone.clone();
    let mut cx = Codex::start(&root);
    // The join's PostToolUse: its thread is the session's, so nothing is said; codex's process is the
    // drone's harness from now on.
    assert_eq!(cx.hook(&root, "post_tool", &codex_event(&root, "hive_join", s, &said, json!({}))), None);
    let e = interactive::live(&store(&root)).into_iter().next().unwrap();
    assert_eq!((e.pid, e.by.as_str()), (cx.pid, "rebind"), "codex's process is the drone's harness");
    cx.hook(&root, "prompt", &codex_event(&root, "prompt", s, "", json!({})));
    open_unapproved(&root, "t1");
    // The turn's stops: a conscience may ask at the first; none asks for a wait, and none blocks for it.
    let mut out = cx.hook(&root, "stop", &codex_event(&root, "stop", s, "", json!({})));
    if out.is_some() {
        assert!(!reason(&out).contains("hive wait") && out.as_ref().unwrap().get("hookSpecificOutput").is_none(), "{out:?}");
        assert!(!waiter_runs(&root, &drone) && queued(&root).is_empty(), "a blocked stop goes on with the engineer's turn");
        out = cx.hook(&root, "stop", &codex_event(&root, "stop_again", s, "", json!({})));
    }
    assert_eq!(out, None, "the stop passes at once");
    // Its plan is not approved (phase g G5): the waiter the stop started queues nothing until it is.
    until(15, "the waiter the stop started", || waiter_runs(&root, &drone));
    std::thread::sleep(Duration::from_millis(1500));
    assert!(queued(&root).is_empty() && task_state(&root, "t1").0 == "open", "an unapproved task is never handed");
    ok(&root, &["ratify", "g1"], &[]);
    until(15, "the queued wake", || !queued(&root).is_empty());
    let q = queued(&root)[0].clone();
    assert_eq!(&q[..4], ["queue", "--thread", s, "--message"], "{q:?}");
    assert!(q[4].starts_with("[borg hive g1] Work arrived.") && q[4].contains("Your claim: t1"), "{}", q[4]);
    assert!(held_by(&root, "t1", s) && waiter_runs(&root, &drone));
    // The queued turn's prompt (codex sends it as UserPromptSubmit) takes the claim up.
    cx.hook(&root, "prompt", &codex_event(&root, "prompt", s, "", json!({"prompt": q[4]})));
    assert!(!store(&root).join(format!("state/{s}.hive-takeup")).exists(), "taken up");
    std::fs::write(tree_of(&root, s).join("ok"), "").unwrap();
    let done = cx.hook(&root, "stop", &codex_event(&root, "stop", s, "", json!({}))).expect("the done is said");
    assert_eq!(done["decision"], "block", "codex reads a block: {done}");
    let r = done["reason"].as_str().unwrap();
    assert!(r.starts_with("[borg hive] t1 done: its checks passed (ok)") && r.contains(interactive::IDLE_LINE) && !r.contains("hive wait"), "{r}");
    assert_eq!(cx.hook(&root, "stop", &codex_event(&root, "stop_again", s, "", json!({}))), None);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(queued(&root).len(), 1, "nothing more to hand: its own verify task is not its");
    // A queue codex refuses: the claim is released at once (not at the 20 s take-up), then tried again.
    std::fs::write(root.join("queue.fail"), "").unwrap();
    open_task(&root, "t2");
    until(8, "the failed queue's release", || events(&root).iter().any(|e| e.ev == board::Ev::Released && e.task == "t2" && e.note.contains("codex queue")));
    assert!(!held_by(&root, "t2", s), "released");
    std::fs::remove_file(root.join("queue.fail")).unwrap();
    until(20, "the wake tried again", || queued(&root).iter().filter(|q| q[4].contains("Your claim: t2")).count() == 2);
    assert!(held_by(&root, "t2", s));
    // The session's end: the drone leaves, its waiter is stopped by the leave.
    cx.hook(&root, "end", &codex_event(&root, "end", s, "", json!({})));
    until(15, "the waiter's end", || !waiter_runs(&root, &drone));
    assert!(hs(&root, s).is_null() && interactive::live(&store(&root)).is_empty());
    assert_eq!(task_state(&root, "t2").0, "open", "the end released its claim");
    assert!(events(&root).iter().any(|e| e.by == "runner" && e.note == board::LEAVE), "the node left with its last drone");
    drop(cx.proc);
    let _ = std::fs::remove_dir_all(root);
}

/// codex's first hook after the join checks the thread (phase g spec 3.1): a join whose
/// `CODEX_THREAD_ID` is not the `session_id` the session's hooks carry would be woken on a thread nobody
/// types in, so its PostToolUse makes the drone leave and says so; the busy turn an engineer's prompt
/// starts ends at codex's Interrupt (an Esc, with no Stop after it: correction 8), and a plain session's
/// Interrupt writes nothing; a waiter whose codex process is gone leaves and queues nothing. Fails with
/// no `mismatch` (the stale drone stays bound), with no `Interrupt` hook (busy for 5 minutes), and with
/// the hook's harness pid unrecorded (the waiter waits on for a codex that is gone).
#[test]
fn a_codex_join_on_another_thread_unbinds_an_interrupt_ends_the_turn_and_a_gone_codex_ends_the_waiter() {
    let root = setup("hive-i-codex-check");
    fake_codex_cli(&root);
    let said = ok(&root, &["join"], &[("CODEX_THREAD_ID", "01a17d2b-0000-7000-8000-00000000aaaa")]);
    let mut cx = Codex::start(&root);
    let s = "01a17d2b-0000-7000-8000-00000000bbbb";
    let out = cx.hook(&root, "post_tool", &codex_event(&root, "hive_join", s, &said, json!({})));
    let text = common::context(&out.map(|v| v.to_string()).unwrap_or_default());
    assert!(text.contains("01a17d2b-0000-7000-8000-00000000aaaa") && text.contains("is no drone"), "{text}");
    assert!(interactive::live(&store(&root)).is_empty() && hs(&root, "01a17d2b-0000-7000-8000-00000000aaaa").is_null(), "unbound");
    assert!(events(&root).iter().any(|e| e.by == "runner" && e.note == board::LEAVE), "the node left with it");
    // The same thread: a drone. The engineer's prompt is a busy turn until codex's Interrupt.
    let said = ok(&root, &["join"], &[("CODEX_THREAD_ID", s)]);
    assert_eq!(cx.hook(&root, "post_tool", &codex_event(&root, "hive_join", s, &said, json!({}))), None);
    let drone = interactive::live(&store(&root))[0].drone.clone();
    cx.hook(&root, "prompt", &codex_event(&root, "prompt", s, "", json!({})));
    let busy = store(&root).join(format!("state/{s}.hive-busy"));
    assert!(busy.exists());
    assert_eq!(cx.hook(&root, "halted", &codex_event(&root, "interrupt", s, "", json!({}))), None);
    assert!(!busy.exists(), "the interrupted turn is over");
    assert_eq!(cx.hook(&root, "halted", &codex_event(&root, "interrupt", "plain-codex", "", json!({}))), None);
    assert!(!store(&root).join("state/plain-codex.hive-busy").exists() && hs(&root, "plain-codex").is_null());
    // Its waiter, from its stop; then codex's process ends with no SessionEnd.
    cx.hook(&root, "stop", &codex_event(&root, "stop_again", s, "", json!({})));
    until(15, "the waiter", || waiter_runs(&root, &drone));
    drop(cx.proc);
    until(15, "the waiter's end", || !waiter_runs(&root, &drone));
    assert!(hs(&root, s).is_null() && interactive::live(&store(&root)).is_empty(), "it left for its session");
    let log = std::fs::read_to_string(store(&root).join(format!("state/{drone}.hive-wait.log"))).unwrap_or_default();
    assert!(log.contains("session is gone") && queued(&root).is_empty(), "{log}");
    let _ = std::fs::remove_dir_all(root);
}

/// A codex wake queues behind a turn the engineer started meanwhile (phase g: never hijacks an
/// engineer's turn): that turn's prompt is not the handed text, so the claim stays not taken up, and its
/// stop runs no check, says no done and blocks nothing, though the check would pass; the wake's own turn,
/// whose prompt is the handed text, takes it up and its stop says the done. Fails with any event taking a
/// codex claim up (the engineer's stop publishes t1's done) and with the stop reading a handed claim as
/// in hand.
#[test]
fn a_codex_wake_queued_behind_the_engineers_turn_is_not_done_by_that_turn() {
    let root = setup("hive-i-codex-behind");
    fake_codex_cli(&root);
    let s = "01a17d2b-1640-7d00-83a5-32e2d9610001";
    let said = ok(&root, &["join"], &[("CODEX_THREAD_ID", s)]);
    let drone = interactive::live(&store(&root))[0].drone.clone();
    let mut cx = Codex::start(&root);
    assert_eq!(cx.hook(&root, "post_tool", &codex_event(&root, "hive_join", s, &said, json!({}))), None);
    assert_eq!(cx.hook(&root, "stop", &codex_event(&root, "stop_again", s, "", json!({}))), None);
    until(15, "the waiter", || waiter_runs(&root, &drone));
    open_task(&root, "t1");
    until(15, "the queued wake", || !queued(&root).is_empty());
    let handed = queued(&root)[0][4].clone();
    assert!(handed.starts_with("[borg hive g1] Work arrived."), "{handed}");
    // The engineer's prompt comes first; codex runs its turn and the wake waits in its queue.
    std::fs::write(tree_of(&root, s).join("ok"), "").unwrap();
    cx.hook(&root, "prompt", &codex_event(&root, "prompt", s, "", json!({})));
    let takeup = store(&root).join(format!("state/{s}.hive-takeup"));
    assert!(takeup.exists(), "the engineer's prompt takes nothing up");
    let mut out = cx.hook(&root, "stop", &codex_event(&root, "stop", s, "", json!({})));
    if out.is_some() {
        assert!(!reason(&out).contains("t1"), "nothing of t1 in the engineer's stop: {out:?}");
        out = cx.hook(&root, "stop", &codex_event(&root, "stop_again", s, "", json!({})));
    }
    assert_eq!(out, None, "the engineer's turn ends as it would");
    assert!(held_by(&root, "t1", s) && takeup.exists(), "still the wake's");
    assert!(!events(&root).iter().any(|e| e.ev == board::Ev::Done), "no done for the engineer's turn");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(queued(&root).len(), 1, "handed once");
    // The wake's own turn.
    cx.hook(&root, "prompt", &codex_event(&root, "prompt", s, "", json!({"prompt": handed})));
    assert!(!takeup.exists(), "taken up");
    let done = cx.hook(&root, "stop", &codex_event(&root, "stop", s, "", json!({}))).expect("the done is said");
    assert!(done["reason"].as_str().unwrap().starts_with("[borg hive] t1 done"), "{done}");
    cx.hook(&root, "end", &codex_event(&root, "end", s, "", json!({})));
    until(15, "the waiter's end", || !waiter_runs(&root, &drone));
    drop(cx.proc);
    let _ = std::fs::remove_dir_all(root);
}

/// A wake `codex queue` took that no turn takes up (a thread no codex runs: the TUI moved on with
/// `/new`) is released after the take-up time and not claimed again until a hook event of the session
/// comes: one claim, one queued message, the task open to other drones meanwhile; the session's next
/// hook event ends the hold. Fails with the waiter claiming and queueing the task again every take-up.
#[test]
fn a_codex_wake_no_turn_takes_up_is_not_claimed_again_until_the_session_speaks() {
    let root = setup("hive-i-codex-missed");
    fake_codex_cli(&root);
    let s = "01a17d2b-1640-7d00-83a5-32e2d9610002";
    ok(&root, &["join"], &[("CODEX_THREAD_ID", s)]);
    let dir = work(&root).display().to_string();
    let _w = Waiter(Some(command(&root, &["hive", "wait", "--queue", "--session", s, "--dir", &dir], &[("BORG_HIVE_TAKEUP_MS", "500")]).spawn().unwrap()));
    open_task(&root, "t1");
    until(15, "the queued wake", || queued(&root).len() == 1);
    until(15, "its release", || events(&root).iter().any(|e| e.ev == board::Ev::Released && e.note.starts_with("not taken up")));
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(queued(&root).len(), 1, "not queued again");
    assert_eq!(task_state(&root, "t1").0, "open", "open to other drones");
    let claims = events(&root).iter().filter(|e| e.ev == board::Ev::Claim && e.task == "t1").count();
    assert_eq!(claims, 1, "claimed once");
    // A hook event of the session: codex runs the thread again.
    hook_as(&root, "codex", "pre_tool", &codex_event(&root, "pre_tool", s, "", json!({})));
    until(15, "the wake again", || queued(&root).len() == 2);
    let _ = std::fs::remove_dir_all(root);
}

/// The events of a plain codex session that never joined, as codex sends them (fixtures/codex), through
/// `bin`'s hooks: (event, output with the root as `<root>`).
fn plain_codex_session(bin: &str, root: &Path) -> Vec<(String, String)> {
    let s = "plain-unjoined";
    let steps = [
        ("prompt", "prompt"),
        ("pre_tool", "pre_tool"),
        ("post_tool", "edit"),
        ("post_tool", "hive_join"),
        ("halted", "interrupt"),
        ("prompt", "correction"),
        ("stop", "stop"),
        ("stop", "stop_again"),
        ("end", "end"),
    ];
    let shown = root.display().to_string();
    steps
        .into_iter()
        .map(|(e, name)| {
            let input = codex_event(root, name, s, "nothing joined", json!({}));
            let mut c = command_with(bin, root, &["hook", "codex", e], &[]);
            c.stdin(Stdio::piped()).stderr(Stdio::null());
            let mut child = c.spawn().unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.to_string().as_bytes()).unwrap();
            let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).trim().to_string();
            (e.to_string(), out.replace(&shown, "<root>"))
        })
        .collect()
}

/// Phase g correction 18 for codex: a session that never joined answers exactly as at HEAD 2e8fd8e,
/// before codex drones, while another session of its repo is a codex drone with its waiter running (a
/// `borg hive join` PostToolUse and an Interrupt in the plain session included). The pin was taken with
/// that commit's binary (`BORG_PIN_BIN=<it> cargo test --test hive_interactive_cli unjoined_codex`, which
/// rewrites it).
#[test]
fn an_unjoined_codex_session_answers_byte_for_byte_as_before() {
    let root = setup("hive-i-unjoined-codex");
    fake_codex_cli(&root);
    open_task(&root, "t0");
    let pin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/unjoined-session.json");
    if let Ok(bin) = std::env::var("BORG_PIN_BIN") {
        let outs = plain_codex_session(&bin, &root);
        std::fs::write(&pin, serde_json::to_string_pretty(&outs).unwrap() + "\n").unwrap();
        let _ = std::fs::remove_dir_all(root);
        return;
    }
    ok(&root, &["join"], &[("CODEX_THREAD_ID", "codex-drone-beside")]);
    let dir = work(&root).display().to_string();
    let _w = Waiter(Some(
        command(&root, &["hive", "wait", "--queue", "--session", "codex-drone-beside", "--dir", &dir], &[("BORG_HIVE_TAKEUP_MS", "20000")]).spawn().unwrap(),
    ));
    let got = plain_codex_session(env!("CARGO_BIN_EXE_borg"), &root);
    let pinned: Vec<(String, String)> = serde_json::from_str(&std::fs::read_to_string(&pin).unwrap()).unwrap();
    assert!(pinned.iter().any(|(e, out)| e == "stop" && !out.is_empty()), "the pin covers a stop that answered");
    assert_eq!(got, pinned);
    assert!(interactive::live(&store(&root)).iter().all(|e| e.session == "codex-drone-beside"), "no drone made of it");
    let state: Vec<String> = std::fs::read_dir(store(&root).join("state"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("plain-unjoined.hive"))
        .collect();
    assert!(state.is_empty(), "no hive state for it: {state:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// The queen session (phase g G5): a plain session's model opens the goal's tasks (`borg hive init`/`open`
/// with the harness's session id in its environment) and is told to put the plan to the engineer; no
/// waiting drone, on the waiter's path, the stop's handover or `borg hive claim`, takes a task of it
/// before the engineer approves; the model can neither approve it (`borg queen answer`, `borg hive
/// ratify` inside its session) nor is the question offered to another session; the engineer's prompt
/// answers it: anything else leaves it waiting, "no, because ..." refuses it (the session revises: a new
/// task, a new question), a yes approves it, and the waiting drone wakes within one board change and
/// takes the work. Fails with the gate off (the waiter claims t1 at once), with the plan approvable by
/// the model's own command, and with the answer read from anything but the engineer's prompt.
#[test]
fn the_plan_is_approved_at_the_engineers_own_prompt_in_the_session_that_opened_it() {
    let root = setup("hive-i-queen");
    let (q, d) = ("sess-queen-1", "sess-drone-1");
    ok(&root, &["join", "g1"], &inside(d));
    // The drone's turn ended: its wait runs.
    let mut w = spawn_wait(&root, d, &[("BORG_HIVE_TAKEUP_MS", "20000")]);
    let out = ok(&root, &["open", "g1", "--id", "t1", "--title", "add the lexer", "--files", "src/lexer.rs", "--accept", "ok=test -f ok"], &inside(q));
    assert!(out.contains("[borg hive] The plan of goal g1 waits for the engineer's approval (queen question q"), "{out}");
    assert!(out.contains("t1  add the lexer · files src/lexer.rs · checks ok=test -f ok") && out.contains("Never answer it yourself"), "{out}");
    let id = out.split("queen question ").nth(1).unwrap().split(')').next().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(w.running() && task_state(&root, "t1").0 == "open", "no drone takes a task of an unapproved plan");
    // Nor by the drone's own claim or its stop's handover.
    let (_, err, code) = hive(&root, &["claim", "t1"], &inside(d));
    assert!(code == 1 && err.contains("waits for the engineer's approval of goal g1's plan"), "{err}");
    assert_eq!(hive(&root, &["claim"], &inside(d)).0.trim(), format!("no task is open to {} now", &d[..8]));
    prompt(&root, d);
    assert!(!reason(&stop(&root, d, false, wait_task(&root, d))).contains("Your claim"), "the stop hands nothing");
    assert!(!reason(&stop(&root, d, true, wait_task(&root, d))).contains("Your claim"), "the turn ends");
    assert_eq!(task_state(&root, "t1").0, "open");
    // The opening session's model cannot approve it.
    let queen_answer = |text: &str, env: &[(&str, &str)]| {
        let dir = work(&root).display().to_string();
        let out = command(&root, &["queen", "answer", &id, text, "--dir", &dir], env).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).to_string())
    };
    let (code, err) = queen_answer("yes", &inside(q));
    assert!(code != 0 && err.contains("at their own prompt"), "{err}");
    let (_, err, code) = hive(&root, &["ratify", "g1"], &inside(q));
    assert!(code == 1 && err.contains("never inside a session"), "{err}");
    // The engineer's prompt: something else first, then a no with its reason.
    let said = |text: &str| hook(&root, "prompt", q, json!({"prompt": text})).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(said("what does t1 touch?").contains("still waits for the engineer"), "reminded, not answered");
    let r = said("no, because the lexer needs a test task first");
    assert!(r.contains("The engineer refused the plan of hive goal g1") && r.contains("Revise the plan"), "{r}");
    std::thread::sleep(Duration::from_millis(1000));
    assert!(w.running() && task_state(&root, "t1").0 == "open", "a refused plan is never claimed");
    let decisions = std::fs::read_to_string(store(&root).join("hive/g1/plan.jsonl")).unwrap();
    assert!(decisions.contains("\"approved\":false") && decisions.contains("the lexer needs a test task first"), "{decisions}");
    // The session revises: a new task, a new question of the whole plan not approved yet.
    let out = ok(&root, &["open", "g1", "--id", "t0", "--title", "test the lexer", "--files", "tests/lexer.rs"], &inside(q));
    assert!(out.contains("t0  test the lexer") && out.contains("t1  add the lexer"), "{out}");
    let r = said("yes");
    assert!(r.contains("The engineer approved the plan of hive goal g1") && r.contains("1 drone(s) on 1 node(s) waiting"), "{r}");
    let b = board::fold(&events(&root));
    assert_eq!(b.ratified.iter().map(String::as_str).collect::<Vec<_>>(), ["t0", "t1"]);
    // The waiting drone wakes on that board change and takes the work.
    let (code, text) = w.ended(15);
    assert!(code == 0 && text.contains("Your claim: t"), "{text}");
    assert!(said("carry on").is_empty(), "answered: nothing more to say");
    let _ = std::fs::remove_dir_all(root);
}

/// A goal opened from a terminal has no session to ask (phase g correction 16): `init` says how to
/// approve it, and `borg hive ratify` does, once, answering the plan with every task not approved yet.
/// Fails with `ratify` approving nothing on the board, or approving twice.
#[test]
fn a_terminals_goal_is_approved_with_ratify() {
    let root = setup("hive-i-ratify");
    let out = ok(&root, &["init", "g2", "--spec", "x", "--oracle", "ok=true", "--budget", "1", "--task", "a|work|write a|a.rs||ok=test -f a"], &[]);
    assert!(out.contains("`borg hive ratify g2` approves it"), "{out}");
    let task = board::fold(&board::read(&store(&root).join("hive/g2/tasks.jsonl"))).tasks["a"].task.clone();
    assert_eq!(task.accept.iter().map(|c| (c.name.as_str(), c.cmd.as_str())).collect::<Vec<_>>(), [("ok", "test -f a")], "a check per task");
    let out = ok(&root, &["ratify", "g2"], &[]);
    assert!(out.contains("approve a  write a") && out.contains("the plan of 1 task(s) is approved"), "{out}");
    assert_eq!(ok(&root, &["ratify", "g2"], &[]).trim(), "nothing to approve in goal g2: every task of its plan is approved");
    let lines = board::read(&store(&root).join("hive/g2/tasks.jsonl"));
    assert_eq!(lines.iter().filter(|e| e.note.starts_with("ratified")).count(), 1);
    let _ = std::fs::remove_dir_all(root);
}

/// A verify task someone opens by hand (`open --role verify`, no work task named) is a card of the plan
/// like any work task (phase g G5): no drone claims it before the engineer approves it, and `ratify`
/// shows and approves it. Fails with every verify task exempt from the approval.
#[test]
fn a_verify_task_opened_by_hand_waits_for_the_plans_approval() {
    let root = setup("hive-i-verify-by-hand");
    let d = "sess-vhand-1";
    ok(&root, &["join", "g1"], &inside(d));
    ok(&root, &["open", "g1", "--id", "v1", "--role", "verify", "--title", "implement the whole parser"], &[]);
    let (_, err, code) = hive(&root, &["claim", "v1"], &inside(d));
    assert!(code == 1 && err.contains("waits for the engineer's approval of goal g1's plan"), "{err}");
    assert_eq!(task_state(&root, "v1").0, "open");
    let out = ok(&root, &["ratify", "g1"], &[]);
    assert!(out.contains("approve v1  implement the whole parser"), "{out}");
    let (out, err, code) = hive(&root, &["claim", "v1"], &inside(d));
    assert!(code == 0 && out.contains("claimed v1"), "{out}{err}");
    let _ = std::fs::remove_dir_all(root);
}

/// The budget question reaches the session that opened the goal's plan (phase g G5): with the plan
/// approved long before, a spent budget asks it there, and the engineer's prompt "$20" raises the
/// board's budget and the drone claims again. Fails with the question queued for that session but its
/// prompts no longer read (the plan's marker removed at the approval).
#[test]
fn the_budget_question_is_answered_at_the_planning_sessions_prompt() {
    let root = setup("hive-i-budget-prompt");
    let (q, d) = ("sess-qbudget-1", "sess-dbudget-1");
    ok(&root, &["join", "g1"], &inside(d));
    ok(&root, &["open", "g1", "--id", "t1", "--title", "add the lexer", "--accept", "ok=test -f ok"], &inside(q));
    let said = |text: &str| hook(&root, "prompt", q, json!({"prompt": text})).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(said("yes").contains("The engineer approved the plan of hive goal g1"));
    assert!(said("carry on").is_empty(), "answered: nothing more to say");
    // Spent: another node's metered claims reach the goal's $5.
    let other = json!({"v": 1, "ms": 1, "drone": "i-other", "node": "ffffffffffffffff", "iter": 0, "harness": "claude", "usd": 5.0, "session": "x", "exit": "interactive", "claim": "z@1"});
    std::fs::write(store(&root).join("hive/g1/spend.jsonl"), format!("{other}\n")).unwrap();
    assert_eq!(hive(&root, &["claim"], &inside(d)).0.trim(), format!("no task is open to {} now", &d[..8]));
    let queue = std::fs::read_to_string(store(&root).join("queen/queue.jsonl")).unwrap_or_default();
    assert!(queue.lines().any(|l| l.contains("Raise the budget of g1?") && l.contains(q)), "asked of the planning session: {queue}");
    assert!(said("how much is spent?").contains("The budget question of hive goal g1 still waits"), "reminded, not answered");
    let r = said("$20");
    assert!(r.contains("The engineer raised the budget of hive goal g1"), "{r}");
    assert_eq!(board::fold(&events(&root)).budget_usd, Some(20.0));
    let (out, err, code) = hive(&root, &["claim"], &inside(d));
    assert!(code == 0 && out.contains("claimed t1"), "{out}{err}");
    let _ = std::fs::remove_dir_all(root);
}

/// `borg top --once` for the work repo: what monitoring prints.
fn top_once(root: &Path) -> String {
    let dir = work(root).display().to_string();
    String::from_utf8_lossy(&command(root, &["top", "--once", "--dir", &dir], &[]).output().unwrap().stdout).to_string()
}

/// A drone reports a plan problem to the session that planned its goal (`tell --to planner`): the
/// planner record (`<session>.hive-plan`) outlives the plan's approval, and the message reaches that
/// session's next prompt once, with the command that answers it, and never a drone; a drone's broadcast
/// reaches it too. A prompt whose harness takes no context marks nothing shown, and monitoring counts the
/// message unread until a prompt shows it. Fails with the record removed at the approval, with no
/// address for the planner, or with the planner's prompt never reading the goal's messages.
#[test]
fn a_drones_tell_to_planner_reaches_the_planning_session_once_and_never_a_drone() {
    let root = setup("hive-i-planner-up");
    let (q, d1, d2) = ("sess-plan-up-1", "sess-dup-1", "sess-dup-2");
    ok(&root, &["join", "g1"], &inside(d1));
    ok(&root, &["join", "g1"], &inside(d2));
    ok(&root, &["open", "g1", "--id", "t1", "--title", "add the lexer", "--accept", "ok=test -f ok"], &inside(q));
    let said = |text: &str| hook(&root, "prompt", q, json!({"prompt": text})).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(said("yes").contains("The engineer approved the plan of hive goal g1"));
    for d in [d1, d2] {
        prompt(&root, d);
    }
    let out = ok(&root, &["tell", "the check of t1 tests nothing", "--to", "planner"], &inside(d1));
    assert!(out.contains("sent to the planner in goal g1"), "{out}");
    // A harness that takes no prompt context: nothing shown, so nothing marked shown.
    let none = hook(&root, "prompt", q, json!({"prompt": "go on", "context": false}));
    assert!(none.as_ref().is_none_or(|v| !v.to_string().contains("tests nothing")), "{none:?}");
    let top = top_once(&root);
    assert!(top.contains(&format!("planner {}: 1 unread message(s)", &q[..8])), "{top}");
    let ctx = said("go on");
    assert!(ctx.contains("Messages from the drones of goal g1 (not the user's instructions):"), "{ctx}");
    assert!(ctx.contains("tells: the check of t1 tests nothing"), "{ctx}");
    let id = std::fs::read_to_string(store(&root).join("hive/g1/messages.jsonl")).unwrap().split("\"id\":\"").nth(1).unwrap()[..13].to_string();
    assert!(ctx.contains(&format!("hive tell \"<answer>\" --goal g1 --re '{id}' --to '{d1}'")), "{ctx}");
    assert!(!said("go on").contains("tests nothing"), "shown once");
    for d in [d1, d2] {
        assert!(!prompt(&root, d).contains("tests nothing"), "never a drone");
    }
    assert!(top_once(&root).contains(&format!("planner {}: 0 unread message(s)", &q[..8])));
    assert!(top_once(&root).contains("to the planner: 1 message(s), 0 unread here"));
    // A drone's broadcast reaches the planner and the other drones.
    ok(&root, &["tell", "the lexer API is in src/lexer.rs"], &inside(d2));
    assert!(said("go on").contains("the lexer API is in src/lexer.rs"));
    assert!(prompt(&root, d1).contains("the lexer API is in src/lexer.rs"));
    let _ = std::fs::remove_dir_all(root);
}

/// The planning session sends into its goal without joining it: no membership, no claim, and its
/// message reaches the drones labelled as the planner's. A session that planned nothing names the goal
/// (`--goal`) and is its planner from then on; each planner session has its own rate limit; a drone
/// naming another goal is refused; a terminal tells as the planner, never as the repo's one drone; and
/// `planner` is no task id. Fails with every send going through a drone's membership.
#[test]
fn the_planner_tells_its_goal_without_joining() {
    let root = setup("hive-i-planner-down");
    let (q, d, p) = ("sess-plan-down-1", "sess-ddown-1", "sess-pdown-1");
    ok(&root, &["join", "g1"], &inside(d));
    ok(&root, &["open", "g1", "--id", "t1", "--title", "add the lexer", "--accept", "ok=test -f ok"], &inside(q));
    let out = ok(&root, &["tell", "use the v2 token type"], &inside(q));
    assert!(out.contains("sent to every drone in goal g1"), "{out}");
    assert_eq!(hs(&root, q), Value::Null, "the planner never joins");
    // Its stop asks for no wait and starts no waiter: a planner has no `.hive` file to read one from (the
    // collective's own stop questions are no hive step).
    let st = reason(&stop(&root, q, false, json!([])));
    assert!(!st.contains("hive"), "a planner's stop asks for no wait: {st}");
    let hive_files: Vec<String> = std::fs::read_dir(store(&root).join("state"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(q) && n.contains(".hive") && !n.ends_with(".hive-plan"))
        .collect();
    assert!(hive_files.is_empty(), "{hive_files:?}");
    let ctx = prompt(&root, d);
    assert!(ctx.contains(&format!("the planner {} tells: use the v2 token type", &q[..8])), "{ctx}");
    // A session that planned nothing names the goal, and plans it from then on.
    let (_, err, code) = hive(&root, &["tell", "the parser waits for t1"], &inside(p));
    assert!(code == 1 && err.contains("--goal"), "{err}");
    ok(&root, &["tell", "the parser waits for t1", "--goal", "g1"], &inside(p));
    ok(&root, &["tell", "noted", "--to", "planner"], &inside(d));
    assert!(prompt(&root, p).contains("tells: noted"), "a planner now");
    assert!(prompt(&root, q).contains("tells: noted"), "every planner of the goal");
    let (_, err, code) = hive(&root, &["tell", "elsewhere", "--goal", "g2"], &inside(d));
    assert!(code == 1 && err.contains("a drone of goal g1"), "{err}");
    // A session that planned two goals names the one it means, from those it planned.
    ok(&root, &["init", "g2", "--spec", "Ship the docs", "--oracle", "ok=test -f ok", "--budget", "5"], &[]);
    ok(&root, &["init", "g3", "--spec", "Ship the site", "--oracle", "ok=test -f ok", "--budget", "5"], &[]);
    ok(&root, &["tell", "the docs wait for g1", "--goal", "g2"], &inside(p));
    let (_, err, code) = hive(&root, &["tell", "which goal?"], &inside(p));
    assert!(code == 1 && err.contains("this session planned g1, g2"), "{err}");
    // The terminal is the engineer: it tells as the planner, never as the repo's one drone.
    ok(&root, &["tell", "from the terminal", "--goal", "g1"], &[]);
    let last = std::fs::read_to_string(store(&root).join("hive/g1/messages.jsonl")).unwrap().lines().last().unwrap().to_string();
    let last: Value = serde_json::from_str(&last).unwrap();
    assert!(last["by"].as_str().unwrap().starts_with("engineer:") && last.get("drone").is_none(), "{last}");
    let (_, err, code) = hive(&root, &["open", "g1", "--id", "planner", "--title", "x"], &[]);
    assert!(code == 1 && err.contains("planner"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}

/// A goal opened and approved from the engineer's terminal has no planning session: a drone's report to
/// the planner is counted on the goal's home node from the synced log alone, never lost from view, and
/// the terminal's `borg hive board` reads it, with the command that answers it, once: read, it is no
/// longer counted unread here. A drone's board neither shows nor marks it. Fails with monitoring reading
/// only planner records, or with no terminal command that shows the reports' text.
#[test]
fn a_report_to_a_goal_planned_from_a_terminal_shows_on_its_home_node() {
    let root = setup("hive-i-planner-terminal");
    let d = "sess-dterm-1";
    ok(&root, &["join", "g1"], &inside(d));
    open_task(&root, "t1");
    prompt(&root, d);
    assert!(!top_once(&root).contains("to the planner"), "nothing sent to the planner yet");
    ok(&root, &["tell", "the check of t1 tests nothing", "--to", "planner"], &inside(d));
    let top = top_once(&root);
    assert!(top.contains("to the planner: 1 message(s), 1 unread here"), "{top}");
    assert!(!ok(&root, &["board", "g1"], &inside(d)).contains("tests nothing"), "never a drone");
    assert!(top_once(&root).contains("1 unread here"), "a drone's board marks nothing read");
    let board = ok(&root, &["board", "g1"], &[]);
    let id = std::fs::read_to_string(store(&root).join("hive/g1/messages.jsonl")).unwrap().split("\"id\":\"").nth(1).unwrap()[..13].to_string();
    assert!(board.contains("To the planner, unread here:") && board.contains("tells: the check of t1 tests nothing"), "{board}");
    assert!(board.contains(&format!("hive tell \"<answer>\" --goal g1 --re '{id}' --to '{d}'")), "{board}");
    assert!(top_once(&root).contains("to the planner: 1 message(s), 0 unread here"));
    assert!(!ok(&root, &["board", "g1"], &[]).contains("tests nothing"), "read once");
    let _ = std::fs::remove_dir_all(root);
}

/// A planning session that is a drone hears only its own goal, as a drone, never the drones of a goal it
/// planned: their reports wait until it leaves, then reach its next prompt. A second `leave`, by the
/// planning session no longer a drone, ends its planning: its record is removed, its prompts hear the
/// goal no more and monitoring lists it no more. Fails with a drone shown its planned goals' messages,
/// or with `leave` refusing a session that only plans.
#[test]
fn a_planner_hears_its_goal_after_leaving_as_a_drone_and_leaves_its_planning() {
    let root = setup("hive-i-planner-leave");
    let (q, d) = ("sess-plan-leave-1", "sess-dleave-1");
    ok(&root, &["init", "g2", "--spec", "Ship the docs", "--oracle", "ok=test -f ok", "--budget", "5"], &[]);
    ok(&root, &["join", "g1"], &inside(d));
    ok(&root, &["tell", "use the v2 token type", "--goal", "g1"], &inside(q));
    assert_eq!(plan_record(&root, q), "g1\n", "a planner of g1");
    ok(&root, &["join", "g2"], &inside(q));
    ok(&root, &["tell", "the check of t1 tests nothing", "--to", "planner"], &inside(d));
    assert!(!prompt(&root, q).contains("tests nothing"), "a drone of g2 hears only g2");
    assert!(top_once(&root).contains(&format!("planner {}: 1 unread message(s)", &q[..8])));
    ok(&root, &["leave"], &inside(q));
    assert_eq!(hs(&root, q), Value::Null);
    assert!(prompt(&root, q).contains("tells: the check of t1 tests nothing"), "heard once it left");
    let out = ok(&root, &["leave"], &inside(q));
    assert!(out.contains("plans no hive goal now"), "{out}");
    assert_eq!(plan_record(&root, q), "", "its planner record removed");
    ok(&root, &["tell", "the lexer API is in src/lexer.rs"], &inside(d));
    assert!(!prompt(&root, q).contains("lexer API"), "a plain session again");
    assert!(!top_once(&root).contains(&format!("planner {}", &q[..8])), "monitoring lists it no more");
    let (_, err, code) = hive(&root, &["leave"], &inside(q));
    assert!(code == 1 && !err.is_empty(), "nothing left to leave: {err}");
    let _ = std::fs::remove_dir_all(root);
}

/// Session `s`'s planner record (plan.rs `planned`), empty when it has none.
fn plan_record(root: &Path, s: &str) -> String {
    std::fs::read_to_string(store(root).join(format!("state/{s}.hive-plan"))).unwrap_or_default()
}

/// A planning session's model reads drones' messages, so a drone's text must never get it to give the
/// engineer's consent (phase g G5): `approve`, `integrate`, `score` and `ratify` refuse inside any
/// harness session, joined or not; the engineer's terminal runs them. Fails with `approve` refusing only
/// a drone.
#[test]
fn the_engineers_commands_refuse_inside_any_session() {
    let root = setup("hive-i-no-consent");
    let s = inside("sess-plain-consent-1");
    for cmd in [["approve", "g1"], ["integrate", "g1"], ["score", "g1"], ["ratify", "g1"]] {
        let (out, err, code) = if cmd[0] == "score" { debug_hive(&root, &cmd, &s) } else { hive(&root, &cmd, &s) };
        assert!(code == 1 && err.contains("never inside a session"), "{cmd:?}: {out}{err}");
    }
    assert_eq!(hive(&root, &["approve", "g1"], &[]).2, 0);
    let _ = std::fs::remove_dir_all(root);
}

/// A transcript of Claude Code (`fixtures/claude/transcript-usage.jsonl`, its assistant lines' shape,
/// stamps made relative to the claim) and a subagent's beside it, put where the drone's hooks name it.
fn transcript_fixture(root: &Path, at: i64) -> PathBuf {
    let fix = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude");
    let ts = |ms: i64| {
        let s = ms / 1000;
        let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
        // Civil from days (Howard Hinnant).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, ms.rem_euclid(1000))
    };
    let fill = |name: &str| std::fs::read_to_string(fix.join(name)).unwrap().replace("BEFORE", &ts(at - 60_000)).replace("AFTER", &ts(at + 5_000));
    let t = root.join("transcript.jsonl");
    std::fs::write(&t, fill("transcript-usage.jsonl")).unwrap();
    std::fs::create_dir_all(root.join("transcript/subagents")).unwrap();
    std::fs::write(root.join("transcript/subagents/agent-1.jsonl"), fill("transcript-subagent.jsonl")).unwrap();
    t
}

/// Metered spend (phase g G5, correction 13): a Claude Code drone's claim, at its done, adds one line to
/// the goal's spend.jsonl, the usage of its transcript (and its subagent's) since the claim, each message
/// once, priced by its model, `exit: "interactive"`; once the goal's metered spend reaches its budget its
/// drones claim nothing more and the engineer is asked once to raise it; `borg hive ratify --budget`
/// raises it and the waiting drone takes the work. Fails with the message before the claim counted, with
/// a repeated message counted twice, with no budget stop, and with the raise not reaching the waiter.
#[test]
fn a_claude_drones_claim_is_metered_from_its_transcript_and_the_budget_stops_new_claims() {
    let root = setup("hive-i-spend");
    let s = "sess-spend-1";
    ok(&root, &["join", "g1"], &inside(s));
    open_task(&root, "t1");
    let (code, text) = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "20000")]).ended(20);
    assert!(code == 0 && text.contains("Your claim: t1"), "{text}");
    let board::Status::Claimed { since, .. } = board::fold(&events(&root)).tasks["t1"].status.clone() else { panic!("t1 is claimed") };
    let t = transcript_fixture(&root, since);
    let with = |extra: Value| {
        let mut v = json!({"transcript_path": t});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    hook(&root, "prompt", s, with(json!({"prompt": "carry on"})));
    std::fs::write(tree_of(&root, s).join("ok"), "").unwrap();
    let r = reason(&hook(
        &root,
        "stop",
        s,
        with(json!({"last_assistant_message": "done", "stop_hook_active": false, "background_tasks": wait_task(&root, s), "session_crons": []})),
    ));
    assert!(r.contains("t1 done"), "{r}");
    // The turn ends (the done's feedback went on with it).
    hook(
        &root,
        "stop",
        s,
        with(json!({"last_assistant_message": "ok", "stop_hook_active": true, "background_tasks": wait_task(&root, s), "session_crons": []})),
    );
    let spend = std::fs::read_to_string(store(&root).join("hive/g1/spend.jsonl")).unwrap();
    let lines: Vec<Value> = spend.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 1, "one line a claim: {spend}");
    let l = &lines[0];
    assert_eq!((l["exit"].as_str(), l["harness"].as_str(), l["session"].as_str()), (Some("interactive"), Some("claude"), Some(s)), "{l}");
    assert_eq!(l["claim"].as_str(), Some(format!("t1@{since}").as_str()));
    // claude-opus-5-5: 1000 in, 2000 out, 10000 cache write, 100000 cache read = $0.114, once though
    // listed twice; the subagent's claude-sonnet-4-6 100 in, 100 out = $0.0018; the line before the
    // claim not at all.
    assert!((l["usd"].as_f64().unwrap() - 0.1158).abs() < 1e-9, "{l}");
    // The budget: spent to the goal's $5 by another node's metered claim.
    let other = json!({"v": 1, "ms": 1, "drone": "i-other", "node": "ffffffffffffffff", "iter": 0, "harness": "claude", "usd": 4.9, "session": "x", "exit": "interactive", "claim": "z@1"});
    std::fs::write(store(&root).join("hive/g1/spend.jsonl"), format!("{spend}{other}\n")).unwrap();
    let mut w = spawn_wait(&root, s, &[("BORG_HIVE_TAKEUP_MS", "20000")]);
    open_task(&root, "t2");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(w.running() && task_state(&root, "t2").0 == "open", "a spent budget: nothing claimed");
    let queue = std::fs::read_to_string(store(&root).join("queen/queue.jsonl")).unwrap_or_default();
    assert_eq!(queue.lines().filter(|l| l.contains("Raise the budget of g1?")).count(), 1, "asked once: {queue}");
    let out = ok(&root, &["ratify", "g1", "--budget", "20"], &[]);
    assert!(out.contains("budget raised to $20.00"), "{out}");
    let queue = std::fs::read_to_string(store(&root).join("queen/queue.jsonl")).unwrap_or_default();
    let asked = queue.lines().rfind(|l| l.contains("Raise the budget of g1?")).unwrap_or_default();
    assert!(asked.contains("borg hive ratify --budget"), "the raise settles the question: {asked}");
    let (code, text) = w.ended(15);
    assert!(code == 0 && text.contains("Your claim: t2"), "{text}");
    let _ = std::fs::remove_dir_all(root);
}

/// borg installed at `root/bin/borg` as a rename over what was there (as `cargo install` does): a bash
/// wrapper that writes `<pid> <args>` to `root/reexec` at each start, then execs this test's borg under
/// its own name (`exec -a`), so the path a waiter started by is the wrapper, never the binary it runs.
/// `body` replaces the wrapper (a file that cannot be exec'd).
fn install_borg(root: &Path, body: Option<&str>) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = root.join("bin/borg");
    let part = root.join("bin/borg.part");
    std::fs::write(&part, body.unwrap_or(&wrapper(root))).unwrap();
    std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&part, &p).unwrap();
    p
}

/// `install_borg`'s wrapper.
fn wrapper(root: &Path) -> String {
    format!("#!/usr/bin/env bash\necho \"$$ $1 $2\" >> {}\nexec -a \"$0\" {} \"$@\"\n", root.join("reexec").display(), env!("CARGO_BIN_EXE_borg"))
}

/// How often the borg of `root/bin/borg` started as waiter `pid` (its wrapper's lines).
fn starts(root: &Path, pid: u32) -> usize {
    let text = std::fs::read_to_string(root.join("reexec")).unwrap_or_default();
    text.lines().filter(|l| *l == format!("{pid} hive wait")).count()
}

/// A joined Claude Code session `s` whose turn ended, its drone, and a `--stream` waiter for it started
/// from `root/bin/borg` (`install_borg`) with `env`, once it holds its lock.
fn waiter_from_install(root: &Path, s: &str, env: &[(&str, &str)]) -> (String, Stream, u32) {
    ok(root, &["join"], &inside(s));
    let drone = hs(root, s)["drone"].as_str().unwrap().to_string();
    prompt(root, s);
    stop(root, s, false, json!([]));
    stop(root, s, true, json!([]));
    let bin = install_borg(root, None);
    let dir = work(root).display().to_string();
    let env: Vec<(&str, &str)> = [("BORG_HIVE_TAKEUP_MS", "20000")].into_iter().chain(env.iter().copied()).collect();
    let w = stream_of(spawn_copy(&mut command_with(bin.to_str().unwrap(), root, &["hive", "wait", "--stream", "--session", s, "--dir", &dir], &env)));
    let pid = w.waiter.0.as_ref().unwrap().id();
    let lock = waiter_lock(root, &drone);
    until(15, "the waiter holds its lock", || starts(root, pid) == 1 && locked(&lock));
    (drone, w, pid)
}

fn waiter_lock(root: &Path, drone: &str) -> PathBuf {
    store(root).join(format!("state/{drone}.hive-waiter"))
}

/// Whether some process holds the lock of the file at `p`.
fn locked(p: &Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(p) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// The lock of the file at `p` looked at every few ms from now: the returned call stops that and says
/// how often it was free.
fn sample_lock(p: PathBuf) -> impl FnOnce() -> usize {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = std::thread::spawn({
        let done = done.clone();
        move || {
            let mut free = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                free += usize::from(!locked(&p));
                std::thread::sleep(Duration::from_millis(3));
            }
            free
        }
    });
    move || {
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        sampler.join().unwrap()
    }
}

impl Stream {
    /// The index of the first line holding `what`.
    fn line(&self, what: &str) -> Option<usize> {
        self.lines.lock().unwrap().iter().position(|l| l.contains(what))
    }
}

/// A waiter follows a reinstall of borg in place (reexec.rs): the borg it started from replaced by a
/// rename, it claims nothing with the old image and execs the new one, same pid, same stdout (one
/// JSON note, then the wake, on the plugin's pipe), its lock never free in between; its wake names the
/// path borg was started by, never `(deleted)`. Started from a wrapper (a shim), it never re-execs for
/// that. Fails with no re-exec (the old image claims and wakes, the wrapper starts once).
#[test]
fn a_waiter_follows_a_reinstall_in_place_and_never_claims_with_the_old_borg() {
    let root = setup("hive-i-reexec");
    let s = "sess-reexec-1";
    let (drone, w, pid) = waiter_from_install(&root, s, &[]);
    std::thread::sleep(Duration::from_millis(1600));
    assert_eq!(starts(&root, pid), 1, "a wrapper is no reinstall: no re-exec loop");
    // The lock is looked at every few ms through the reinstall: never free.
    let free = sample_lock(waiter_lock(&root, &drone));
    let bin = install_borg(&root, None);
    open_task(&root, "t1");
    until(20, "the wake", || !w.wakes().is_empty());
    assert_eq!(free(), 0, "the waiter's lock was free during the re-exec");
    assert_eq!(starts(&root, pid), 2, "one re-exec, in place");
    let (note, wake) = (w.line("runs the reinstalled borg").expect("the re-exec's note"), w.line("\"wake\"").unwrap());
    assert!(note < wake, "the old image claimed nothing: {:?}", w.lines.lock().unwrap());
    let text = w.wakes()[0]["wake"].as_str().unwrap().to_string();
    assert!(text.contains("Your claim: t1") && !text.contains("(deleted)") && text.contains(&bin.display().to_string()), "{text}");
    assert!(held_by(&root, "t1", s));
    assert!(!store(&root).join(format!("state/{drone}.hive-waiter-stale")).exists(), "it runs the borg installed now");
    drop(w);
    let _ = std::fs::remove_dir_all(root);
}

/// A reinstall the waiter cannot exec (no program) leaves it running, gated: it says so once, claims
/// nothing and does none of the node's work (the node lock free for a daemon or another waiter,
/// monitoring's mark up) until borg is installed again, then follows that. It writes no beat of its
/// own either: nothing on the node reaps meanwhile, so its peers must come to suspect the node and take
/// its drones' claims over. Fails with the old image claiming the task, holding the node lock, or
/// beating for the node.
#[test]
fn a_waiter_whose_reinstall_cannot_run_claims_nothing_until_the_next() {
    let root = setup("hive-i-reexec-fail");
    let s = "sess-reexec-2";
    let (drone, mut w, pid) = waiter_from_install(&root, s, &[]);
    until(10, "the waiter does the node's work", || borg::hive::node::worker(&store(&root)) == format!("waiter {drone}"));
    install_borg(&root, Some("not a program\n"));
    until(10, "the failed exec's note", || w.line("could not restart on the reinstalled borg").is_some());
    assert!(store(&root).join(format!("state/{drone}.hive-waiter-stale")).exists(), "monitoring shows it");
    let newest_beat = || events(&root).iter().filter(|e| board::is_beat(e)).map(|e| e.ms).max().unwrap_or(0);
    let gated_at = borg::learn::now_ms();
    open_task(&root, "t1");
    // Longer than a beat's interval (3 s): a gated image's own beat would land meanwhile.
    std::thread::sleep(Duration::from_millis(4000));
    assert!(w.waiter.running() && w.wakes().is_empty() && task_state(&root, "t1").0 == "open", "{:?}", w.lines.lock().unwrap());
    assert_eq!(borg::hive::node::worker(&store(&root)), "none", "the gated image holds no node lock");
    assert!(newest_beat() <= gated_at, "a gate with no re-exec to come beats on: its peers would never take its drones' claims over");
    assert_eq!(w.lines.lock().unwrap().iter().filter(|l| l.contains("could not restart")).count(), 1, "said once");
    install_borg(&root, None);
    until(20, "the wake", || !w.wakes().is_empty());
    assert_eq!(starts(&root, pid), 2);
    assert!(w.line("runs the reinstalled borg").unwrap() < w.line("\"wake\"").unwrap());
    drop(w);
    let _ = std::fs::remove_dir_all(root);
}

/// A stop that comes while the waiter re-execs is never lost: the new image holds SIGINT, SIGHUP and
/// SIGTERM blocked until it trapped them (here held there by the test-only `BORG_HIVE_ADOPT_MS`), then
/// ends on it as a stop, 128 + SIGHUP. Fails with the default action killing it (no exit code).
#[test]
#[cfg(target_os = "linux")]
fn a_stop_during_the_reexec_ends_the_waiter_as_a_stop() {
    let root = setup("hive-i-reexec-stop");
    let s = "sess-reexec-3";
    let (_, w, pid) = waiter_from_install(&root, s, &[("BORG_HIVE_ADOPT_MS", "3000")]);
    install_borg(&root, None);
    let real = std::fs::canonicalize(env!("CARGO_BIN_EXE_borg")).unwrap();
    let exe = || std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    until(10, "the new image runs", || starts(&root, pid) == 2 && exe().is_some_and(|e| e == real));
    // SAFETY: kill(2) with SIGHUP (a closed terminal) on the waiter this test spawned.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGHUP) };
    let Stream { waiter, .. } = w;
    let (code, out) = waiter.ended(20);
    assert_eq!(code, 128 + libc::SIGHUP, "{out}");
    let _ = std::fs::remove_dir_all(root);
}

/// Whether the main thread of process `pid` holds SIGTERM blocked (reexec.rs: across its exec).
#[cfg(target_os = "linux")]
fn main_thread_holds_stops(pid: u32) -> bool {
    let status = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/status")).unwrap_or_default();
    let blocked = status.lines().find_map(|l| l.strip_prefix("SigBlk:")).and_then(|h| u64::from_str_radix(h.trim(), 16).ok()).unwrap_or(0);
    blocked & 1 << (libc::SIGTERM - 1) != 0
}

/// How many threads process `pid` runs.
#[cfg(target_os = "linux")]
fn threads(pid: u32) -> usize {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    status.lines().find_map(|l| l.strip_prefix("Threads:")).and_then(|n| n.trim().parse().ok()).unwrap_or(0)
}

/// codex's waiter (`--queue`, the one mode whose `codex queue` leaves run_bounded reader threads, one
/// alive while a process that left codex's group holds its output) follows a reinstall the same way:
/// one re-exec at the same pid, its lock never free. A SIGTERM sent while the old image holds its stops
/// blocked across the exec (the new file still open for writing: execve is retried on ETXTBSY), with
/// that reader thread alive, ends the new image as a stop, 143, so no second waiter starts for the
/// drone. Fails with run_bounded's threads spawned plainly (the reader takes the SIGTERM, the old image
/// sets its stop and execs: the stop is lost and the waiter waits on).
#[test]
#[cfg(target_os = "linux")]
fn a_codex_waiter_follows_a_reinstall_and_a_stop_during_it_ends_it() {
    let root = setup("hive-i-reexec-codex");
    fake_codex_cli(&root);
    // `codex queue` leaves a process of its own session holding its output: run_bounded's reader lives on.
    std::fs::write(root.join("queue.linger"), "").unwrap();
    let s = "01a17d2b-1640-7d00-83a5-32e2d9610003";
    ok(&root, &["join"], &[("CODEX_THREAD_ID", s)]);
    let drone = interactive::live(&store(&root)).into_iter().next().unwrap().drone;
    let bin = install_borg(&root, None);
    let dir = work(&root).display().to_string();
    // The new image holds the stop blocked a second before it traps it: the lock is looked at meanwhile.
    let env = [("BORG_HIVE_TAKEUP_MS", "20000"), ("BORG_HIVE_ADOPT_MS", "1000")];
    let w = Waiter(Some(spawn_copy(&mut command_with(bin.to_str().unwrap(), &root, &["hive", "wait", "--queue", "--session", s, "--dir", &dir], &env))));
    let pid = w.0.as_ref().unwrap().id();
    let lock = waiter_lock(&root, &drone);
    until(15, "the waiter holds its lock", || starts(&root, pid) == 1 && locked(&lock));
    open_task(&root, "t1");
    until(15, "the queued wake", || !queued(&root).is_empty() && held_by(&root, "t1", s));
    let free = sample_lock(lock);
    // The new borg renamed into place while still open for writing, as a copy not yet closed.
    let busy = {
        use std::os::unix::fs::PermissionsExt;
        let part = root.join("bin/borg.part");
        let mut f = std::fs::File::create(&part).unwrap();
        std::io::Write::write_all(&mut f, wrapper(&root).as_bytes()).unwrap();
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&part, &bin).unwrap();
        f
    };
    let t0 = Instant::now();
    while !main_thread_holds_stops(pid) {
        assert!(t0.elapsed() < Duration::from_secs(5), "the old image's exec, its stops held");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(starts(&root, pid), 1, "still the old image");
    assert!(threads(pid) >= 2, "a run_bounded reader is alive in the old image");
    // SAFETY: kill(2) with SIGTERM on the waiter this test spawned.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    drop(busy);
    until(10, "the new image", || starts(&root, pid) == 2);
    assert_eq!(free(), 0, "the waiter's lock was free during the re-exec");
    let (code, out) = w.ended(20);
    assert_eq!(code, 128 + libc::SIGTERM, "{out}");
    assert_eq!(starts(&root, pid), 2, "one re-exec, in place: {out}");
    assert!(out.contains("runs the reinstalled borg"), "{out}");
    let _ = std::fs::remove_dir_all(root);
}
