//! borg-eval: run drone arms over an eval workload and record cost, solve
//! rate and exploration per task.
//!
//! usage: borg-eval --workload eval/trap-ledger [--arm vanilla] [--seeds 3]
//!                  [--model sonnet] [--tasks 4] [--budget-usd 2] [--timeout-s 900]
//!                  [--settings arm.json] [--run NAME]
//!
//! Sequential mode: per seed, one fresh repo; tasks run in order, each in a
//! fresh drone session. Between tasks the harness commits tracked changes and
//! runs `git clean -fdx` (a fresh clone, as far as untracked state goes), and
//! applies `drift/before-NN-*.sh` scripts. Results go to
//! `target/eval-runs/<run>/results.jsonl`; `summary.md` is printed.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use borg_trace::{Kind, claude};
use serde_json::{Value, json};

mod dogfood;
// Replay evals over private data: optional, see build.rs. rustfmt cannot resolve absent module files, so
// `cargo fmt` skips these; scripts/check.sh checks them with rustfmt directly when they are present.
#[cfg(replay_evals)]
#[rustfmt::skip]
mod preempt;
#[cfg(replay_evals)]
#[rustfmt::skip]
mod queen_replay;
#[cfg(replay_evals)]
#[rustfmt::skip]
mod queen_test;

const SYSTEM: &str = "You are working autonomously in a sandboxed repository. Do the task fully without asking questions. \
Do not use plan mode, do not create task-tracking files, and do not commit.";

struct Args {
    workload: PathBuf,
    arm: String,
    seeds: u32,
    model: String,
    tasks: usize,
    budget_usd: String,
    timeout_s: u32,
    settings: Option<PathBuf>,
    run: String,
}

fn args() -> Args {
    let mut a = Args {
        workload: "eval/trap-ledger".into(),
        arm: "vanilla".into(),
        seeds: 1,
        model: "sonnet".into(),
        tasks: usize::MAX,
        budget_usd: "2".into(),
        timeout_s: 900,
        settings: None,
        run: String::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let v = it.next().unwrap_or_default();
        match flag.as_str() {
            "--workload" => a.workload = v.into(),
            "--arm" => a.arm = v,
            "--seeds" => a.seeds = v.parse().expect("--seeds"),
            "--model" => a.model = v,
            "--tasks" => a.tasks = v.parse().expect("--tasks"),
            "--budget-usd" => a.budget_usd = v,
            "--timeout-s" => a.timeout_s = v.parse().expect("--timeout-s"),
            "--settings" => a.settings = Some(fs::canonicalize(v).expect("--settings")),
            "--run" => a.run = v,
            _ => panic!("unknown flag {flag}"),
        }
    }
    if a.run.is_empty() {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        a.run = format!("{}-{}-{secs}", a.arm, a.model);
    }
    a.workload = fs::canonicalize(&a.workload).expect("--workload");
    a
}

pub(crate) fn sh(dir: &Path, cmd: &str) -> (bool, String) {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .envs([("GIT_AUTHOR_NAME", "eval"), ("GIT_AUTHOR_EMAIL", "eval@borg"), ("GIT_COMMITTER_NAME", "eval"), ("GIT_COMMITTER_EMAIL", "eval@borg")])
        .output()
        .expect("sh");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn sorted_files(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.retain(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix)));
    v.sort();
    v
}

/// Fresh git repo from the workload's starting state.
fn fresh_repo(workload: &Path, work: &Path) {
    let _ = fs::remove_dir_all(work);
    fs::create_dir_all(work.parent().unwrap()).unwrap();
    let (ok, out) = sh(Path::new("/"), &format!("cp -r '{}' '{}'", workload.join("repo").display(), work.display()));
    assert!(ok, "copy repo: {out}");
    let (ok, out) = sh(work, "git init -q && git add -A && git commit -qm init");
    assert!(ok, "git init: {out}");
}

/// The eval sandbox: the system read-only but NOT the user's home. No original repos
/// (future commits), no transcripts (the corrections), no eval data (criteria, later lessons),
/// no git/gh/ssh credentials. Only the claude binary, its credentials, a sanitized
/// `~/.claude.json`, `cwd`, `readonly` and `writable` are bound. Returns the bwrap command up to
/// `--chdir cwd --` (append the program) and a private dir to remove afterwards.
#[cfg(replay_evals)]
pub(crate) fn sandbox(cwd: &Path, writable: &[&Path], readonly: &[&Path], timeout_s: u32) -> (Command, PathBuf) {
    sandbox_with(cwd, writable, readonly, &Extra::default(), timeout_s)
}

/// What some arms add to the plain sandbox.
#[derive(Default)]
pub(crate) struct Extra {
    /// Read-only binds into the hidden HOME: (source, destination relative to HOME), e.g. an
    /// installed skill at `.claude/skills/<name>`.
    pub home_ro: Vec<(PathBuf, PathBuf)>,
    /// `--setting-sources` for claude (default "project"); user skills load only with "user".
    pub setting_sources: Option<String>,
    /// A persistent HOME of its own (dogfood.rs): bound read-write at its own path and exported as
    /// `$HOME`, so Claude Code reads its *user* settings (hooks) from there and borg resolves its
    /// store and config under it. The real HOME stays hidden (tmpfs) and its credentials are not
    /// bound: the caller keeps a copy outside this HOME and binds it in with `file_rw` for the
    /// sandbox's lifetime only (`dogfood::Secrets`), so no token is left in the persistent HOME and
    /// nothing is written back. The environment is cleared down to `SANDBOX_HOME_ENV`, so no harness
    /// or borg variable of the caller (`CLAUDE_*`, `BORG_*`, `XDG_*`) reaches inside.
    pub sandbox_home: Option<PathBuf>,
    /// Read-write file binds (source, absolute destination), mounted last: e.g. a credentials copy
    /// kept outside `sandbox_home` and visible inside it only while the sandbox runs.
    pub file_rw: Vec<(PathBuf, PathBuf)>,
}

/// The only variables a sandbox with its own HOME inherits (besides HOME and PATH, which it sets).
pub(crate) const SANDBOX_HOME_ENV: [&str; 4] = ["LANG", "LC_ALL", "TERM", "USER"];

pub(crate) fn sandbox_with(cwd: &Path, writable: &[&Path], readonly: &[&Path], extra: &Extra, timeout_s: u32) -> (Command, PathBuf) {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let private = std::env::temp_dir().join(format!(
        "borg-eval-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(&private).unwrap();
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout_s.to_string()).arg("bwrap");
    // If `timeout` dies (its caller was killed, see `sandbox_exec`), the drone dies with it.
    cmd.arg("--die-with-parent");
    cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"]);
    cmd.arg("--tmpfs").arg(&home);
    for ro in [home.join(".local/share/claude"), home.join(".local/bin")] {
        cmd.arg("--ro-bind").arg(&ro).arg(&ro);
    }
    if let Some(own) = &extra.sandbox_home {
        cmd.arg("--bind").arg(own).arg(own);
        cmd.arg("--clearenv");
        for k in SANDBOX_HOME_ENV {
            if let Some(v) = std::env::var_os(k) {
                cmd.arg("--setenv").arg(k).arg(v);
            }
        }
        cmd.arg("--setenv").arg("HOME").arg(own);
        // `<own home>/bin` first (dogfood links borg there), then the claude binary.
        cmd.arg("--setenv").arg("PATH").arg(format!("{}:{}:/usr/local/bin:/usr/bin:/bin", own.join("bin").display(), home.join(".local/bin").display()));
        cmd.args(["--setenv", "DISABLE_AUTOUPDATER", "1"]);
    } else {
        let mut cfg: Value = fs::read_to_string(home.join(".claude.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        if let Some(o) = cfg.as_object_mut() {
            o.remove("projects");
            o.remove("mcpServers");
        }
        fs::write(private.join("claude.json"), cfg.to_string()).unwrap();
        cmd.arg("--dir").arg(home.join(".claude"));
        let creds = home.join(".claude/.credentials.json");
        cmd.arg("--bind").arg(&creds).arg(&creds);
        cmd.arg("--bind").arg(private.join("claude.json")).arg(home.join(".claude.json"));
    }
    for (src, rel) in &extra.home_ro {
        cmd.arg("--ro-bind").arg(src).arg(home.join(rel));
    }
    if !writable.iter().any(|w| cwd.starts_with(w)) {
        cmd.arg("--ro-bind").arg(cwd).arg(cwd);
    }
    for ro in readonly.iter().filter(|p| p.exists()) {
        cmd.arg("--ro-bind").arg(ro).arg(ro);
    }
    for rw in writable.iter().filter(|p| p.exists()) {
        cmd.arg("--bind").arg(rw).arg(rw);
    }
    for (src, dst) in &extra.file_rw {
        cmd.arg("--bind").arg(src).arg(dst);
    }
    cmd.arg("--chdir").arg(cwd).arg("--");
    (cmd, private)
}

/// Run `claude -p` in the eval sandbox. Streams JSON to `stream`; returns the final
/// `result` message and wall time.
pub(crate) fn bwrap_claude(cwd: &Path, writable: &[&Path], readonly: &[&Path], claude_args: &[String], stream: &Path, timeout_s: u32) -> (Option<Value>, f64) {
    bwrap_claude_with(cwd, writable, readonly, &Extra::default(), claude_args, stream, timeout_s)
}

pub(crate) fn bwrap_claude_with(
    cwd: &Path,
    writable: &[&Path],
    readonly: &[&Path],
    extra: &Extra,
    claude_args: &[String],
    stream: &Path,
    timeout_s: u32,
) -> (Option<Value>, f64) {
    let (mut cmd, private) = sandbox_with(cwd, writable, readonly, extra, timeout_s);
    cmd.arg("claude").args(claude_args);
    cmd.args(["--output-format", "stream-json", "--verbose"]);
    cmd.args(["--setting-sources", extra.setting_sources.as_deref().unwrap_or("project"), "--strict-mcp-config", "--no-session-persistence"]);
    let started = Instant::now();
    let out = fs::File::create(stream).unwrap();
    let status = cmd.stdout(out).stderr(Stdio::null()).status().expect("spawn bwrap/claude");
    let secs = started.elapsed().as_secs_f64();
    if !status.success() {
        eprintln!("    claude exited with {status}");
    }
    let _ = fs::remove_dir_all(&private);
    let result = fs::read_to_string(stream)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .rfind(|v| v.get("type").and_then(Value::as_str) == Some("result"));
    (result, secs)
}

fn run_drone(a: &Args, work: &Path, prompt: &str, stream: &Path) -> (Option<Value>, f64) {
    let mut args: Vec<String> = ["-p", prompt, "--model", &a.model, "--max-budget-usd", &a.budget_usd].map(String::from).to_vec();
    args.extend(["--permission-mode", "bypassPermissions", "--append-system-prompt", SYSTEM].map(String::from));
    if let Some(s) = &a.settings {
        args.extend(["--settings".to_string(), s.display().to_string()]);
    }
    bwrap_claude(work, &[work], &[], &args, stream, a.timeout_s)
}

/// `borg-eval report <results.jsonl>...`: per-arm, per-task table.
/// `borg-eval sandbox-exec <cwd> [--rw P]... [--ro P]... [--timeout S] -- argv...`: run any
/// command in the eval sandbox (e.g. a memory system's warm-up hook), inheriting the environment.
fn sandbox_exec(raw: &[String]) {
    let cwd = PathBuf::from(raw.first().expect("sandbox-exec <cwd> ... -- argv"));
    let (mut rw, mut ro, mut timeout) = (Vec::new(), Vec::new(), 1800u32);
    let mut extra = Extra::default();
    let mut it = raw[1..].iter();
    let mut argv = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rw" => rw.push(PathBuf::from(it.next().unwrap())),
            // `--bind SRC DST`: SRC read-write at DST (e.g. a per-run ~/.claude/projects, so a later call
            // can --resume the session while the real one stays hidden).
            "--bind" => {
                let src = PathBuf::from(it.next().unwrap());
                extra.file_rw.push((src, PathBuf::from(it.next().unwrap())));
            }
            "--ro" => ro.push(PathBuf::from(it.next().unwrap())),
            "--timeout" => timeout = it.next().unwrap().parse().unwrap(),
            "--" => {
                argv = it.cloned().collect();
                break;
            }
            other => panic!("unknown flag {other}"),
        }
    }
    let rw: Vec<&Path> = rw.iter().map(PathBuf::as_path).collect();
    let ro: Vec<&Path> = ro.iter().map(PathBuf::as_path).collect();
    let (mut cmd, private) = sandbox_with(&cwd, &rw, &ro, &extra, timeout);
    // A caller that kills us on its own timeout (the handbook runner SIGKILLs its claude) must not
    // leave the sandboxed drone running: `timeout` gets SIGKILL when we die, and bwrap
    // (--die-with-parent) takes the sandbox down with it.
    // SAFETY: prctl is async-signal-safe and touches no memory of the parent.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let status = cmd.args(&argv).status().expect("spawn sandbox");
    let _ = fs::remove_dir_all(&private);
    std::process::exit(status.code().unwrap_or(1));
}

/// `borg-eval sandbox-probe <cwd>`: a cheap model lists what a drone can see from the
/// sandbox, to prove that nothing past the decision point is reachable.
fn sandbox_probe(cwd: PathBuf) {
    // The engineer's real HOME: its repos must not be visible from inside.
    let home = std::env::var("HOME").unwrap_or_default();
    let probe = format!(
        "Run exactly this one Bash command and reply with its full output verbatim, nothing else: \
ls -A ~ ; ls {home}/* 2>&1 | head; ls ~/.claude ~/.local/share/borg 2>&1; \
git log --all --oneline | wc -l; git branch -a; git remote -v; git stash list; gh auth status 2>&1 | head -3; \
ls ~/.ssh ~/.config 2>&1"
    );
    let stream = std::env::temp_dir().join("borg-sandbox-probe.jsonl");
    let args: Vec<String> =
        ["-p", probe.as_str(), "--model", "haiku", "--permission-mode", "bypassPermissions", "--max-budget-usd", "0.3"].map(String::from).to_vec();
    let (result, _) = bwrap_claude(&cwd, &[], &[], &args, &stream, 300);
    println!("{}", result.and_then(|r| r["result"].as_str().map(str::to_string)).unwrap_or_else(|| "no result (see stream)".into()));
}

fn report(files: &[String]) {
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Agg {
        n: u32,
        solved: u32,
        cost: f64,
        explore: u64,
        failed_bash: u32,
        turns: u64,
    }
    let mut by: BTreeMap<(String, u64), Agg> = BTreeMap::new();
    for f in files {
        for line in fs::read_to_string(f).unwrap_or_default().lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let key = (v["arm"].as_str().unwrap_or("?").to_string(), v["task"].as_u64().unwrap_or(0));
            let g = by.entry(key).or_default();
            g.n += 1;
            g.solved += v["solved"].as_bool().unwrap_or(false) as u32;
            g.cost += v["cost_usd"].as_f64().unwrap_or(0.0);
            g.explore += v["explore_tokens"].as_u64().unwrap_or(0);
            g.failed_bash += v["failed_bash"].as_u64().unwrap_or(0) as u32;
            g.turns += v["turns"].as_u64().unwrap_or(0);
        }
    }
    println!("| Arm | Task | Runs | Solved | Mean cost | Mean turns | Mean explore tok | Failed cmds/run |");
    println!("|---|---|---|---|---|---|---|---|");
    let mut arms: BTreeMap<String, (u32, f64)> = BTreeMap::new();
    for ((arm, task), g) in &by {
        let n = g.n.max(1) as f64;
        println!(
            "| {arm} | {task:02} | {} | {}/{} | ${:.3} | {:.1} | {:.0} | {:.2} |",
            g.n,
            g.solved,
            g.n,
            g.cost / n,
            g.turns as f64 / n,
            g.explore as f64 / n,
            g.failed_bash as f64 / n
        );
        let a = arms.entry(arm.clone()).or_default();
        a.0 += g.solved;
        a.1 += g.cost;
    }
    println!();
    for (arm, (solved, cost)) in arms {
        println!("**{arm}**: {solved} solved, total ${cost:.2}, cost per solved task ${:.3}", cost / solved.max(1) as f64);
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match raw.first().map(String::as_str) {
        Some("report") => return report(&raw[1..]),
        #[cfg(replay_evals)]
        Some("preempt") => return preempt::main(&raw[1..]),
        #[cfg(replay_evals)]
        Some("queen-replay") => return queen_replay::main(&raw[1..]),
        #[cfg(replay_evals)]
        Some("queen-test") => return queen_test::main(&raw[1..]),
        Some("sandbox-exec") => return sandbox_exec(&raw[1..]),
        #[cfg(replay_evals)]
        Some("expert-hook") => return preempt::expert_hook(),
        Some("dogfood") => return dogfood::main(&raw[1..]),
        Some("sandbox-probe") => return sandbox_probe(raw.get(1).map(PathBuf::from).expect("sandbox-probe <cwd>")),
        _ => {}
    }
    let a = args();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out_dir = root.join("target/eval-runs").join(&a.run);
    fs::create_dir_all(&out_dir).unwrap();
    let mut results = fs::OpenOptions::new().create(true).append(true).open(out_dir.join("results.jsonl")).unwrap();
    let tasks: Vec<PathBuf> = sorted_files(&a.workload.join("tasks"), "").into_iter().take(a.tasks).collect();
    let hidden = a.workload.join("hidden");
    let drift = sorted_files(&a.workload.join("drift"), "before-");
    eprintln!("run {} · arm {} · {} tasks × {} seeds · {}", a.run, a.arm, tasks.len(), a.seeds, out_dir.display());

    for seed in 0..a.seeds {
        let work = out_dir.join(format!("s{seed}/repo"));
        fresh_repo(&a.workload, &work);
        for (i, task) in tasks.iter().enumerate() {
            let n = format!("{:02}", i + 1);
            for d in drift.iter().filter(|d| d.file_name().unwrap().to_string_lossy().starts_with(&format!("before-{n}"))) {
                let (ok, out) = sh(&work, &format!("sh '{}'", d.display()));
                eprintln!("  drift {} → {}", d.file_name().unwrap().to_string_lossy(), if ok { "applied" } else { out.trim() });
            }
            let prompt = fs::read_to_string(task).unwrap();
            let stream = out_dir.join(format!("s{seed}/task{n}.stream.jsonl"));
            eprintln!("  seed {seed} task {n} …");
            let (result, secs) = run_drone(&a, &work, &prompt, &stream);

            // Grade: hidden tests for this task plus invariants and all earlier tasks.
            let mut tests = vec![hidden.join("test_invariants.py")];
            tests.extend((1..=i + 1).map(|k| hidden.join(format!("test_{k:02}.py"))).filter(|p| p.exists()));
            let grade = a.workload.join("grade.sh");
            let list: Vec<String> = tests.iter().map(|p| format!("'{}'", p.display())).collect();
            let (passed, grade_out) = sh(&work, &format!("'{}' '{}' {}", grade.display(), work.display(), list.join(" ")));
            let (own_passed, _) = sh(&work, &format!("'{}' '{}' '{}'", grade.display(), work.display(), hidden.join(format!("test_{n}.py")).display()));

            let sess = claude::parse(&stream).ok();
            let (explore, failed_bash, reads) = sess.as_ref().map_or((0, 0, 0), |s| {
                s.events.iter().fold((0u64, 0u32, 0u32), |acc, e| match &e.kind {
                    Kind::Read { .. } => (acc.0 + e.result_tokens, acc.1, acc.2 + 1),
                    Kind::Search => (acc.0 + e.result_tokens, acc.1, acc.2),
                    Kind::Bash { failed, .. } => (acc.0 + e.result_tokens, acc.1 + *failed as u32, acc.2),
                    _ => acc,
                })
            });
            let r = result.unwrap_or(Value::Null);
            let rec = json!({
                "run": a.run, "arm": a.arm, "model": a.model, "seed": seed, "task": i + 1,
                "solved": own_passed, "all_passed": passed,
                "cost_usd": r.get("total_cost_usd").and_then(Value::as_f64),
                "turns": r.get("num_turns"), "is_error": r.get("is_error"),
                "usage": r.get("usage"), "wall_s": secs,
                "explore_tokens": explore, "failed_bash": failed_bash, "reads": reads,
                "grade_tail": grade_out.lines().rev().take(3).collect::<Vec<_>>(),
            });
            writeln!(results, "{rec}").unwrap();
            eprintln!(
                "    solved={own_passed} all={passed} cost=${:.3} turns={} explore={explore} failed_bash={failed_bash} {:.0}s",
                rec["cost_usd"].as_f64().unwrap_or(0.0),
                rec["turns"],
                secs
            );

            let (ok, out) = sh(&work, "git add -A && git commit -qm task --allow-empty && git clean -fdxq");
            assert!(ok, "commit/clean: {out}");
        }
    }
    eprintln!("results: {}", out_dir.join("results.jsonl").display());
}
