//! The mesh daemon as its node's worker for interactive drones (phase g G4, borg hive/node.rs), through
//! the binaries as an engineer runs them: `borg-mesh serve` beside drones that joined from their
//! sessions (`borg hive join` with Claude Code's session id in the environment) and do their work with
//! `borg hive` commands, no wait of theirs running. The daemon beats for the node, its stop (SIGINT, as
//! Ctrl-C stops it) writes no leave and its start no join, so a drone's claim lives through a restart;
//! it reaps a drone whose session died mid-task; and it integrates a goal whose drones are all idle, as
//! a `borg hive integrate` child. Relays are off and no model is called: every model CLI on PATH is a
//! stub that fails.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use borg::hive::{self, board};
use serde_json::json;

fn borg_bin() -> PathBuf {
    let p = Path::new(env!("CARGO_BIN_EXE_borg-mesh")).with_file_name("borg");
    assert!(p.is_file(), "no borg binary at {}: cargo build -p borg-ai --bin borg", p.display());
    p
}

fn app(root: &Path) -> PathBuf {
    root.join("work/app")
}

fn store(root: &Path) -> PathBuf {
    root.join("base/collectives/corp")
}

/// `bin` with the machine's environment: its store under `base`, its config, no model; the drones'
/// scratch state where borg keeps it by default (the store's `state`), which the daemon must find too.
fn cmd(root: &Path, bin: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .env_clear()
        .env("PATH", format!("{}:/usr/local/bin:/usr/bin:/bin", root.join("bin").display()))
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .env("BORG_LEARN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_MATCH", "bm25")
        .env("BORG_PREDICT_MODEL", "off")
        .env("BORG_STOP_ATTENTION", "off")
        .env("BORG_MODE", "hook")
        .envs(env.iter().copied())
        .current_dir(app(root))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// `borg hive <args> --dir <dir>` as session `s` (none: the engineer): its output, asserted to succeed.
fn hive(root: &Path, dir: &Path, s: Option<&str>, args: &[&str]) -> String {
    let d = dir.display().to_string();
    let all: Vec<&str> = ["hive"].into_iter().chain(args.iter().copied()).chain(["--dir", d.as_str()]).collect();
    let env: Vec<(&str, &str)> = s.map(|s| ("CLAUDE_CODE_SESSION_ID", s)).into_iter().collect();
    let out = cmd(root, &borg_bin(), &all, &env).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "borg hive {args:?}: {text}");
    text
}

fn git(dir: &Path, args: &[&str]) {
    let out =
        Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=E"]).args(args).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A team collective `corp` routing `work/` with this machine as its admin, a committed repo `work/app`,
/// and goal `g1` (oracle: `ok` exists) with task t1.
fn setup(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-mesh-node-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["home", "bin", "work/app"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    for cli in ["claude", "codex", "opencode", "omp"] {
        let p = root.join("bin").join(cli);
        std::fs::write(&p, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cfg = json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("work")], "share": "team", "relay": "disabled"}]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    let out = cmd(&root, Path::new(env!("CARGO_BIN_EXE_borg-mesh")), &["init", "--dir", &app(&root).display().to_string()], &[]).output().unwrap();
    assert!(out.status.success(), "init: {}", String::from_utf8_lossy(&out.stderr));
    let a = app(&root);
    git(&a, &["init", "-q", "-b", "main"]);
    std::fs::write(a.join("README"), "app\n").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-q", "-m", "start"]);
    hive(&root, &a, None, &["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=test -f ok", "--budget", "5"]);
    hive(&root, &a, None, &["open", "g1", "--id", "t1", "--title", "do t1"]);
    root
}

fn events(root: &Path) -> Vec<board::Event> {
    board::read(&hive::tasks_path(&store(root), "g1"))
}

fn notes(root: &Path, note: &str) -> usize {
    events(root).iter().filter(|e| e.by == "runner" && e.note == note).count()
}

fn held_by(root: &Path, id: &str, s: &str) -> bool {
    board::fold(&events(root)).tasks.get(id).is_some_and(|t| t.live(borg::learn::now_ms()).is_some_and(|l| l.0 == s))
}

/// The newest beat of this node held in the goal's beats store, if any.
fn newest_beat(root: &Path) -> Option<i64> {
    events(root).iter().filter(|e| board::is_beat(e)).map(|e| e.ms).max()
}

fn tree_of(s: &str, root: &Path) -> PathBuf {
    let hs: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(store(root).join(format!("state/{s}.hive"))).unwrap()).unwrap();
    PathBuf::from(hs["worktree"].as_str().unwrap())
}

fn until(secs: u64, what: &str, done: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !done() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "{what} within {secs}s");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A process this test started (`borg-mesh serve`, a harness): never outlives its test. Stopped as an
/// engineer stops it, Ctrl-C, then SIGHUP, each waited for.
struct Proc(Option<Child>);

impl Proc {
    /// SIGINT, as Ctrl-C at the terminal: its exit code once it ended.
    fn interrupt(mut self) -> i32 {
        let mut c = self.0.take().unwrap();
        signal(&c, "INT");
        c.wait().unwrap().code().unwrap_or(-1)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else { return };
        for sig in ["INT", "HUP"] {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            signal(&child, sig);
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(10) && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// `kill -<sig>` on a child this test spawned and has not reaped.
fn signal(c: &Child, sig: &str) {
    let _ = Command::new("kill").args([format!("-{sig}"), c.id().to_string()]).status();
}

fn serve(root: &Path, n: u32) -> Proc {
    let log = std::fs::File::create(root.join(format!("serve-{n}.log"))).unwrap();
    let mut c = cmd(
        root,
        Path::new(env!("CARGO_BIN_EXE_borg-mesh")),
        &["serve", "--bind", "127.0.0.1:0", "--interval", "1", "--dir", &app(root).display().to_string()],
        &[],
    );
    c.stdout(log.try_clone().unwrap()).stderr(log);
    Proc(Some(c.spawn().unwrap()))
}

/// The whole of G4 on one node, in the order an engineer meets it:
/// 1. A drone joins and claims t1, its session's harness a perl process; no wait of it runs. The daemon
///    starts and beats for the node, and names the drone in its roster of takers (it holds a claim).
/// 2. Ctrl-C stops the daemon: no leave of the node, the claim still held. Started again: it beats on,
///    with no second join (phase g correction 2: either would end the claim on every peer).
/// 3. The drone's session dies mid-task: the daemon reaps it, its claim released `its session is gone`,
///    and with the node's last drone the node leaves.
/// 4. Two new drones do t1 and verify it, then idle with no wait running: the daemon integrates the
///    goal (its `borg hive integrate` child), says it finished, and its stop writes no leave.
///
/// Each step fails with its part of the worker taken out: no worker thread (no beat), a leave on the
/// daemon's stop or a join at its start (the counts), no reaper (the claim held for its lease), no
/// integration child (the goal never finishes).
#[test]
fn the_mesh_daemon_does_its_nodes_goal_work_for_interactive_drones() {
    let root = setup("worker");
    let a = app(&root);
    let sa = "sess-node-cli-a";
    let script = format!("{b} hive join g1 --dir {d} && {b} hive claim --dir {d}", b = borg_bin().display(), d = a.display());
    let mut perl = cmd(&root, Path::new("perl"), &["-e", "system('sh', '-c', $ARGV[0]); sleep 600", &script], &[("CLAUDE_CODE_SESSION_ID", sa)]);
    perl.stdout(Stdio::null()).stderr(Stdio::null());
    let mut harness = Proc(Some(perl.spawn().unwrap()));
    until(20, "the claim", || held_by(&root, "t1", sa));
    assert_eq!(newest_beat(&root), None, "no beat before a worker runs");

    // 1. The daemon beats for the node and keeps the roster of its takers.
    let d1 = serve(&root, 1);
    until(20, "the daemon's beat", || newest_beat(&root).is_some());
    until(10, "the roster", || events(&root).iter().any(|e| e.by == "runner" && e.note.starts_with("drones i-")));

    // 2. Its stop and its start write no membership line; the claim lives through both.
    assert_eq!(d1.interrupt(), 0, "Ctrl-C stops it cleanly");
    let before = newest_beat(&root).unwrap();
    assert_eq!((notes(&root, board::JOIN), notes(&root, board::LEAVE)), (1, 0), "no leave on its stop");
    assert!(held_by(&root, "t1", sa));
    let d2 = serve(&root, 2);
    until(20, "a beat after the restart", || newest_beat(&root).is_some_and(|b| b > before));
    assert_eq!((notes(&root, board::JOIN), notes(&root, board::LEAVE)), (1, 0), "no join on its start");
    assert!(held_by(&root, "t1", sa), "the claim lives through the restart");

    // 3. The session dies mid-task, no wait running: reaped.
    let mut perl = harness.0.take().unwrap();
    signal(&perl, "TERM");
    let _ = perl.wait();
    until(15, "the reap", || events(&root).iter().any(|e| e.ev == board::Ev::Released && e.by == sa && e.note == "its session is gone"));
    until(5, "the node's leave with its last drone", || notes(&root, board::LEAVE) == 1);

    // 4. t1 done and verified by two new drones, which then idle: the daemon integrates.
    let (sb, sc) = ("sess-node-cli-b", "sess-node-cli-c");
    hive(&root, &a, Some(sb), &["join", "g1"]);
    hive(&root, &a, Some(sc), &["join", "g1"]);
    let (tb, tc) = (tree_of(sb, &root), tree_of(sc, &root));
    hive(&root, &tb, Some(sb), &["claim"]);
    std::fs::write(tb.join("ok"), "").unwrap();
    hive(&root, &tb, Some(sb), &["done"]);
    assert!(hive(&root, &tc, Some(sc), &["claim"]).contains("t1.v1"));
    hive(&root, &tc, Some(sc), &["verify", "--pass"]);
    until(60, "the daemon's integration", || board::fold(&events(&root)).finished());
    let b = board::fold(&events(&root));
    assert!(b.integrations.iter().any(|i| i.task == "integrate.1"), "{:?}", b.integrations);
    until(10, "the node's finished line", || notes(&root, &board::finished_note("integrate.1")) == 1);
    let leaves = notes(&root, board::LEAVE);
    assert_eq!(d2.interrupt(), 0);
    assert_eq!(notes(&root, board::LEAVE), leaves, "no leave on its stop");
    hive(&root, &a, Some(sb), &["leave"]);
    hive(&root, &a, Some(sc), &["leave"]);
    let _ = std::fs::remove_dir_all(root);
}
