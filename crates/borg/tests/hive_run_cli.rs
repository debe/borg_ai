//! `borg hive run` through the binary (hive/run.rs): drones of the simulated harness (a scripted drone,
//! hive/sim_drone.rs) in worktrees of a real git repo, against the real board and the real hooks. No
//! model call: the stop runs with word matching only and no prediction model.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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

/// A `borg hive run` this test started in the background (`spawn`). It never outlives its test: dropped
/// while it still runs (a failed assertion, a panic) it is stopped as an engineer stops it, Ctrl-C, then
/// a closed terminal (SIGHUP), each waited for, with its output drained so no full pipe holds it, and
/// reaped. On Linux the kernel also sends it SIGINT should the test's thread die first (a test binary
/// stopped from outside). Before, a run that waited for the plan outlived a failed test by days.
struct Runner(Option<Child>);

/// `c` started in the background as a `Runner`.
fn spawn(c: &mut Command) -> Runner {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl(2) between fork and exec only sets this process's own parent-death signal.
        unsafe {
            c.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGINT);
                Ok(())
            });
        }
    }
    Runner(Some(c.spawn().unwrap()))
}

impl Runner {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }

    fn id(&mut self) -> u32 {
        self.child().id()
    }

    /// Whether the run still runs.
    fn running(&mut self) -> bool {
        self.child().try_wait().unwrap().is_none()
    }

    /// The run's output once it ends, however long that takes.
    fn output(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else { return };
        if let Some(mut out) = child.stdout.take() {
            std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
        }
        if let Some(mut err) = child.stderr.take() {
            std::thread::spawn(move || std::io::copy(&mut err, &mut std::io::sink()));
        }
        for sig in [libc::SIGINT, libc::SIGHUP] {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            // SAFETY: kill(2) on the runner this test spawned and has not reaped (try_wait above).
            unsafe { libc::kill(child.id() as libc::pid_t, sig) };
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(10) {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        eprintln!("borg hive run {} outlived SIGINT and SIGHUP", child.id());
    }
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
    let files: Vec<String> = (1..=tasks).map(|i| format!("f{i}.txt")).collect();
    setup_files(name, &files.iter().map(String::as_str).collect::<Vec<_>>(), budget, script)
}

/// `setup` with task tN scoped to `files[N-1]` (its oracle wants `done tN` there).
fn setup_files(name: &str, files: &[&str], budget: &str, script: Value) -> PathBuf {
    let root = common::setup(&format!("hive-run-{name}"));
    let w = work(&root);
    git(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "orion\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-q", "-m", "start"]);
    std::fs::write(root.join("script.json"), script.to_string()).unwrap();
    ok(&root, &["init", "g", "--spec", "Write the files", "--oracle", "ok=true", "--budget", budget]);
    for (i, file) in files.iter().enumerate() {
        let id = format!("t{}", i + 1);
        let accept = format!("ok=grep -q 'done {id}' {file}");
        ok(&root, &["open", "g", "--id", &id, "--title", &format!("write {file}"), "--files", file, "--accept", &accept]);
    }
    root
}

/// `borg hive run g <args> --yes`: the engineer approved the plan on the command line (plan.rs), as
/// every suite here but the approval's own does.
fn run(root: &Path, args: &[&str]) -> (String, String, i32) {
    let all: Vec<&str> = ["run", "g"].iter().copied().chain(args.iter().copied()).chain(["--yes"]).collect();
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

/// `borg hive <args> --dir <dir>`: (stdout, stderr, exit code).
fn hive_in(root: &Path, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let dir = dir.display().to_string();
    let all: Vec<&str> = ["hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
    let out = command(root, &all, &[]).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

fn ok_in(root: &Path, dir: &Path, args: &[&str]) -> String {
    let (out, err, code) = hive_in(root, dir, args);
    assert_eq!(code, 0, "borg hive {args:?} in {}: {out}{err}", dir.display());
    out
}

/// `setup` with one task: t2 after t1, scoped to f2.txt, whose oracle wants both t1's and its own work.
fn setup_dependent(name: &str, script: Value) -> PathBuf {
    let root = setup(name, 1, "3", script);
    let accept = "ok=grep -q 'done t1' f1.txt && grep -q 'done t2' f2.txt";
    ok(&root, &["open", "g", "--id", "t2", "--title", "write f2.txt on t1", "--files", "f2.txt", "--deps", "t1", "--accept", accept]);
    root
}

#[test]
fn a_dependent_task_starts_from_its_dependencys_verified_work() {
    let root = setup_dependent("deps", json!({}));
    let w = work(&root);
    std::fs::write(w.join(".git/info/exclude"), "/.worktrees/\n").unwrap();
    let dirs: Vec<PathBuf> = ["a", "b", "c"].iter().map(|d| w.join(format!(".worktrees/{d}"))).collect();
    for (d, n) in dirs.iter().zip(["a", "b", "c"]) {
        git(&w, &["worktree", "add", "-q", "-b", &format!("wt-{n}"), d.to_str().unwrap()]);
    }
    let (a, b, c) = (&dirs[0], &dirs[1], &dirs[2]);
    let (sa, sb, sc) = ("sess-a-dep", "sess-b-dep", "sess-c-dep");
    for s in [sa, sb, sc] {
        ok(&root, &["join", "g", "--session", s]);
    }
    let (_, err, code) = hive(&root, &["open", "g", "--id", "t9", "--title", "x", "--deps", "nope"], &[]);
    assert!(code == 1 && err.contains("its dep nope names no task"), "{err}");
    // 1. Before t1 is verified, t2 is not open to anyone.
    let (_, err, code) = hive_in(&root, b, &["claim", "t2", "--session", sb]);
    assert!(code == 1 && err.contains("t2 is not open"), "{err}");
    // 2. sa does t1 in A; sb verifies it.
    ok_in(&root, a, &["claim", "t1", "--session", sa]);
    std::fs::write(a.join("f1.txt"), "done t1\n").unwrap();
    ok_in(&root, a, &["done", "--session", sa]);
    ok_in(&root, b, &["claim", "t1.v1", "--session", sb]);
    assert!(ok_in(&root, b, &["verify", "--pass", "--session", sb]).contains("t1 is verified"));
    // 3. sb's claim of t2 brings t1's verified work into B, and leaves B's index and HEAD alone.
    let head_b = git(b, &["rev-parse", "HEAD"]);
    let out = ok_in(&root, b, &["claim", "t2", "--session", sb]);
    assert!(out.contains("after t1 (its verified work is in your tree)"), "{out}");
    assert_eq!(std::fs::read_to_string(b.join("f1.txt")).unwrap(), "done t1\n");
    assert_eq!((git(b, &["rev-parse", "HEAD"]), git(b, &["diff", "--cached", "--name-only"])), (head_b, String::new()));
    assert_eq!(git(b, &["status", "--porcelain"]), "?? f1.txt");
    let ev = events(&root);
    let t1_head = ev.iter().find(|e| e.ev == board::Ev::Done && e.task == "t1").unwrap().head.clone();
    let claim = ev.iter().find(|e| e.ev == board::Ev::Claim && e.task == "t2" && e.by == sb).unwrap().clone();
    assert_eq!(claim.deps_in, [format!("t1@{t1_head}")]);
    // 4. Its work is only f2.txt; its oracle passes on the tree with t1's work, and so does its verify.
    std::fs::write(b.join("f2.txt"), "done t2\n").unwrap();
    ok_in(&root, b, &["done", "--session", sb]);
    let t2_head = events(&root).iter().find(|e| e.ev == board::Ev::Done && e.task == "t2").unwrap().head.clone();
    assert_eq!(git(&w, &["diff", "--name-only", &claim.base, &t2_head]), "f2.txt");
    ok_in(&root, a, &["claim", "t2.v1", "--session", sa]);
    let out = ok_in(&root, a, &["verify", "--pass", "--session", sa]);
    assert!(out.contains("t2 is verified"), "{out}");
    // 5. A claim that did not bring its dep's work in (a hand-written line): its pass is not verified.
    let accept = "ok=grep -q 'done t3' f3.txt";
    ok(&root, &["open", "g", "--id", "t3", "--title", "write f3.txt on t1", "--files", "f3.txt", "--deps", "t1", "--accept", accept]);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let line = json!({"v": 1, "ev": "claim", "task": "t3", "ms": now, "by": sc, "base": git(c, &["rev-parse", "HEAD"])});
    let path = goal_dir(&root).join("tasks.jsonl");
    std::fs::write(&path, format!("{}{line}\n", std::fs::read_to_string(&path).unwrap())).unwrap();
    std::fs::write(c.join("f3.txt"), "done t3\n").unwrap();
    ok_in(&root, c, &["done", "--session", sc]);
    ok_in(&root, b, &["claim", "t3.v1", "--session", sb]);
    let out = ok_in(&root, b, &["verify", "--pass", "--session", sb]);
    assert!(out.contains("t3 not verified (the work started from a tree without its dependencies' verified work (t1)"), "{out}");
    let b_now = fold(&root);
    assert_eq!((b_now.tasks["t3"].status.name(), b_now.tasks["t3"].rework), ("open", 1));
    assert!(events(&root).iter().any(|e| e.ev == board::Ev::Rejected && e.task == "t3.v1" && e.hacked.is_none()));
    // 6. A tree whose own f1.txt differs cannot take t1's work: the claim is refused and changes nothing.
    std::fs::write(c.join("f1.txt"), "mine\n").unwrap();
    let (_, err, code) = hive_in(&root, c, &["claim", "t3", "--session", sc]);
    assert!(code == 1 && err.contains("cannot bring t1's verified work into this tree: ") && err.contains("f1.txt"), "{err}");
    assert_eq!(std::fs::read_to_string(c.join("f1.txt")).unwrap(), "mine\n");
    assert_eq!(fold(&root).tasks["t3"].status.name(), "open");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn three_sim_drones_finish_a_dependent_board() {
    let root = setup_dependent("deps-run", json!({"usd_per_iter": 0.10}));
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    let b = fold(&root);
    assert!(b.finished(), "{out}");
    let head = b.integrated.clone().unwrap().head;
    for i in 1..=2 {
        assert!(git(&work(&root), &["show", &format!("{head}:f{i}.txt")]).starts_with(&format!("done t{i}")), "f{i}.txt");
    }
    let ev = events(&root);
    let claim = ev.iter().rev().find(|e| e.ev == board::Ev::Claim && e.task == "t2" && !e.deps_in.is_empty());
    assert!(claim.is_some_and(|c| c.deps_in.len() == 1 && c.deps_in[0].starts_with("t1@")), "{ev:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn idle_drones_wait_for_work_in_flight_to_verify_it() {
    // One task, three drones: whoever claims it works past the idle polls (3 × 3 s); the other two
    // must still be there when its verify task opens, or nobody but its author is left to verify it.
    let slow = json!({"work_s": 12});
    let root = setup("in-flight", 1, "3", json!({"usd_per_iter": 0.10, "drones": {"d1": slow, "d2": slow, "d3": slow}}));
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(fold(&root).finished(), "{out}");
    let _ = std::fs::remove_dir_all(root);
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
    // The hive integrated the accepted attempts once, and the combined tree holds every task's work.
    let integrations: Vec<&board::TaskState> = b.tasks.values().filter(|s| s.task.role == board::Role::Integrate).collect();
    assert_eq!(integrations.iter().map(|s| (s.task.id.as_str(), s.status.name())).collect::<Vec<_>>(), [("integrate.1", "verified")]);
    let head = git(&work(&root), &["rev-parse", "refs/heads/hive/g/integrated"]);
    assert_eq!(b.integrated.as_ref().map(|i| i.head.as_str()), Some(head.as_str()));
    for i in 1..=3 {
        assert!(git(&work(&root), &["show", &format!("{head}:f{i}.txt")]).starts_with(&format!("done t{i}")), "f{i}.txt");
    }
    assert!(out.contains(&format!("integrated {head}")), "{out}");
    // Every work task's verdict came from another drone than its author's.
    let drone = |session: &str| b.drones.iter().find(|((_, s), _)| s == session).map(|(_, d)| d.clone()).unwrap_or_default();
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
    // A session that had not claimed by the end of its task's notice time said so on the board (another
    // node leaves this node's task to it until it claims), at most once per iteration, and ended with a
    // claim or a line of its end: every session of the finished run is settled.
    let marks = |note: &str| events(&root).iter().filter(|e| e.by == "runner" && e.note.starts_with(note)).count();
    assert!(marks("session-start") <= lines.len() && marks("session-end") <= marks("session-start"), "{} {}", marks("session-start"), marks("session-end"));
    assert!(b.sessions.values().all(|(started, settled)| started <= settled), "{:?}", b.sessions);
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

/// Phase f F4: SIGINT stops a run as a user stops it, not as a death. The runner joined the goal and
/// beats; its one drone's session has said that it started and has not claimed (its model takes a
/// minute). On SIGINT the runner says on the board that the session ended and that its node left the
/// goal, then exits 130: a peer neither leaves a task to that session nor waits on its drones' leases.
#[test]
fn sigint_ends_the_unsettled_session_and_leaves_the_goal() {
    let root = setup("sigint", 1, "3", json!({"usd_per_iter": 0.10, "drones": {"d1": {"claim_s": 60}}}));
    let dir = work(&root).display().to_string();
    let args = ["hive", "run", "g", "--drones", "1", "--harness", "sim", "--yes", "--dir", dir.as_str()];
    let mut child = spawn(&mut command(&root, &args, &[]));
    let notes = |root: &Path| events(root).into_iter().filter(|e| e.by == "runner" && e.task.is_empty()).map(|e| e.note).collect::<Vec<_>>();
    let t0 = Instant::now();
    while !(notes(&root).iter().any(|n| n == "session-start d1") && notes(&root).iter().any(|n| n == board::BEAT)) {
        assert!(t0.elapsed() < Duration::from_secs(60), "the session never said it started: {:?}", notes(&root));
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap().success());
    let out = child.output();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(130), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("stopped by SIGINT"), "{text}");
    let notes = notes(&root);
    let at = |note: &str| notes.iter().rposition(|n| n == note);
    assert!(at(board::JOIN) < at("session-start d1") && at("session-start d1") < at("session-end d1"), "{notes:?}");
    assert_eq!(notes.last().map(String::as_str), Some(board::LEAVE), "{notes:?}");
    let b = fold(&root);
    assert!(b.sessions.values().all(|(started, settled)| started <= settled), "{:?}", b.sessions);
    assert!(b.members.values().all(|(join, leave)| join < leave), "{:?}", b.members);
    let _ = std::fs::remove_dir_all(root);
}

/// Phase f F4, after review: once the drones ended and the node left the goal, Ctrl-C ends the run at
/// once again, as before the run caught it: here during the hidden oracle's scoring of the verified
/// attempt (`after_run`, an oracle that takes 30 s). Before, the handler kept noting the signal with no
/// thread left to act on it, and the run went on to the end of the scoring and its summary.
#[test]
fn sigint_after_the_drones_ended_stops_the_run_at_once() {
    let root = common::setup("hive-run-sigint-after");
    let w = work(&root);
    git(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "orion\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-q", "-m", "start"]);
    std::fs::write(root.join("script.json"), json!({"usd_per_iter": 0.10}).to_string()).unwrap();
    let held = root.join("held");
    std::fs::create_dir_all(&held).unwrap();
    let held = held.display().to_string();
    ok(&root, &["init", "g", "--spec", "Write the file", "--oracle", "ok=true", "--budget", "3", "--hidden-dir", &held, "--hidden", "sleep 30"]);
    ok(&root, &["open", "g", "--id", "t1", "--title", "write f1.txt", "--files", "f1.txt", "--accept", "ok=grep -q 'done t1' f1.txt"]);
    let dir = w.display().to_string();
    let args = ["hive", "run", "g", "--drones", "2", "--harness", "sim", "--yes", "--dir", dir.as_str()];
    let mut child = spawn(command(&root, &args, &[]).stdout(Stdio::null()).stderr(Stdio::null()));
    let left = |root: &Path| events(root).iter().any(|e| e.by == "runner" && e.task.is_empty() && e.note == board::LEAVE);
    let t0 = Instant::now();
    while !left(&root) {
        assert!(t0.elapsed() < Duration::from_secs(120), "the run never left the goal");
        assert!(child.running(), "the run ended before the scoring");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(child.running(), "the run is scoring");
    assert!(Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap().success());
    let t0 = Instant::now();
    let status = loop {
        if let Some(status) = child.child().try_wait().unwrap() {
            break status;
        }
        if t0.elapsed() > Duration::from_secs(10) {
            let _ = Command::new("kill").args(["-HUP", &child.id().to_string()]).status();
            panic!("SIGINT did not stop the run during the scoring");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    use std::os::unix::process::ExitStatusExt;
    assert!(status.code() == Some(130) || status.signal() == Some(2), "{status:?}");
    assert!(fold(&root).finished(), "the run had finished the goal");
    let _ = std::fs::remove_dir_all(root);
}

/// `borg learn <args>` on the collective's store, learning on, its model the fake `claude` (common.rs):
/// (stdout, stderr, exit code).
fn learn(root: &Path, args: &[&str]) -> (String, String, i32) {
    let all: Vec<&str> = ["learn"].iter().copied().chain(args.iter().copied()).collect();
    let store = store(root).display().to_string();
    let env = [("BORG_HOME", store.as_str()), ("BORG_LEARN", "on"), ("BORG_LEARN_MODEL", "claude:fake")];
    let out = command(root, &all, &env).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

#[test]
fn a_group_of_three_drones_integrates_the_winner_and_reflection_learns_what_it_did_once() {
    let root = common::setup("hive-run-group");
    let repo = work(&root);
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "orion\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    std::fs::write(root.join("script.json"), json!({"usd_per_iter": 0.10}).to_string()).unwrap();
    // The held-out check passes only the attempt of t1.g2, whichever drone made it: a reward spread of
    // one winner and two losers.
    let held = root.join("held");
    std::fs::create_dir_all(&held).unwrap();
    let held = held.display().to_string();
    ok(
        &root,
        &["init", "g", "--spec", "Write the file", "--oracle", "ok=true", "--budget", "3", "--hidden-dir", &held, "--hidden", "grep -q 'done t1.g2 by' f1.txt"],
    );
    let opened =
        ok(&root, &["open", "g", "--id", "t1", "--title", "write f1.txt", "--files", "f1.txt", "--accept", "ok=grep -q 'done t1' f1.txt", "--group", "3"]);
    assert!(opened.contains("opened t1.g3") && opened.contains("group t1: 3 attempts"), "{opened}");
    // Three drones cannot make three attempts that are each another drone's and have them verified by
    // a drone that made none.
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("group t1 has 3 attempts") && err.contains("at least 4 drones") && spend(&root).is_empty(), "{err}");

    let (out, err, code) = run(&root, &["--drones", "4", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    let b = fold(&root);
    assert!(b.finished(), "{out}");
    // Each attempt is another drone's, and none of them verified a rival's.
    let of = |session: &str| b.drones.iter().find(|((_, s), _)| s == session).map(|(_, d)| d.clone()).unwrap();
    let drone = |t: &str| of(b.tasks[t].author.as_ref().unwrap());
    let mut authors: Vec<String> = (1..=3).map(|n| drone(&format!("t1.g{n}"))).collect();
    authors.sort();
    authors.dedup();
    assert_eq!(authors.len(), 3, "{authors:?}");
    for n in 1..=3 {
        let judges: Vec<String> =
            b.spans.iter().filter(|sp| sp.task.starts_with(&format!("t1.g{n}.v"))).map(|sp| b.drone(&sp.node, &sp.by).unwrap().to_string()).collect();
        assert!(!judges.is_empty() && judges.iter().all(|j| !authors.contains(j)), "t1.g{n} judged by {judges:?}, attempts by {authors:?}");
    }
    let w = drone("t1.g2");
    // Every attempt was scored before the integration, which merged only the winner's.
    let scores: Vec<Value> = std::fs::read_to_string(goal_dir(&root).join("oracle.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((scores.len(), scores.iter().filter(|s| s["pass"] == true).count()), (3, 1));
    let head = git(&repo, &["rev-parse", "refs/heads/hive/g/integrated"]);
    assert!(git(&repo, &["show", &format!("{head}:f1.txt")]).ends_with(&format!("done t1.g2 by {w}")), "the integrated tree holds the winner's attempt");
    let winner = "t1.g2";
    let dir = repo.display().to_string();
    let top = String::from_utf8_lossy(&command(&root, &["top", "--dir", &dir], &[]).output().unwrap().stdout).into_owned();
    let shown = format!("group t1: 3 attempt(s) of 3 · won {w} · lost ");
    assert!(top.contains(&shown) && top.contains(&format!("integrates {winner} · to reflect")), "{top}");

    // `borg learn` (as at any session's end) reflects on the settled group: one model call, the group's
    // prompt, winners first; the lesson keeps the group as its provenance.
    std::fs::write(
        root.join("answer.json"),
        json!({"structured_output": {"is_lesson": true, "question": "Does the file name the drone the held-out check expects?", "situations": ["writing a file a held-out check reads"],
            "consequence": "the held-out check fails", "same_as": "", "area": "verification", "supersedes": [], "reach": "team"}, "total_cost_usd": 0})
        .to_string(),
    )
    .unwrap();
    let (out, err, code) = learn(&root, &[]);
    assert_eq!(code, 0, "{out}{err}");
    let prompts: Vec<String> = std::fs::read_dir(root.join("prompts")).unwrap().flatten().map(|e| std::fs::read_to_string(e.path()).unwrap()).collect();
    let asked: Vec<&String> = prompts.iter().filter(|p| p.starts_with("Several coding agents attempted the same task")).collect();
    assert_eq!(asked.len(), 1, "{prompts:?}");
    let p = asked[0];
    let (won, lost) = (p.find(&format!("### WON: the held-out tests pass (drone {w}")).unwrap(), p.find("### LOST: the held-out tests fail").unwrap());
    assert!(won < lost && p.contains("decided by the hidden oracle") && p.contains(&format!("by {w}")), "{p}");
    let lessons: Vec<Value> = std::fs::read_to_string(store(&root).join("lessons.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let learned: Vec<&Value> = lessons.iter().filter(|l| l["episodes"] == json!(["group-g.t1"])).collect();
    assert_eq!(learned.len(), 1, "{lessons:?}");
    assert_eq!(learned[0]["id"], "Lgroup-g.t1");
    // Once per group: neither the next run nor an explicit reflection asks again.
    let (_, _, code) = learn(&root, &[]);
    assert_eq!(code, 0);
    let (out, _, code) = learn(&root, &["--group", "g/t1"]);
    assert!(code == 0 && out.contains("reflected before (group-g.t1)"), "{out}");
    let again =
        std::fs::read_dir(root.join("prompts")).unwrap().flatten().filter(|e| std::fs::read_to_string(e.path()).unwrap().starts_with("Several coding")).count();
    assert_eq!(again, 1);
    let top = String::from_utf8_lossy(&command(&root, &["top", "--dir", &dir], &[]).output().unwrap().stdout).into_owned();
    assert!(top.contains("· lesson Lgroup-g.t1"), "{top}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_rejected_attempt_and_its_verified_rework_become_one_lesson_once() {
    // The first verdict rejects; the rework names its check and is verified.
    let root = setup("rework", 1, "2", json!({"usd_per_iter": 0.10, "reject_first": true}));
    let (out, err, code) = run(&root, &["--drones", "2", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    let b = fold(&root);
    assert!(b.finished() && b.tasks["t1"].rework == 1, "{out}");
    assert_eq!(b.tasks["t1"].accepted.as_deref(), Some("t1.v2"));
    let dir = work(&root).display().to_string();
    let top = String::from_utf8_lossy(&command(&root, &["top", "--dir", &dir], &[]).output().unwrap().stdout).into_owned();
    assert!(top.contains("rework t1: rejected 1 time(s), then verified · ") && top.contains("· to reflect"), "{top}");

    std::fs::write(
        root.join("answer.json"),
        json!({"structured_output": {"is_lesson": true, "question": "Does the change say which check passed it?", "situations": ["finishing work a verifier reviews"],
            "consequence": "the verifier rejects it", "same_as": "", "area": "verification", "supersedes": [], "reach": "team"}, "total_cost_usd": 0})
        .to_string(),
    )
    .unwrap();
    let rework_prompts = || -> Vec<String> {
        std::fs::read_dir(root.join("prompts"))
            .map(|d| d.flatten().map(|e| std::fs::read_to_string(e.path()).unwrap()).filter(|p| p.starts_with(borg::hive::rework::HEAD)).collect())
            .unwrap_or_default()
    };
    let (out, err, code) = learn(&root, &[]);
    assert_eq!(code, 0, "{out}{err}");
    let asked = rework_prompts();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let p = &asked[0];
    assert!(p.contains("## The rejection\nrework g/t1: rejected 1 time(s)") && p.contains("the file does not say which check passed it"), "{p}");
    assert!(p.contains("What it changed relative to the rejected attempt (excerpt):") && p.contains("+checked by: ok"), "{p}");
    let lessons: Vec<Value> = std::fs::read_to_string(store(&root).join("lessons.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let learned: Vec<&Value> = lessons.iter().filter(|l| l["episodes"] == json!(["rework-g.t1"])).collect();
    assert_eq!(learned.len(), 1, "{lessons:?}");
    assert_eq!(learned[0]["id"], "Lrework-g.t1");
    // Once per task: the next run asks nothing again.
    let (_, _, code) = learn(&root, &[]);
    assert_eq!((code, rework_prompts().len()), (0, 1));
    let top = String::from_utf8_lossy(&command(&root, &["top", "--dir", &dir], &[]).output().unwrap().stdout).into_owned();
    assert!(top.contains("· lesson Lrework-g.t1"), "{top}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_group_of_one_is_a_plain_task_and_reflects_nothing() {
    let root = setup("group-one", 0, "1", json!({"usd_per_iter": 0.10}));
    let opened =
        ok(&root, &["open", "g", "--id", "t1", "--title", "write f1.txt", "--files", "f1.txt", "--accept", "ok=grep -q 'done t1' f1.txt", "--group", "1"]);
    assert!(opened.contains("opened t1 (work)") && !opened.contains("group"), "{opened}");
    let b = fold(&root);
    assert_eq!((b.tasks.len(), b.tasks["t1"].task.group.clone()), (1, None));
    let (out, _, code) = learn(&root, &["--group", "g/t1"]);
    assert!(code == 0 && out.contains("nothing to reflect: t1 is no group of goal g"), "{out}");
    assert!(!store(&root).join("episodes.jsonl").exists());
    // A group of a plan task, or with --again, is refused.
    let (_, err, code) = hive(&root, &["open", "g", "--id", "p", "--role", "plan", "--title", "plan it", "--group", "2"], &[]);
    assert!(code == 1 && err.contains("only a work task"), "{err}");
    let (_, _, code) = hive(&root, &["open", "g", "--id", "t1", "--again", "--group", "2"], &[]);
    assert_eq!(code, 2);
    // Work built on one attempt of a group is refused: only the group's best attempt is integrated.
    ok(&root, &["open", "g", "--id", "t2", "--title", "write f2.txt", "--files", "f2.txt", "--group", "2"]);
    let (_, err, code) = hive(&root, &["open", "g", "--id", "u", "--title", "use f2.txt", "--deps", "t2.g1"], &[]);
    assert!(code == 1 && err.contains("its dep t2.g1 is an attempt of a group"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_drone_that_grows_an_existing_test_is_not_hacked() {
    // Every drone appends to one test file in its worktree: the attempts may conflict there, and the fix
    // task that drops the conflict markers keeps every appended line.
    let grow = json!({"grow_tests": true});
    let root = setup("grow", 2, "4", json!({"usd_per_iter": 0.10, "drones": {"d1": grow, "d2": grow, "d3": grow}}));
    let w = work(&root);
    std::fs::create_dir_all(w.join("tests")).unwrap();
    std::fs::write(w.join("tests/sim_edit.txt"), "original\n").unwrap();
    git(&w, &["add", "."]);
    git(&w, &["commit", "-q", "-m", "a test"]);
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    let b = fold(&root);
    assert!(b.finished(), "{out}");
    assert!(["t1", "t2"].iter().all(|t| b.tasks[*t].status.name() == "verified"), "{out}");
    let ev = events(&root);
    assert!(ev.iter().all(|e| e.hacked.is_none()), "an appended test line is no hack: {ev:?}");
    let head = b.integrated.clone().unwrap().head;
    let grown = git(&w, &["show", &format!("{head}:tests/sim_edit.txt")]);
    assert!(grown.starts_with("original\n") && grown.matches("grown by d").count() >= 2 && !grown.contains("<<<<<<<"), "{grown}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_conflicting_integration_opens_a_fix_task_and_the_hive_finishes() {
    // Both tasks write shared.txt from the same base: each passes alone, their attempts conflict.
    // Each drone holds its work claim 2 s, so t2 goes to another drone while t1 is held: one drone
    // doing both in its own tree builds t2 on t1 (no conflict, the scenario under test is gone).
    let slow = json!({"work_s": 2});
    let root = setup_files("conflict", &["shared.txt", "shared.txt"], "4", json!({"usd_per_iter": 0.10, "drones": {"d1": slow, "d2": slow, "d3": slow}}));
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    let b = fold(&root);
    assert!(b.finished(), "{out}");
    let i1 = &b.tasks["integrate.1"];
    assert!(i1.last_progress.starts_with("rejected: conflict: t") && i1.last_progress.ends_with("in shared.txt"), "{}", i1.last_progress);
    let fix = &b.tasks["integrate.1.fix"];
    let m = fix.task.from.clone().expect("the fix starts from the combined tree");
    assert_eq!((fix.task.files.as_slice(), fix.status.name()), (["shared.txt".to_string()].as_slice(), "verified"));
    let ev = events(&root);
    let claim = ev.iter().find(|e| e.ev == board::Ev::Claim && e.task == "integrate.1.fix" && e.base == m);
    assert!(claim.is_some_and(|c| b.drone(&c.node, &c.by).is_some_and(|d| d.starts_with('d'))), "a runner drone claimed it from M: {ev:?}");
    assert_eq!(b.tasks["integrate.2"].status.name(), "verified");
    let head = b.integrated.clone().unwrap().head;
    let shared = git(&work(&root), &["show", &format!("{head}:shared.txt")]);
    assert!(shared.contains("done t1") && shared.contains("done t2") && !shared.contains("<<<<<<<"), "{shared}");
    assert_eq!(git(&work(&root), &["rev-parse", &format!("{head}^1")]), m, "the second round replayed the first to the same tree");
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
        ev.iter().filter(|e| e.ev == board::Ev::Claim && !b.lost.iter().any(|l| l.0 == e.by && l.2 == e.task && l.3 == e.ms)).map(|e| e.by.as_str()).collect();
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
    // Also on a harness that keeps its own session ids and names none (omp, opencode): the runner
    // finds the cut-off session by the drone its claim carries.
    for own in [false, true] {
        let root = setup(&format!("hang-{own}"), 1, "1", json!({"drones": {"d1": {"hang": true, "own_session": own}}}));
        let t0 = Instant::now();
        // The cap must outlast the drone's first claim, which takes seconds when the suites run in
        // parallel: cut off before it claims, there is nothing to release and the test proves nothing.
        let (out, err, code) = run(&root, &["--drones", "1", "--harness", "sim", "--iter-timeout", "15"]);
        assert!(t0.elapsed() < Duration::from_secs(60), "{:?}", t0.elapsed());
        assert_eq!(code, 1, "{out}{err}");
        let lines = spend(&root);
        // Cut off without naming its cost, it is charged the rest of the share (no dearer iteration yet).
        assert_eq!(lines.iter().map(|l| (l["exit"].as_str().unwrap(), l["est"].as_f64())).collect::<Vec<_>>(), [("timeout", Some(1.0))]);
        assert!(out.contains("end: budget"), "{out}");
        let notes: Vec<String> = events(&root).iter().filter(|e| e.ev == board::Ev::Released).map(|e| e.note.clone()).collect();
        assert_eq!(notes, ["budget"], "own session {own}: the cut-off session's claim is released");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn drones_of_a_harness_with_its_own_session_ids_are_tracked_by_drone() {
    // omp and opencode ignore the runner's session id and name none at their end: each iteration's
    // board lines are found by the drone their claims carry, so progress counts and nothing stalls.
    let own = json!({"own_session": true});
    let root = setup("own-session", 3, "3", json!({"usd_per_iter": 0.10, "drones": {"d1": own, "d2": own, "d3": own}}));
    let (out, err, code) = run(&root, &["--drones", "3", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(fold(&root).finished() && !out.contains("end: stalled"), "{out}");
    let done: Vec<&str> = out.lines().filter_map(|l| l.split("tasks done: ").nth(1)).flat_map(|d| d.split([' ', ','])).collect();
    for t in ["t1", "t2", "t3"] {
        assert!(done.contains(&t), "{t} is in some drone's tasks done: {out}");
    }
    let sessions: Vec<String> = spend(&root).iter().map(|l| l["session"].as_str().unwrap().to_string()).collect();
    assert!(sessions.iter().any(|s| s.contains("sim-d")), "the spend lines name the harness's own sessions: {sessions:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_failed_iteration_that_names_no_cost_is_charged() {
    // The harness dies holding a claim and prints no cost: what it spent is estimated, as a timeout's.
    let root = setup("crash", 1, "1", json!({"drones": {"d1": {"crash": true}}}));
    let (out, err, code) = run(&root, &["--drones", "1", "--harness", "sim"]);
    assert_eq!(code, 1, "{out}{err}");
    let lines = spend(&root);
    assert_eq!(lines.iter().map(|l| (l["exit"].as_str().unwrap(), l["est"].as_f64())).collect::<Vec<_>>(), [("error", Some(1.0))]);
    assert!(out.contains("end: budget"), "{out}");
    let notes: Vec<String> = events(&root).iter().filter(|e| e.ev == board::Ev::Released).map(|e| e.note.clone()).collect();
    assert_eq!(notes, ["budget"], "{out}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_killed_runs_iterations_are_charged_and_their_claims_released_by_the_next_run() {
    let root = setup("killed", 1, "2", json!({"drones": {"d1": {"hang": true}}}));
    let w = work(&root).display().to_string();
    let mut child = spawn(&mut command(&root, &["hive", "run", "g", "--drones", "1", "--harness", "sim", "--yes", "--dir", &w], &[]));
    let t0 = Instant::now();
    while !events(&root).iter().any(|e| e.ev == board::Ev::Claim) {
        assert!(t0.elapsed() < Duration::from_secs(30), "no claim");
        std::thread::sleep(Duration::from_millis(100));
    }
    // Stopped mid-iteration the way an engineer stops it, Ctrl-C: nothing of its own is written any
    // more, and its drone (a process group of its own, which the terminal's signal never reaches) dies
    // with it (runner.rs `Watchdog`).
    // SAFETY: kill(2) with SIGINT on the runner this test spawned and has not reaped.
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) }, 0);
    let status = child.child().wait().unwrap();
    assert!(!status.success(), "Ctrl-C ends the run: {status:?}");
    std::fs::write(root.join("script.json"), json!({}).to_string()).unwrap();
    let mut rerun = spawn(&mut command(&root, &["hive", "run", "g", "--drones", "1", "--harness", "sim", "--yes", "--dir", &w], &[]));
    let t0 = Instant::now();
    while rerun.running() {
        if t0.elapsed() > Duration::from_secs(60) {
            panic!("the rerun waits on the dead session's claim");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = String::from_utf8_lossy(&rerun.output().stdout).to_string();
    // The dead iteration is charged its estimate (the rest of its share): nothing is left to spend.
    let lines = spend(&root);
    assert_eq!(lines.iter().map(|l| (l["exit"].as_str().unwrap(), l["est"].as_f64())).collect::<Vec<_>>(), [("killed", Some(2.0))], "{out}");
    assert!(out.contains("end: budget"), "{out}");
    let b = fold(&root);
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    let why = format!(
        "{out}\nboard:\n{}\njoins:\n{}\nnode: {}",
        read(goal_dir(&root).join("tasks.jsonl")),
        read(goal_dir(&root).join("joins.jsonl")),
        read(store(&root).join("stats/node"))
    );
    assert_eq!(b.tasks["t1"].status, Status::Open, "the dead session's claim is released: {why}");
    assert!(events(&root).iter().any(|e| e.ev == board::Ev::Released && e.note == "its run ended"), "{why}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_hook_planted_in_the_shared_git_dir_never_runs_in_the_runner() {
    // A drone's worktree shares the repo's .git/hooks: the runner's own git calls run none of them.
    let root = setup("hooks", 1, "1", json!({"usd_per_iter": 0.10}));
    let hook = work(&root).join(".git/hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, format!("#!/bin/sh\necho \"$1\" >> {}\n", root.join("ran").display())).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (out, err, _) = run(&root, &["--drones", "2", "--harness", "sim"]);
    assert!(work(&root).join(".worktrees/hive-g-2").is_dir(), "{out}{err}");
    // `worktree add` runs post-checkout with the null ref as the previous HEAD.
    let ran = std::fs::read_to_string(root.join("ran")).unwrap_or_default();
    assert!(!ran.lines().any(|l| !l.is_empty() && l.chars().all(|c| c == '0')), "worktree add ran the planted hook: {ran}");
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

/// `borg queen <args>` in the work repo: (stdout, stderr, exit code).
fn queen(root: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
    let all: Vec<&str> = ["queen"].iter().copied().chain(args.iter().copied()).collect();
    let out = command(root, &all, env).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

/// `borg hive run g --drones <n> --harness sim` without `--yes`, started in the background.
fn spawn_run(root: &Path, drones: &str) -> Runner {
    let w = work(root).display().to_string();
    spawn(&mut command(root, &["hive", "run", "g", "--drones", drones, "--harness", "sim", "--dir", &w], &[]))
}

/// The open plan question of the waiting run `child`, once it is queued (within 30 s).
fn plan_question(root: &Path, child: &mut Runner) -> borg::queen::Entry {
    let t0 = Instant::now();
    loop {
        if let Some(e) = borg::queen::open(&store(root)).into_iter().find(|e| e.kind == borg::queen::Kind::Plan) {
            return e;
        }
        assert!(child.running(), "the run ended without asking");
        assert!(t0.elapsed() < Duration::from_secs(30), "no plan question");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The run's exit code and stdout once it ends (within `secs`); past that the test fails, and the run
/// is stopped as an engineer stops it, Ctrl-C (`Runner`).
fn ended(mut child: Runner, secs: u64) -> (i32, String) {
    let t0 = Instant::now();
    while child.running() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "the run did not end within {secs}s");
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = child.output();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into())
}

/// Phase g G0: a run a failed test started does not outlive it. The run waits for the plan (no `--yes`)
/// when an assertion of the test fails; unwinding drops its `Runner`, which stops it with Ctrl-C and
/// reaps it, so no `borg hive run` is left waiting (before, two such runs waited for days).
#[test]
fn a_failed_test_stops_the_run_it_started() {
    let root = setup("dropped", 1, "1", json!({}));
    let pid = std::sync::atomic::AtomicU32::new(0);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut child = spawn_run(&root, "1");
        plan_question(&root, &mut child);
        pid.store(child.id(), std::sync::atomic::Ordering::SeqCst);
        panic!("an assertion fails while the run waits for the plan");
    }));
    assert!(failed.is_err());
    let pid = pid.into_inner() as libc::pid_t;
    // SAFETY: kill(2) with signal 0 sends nothing; it only asks whether the process is there.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "the run {pid} outlived its test");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_run_waits_for_the_plan_and_a_no_stops_it_with_nothing_claimed() {
    let root = setup("plan-no", 2, "1", json!({"usd_per_iter": 0.10}));
    let mut child = spawn_run(&root, "2");
    let q = plan_question(&root, &mut child);
    assert!(q.question.starts_with("Hive goal g: approve its plan before drones start? 2 task(s): t1 \"write f1.txt\" (normal)"), "{}", q.question);
    assert_eq!(
        q.evidence,
        [
            "t1  write f1.txt · files f1.txt · checks ok=grep -q 'done t1' f1.txt · risk normal (ordinary work)",
            "t2  write f2.txt · files f2.txt · checks ok=grep -q 'done t2' f2.txt · risk normal (ordinary work)"
        ]
    );
    // It waits: no drone, no worktree, no run line, no claim.
    std::thread::sleep(Duration::from_secs(2));
    assert!(child.running(), "the run waits for the engineer");
    assert!(!work(&root).join(".worktrees").exists() && !goal_dir(&root).join("runs.jsonl").exists());
    assert!(!events(&root).iter().any(|e| e.ev == board::Ev::Claim));
    let top = command(&root, &["top", "--dir", &work(&root).display().to_string()], &[]).output().unwrap();
    let top = String::from_utf8_lossy(&top.stdout);
    assert!(top.contains(&format!("plan waiting for the engineer (borg queen answer {})", q.id)), "{top}");
    // A drone never answers it.
    let (_, err, code) = queen(&root, &["answer", &q.id, "a"], &[("BORG_HIVE", "g")]);
    assert!(code == 1 && err.contains("never a hive drone's"), "{err}");
    // Nor a drone whose harness dropped the env, in a worktree `borg hive run` joined.
    let wt = work(&root).join(".git/wt-drone");
    std::fs::create_dir_all(&wt).unwrap();
    let join = json!({"v": 1, "ms": 1, "goal": "g", "dir": wt.display().to_string(), "drone": "d9", "by": "run"});
    let mut joins = std::fs::OpenOptions::new().create(true).append(true).open(goal_dir(&root).join("joins.jsonl")).unwrap();
    std::io::Write::write_all(&mut joins, format!("{join}\n").as_bytes()).unwrap();
    let (_, err, code) = queen(&root, &["answer", &q.id, "a", "--dir", &wt.display().to_string()], &[]);
    assert!(code == 1 && err.contains("never a hive drone's"), "{err}");
    // The engineer says no, with a reason: the run stops, nothing claimed, and the "no" is a veto.
    let (out, err, code) = queen(&root, &["answer", &q.id, "no: t2 must come before t1"], &[("BORG_LEARN", "on")]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("the plan is refused"), "{out}");
    let (code, out) = ended(child, 30);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("the engineer said no to the plan") && out.contains("t2 must come before t1") && out.contains("nothing was started or claimed"),
        "{out}"
    );
    assert!(!work(&root).join(".worktrees").exists() && !goal_dir(&root).join("runs.jsonl").exists() && spend(&root).is_empty());
    assert!(!events(&root).iter().any(|e| e.ev == board::Ev::Claim));
    let episodes = std::fs::read_to_string(store(&root).join("episodes.jsonl")).unwrap_or_default();
    assert!(episodes.contains("\"kind\":\"veto\"") && episodes.contains("t2 must come before t1"), "{episodes}");
    // The same plan again: refused at once, not asked again.
    let (out, _, code) = hive(&root, &["run", "g", "--drones", "2", "--harness", "sim"], &[]);
    assert!(code == 1 && out.contains("the engineer said no to the plan"), "{out}");
    assert_eq!(borg::queen::entries(&store(&root)).len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_approved_plan_runs_to_the_end_and_a_high_risk_claim_escalates_once() {
    let root = setup("plan-yes", 1, "3", json!({"usd_per_iter": 0.10}));
    let accept = "ok=grep -q 'done t2' f2.txt";
    ok(&root, &["open", "g", "--id", "t2", "--title", "drop the users table from f2.txt", "--files", "f2.txt", "--accept", accept]);
    let mut child = spawn_run(&root, "2");
    let q = plan_question(&root, &mut child);
    assert!(q.question.contains("2 task(s), 1 high-risk") && q.evidence[1].ends_with("risk high (an irreversible change)"), "{q:?}");
    let (out, err, code) = queen(&root, &["answer", &q.id, "a"], &[]);
    assert!(code == 0 && out.contains("the plan is approved"), "{out}{err}");
    let (code, out) = ended(child, 240);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("plan approved (queen question {}): 2 task(s), 1 high-risk", q.id)), "{out}");
    let b = fold(&root);
    assert!(b.finished(), "{out}");
    // t2 was claimed by its worker and by its verifier, and escalated once, naming the worker's session.
    let asked: Vec<borg::queen::Entry> = borg::queen::entries(&store(&root)).into_iter().filter(|e| e.kind != borg::queen::Kind::Plan).collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!((asked[0].kind, asked[0].payload.as_ref().map(|p| p["task"].clone())), (borg::queen::Kind::Irreversible, Some(json!("t2"))));
    assert_eq!(Some(&asked[0].session), b.tasks["t2"].author.as_ref(), "the claiming session is the escalation's");
    let w = work(&root).display().to_string();
    let top = command(&root, &["top", "--dir", &w], &[]).output().unwrap();
    let top = String::from_utf8_lossy(&top.stdout);
    assert!(top.contains(&format!("plan approved ({}) · 2 card(s), 1 high-risk", q.id)) && top.contains("card t2  drop the users table"), "{top}");
    // A rerun of the approved plan asks nothing.
    let (out, _, code) = hive(&root, &["run", "g", "--drones", "2", "--harness", "sim"], &[]);
    assert!(code == 0 && out.contains("plan approved"), "{out}");
    assert_eq!(borg::queen::entries(&store(&root)).iter().filter(|e| e.kind == borg::queen::Kind::Plan).count(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_task_opened_while_the_plan_waits_is_asked_about_before_any_drone_starts() {
    let root = setup("plan-changed", 1, "3", json!({"usd_per_iter": 0.10}));
    let mut child = spawn_run(&root, "2");
    let first = plan_question(&root, &mut child);
    assert!(first.question.contains("1 task(s)"), "{}", first.question);
    // Another shell opens a task while the run waits, then the engineer approves the plan they saw.
    ok(&root, &["open", "g", "--id", "t2", "--title", "write f2.txt", "--files", "f2.txt", "--accept", "ok=grep -q 'done t2' f2.txt"]);
    let (out, err, code) = queen(&root, &["answer", &first.id, "a"], &[]);
    assert!(code == 0 && out.contains("the plan is approved"), "{out}{err}");
    // The run asks again for the plan with t2 on it, and nothing is claimed before that answer.
    let t0 = Instant::now();
    let second = loop {
        if let Some(e) = borg::queen::open(&store(&root)).into_iter().find(|e| e.kind == borg::queen::Kind::Plan && e.id != first.id) {
            break e;
        }
        assert!(child.running(), "the run ended without asking again");
        assert!(t0.elapsed() < Duration::from_secs(30), "no second plan question");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(second.question.contains("2 task(s)"), "{}", second.question);
    assert!(!events(&root).iter().any(|e| e.ev == board::Ev::Claim), "no claim before the changed plan is approved");
    let (out, err, code) = queen(&root, &["answer", &second.id, "a"], &[]);
    assert!(code == 0, "{out}{err}");
    let (code, out) = ended(child, 240);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("the plan changed while it waited for approval") && out.contains(&format!("plan approved (queen question {}): 2 task(s)", second.id)),
        "{out}"
    );
    assert!(fold(&root).finished(), "{out}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn yes_approves_the_plan_without_a_question_or_a_wait() {
    let root = setup("plan-flag", 1, "1", json!({"usd_per_iter": 0.10}));
    let t0 = Instant::now();
    let (out, err, code) = run(&root, &["--drones", "2", "--harness", "sim"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("plan approved (--yes): 1 task(s), 0 high-risk") && fold(&root).finished(), "{out}");
    assert!(t0.elapsed() < Duration::from_secs(120));
    assert!(borg::queen::entries(&store(&root)).is_empty(), "nothing asked");
    let plan: Vec<Value> = std::fs::read_to_string(goal_dir(&root).join("plan.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((plan.len(), plan[0]["approved"].as_bool(), plan[0]["via"].as_str()), (1, Some(true), Some("yes")));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_claimant_of_a_high_risk_task_is_told_once() {
    let root = setup("plan-claim", 0, "1", json!({}));
    let accept = "ok=grep -q 'done t1' f1.txt";
    ok(&root, &["open", "g", "--id", "t1", "--title", "deploy to production from f1.txt", "--files", "f1.txt", "--accept", accept]);
    for s in ["sess-risk-a", "sess-risk-b"] {
        ok(&root, &["join", "g", "--session", s]);
    }
    let out = ok(&root, &["claim", "t1", "--session", "sess-risk-a"]);
    assert!(out.contains("t1 is high-risk (an external mutation)") && out.contains("Until they answer: hold: prepare it, but do not run it"), "{out}");
    ok(&root, &["release", "--session", "sess-risk-a"]);
    let out = ok(&root, &["claim", "t1", "--session", "sess-risk-b"]);
    assert!(!out.contains("high-risk"), "asked once per task: {out}");
    let asked = borg::queen::entries(&store(&root));
    assert_eq!(asked.iter().map(|e| (e.kind, e.session.as_str())).collect::<Vec<_>>(), [(borg::queen::Kind::ExternalMutation, "sess-risk-a")]);
    let _ = std::fs::remove_dir_all(root);
}

/// The real git on the test's PATH (the wrapper on the drones' PATH execs it).
fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|d| d.join("git")).find(|p| p.is_file()).expect("git on PATH")
}

/// A `git` on the drones' PATH that makes a snapshot fail (its `write-tree` under a scratch index,
/// verdict.rs `snapshot`) as a git cut off at its 10 s bound under load does: every one while
/// `<root>/fail-snapshot` exists, and the first one in a tree whose HEAD is an integration step (a
/// fix task's done) while `<root>/fail-fix` exists, which it then removes.
fn failing_git(root: &Path) {
    let script = format!(
        r#"#!/bin/sh
real='{git}'
case "$GIT_INDEX_FILE" in *borg-hive-index*)
  dir=.; prev=; tree=no
  for a in "$@"; do [ "$prev" = -C ] && dir=$a; [ "$a" = write-tree ] && tree=yes; prev=$a; done
  if [ $tree = yes ]; then
    [ -f '{r}/fail-snapshot' ] && exit 1
    if [ -f '{r}/fail-fix' ] && "$real" -C "$dir" log -1 --format=%s HEAD 2>/dev/null | grep -q '^integrate '; then
      rm -f '{r}/fail-fix'; exit 1
    fi
  fi;;
esac
exec "$real" "$@"
"#,
        git = real_git().display(),
        r = root.display()
    );
    let path = root.join("bin/git");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_verified_fix_ends_the_conflict_its_integration_found_and_a_missing_snapshot_is_retried() {
    // The 2026-10-25 gate run, reproduced: t1's claim got no snapshot (git timed out under load), its
    // drone went on to t2 in the same tree, then reworked t1 from a base that held its first attempt.
    // Integrated after t2, t1's change is a modify/delete conflict in f1.txt (no markers), which every
    // verified fix left as it was: 11 integrations, each rejected the same way, until the budget ran out.
    let root = setup("conflict-loop", 2, "2", json!({"usd_per_iter": 0.10}));
    failing_git(&root);
    let w = work(&root);
    std::fs::write(w.join(".git/info/exclude"), "/.worktrees/\n").unwrap();
    let (a, b) = (w.join(".worktrees/a"), w.join(".worktrees/b"));
    for (d, n) in [(&a, "a"), (&b, "b")] {
        git(&w, &["worktree", "add", "-q", "-b", &format!("wt-{n}"), d.to_str().unwrap()]);
    }
    let (sa, sb) = ("sess-a-loop", "sess-b-loop");
    for s in [sa, sb] {
        ok(&root, &["join", "g", "--session", s]);
    }
    let fail = root.join("fail-snapshot");
    std::fs::write(&fail, "").unwrap();
    ok_in(&root, &a, &["claim", "t1", "--session", sa]);
    std::fs::remove_file(&fail).unwrap();
    assert!(events(&root).iter().any(|e| e.ev == board::Ev::Claim && e.task == "t1" && e.by == sa && e.base.is_empty()), "the claim has no snapshot");
    std::fs::write(a.join("f1.txt"), "done t1\n").unwrap();
    ok_in(&root, &a, &["done", "--session", sa]);
    ok_in(&root, &b, &["claim", "t1.v1", "--session", sb]);
    assert!(ok_in(&root, &b, &["verify", "--pass", "--session", sb]).contains("t1 not verified (the work has no snapshot"));
    ok_in(&root, &a, &["claim", "t2", "--session", sa]);
    std::fs::write(a.join("f2.txt"), "done t2\n").unwrap();
    ok_in(&root, &a, &["done", "--session", sa]);
    ok_in(&root, &b, &["claim", "t2.v1", "--session", sb]);
    assert!(ok_in(&root, &b, &["verify", "--pass", "--session", sb]).contains("t2 is verified"));
    ok_in(&root, &a, &["claim", "t1", "--session", sa]);
    std::fs::write(a.join("f1.txt"), "done t1\nchecked by: ok\n").unwrap();
    ok_in(&root, &a, &["done", "--session", sa]);
    ok_in(&root, &b, &["claim", "t1.v2", "--session", sb]);
    assert!(ok_in(&root, &b, &["verify", "--pass", "--session", sb]).contains("t1 is verified"));
    // The drones integrate, fix and verify; the fix's first done gets no snapshot either.
    std::fs::write(root.join("fail-fix"), "").unwrap();
    let (out, err, code) = run(&root, &["--drones", "2", "--harness", "sim"]);
    let bd = fold(&root);
    let integrations: Vec<(&str, &str)> =
        bd.tasks.values().filter(|s| s.task.role == board::Role::Integrate).map(|s| (s.task.id.as_str(), s.status.name())).collect();
    assert_eq!((code, integrations.as_slice()), (0, [("integrate.1", "verified"), ("integrate.2", "verified")].as_slice()), "{out}{err}");
    assert!(out.contains("integrate.1") && out.contains("conflict: t1 onto t2 in f1.txt"), "{out}");
    assert!(!root.join("fail-fix").exists(), "the fix's done lost its snapshot once");
    // That fix was retried as what it was, a missing snapshot: its verify rejected it unchecked, and
    // its rework was integrated; no second conflict, no second fix.
    let ev = events(&root);
    assert!(ev.iter().any(|e| e.ev == board::Ev::Rejected && e.task == "integrate.1.fix.v1" && e.note.contains("no snapshot")), "{out}");
    assert_eq!(bd.tasks.keys().filter(|t| t.ends_with(".fix")).count(), 1);
    let i1 = ev.iter().find(|e| e.ev == board::Ev::Rejected && e.task == "integrate.1").unwrap().head.clone();
    let head = bd.integrated.clone().unwrap().head;
    assert_eq!(git(&w, &["rev-parse", &format!("{head}^1")]), i1, "the passing integration builds on the tree the fix verified");
    assert_eq!(git(&w, &["show", &format!("{head}:f1.txt")]), "done t1\nchecked by: ok");
    assert!(git(&w, &["show", &format!("{head}:f2.txt")]).starts_with("done t2"));
    let _ = std::fs::remove_dir_all(root);
}
