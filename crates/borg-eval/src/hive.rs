//! `borg-eval hive`: the hive pilot's harness (docs/design/hive.md "The hive against one session" and
//! C2–C3; the pre-registered rule is in tasks/todo.md "Hive pilot"). It measures the rule, it does not
//! change it: arms A, B, D3 and F, 3 seeds per cell; H1 holds on a workload iff D3's median wall-clock
//! ≤ 0.8 × B's AND D3's mean hidden solve share ≥ B's − 1/K.
//!
//! ```text
//! borg-eval hive --workload DIR --arm a|b|d3|e|f --seed N --cap-usd X
//!                [--model sonnet] [--timeout-s 3600] [--run NAME] [--stop-at-usd Y]
//!                [--borg BIN] [--claude BIN] [--harness claude|sim]
//!                [--learn frozen|l0|l01] [--chain DIR] [--goal ID] [--position N]
//! borg-eval hive validate --workload DIR [--timeout-s 3600]
//! borg-eval hive promote --chain DIR --after GOAL
//! borg-eval hive report <results.jsonl>...
//! ```
//!
//! A goal sequence (hive_seq.rs; tasks/todo.md "H5"): `--learn` picks D3's learning arm, `--chain` keeps one
//! store across the goals of an (arm, seed) chain, `--goal` names the hive goal (default `pilot`) and
//! `--position` is recorded. Such a row's `arm` is the learning arm; it carries a `seq` object (borg's own
//! model dollars, mistakes, lessons and playbook counts, $ per solve) and `hive report` adds the H5 block.
//!
//! A workload (`<W>/workload.json`, kept under `~/.local/share/borg/eval/hive-pilot/`, outside every
//! repo and sandbox) names source repos at pinned commits, the visible task text and check, and the
//! hidden checks (K of them) in a directory no agent sandbox ever sees. Each cell builds its workspace
//! from `git archive` of each source (read-only on the source, no history), one commit `base`; every
//! arm gets the same workspace, model, dollar cap, wall cap and sandbox HOME (dogfood.rs
//! `prepare_home_with`: a fresh store, no lessons, the real HOME hidden).
//!
//! - A: one `claude -p` session, subagents disallowed. B: one session invited to use subagents.
//! - F: three A sessions in parallel at cap/3 each, the best chosen by the visible check (then cost,
//!   then index); wall = the slowest candidate + the selection.
//! - D3: `borg hive init` + `borg hive run --drones 3` inside the sandbox (no hidden oracle there),
//!   under the same wall cap as one A/B session, its drones' `claude` given the same appended system
//!   prompt. The hive integrates (borg hive/integrate.rs: the accepted attempts merged, the oracle run
//!   on the combined tree, a fix task on a failure); the harness scores `refs/heads/hive/pilot/integrated`
//!   as the board names it (the base when no integration merged anything); wall = the run.
//! - E: D3 across two nodes on this machine (hive/arm_e.rs; tasks/todo.md "Arm E on one machine"): two
//!   stores of one collective, each node's workspace a clone of one bare remote with its own objects,
//!   synced by borg-mesh over loopback while `borg hive run` runs on both (2 drones on A, 1 on B).
//!
//! Every agent sandbox and the scorer have a PID namespace of their own: nothing started inside (a
//! Gradle daemon) outlives them; /run is masked in both (no Docker daemon, agent or bus socket). A
//! workload whose secrets (it, its hidden checks, its reference) or `runs` lie outside HOME and /tmp,
//! the paths the agent sandbox hides, is refused. Dollars are metered alike for every arm (`Cost`):
//! `charged_usd` is what the sessions reported plus, for each one cut off without a report, the
//! budget it was given.
//!
//! Scoring runs after the arm, in its own sandbox (no network, no credentials, git hooks off, an empty
//! HOME per step) on a fresh clone of the result commit, the check scripts read-only: the tests the
//! result weakened or the CI and check files it changed, read off the clone before any check runs
//! the result's code in it (`verdict::protected` on the base-to-result diff, scoped by the workload's
//! task `files`, the same for every arm: `hacked`, with why in `hacked_why`; a diff git cannot make
//! errs the cell), then the hidden checks (solve share) and the visible check. One row per cell goes
//! to `target/eval-runs/<run>/results.jsonl`, an errored one with its `error_stage`; `hive report`
//! prints the H1 and kill lines.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use borg::hive::{board, goal, guard, run as hive_run, verdict};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::dogfood::{Secrets, prepare_home_bare, prepare_home_with};
use crate::hive_seq::{self, Learn};
use crate::{Extra, sandbox_with};

mod arm_e;

/// The default hive goal (`--goal`), and the dummy goal id `visible_files` builds.
const GOAL: &str = "pilot";
const DRONES: usize = 3;
/// The sandbox HOME's collectives config: the workspace (`~/work/<W>`) routes to one local collective.
const COLLECTIVES: &str = r#"{"default":"off","collectives":[{"name":"pilot","paths":["~/work"],"share":"local"}]}"#;
/// B's invitation, appended to the prompt.
const INVITE: &str = "Split the work into independent parts and run them in parallel with subagents (the Task tool); \
integrate their results into this working tree before you finish.";
/// Every model call of borg off inside an agent sandbox: no learning, no queen, no prediction model.
const BORG_OFF: [(&str, &str); 10] = [
    ("BORG_LEARN", "off"),
    ("BORG_LEARN_MODEL", "off"),
    ("BORG_QUEEN", "off"),
    ("BORG_PREDICT_MODEL", "off"),
    ("BORG_VECTORS", "off"),
    ("BORG_MATCH", "bm25"),
    ("BORG_STOP_ATTENTION", "off"),
    ("BORG_MODE", "hook"),
    ("BORG_REGRESS", "off"),
    ("BORG_NOTIFY", "off"),
];
/// HOME-relative paths a workload may never bind into a sandbox: credentials, borg's own stores and
/// config (real lessons, the eval data with its hidden oracles).
const HOME_DENY: [&str; 14] = [
    ".claude",
    ".claude.json",
    ".local/share/borg",
    ".config/borg",
    ".ssh",
    ".gnupg",
    ".config/gh",
    ".git-credentials",
    ".netrc",
    ".gradle/gradle.properties",
    ".m2/settings.xml",
    ".docker",
    ".aws",
    ".local/share/claude",
];
/// git with nothing a repo's own config could run (hooks, fsmonitor, signing) and the eval identity.
const GIT: &str = "git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c commit.gpgsign=false -c user.name=eval -c user.email=eval@borg";
/// The tail of a check's output kept in the row's score files.
const TAIL: usize = 2000;
/// What `build_workspace` allows each `git archive` and `tar`.
const ARCHIVE_TIMEOUT_S: u32 = 600;

// ------------------------------------------------------------------------------------- workload

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkload {
    id: String,
    shape: String,
    repos: Vec<RawRepo>,
    #[serde(default)]
    inject: Option<String>,
    spec: String,
    visible: String,
    #[serde(default = "default_visible_timeout")]
    visible_timeout_s: u32,
    hidden: RawHidden,
    #[serde(default)]
    reference: Option<String>,
    hive: RawHive,
    #[serde(default)]
    home_ro: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    sim: Option<Value>,
    /// The workspace's directory name (default: the id). Goals of one family share it: borg keys playbooks
    /// and lesson scopes by the workspace's basename.
    #[serde(default)]
    ws: Option<String>,
}

fn default_visible_timeout() -> u32 {
    600
}

fn default_check_timeout() -> u32 {
    900
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    src: String,
    commit: String,
    dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHidden {
    dir: String,
    checks: Vec<HiddenCheck>,
    /// When set, validation also requires every check to exit with exactly this code on the base: the
    /// workload's way of telling a failed assertion from a check that could not run (a compile error).
    #[serde(default)]
    base_fail_rc: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HiddenCheck {
    id: String,
    cmd: String,
    #[serde(default = "default_check_timeout")]
    timeout_s: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHive {
    tasks: Vec<String>,
}

#[derive(Debug, Clone)]
struct Repo {
    src: PathBuf,
    commit: String,
    dir: String,
}

/// A parsed, checked workload.
#[derive(Debug, Clone)]
struct Workload {
    id: String,
    shape: String,
    repos: Vec<Repo>,
    inject: Option<PathBuf>,
    spec_path: PathBuf,
    spec: String,
    visible: String,
    visible_timeout_s: u32,
    hidden_dir: PathBuf,
    checks: Vec<HiddenCheck>,
    reference: Option<PathBuf>,
    tasks: Vec<String>,
    /// HOME-relative, bound read-only at the same place under a sandbox HOME.
    home_ro: Vec<String>,
    /// Values may name `$HOME`: the sandbox HOME.
    env: Vec<(String, String)>,
    sim: Option<Value>,
    /// The workspace's directory name.
    ws: String,
    base_fail_rc: Option<i32>,
    /// blake3 of workload.json and every file it names (spec, inject, reference, the hidden dir).
    sha: String,
}

impl Workload {
    fn k(&self) -> usize {
        self.checks.len()
    }

    /// The union of the tasks' `files` (`ID|role|title|files,..`): the test and CI files the workload
    /// lets any arm change, so every arm's `hacked` is judged alike.
    fn scope(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .tasks
            .iter()
            .filter_map(|t| t.split('|').nth(3))
            .flat_map(|f| f.split(','))
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(String::from)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The task text every arm gets (D3's goal spec too).
    fn prompt(&self) -> String {
        format!("{}\n\nVisible check: run `{}` from the workspace root.", self.spec.trim_end(), self.visible)
    }

    /// `env` with `$HOME` as `home`.
    fn env_for(&self, home: &Path) -> Vec<(String, String)> {
        env_in(&self.env, home)
    }

    /// (resolved source under the real HOME, HOME-relative destination) of each `home_ro` that exists:
    /// the bound source is the path `check_home_ro` checked, symlinks resolved.
    fn home_binds(&self, real_home: &Path) -> Vec<(PathBuf, PathBuf)> {
        self.home_ro.iter().filter_map(|r| fs::canonicalize(real_home.join(r)).ok().map(|src| (src, PathBuf::from(r)))).collect()
    }
}

/// `env` with `$HOME` read as `home`.
fn env_in(env: &[(String, String)], home: &Path) -> Vec<(String, String)> {
    env.iter().map(|(k, v)| (k.clone(), v.replace("$HOME", &home.display().to_string()))).collect()
}

fn expand_home(p: &str, real_home: &Path) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => real_home.join(rest),
        None if p == "~" => real_home.to_path_buf(),
        None => PathBuf::from(p),
    }
}

/// `p` lies under (or is) `root`, lexically after canonicalising both where they exist.
fn under(p: &Path, root: &Path) -> bool {
    let canon = |x: &Path| fs::canonicalize(x).unwrap_or_else(|_| x.to_path_buf());
    canon(p).starts_with(canon(root))
}

fn valid_task_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn plain_list(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/,*".contains(&b))
}

/// `ID|role|title[|files,..[|deps,..]]` as `borg hive init --task` takes it. A title cannot hold a
/// `|`: the field after it would read as a file list, so one with spaces is refused as that.
fn check_task(s: &str) -> Result<(), String> {
    let f: Vec<&str> = s.split('|').map(str::trim).collect();
    if !(3..=5).contains(&f.len()) {
        return Err(format!("task {s:?}: give ID|role|title[|files,..[|deps,..]] (no `|` inside the title)"));
    }
    if !valid_task_id(f[0]) {
        return Err(format!("task {s:?}: bad id {:?} (letters, digits, '.', '_', '-', at most 40)", f[0]));
    }
    if !["plan", "work", "verify", "integrate"].contains(&f[1]) {
        return Err(format!("task {s:?}: role is plan, work, verify or integrate"));
    }
    if f[2].is_empty() {
        return Err(format!("task {s:?}: empty title"));
    }
    if f.len() > 3 && !f[3..].iter().all(|x| plain_list(x)) {
        return Err(format!("task {s:?}: a `|` inside the title (files and deps are plain comma lists)"));
    }
    if let Some(deps) = f.get(4)
        && !deps.split(',').filter(|d| !d.is_empty()).all(valid_task_id)
    {
        return Err(format!("task {s:?}: bad dependency id"));
    }
    Ok(())
}

/// The workload's tasks as a plan: every dep names one of them, and none is on a cycle (`borg hive
/// init` refuses either; the board could never finish). Each task passed `check_task`.
fn check_deps(tasks: &[String]) -> Result<(), String> {
    let parsed: Vec<(&str, Vec<&str>)> = tasks
        .iter()
        .map(|t| {
            let f: Vec<&str> = t.split('|').map(str::trim).collect();
            (f[0], f.get(4).map(|d| d.split(',').map(str::trim).filter(|d| !d.is_empty()).collect()).unwrap_or_default())
        })
        .collect();
    for (id, deps) in &parsed {
        if let Some(d) = deps.iter().find(|d| !parsed.iter().any(|(o, _)| o == *d)) {
            return Err(format!("task {id}: its dep {d} names no task of the workload"));
        }
    }
    let mut placed: Vec<&str> = Vec::new();
    while let Some((id, _)) = parsed.iter().find(|(id, deps)| !placed.contains(id) && deps.iter().all(|d| placed.contains(d))) {
        placed.push(id);
    }
    let cycle: Vec<&str> = parsed.iter().map(|(id, _)| *id).filter(|id| !placed.contains(id)).collect();
    match cycle.is_empty() {
        true => Ok(()),
        false => Err(format!("tasks {} depend on each other in a cycle", cycle.join(", "))),
    }
}

/// A `home_ro` entry: HOME-relative, plain, and neither a credential or borg store nor holding one,
/// nor a source repo, nor a `secret` (the workload, its hidden checks and reference, where workspaces
/// are built). Checked on the path as written AND on where it resolves (bwrap follows symlinks: a
/// link to the hidden dir would bind it).
fn check_home_ro(rel: &str, real_home: &Path, srcs: &[PathBuf], secret: &[&Path]) -> Result<(), String> {
    let p = Path::new(rel);
    if rel.is_empty() || p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("home_ro {rel:?}: give a plain path relative to HOME"));
    }
    for d in HOME_DENY {
        let d = Path::new(d);
        if p.starts_with(d) || d.starts_with(p) {
            return Err(format!("home_ro {rel:?}: overlaps {} (credentials or borg's stores)", d.display()));
        }
    }
    let abs = real_home.join(p);
    let resolved = fs::canonicalize(&abs).unwrap_or_else(|_| abs.clone());
    let deny = HOME_DENY.iter().map(|d| real_home.join(d));
    for s in srcs.iter().cloned().chain(secret.iter().map(|p| p.to_path_buf())).chain(deny) {
        for a in [&abs, &resolved] {
            if under(a, &s) || under(&s, a) {
                return Err(format!("home_ro {rel:?}: {} overlaps {}", a.display(), s.display()));
            }
        }
    }
    Ok(())
}

fn hash_tree(h: &mut blake3::Hasher, root: &Path, dir: &Path) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let Ok(m) = fs::symlink_metadata(&p) else { continue };
        if m.is_dir() {
            hash_tree(h, root, &p);
        } else if m.is_file() {
            h.update(p.strip_prefix(root).unwrap_or(&p).to_string_lossy().as_bytes());
            h.update(b"\0");
            h.update(&fs::read(&p).unwrap_or_default());
            h.update(b"\0");
        }
    }
}

/// Reads and checks `<dir>/workload.json`. `runs` is where every workspace is built
/// (`target/eval-runs`): the hidden checks may not lie there or in a source.
fn load_workload(dir: &Path, real_home: &Path, runs: &Path) -> Result<Workload, String> {
    let dir = fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let bytes = fs::read(dir.join("workload.json")).map_err(|e| format!("workload.json: {e}"))?;
    let raw: RawWorkload = serde_json::from_slice(&bytes).map_err(|e| format!("workload.json: {e}"))?;
    let wl = parse_workload(raw, &dir, real_home, runs)?;
    let mut h = blake3::Hasher::new();
    h.update(&bytes);
    for f in [Some(wl.spec_path.clone()), wl.inject.clone(), wl.reference.clone()].into_iter().flatten() {
        h.update(&fs::read(f).unwrap_or_default());
        h.update(b"\0");
    }
    hash_tree(&mut h, &wl.hidden_dir, &wl.hidden_dir);
    Ok(Workload { sha: h.finalize().to_hex().to_string(), ..wl })
}

fn parse_workload(raw: RawWorkload, dir: &Path, real_home: &Path, runs: &Path) -> Result<Workload, String> {
    if raw.id.is_empty() || raw.id.starts_with('.') || !raw.id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
        return Err(format!("id {:?}: letters, digits, '.', '_', '-'", raw.id));
    }
    if !["parallel", "cross-repo"].contains(&raw.shape.as_str()) {
        return Err(format!("shape {:?}: parallel or cross-repo", raw.shape));
    }
    if raw.repos.is_empty() {
        return Err("repos: at least one".into());
    }
    let mut repos = Vec::new();
    let mut dirs = BTreeSet::new();
    for r in raw.repos {
        if r.commit.len() != 40 || !r.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("repo {}: commit {:?} is not a 40-hex sha", r.src, r.commit));
        }
        let plain = !r.dir.is_empty() && !r.dir.starts_with('.') && r.dir.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !plain || !dirs.insert(r.dir.clone()) {
            return Err(format!("repo dir {:?}: one plain, unique path component", r.dir));
        }
        let src = expand_home(&r.src, real_home);
        let src = fs::canonicalize(&src).map_err(|e| format!("repo {}: {e}", src.display()))?;
        repos.push(Repo { src, commit: r.commit.to_lowercase(), dir: r.dir });
    }
    let srcs: Vec<PathBuf> = repos.iter().map(|r| r.src.clone()).collect();
    let rel = |p: &str| if Path::new(p).is_absolute() { PathBuf::from(p) } else { dir.join(p) };
    let hidden_dir = fs::canonicalize(rel(&raw.hidden.dir)).map_err(|e| format!("hidden dir {}: {e}", raw.hidden.dir))?;
    for s in srcs.iter().chain([&runs.to_path_buf()]) {
        if under(&hidden_dir, s) || under(s, &hidden_dir) {
            return Err(format!("hidden dir {} overlaps {}: it must stay outside every workspace and source", hidden_dir.display(), s.display()));
        }
    }
    if raw.hidden.checks.is_empty() {
        return Err("hidden.checks: at least one (K)".into());
    }
    let mut ids = BTreeSet::new();
    for c in &raw.hidden.checks {
        if !valid_task_id(&c.id) || !ids.insert(c.id.clone()) || c.cmd.trim().is_empty() || c.timeout_s == 0 {
            return Err(format!("hidden check {:?}: a unique id, a command and a timeout above 0", c.id));
        }
    }
    let file = |p: &Option<String>, what: &str| -> Result<Option<PathBuf>, String> {
        p.as_deref().map(|p| fs::canonicalize(rel(p)).map_err(|e| format!("{what} {p}: {e}"))).transpose()
    };
    let inject = file(&raw.inject, "inject")?;
    let reference = file(&raw.reference, "reference")?;
    // The agent sandbox hides HOME and /tmp only (main.rs `sandbox_with`): what tells or holds the
    // answer (workload.json's check commands, the hidden checks, the reference solution, every cell
    // and validate tree under `runs`) is readable by every agent anywhere else.
    let secret: Vec<&Path> = [dir, hidden_dir.as_path(), runs].into_iter().chain(reference.as_deref()).collect();
    if let Some(p) = secret.iter().find(|p| !under(p, real_home) && !under(p, Path::new("/tmp"))) {
        return Err(format!("{} lies outside HOME and /tmp, the only paths the agent sandbox hides", p.display()));
    }
    let spec_path = rel(&raw.spec);
    let spec = fs::read_to_string(&spec_path).map_err(|e| format!("spec {}: {e}", raw.spec))?;
    if raw.visible.trim().is_empty() || raw.visible_timeout_s == 0 {
        return Err("visible: a command and a timeout above 0".into());
    }
    if raw.hive.tasks.is_empty() {
        return Err("hive.tasks: at least one".into());
    }
    for t in &raw.hive.tasks {
        check_task(t)?;
    }
    check_deps(&raw.hive.tasks)?;
    for r in &raw.home_ro {
        check_home_ro(r, real_home, &srcs, &secret)?;
    }
    let ws = raw.ws.unwrap_or_else(|| raw.id.clone());
    if ws.is_empty() || ws.starts_with('.') || !ws.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
        return Err(format!("ws {ws:?}: one plain path component"));
    }
    let mut env = Vec::new();
    for (k, v) in raw.env {
        let name = !k.is_empty() && k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !name || ["HOME", "PATH", "LD_PRELOAD", "LD_LIBRARY_PATH"].contains(&k.as_str()) || k.starts_with("BORG_") || k.starts_with("CLAUDE") {
            return Err(format!("env {k:?}: not settable by a workload"));
        }
        env.push((k, v));
    }
    Ok(Workload {
        id: raw.id,
        shape: raw.shape,
        repos,
        inject,
        spec_path,
        spec,
        visible: raw.visible,
        visible_timeout_s: raw.visible_timeout_s,
        hidden_dir,
        checks: raw.hidden.checks,
        base_fail_rc: raw.hidden.base_fail_rc,
        reference,
        tasks: raw.hive.tasks,
        home_ro: raw.home_ro,
        env,
        sim: raw.sim,
        ws,
        sha: String::new(),
    })
}

// ------------------------------------------------------------------------------------- process

/// Single-quoted for `sh`.
fn q(s: impl AsRef<str>) -> String {
    format!("'{}'", s.as_ref().replace('\'', r"'\''"))
}

/// `sh -c cmd` in `dir` under `timeout`, with the eval git identity: (success, exit code, output).
fn run_t(dir: &Path, cmd: &str, timeout_s: u32) -> (bool, i32, String) {
    // Bounded in-process, not by coreutils' `timeout` (macOS has none): the whole process group is
    // killed at the cap, and a cut-off run reports 124 as `timeout` did.
    let mut c = Command::new("sh");
    c.args(["-c", &format!("exec 2>&1\n{cmd}")])
        .current_dir(dir)
        .envs([("GIT_AUTHOR_NAME", "eval"), ("GIT_AUTHOR_EMAIL", "eval@borg"), ("GIT_COMMITTER_NAME", "eval"), ("GIT_COMMITTER_EMAIL", "eval@borg")])
        .env("GIT_TERMINAL_PROMPT", "0");
    match borg::runner::run_bounded(c, None, Duration::from_secs(u64::from(timeout_s))) {
        Some(b) => {
            let text = String::from_utf8_lossy(&b.out).into_owned();
            match b.status {
                Some(s) => (s.success(), s.code().unwrap_or(-1), text),
                None => (false, 124, text),
            }
        }
        None => (false, -1, "sh could not start".into()),
    }
}

/// Where post-arm scripts run: the scorer sandbox (no network, no credentials, an empty HOME of each
/// call's own), or, in unit tests only, a plain `sh`.
enum Jail {
    Sandbox {
        /// Where each call's HOME is made, and removed after it: what one step leaves in its HOME (the
        /// agent's .git config runs in the result commit's `git add`, a select's build) never reaches
        /// a later step's checks.
        homes: PathBuf,
        extra_ro: Vec<PathBuf>,
        /// The workload's variables, `$HOME` not yet resolved.
        env: Vec<(String, String)>,
        home_ro: Vec<(PathBuf, PathBuf)>,
    },
    #[cfg(all(test, target_os = "linux"))]
    Plain,
}

impl Jail {
    fn scorer(cell: &Path, wl: &Workload, real_home: &Path) -> Jail {
        let homes = cell.join("scorer-home");
        let _ = fs::create_dir_all(&homes);
        Jail::Sandbox { homes, extra_ro: Vec::new(), env: wl.env.clone(), home_ro: wl.home_binds(real_home) }
    }

    /// A HOME no call used before, under `homes`.
    fn fresh_home(homes: &Path) -> Result<PathBuf, String> {
        for n in 1u32.. {
            let h = homes.join(n.to_string());
            match fs::create_dir(&h) {
                Ok(()) => return Ok(h),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("{}: {e}", h.display())),
            }
        }
        Err(format!("{}: no fresh HOME", homes.display()))
    }

    fn extra(scorer_home: &Path, env: &[(String, String)], home_ro: &[(PathBuf, PathBuf)]) -> Extra {
        Extra {
            sandbox_home: Some(scorer_home.to_path_buf()),
            env: env.to_vec(),
            home_ro: home_ro.to_vec(),
            unshare_net: true,
            unshare_pid: true,
            ..Extra::default()
        }
    }

    /// The roots a call's checks may leave reports in besides the tree: the sandbox's own `/tmp`
    /// (a fresh tmpfs per call, so nothing of another call's). Plain shares the host's: none.
    fn scratch(&self) -> Vec<PathBuf> {
        match self {
            Jail::Sandbox { .. } => vec![PathBuf::from("/tmp")],
            #[cfg(all(test, target_os = "linux"))]
            Jail::Plain => Vec::new(),
        }
    }

    /// `script` in `cwd`, `rw` writable and `ro` read-only: (exit code, output).
    fn exec(&self, cwd: &Path, rw: &[&Path], ro: &[&Path], script: &str, timeout_s: u32) -> (i32, String) {
        match self {
            Jail::Sandbox { homes, extra_ro, env, home_ro } => {
                let home = match Self::fresh_home(homes) {
                    Ok(h) => h,
                    Err(e) => return (-1, e),
                };
                let mut ro: Vec<&Path> = ro.to_vec();
                ro.extend(extra_ro.iter().map(PathBuf::as_path));
                let (mut cmd, private) = sandbox_with(cwd, rw, &ro, &Self::extra(&home, &env_in(env, &home), home_ro), timeout_s);
                let out = cmd.args(["sh", "-c", script]).stdin(Stdio::null()).output();
                let _ = fs::remove_dir_all(private);
                // A build's caches may be read-only (Go's module cache).
                let _ = run_t(homes, &format!("chmod -R u+w {h} 2>/dev/null; rm -rf {h}", h = q(home.display().to_string())), 600);
                match out {
                    Ok(o) => (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
                    Err(e) => (-1, e.to_string()),
                }
            }
            #[cfg(all(test, target_os = "linux"))]
            Jail::Plain => {
                let (_, code, out) = run_t(cwd, script, timeout_s);
                (code, out)
            }
        }
    }
}

// ------------------------------------------------------------------------------------- workspace

/// The cell's workspace at `dst`: each source's tree at its pinned commit (`git archive`: read-only on
/// the source, no history, refs, remotes or reflog), the injected patch, one commit. Returns `base`.
fn build_workspace(wl: &Workload, dst: &Path) -> Result<String, String> {
    if dst.exists() {
        return Err(format!("{} exists", dst.display()));
    }
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let parent = dst.parent().unwrap_or(dst);
    for r in &wl.repos {
        let into = dst.join(&r.dir);
        fs::create_dir_all(&into).map_err(|e| e.to_string())?;
        let tar = parent.join(format!(".archive-{}-{}.tar", wl.id, r.dir));
        let cmd = format!(
            "git -C {} archive --format=tar -o {} {} && tar -x -f {} -C {}; rc=$?; rm -f {}; exit $rc",
            q(r.src.display().to_string()),
            q(tar.display().to_string()),
            q(&r.commit),
            q(tar.display().to_string()),
            q(into.display().to_string()),
            q(tar.display().to_string())
        );
        let (ok, _, out) = run_t(parent, &cmd, ARCHIVE_TIMEOUT_S);
        if !ok {
            return Err(format!("archive {}@{}: {}", r.src.display(), r.commit, out.trim()));
        }
    }
    let (ok, _, out) = run_t(dst, &format!("{GIT} init -q -b main"), 60);
    if !ok {
        return Err(format!("git init: {out}"));
    }
    if let Some(p) = &wl.inject {
        let (ok, _, out) = run_t(dst, &format!("{GIT} apply --whitespace=nowarn {}", q(p.display().to_string())), 120);
        if !ok {
            return Err(format!("inject {}: {out}", p.display()));
        }
    }
    let (ok, _, out) = run_t(dst, &format!("{GIT} add -A && {GIT} commit -q -m base && {GIT} rev-parse HEAD"), 600);
    if !ok {
        return Err(format!("base commit: {out}"));
    }
    Ok(out.trim().lines().last().unwrap_or_default().to_string())
}

/// `dst`: a `--no-local` clone of the bare repo `remote` (objects copied, not linked). Returns its HEAD.
fn clone_workspace(remote: &Path, dst: &Path) -> Result<String, String> {
    if dst.exists() {
        return Err(format!("{} exists", dst.display()));
    }
    let parent = dst.parent().unwrap_or(dst);
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let cmd = format!(
        "{GIT} clone -q --no-local {} {} && {GIT} -C {} rev-parse HEAD",
        q(remote.display().to_string()),
        q(dst.display().to_string()),
        q(dst.display().to_string())
    );
    let (ok, _, out) = run_t(parent, &cmd, ARCHIVE_TIMEOUT_S);
    if !ok {
        return Err(format!("clone {}: {}", remote.display(), out.trim()));
    }
    Ok(out.trim().lines().last().unwrap_or_default().to_string())
}

/// Index locks a session killed mid-`git` left (it is over: nothing else writes the repo now).
const STALE_LOCKS: &str = "find .git -name index.lock -type f -delete 2>/dev/null\n";

/// The A/B/F result: everything the agent left, one commit on top of whatever it did.
fn result_script() -> String {
    format!("set -u\n{STALE_LOCKS}{GIT} add -A && {GIT} commit -q --allow-empty -m result && echo \"HEAD $({GIT} rev-parse HEAD)\"\n")
}

/// The `HEAD <sha>` line a result script prints.
fn parse_head(out: &str) -> Option<String> {
    out.lines().filter_map(|l| l.strip_prefix("HEAD ")).map(str::trim).rfind(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(String::from)
}

/// What D3's board says of its integration.
#[derive(Debug, PartialEq)]
struct D3Result {
    /// What is scored: the passing integration's commit when the board is finished, else the newest
    /// integration's (a failed one's tree), else `base` (nothing was merged).
    head: String,
    integrated: bool,
    /// Integrate tasks opened.
    integrations: usize,
    /// Integrations rejected for a merge conflict.
    conflicts: usize,
}

fn d3_result(tasks: &Path, base: &str) -> D3Result {
    let events = board::read(tasks);
    let b = board::fold(&events);
    let integrate = |t: &str| b.tasks.get(t).is_some_and(|s| s.task.role == board::Role::Integrate);
    // The verdicts the fold took (a tenure they closed), newest last.
    let verdicts: Vec<&board::Event> = events
        .iter()
        .filter(|e| matches!(e.ev, board::Ev::Verified | board::Ev::Rejected) && integrate(&e.task))
        .filter(|e| b.spans.iter().any(|s| s.task == e.task && s.by == e.by && s.to_ms == Some(e.ms) && s.end == Some(e.ev)))
        .collect();
    let newest = verdicts.iter().filter(|e| !e.head.is_empty()).max_by_key(|e| e.ms).map(|e| e.head.clone());
    let head = match &b.integrated {
        Some(i) if b.finished() => i.head.clone(),
        _ => newest.unwrap_or_else(|| base.to_string()),
    };
    D3Result {
        head,
        integrated: b.finished(),
        integrations: b.tasks.values().filter(|s| s.task.role == board::Role::Integrate).count(),
        conflicts: verdicts.iter().filter(|e| e.ev == board::Ev::Rejected && e.note.starts_with("conflict")).count(),
    }
}

// ------------------------------------------------------------------------------------- scoring

#[derive(Debug, Clone, PartialEq)]
struct CheckResult {
    id: String,
    pass: bool,
    /// The check's exit code (-1 when it left none).
    rc: i32,
    timeout: bool,
    dur_ms: u64,
    tail: String,
}

#[derive(Debug, Default)]
struct Score {
    checks: Vec<CheckResult>,
    visible_pass: Option<bool>,
    hacked_paths: Vec<String>,
    /// Why each of `hacked_paths` is protected, in the same order.
    hacked_why: Vec<String>,
}

impl Score {
    fn passed(&self) -> usize {
        self.checks.iter().filter(|c| c.pass).count()
    }

    fn share(&self, k: usize) -> f64 {
        self.passed() as f64 / k.max(1) as f64
    }

    fn json(&self, k: usize) -> Value {
        json!({
            "checks": self.checks.iter().map(|c| json!({"id": c.id, "pass": c.pass, "timeout": c.timeout, "dur_ms": c.dur_ms})).collect::<Vec<_>>(),
            "passed": self.passed(),
            "solve_share": self.share(k),
        })
    }
}

/// The clone script: a fresh clone of `ws` at `head` in `<out>/tree` (no build output, untracked
/// file, replace ref or .git config of the agent's carries over).
fn clone_script(ws: &Path, head: &str, out: &Path) -> String {
    let (d, t) = (q(out.display().to_string()), q(out.join("tree").display().to_string()));
    format!(
        "set -u\nG=\"{GIT}\"\nrm -rf {t}\n$G clone -q --no-hardlinks {} {t} >{d}/clone.log 2>&1 && $G -C {t} checkout -q {} >>{d}/clone.log 2>&1 || {{ echo CLONE-FAILED; exit 3; }}\n",
        q(ws.display().to_string()),
        q(head)
    )
}

/// The checks script, on the clone in `<out>/tree`: each check from its own file under `checks`
/// (bound read-only: code the result's build runs during one check cannot rewrite a later one), from
/// the tree's root, with `BORG_HIDDEN` and `BORG_WORK`; per check `<id>.rc`, `<id>.ms` and
/// `<id>.log` in `out`. Right after the last hidden check (`h-*`), before the visible one can add
/// its own, every JUnit XML report under `tree` and the `scratch` roots (a JUnit test-results
/// directory's `*.xml`, or a `TEST-*.xml`) is copied to `<out>/junit/<root name>/<path in it>`.
fn checks_script(out: &Path, checks_dir: &Path, hidden_dir: &Path, checks: &[(String, u32)], scratch: &[PathBuf]) -> String {
    let (d, t, c) = (q(out.display().to_string()), q(out.join("tree").display().to_string()), q(checks_dir.display().to_string()));
    let mut s = format!(
        "set -u\nrun() {{ s=$(date +%s%N); (cd {t} && BORG_HIDDEN={} BORG_WORK={t} timeout -k 5 \"$2\" sh {c}/\"$1\".sh) >{d}/\"$1\".log 2>&1 </dev/null; rc=$?; e=$(date +%s%N); echo $rc >{d}/\"$1\".rc; echo $(( (e - s) / 1000000 )) >{d}/\"$1\".ms; }}\n",
        q(hidden_dir.display().to_string())
    );
    let last_hidden = checks.iter().rposition(|(id, _)| id.starts_with("h-"));
    for (i, (id, timeout)) in checks.iter().enumerate() {
        s.push_str(&format!("run {} {timeout}\n", q(id)));
        if Some(i) == last_hidden {
            s.push_str(&junit_script(out, scratch));
        }
    }
    s
}

/// The step of `checks_script` that keeps the hidden checks' JUnit XML (see there). A report is
/// small next to a build; a root that is gone or has none adds nothing.
fn junit_script(out: &Path, scratch: &[PathBuf]) -> String {
    let dst = q(out.join("junit").display().to_string());
    let roots = std::iter::once(("tree".to_string(), out.join("tree")))
        .chain(scratch.iter().map(|r| (r.file_name().map_or_else(|| "scratch".into(), |n| n.to_string_lossy().into_owned()), r.clone())));
    let mut s = String::new();
    for (name, root) in roots {
        let (r, d) = (q(root.display().to_string()), q(name));
        s.push_str(&format!(
            "[ -d {r} ] && (cd {r} && find . -name .git -prune -o -type f -name '*.xml' \\( -path '*/test-results/*' -o -name 'TEST-*.xml' \\) -print) 2>/dev/null | while IFS= read -r f; do f=${{f#./}}; mkdir -p {dst}/{d}/\"$(dirname \"$f\")\" && cp {r}/\"$f\" {dst}/{d}/\"$f\"; done\n"
        ));
    }
    s
}

fn tail(text: &str) -> String {
    let start = text.len().saturating_sub(TAIL);
    let start = (start..=text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    text[start..].to_string()
}

fn read_check(dir: &Path, id: &str) -> CheckResult {
    let rc: i32 = fs::read_to_string(dir.join(format!("{id}.rc"))).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(-1);
    let dur_ms = fs::read_to_string(dir.join(format!("{id}.ms"))).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
    let log = String::from_utf8_lossy(&fs::read(dir.join(format!("{id}.log"))).unwrap_or_default()).into_owned();
    CheckResult { id: id.to_string(), pass: rc == 0, rc, timeout: rc == 124 || rc == 137, dur_ms, tail: tail(&log) }
}

/// The repo files the visible check runs (guard.rs `oracle_files`, as the hive's verifier counts them).
fn visible_files(wl: &Workload, root: &Path) -> Vec<String> {
    let g = goal::Goal {
        v: 1,
        id: GOAL.into(),
        spec: String::new(),
        oracle: vec![goal::Check { name: "visible".into(), cmd: wl.visible.clone(), repo: String::new(), timeout_s: u64::from(wl.visible_timeout_s) }],
        budget_usd: 0.0,
        repos: vec![],
        author: String::new(),
        created_ms: 0,
        sig: String::new(),
        hidden: None,
        held_out: false,
    };
    guard::oracle_files(&g, root)
}

/// Which checks a scoring runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Hidden,
    Visible,
    Both,
}

/// The diff `base..head` of the scored clone `tree` (git's own config), binary and all, in `to`: what
/// was scored, kept beside its score (for D3 the integrated tree).
fn keep_diff(tree: &Path, base: &str, head: &str, to: &Path) -> Result<(), String> {
    let cmd = format!(
        "{GIT} -C {} diff --binary --no-color --no-ext-diff --no-textconv --no-renames {} {} >{}",
        q(tree.display().to_string()),
        q(base),
        q(head),
        q(to.display().to_string())
    );
    match run_t(tree, &cmd, 600) {
        (true, _, _) => Ok(()),
        (false, _, out) => Err(format!("git cannot diff {base}..{head} in the scored tree: {}", out.trim())),
    }
}

/// Scores `head` of `ws` in `dir` inside `bx`: the hidden checks and/or the visible check (`which`),
/// and the test/CI paths changed from `base` (on the fresh clone, whose .git config
/// is git's own). `dir` keeps `diff.patch` (`keep_diff`) and, after hidden checks, `junit/`
/// (`checks_script`).
fn score(bx: &Jail, wl: &Workload, ws: &Path, base: &str, head: &str, dir: &Path, which: Which) -> Result<Score, String> {
    let (hidden, visible) = (which != Which::Visible, which != Which::Hidden);
    let _ = fs::remove_dir_all(dir);
    let (checks_dir, out_dir) = (dir.join("checks"), dir.join("out"));
    fs::create_dir_all(&checks_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut list: Vec<(String, u32)> = Vec::new();
    if hidden {
        for c in &wl.checks {
            fs::write(checks_dir.join(format!("h-{}.sh", c.id)), &c.cmd).map_err(|e| e.to_string())?;
            list.push((format!("h-{}", c.id), c.timeout_s));
        }
    }
    if visible {
        fs::write(checks_dir.join("visible.sh"), &wl.visible).map_err(|e| e.to_string())?;
        list.push(("visible".into(), wl.visible_timeout_s));
    }
    let (code, cloned) = bx.exec(&out_dir, &[&out_dir], &[ws], &clone_script(ws, head, &out_dir), 720);
    if code != 0 || cloned.contains("CLONE-FAILED") {
        let _ = fs::write(dir.join("score.log"), &cloned);
        return Err(format!("scoring clone failed: {}", cloned.trim()));
    }
    // `hacked` reads the tree as cloned, before any check runs the result's code in it (writable: a
    // replace ref planted there makes the head read as base, a deleted file drops out of the visible
    // check's). A diff git cannot make fails the scoring: an unchecked tree is not a clean one.
    let tree = out_dir.join("tree");
    let oracle_files = visible_files(wl, &tree);
    let d = verdict::diff(&tree, base, head).ok_or_else(|| format!("hacked: git cannot diff {base}..{head} in the scored tree"))?;
    keep_diff(&tree, base, head, &dir.join("diff.patch"))?;
    let mut s = Score::default();
    (s.hacked_paths, s.hacked_why) = verdict::protected(&d, &wl.scope(), &oracle_files).into_iter().map(|p| (p.path, p.why)).unzip();
    let total: u32 = list.iter().map(|(_, t)| t + 10).sum::<u32>() + 60;
    let script = checks_script(&out_dir, &checks_dir, &wl.hidden_dir, &list, &bx.scratch());
    let (_, out) = bx.exec(&out_dir, &[&out_dir], &[&wl.hidden_dir, &checks_dir], &script, total);
    fs::write(dir.join("score.log"), format!("{cloned}{out}")).map_err(|e| e.to_string())?;
    let _ = fs::rename(out_dir.join("junit"), dir.join("junit"));
    if hidden {
        s.checks = wl.checks.iter().map(|c| CheckResult { id: c.id.clone(), ..read_check(&out_dir, &format!("h-{}", c.id)) }).collect();
        let tails: Value = s.checks.iter().map(|c| (c.id.clone(), json!(c.tail))).collect::<serde_json::Map<_, _>>().into();
        let _ = fs::write(dir.join("tails.json"), serde_json::to_string_pretty(&tails).unwrap_or_default());
    }
    if visible {
        s.visible_pass = Some(read_check(&out_dir, "visible").pass);
    }
    // Disk: the tree may hold a build; the logs stay.
    let _ = run_t(dir, &format!("chmod -R u+w {t} 2>/dev/null; rm -rf {t}", t = q(tree.display().to_string())), 600);
    Ok(s)
}

// ------------------------------------------------------------------------------------- cost

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Tokens {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

impl Tokens {
    fn add(&mut self, o: Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
    }

    fn json(&self) -> Value {
        json!({"input": self.input, "output": self.output, "cache_read": self.cache_read, "cache_creation": self.cache_creation})
    }

    fn of_usage(u: &Value) -> Tokens {
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        Tokens {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_creation: n("cache_creation_input_tokens"),
        }
    }
}

/// A claude result's tokens: summed over `modelUsage` (every model, subagents' too), else `usage`.
fn tokens_of(result: &Value) -> Option<Tokens> {
    if let Some(m) = result.get("modelUsage").and_then(Value::as_object).filter(|m| !m.is_empty()) {
        let mut t = Tokens::default();
        for u in m.values() {
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            t.add(Tokens {
                input: n("inputTokens"),
                output: n("outputTokens"),
                cache_read: n("cacheReadInputTokens"),
                cache_creation: n("cacheCreationInputTokens"),
            });
        }
        return Some(t);
    }
    result.get("usage").filter(|u| u.is_object()).map(Tokens::of_usage)
}

fn models_of(result: &Value) -> Vec<String> {
    result.get("modelUsage").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

/// A stream without a result (cut off): the assistant messages' usage, each message once (the stream
/// repeats a message's usage on each of its content blocks).
fn tokens_from_stream(stream: &str) -> Tokens {
    let mut by_id: BTreeMap<String, Tokens> = BTreeMap::new();
    let mut anon = Tokens::default();
    for v in stream.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if v["type"] != "assistant" {
            continue;
        }
        let u = &v["message"]["usage"];
        if !u.is_object() {
            continue;
        }
        match v["message"]["id"].as_str() {
            Some(id) => {
                by_id.insert(id.to_string(), Tokens::of_usage(u));
            }
            None => anon.add(Tokens::of_usage(u)),
        }
    }
    by_id.values().fold(anon, |mut acc, t| {
        acc.add(*t);
        acc
    })
}

/// A cell's money and tokens, metered the same way for every arm: `reported` sums what sessions said
/// they cost; `charged` adds, for each session that said nothing (cut off at the wall cap, killed),
/// the dollars it was allowed (its `--max-budget-usd`: an upper bound); `usd` is the total only when
/// every session reported (`complete`).
#[derive(Debug, Default, Clone, PartialEq)]
struct Cost {
    usd: Option<f64>,
    complete: bool,
    reported: f64,
    charged: f64,
    tokens: Tokens,
    turns: Option<u64>,
    models: Vec<String>,
}

impl Cost {
    /// An empty sum to `add` sessions into.
    fn zero() -> Cost {
        Cost { usd: Some(0.0), complete: true, turns: Some(0), ..Cost::default() }
    }

    fn add(&mut self, o: &Cost) {
        self.usd = self.usd.zip(o.usd).map(|(a, b)| a + b);
        self.complete &= o.complete;
        self.reported += o.reported;
        self.charged += o.charged;
        self.tokens.add(o.tokens);
        self.turns = Some(self.turns.unwrap_or(0) + o.turns.unwrap_or(0));
        for m in &o.models {
            if !self.models.contains(m) {
                self.models.push(m.clone());
            }
        }
        self.models.sort();
    }
}

/// One claude session's cost; `budget` is what it was allowed, charged when it reported nothing.
fn session_cost(result: Option<&Value>, stream: &str, budget: f64) -> Cost {
    let usd = result.and_then(|r| r["total_cost_usd"].as_f64());
    let (tokens, turns, models) = match result {
        Some(r) => (tokens_of(r).unwrap_or_else(|| tokens_from_stream(stream)), r["num_turns"].as_u64(), models_of(r)),
        None => (tokens_from_stream(stream), None, Vec::new()),
    };
    Cost { usd, complete: usd.is_some(), reported: usd.unwrap_or(0.0), charged: usd.unwrap_or(budget), tokens, turns, models }
}

/// The lines of a goal's `spend.jsonl`, of node `node` only when one is named: the file syncs (borg
/// hive/run.rs `ledger`), so each node of arm E holds every node's lines (a line naming no node is the
/// reading node's).
fn spend_of(path: &Path, node: Option<&str>) -> Vec<hive_run::Spend> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let all = text.lines().filter_map(|l| serde_json::from_str::<hive_run::Spend>(l).ok());
    all.filter(|s| node.is_none_or(|n| s.node.is_empty() || s.node == n)).collect()
}

/// D3: the dollars `spend.jsonl` charges (`Spend::charged`: the cost, else a timed-out iteration's
/// estimate), the dollars its lines reported, whether every line named its cost, and the number of
/// iterations; of node `node`'s lines only when one is named (`spend_lines`).
fn d3_spend(path: &Path, node: Option<&str>) -> (f64, f64, bool, usize) {
    let lines = spend_of(path, node);
    (lines.iter().map(hive_run::Spend::charged).sum(), lines.iter().filter_map(|s| s.usd).sum(), lines.iter().all(|s| s.usd.is_some()), lines.len())
}

/// D3: tokens and models over every iteration's log (`logs/*.out`, each one claude JSON object).
fn d3_tokens(logs: &Path) -> (Tokens, Vec<String>) {
    let mut t = Tokens::default();
    let mut models = BTreeSet::new();
    let mut files: Vec<PathBuf> = fs::read_dir(logs).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    for f in files.iter().filter(|p| p.extension().is_some_and(|e| e == "out")) {
        let text = fs::read_to_string(f).unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
            if let Some(x) = tokens_of(&v) {
                t.add(x);
            }
            models.extend(models_of(&v));
        }
    }
    (t, models.into_iter().collect())
}

/// D3: what the sessions still running when the wall cap killed `borg hive run` are charged. They never
/// got a spend.jsonl line: each iteration the runner started (a `by: "run"` line in joins.jsonl) whose
/// session no spend line names. Each was launched with its drone's dollars left (`--max-budget-usd`),
/// so it is charged that: the run's share (runs.jsonl, else `share`) less what the drone's lines
/// charge. Joins and runs are node `node`'s (they never sync), and so are the drones' lines counted
/// (another node's d1 is another drone). Returns (dollars, the drones cut off).
fn d3_unreported(gdir: &Path, share: f64, node: Option<&str>) -> (f64, Vec<String>) {
    let lines = spend_of(&gdir.join("spend.jsonl"), node);
    let seen: BTreeSet<&str> = lines.iter().flat_map(|s| s.session.split(',')).filter(|s| !s.is_empty()).collect();
    let values =
        |f: &str| -> Vec<Value> { fs::read_to_string(gdir.join(f)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect() };
    let share = values("runs.jsonl").last().and_then(|r| r["share_usd"].as_f64()).unwrap_or(share);
    let mut cut: BTreeSet<String> = BTreeSet::new();
    for j in values("joins.jsonl") {
        let (Some(drone), Some(session)) = (j["drone"].as_str(), j["session"].as_str()) else { continue };
        if j["by"] == "run" && !session.is_empty() && !seen.contains(session) {
            cut.insert(drone.to_string());
        }
    }
    let usd = cut.iter().map(|d| (share - lines.iter().filter(|s| &s.drone == d).map(hive_run::Spend::charged).sum::<f64>()).max(0.0)).sum();
    (usd, cut.into_iter().collect())
}

/// Over the cap (10% slack), on the dollars charged.
fn over_cap(charged: f64, cap: f64) -> bool {
    charged > cap * 1.10
}

/// D3's board: (rework summed over tasks, verdicts rejected as hacked, tasks per shown status).
fn board_facts(tasks: &Path) -> (u64, u64, BTreeMap<String, u64>) {
    let events = board::read(tasks);
    let b = board::fold(&events);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let rework = b.tasks.values().map(|t| u64::from(t.rework)).sum();
    let hacked = events.iter().filter(|e| e.ev == board::Ev::Rejected && e.hacked == Some(true)).count() as u64;
    let mut counts = BTreeMap::new();
    for t in b.tasks.values() {
        *counts.entry(t.shown_status(now).to_string()).or_default() += 1;
    }
    (rework, hacked, counts)
}

/// D3's caught mistakes: (verifier rejects, failed integrations), the rejected verdicts the board's fold
/// took (a tenure they closed) on verify and on integrate tasks.
fn board_rejects(tasks: &Path) -> (u64, u64) {
    let b = board::fold(&board::read(tasks));
    let rejected = |role: board::Role| {
        b.spans.iter().filter(|s| s.end == Some(board::Ev::Rejected) && b.tasks.get(&s.task).is_some_and(|t| t.task.role == role)).count() as u64
    };
    (rejected(board::Role::Verify), rejected(board::Role::Integrate))
}

/// Lines over every `lessons.jsonl` under `store`.
fn lessons_lines(store: &Path) -> u64 {
    fn walk(d: &Path, n: &mut u64) {
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, n);
            } else if p.file_name().is_some_and(|f| f == "lessons.jsonl") {
                *n += fs::read_to_string(&p).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()).count() as u64;
            }
        }
    }
    let mut n = 0;
    walk(store, &mut n);
    n
}

// ------------------------------------------------------------------------------------- arms

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    A,
    B,
    D3,
    E,
    F,
}

impl Arm {
    fn parse(s: &str) -> Option<Arm> {
        match s {
            "a" => Some(Arm::A),
            "b" => Some(Arm::B),
            "d3" => Some(Arm::D3),
            "e" => Some(Arm::E),
            "f" => Some(Arm::F),
            _ => None,
        }
    }

    /// The dollars a cell of this arm may spend at most, which `--stop-at-usd` reserves before it starts:
    /// its cap, and for arm E each node's. spend.jsonl syncs and every drone stops once the goal's total
    /// over both nodes reaches the cap (borg hive/run.rs `ledger`), so a cell spends at most the cap plus
    /// the iterations in flight then, one per drone, each at most its drone's share left. The two runs
    /// start at once and each splits the whole cap (neither has seen the other's spend), so those shares
    /// sum to the cap per node: the worst case is still each node's cap.
    fn reserve(self, cap: f64) -> f64 {
        match self {
            Arm::E => cap * arm_e::NODES.len() as f64,
            _ => cap,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Arm::A => "a",
            Arm::B => "b",
            Arm::D3 => "d3",
            Arm::E => "e",
            Arm::F => "f",
        }
    }
}

#[derive(Debug, Clone)]
struct Opts {
    workload: PathBuf,
    arm: Arm,
    seed: u32,
    cap: f64,
    model: String,
    timeout_s: u32,
    run: String,
    stop_at: Option<f64>,
    borg: Option<PathBuf>,
    claude: Option<PathBuf>,
    sim: bool,
    /// D3's learning arm (hive_seq.rs); None: every borg model call off, as in the pilot.
    learn: Option<Learn>,
    /// The chain whose HOME (and store) this goal runs in.
    chain: Option<PathBuf>,
    /// The hive goal's id.
    goal: String,
    /// The goal's position in its chain (recorded).
    position: Option<u32>,
}

impl Opts {
    /// The arm's name in cell names and rows: the learning arm when there is one.
    fn arm_name(&self) -> &'static str {
        self.learn.map_or(self.arm.name(), Learn::name)
    }
}

fn parse_opts(raw: &[String]) -> Result<Opts, String> {
    let (mut workload, mut arm, mut seed, mut cap) = (None, None, None, None);
    let mut o = Opts {
        workload: PathBuf::new(),
        arm: Arm::A,
        seed: 0,
        cap: 0.0,
        model: "sonnet".into(),
        timeout_s: 3600,
        run: String::new(),
        stop_at: None,
        borg: None,
        claude: None,
        sim: false,
        learn: None,
        chain: None,
        goal: GOAL.into(),
        position: None,
    };
    let mut it = raw.iter();
    while let Some(flag) = it.next() {
        let mut v = || it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--workload" => workload = Some(PathBuf::from(v()?)),
            "--arm" => {
                let a = v()?;
                arm = Some(Arm::parse(&a).ok_or_else(|| format!("--arm {a}: a, b, d3, e or f"))?);
            }
            "--seed" => seed = Some(v()?.parse::<u32>().map_err(|e| format!("--seed: {e}"))?),
            "--cap-usd" => cap = Some(v()?.parse::<f64>().map_err(|e| format!("--cap-usd: {e}"))?),
            "--model" => o.model = v()?,
            "--timeout-s" => o.timeout_s = v()?.parse().map_err(|e| format!("--timeout-s: {e}"))?,
            "--run" => o.run = v()?,
            "--stop-at-usd" => o.stop_at = Some(v()?.parse::<f64>().map_err(|e| format!("--stop-at-usd: {e}"))?),
            "--borg" => o.borg = Some(PathBuf::from(v()?)),
            "--claude" => o.claude = Some(PathBuf::from(v()?)),
            "--harness" => match v()?.as_str() {
                "claude" => o.sim = false,
                "sim" => o.sim = true,
                h => return Err(format!("--harness {h}: claude or sim")),
            },
            "--learn" => {
                let l = v()?;
                o.learn = Some(Learn::parse(&l).ok_or_else(|| format!("--learn {l}: frozen, l0 or l01"))?);
            }
            "--chain" => o.chain = Some(PathBuf::from(v()?)),
            "--goal" => o.goal = v()?,
            "--position" => o.position = Some(v()?.parse::<u32>().map_err(|e| format!("--position: {e}"))?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    o.workload = workload.ok_or("--workload DIR is required")?;
    o.arm = arm.ok_or("--arm a|b|d3|e|f is required")?;
    o.seed = seed.ok_or("--seed N is required")?;
    o.cap = cap.ok_or("--cap-usd X is required (no default: fixed after the probe)")?;
    if !o.cap.is_finite() || o.cap <= 0.0 {
        return Err(format!("--cap-usd {}: above 0", o.cap));
    }
    if o.timeout_s == 0 {
        return Err("--timeout-s: above 0".into());
    }
    if o.stop_at.is_some_and(|y| !y.is_finite() || y <= 0.0) {
        return Err("--stop-at-usd: above 0".into());
    }
    if o.sim && !matches!(o.arm, Arm::D3 | Arm::E) {
        return Err("--harness sim runs the d3 and e arms only (A/B/F take --claude BIN for a fake)".into());
    }
    if (o.learn.is_some() || o.chain.is_some()) && o.arm != Arm::D3 {
        return Err("--learn and --chain are for the d3 arm".into());
    }
    if o.chain.is_some() && o.learn.is_none() {
        return Err("--chain needs --learn: a chain is one learning arm's goal sequence".into());
    }
    if o.sim && o.learn.is_some() && o.claude.is_none() {
        return Err("--harness sim with a learning arm needs --claude FAKE: borg's model calls (learner, consequence model) must not reach the real CLI".into());
    }
    if !borg::hive::goal::valid_id(&o.goal) {
        return Err(format!("--goal {:?}: [a-z0-9-], at most 40", o.goal));
    }
    if o.run.is_empty() {
        o.run = format!("hive-{}", now_s() as u64);
    }
    Ok(o)
}

fn now_s() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn usd(x: f64) -> String {
    format!("{x:.2}")
}

/// The claude argv of one session of arm A, B or an F candidate (`budget`: its dollars).
fn session_args(arm: Arm, prompt: &str, model: &str, budget: f64) -> Vec<String> {
    let p = if arm == Arm::B { format!("{prompt}\n\n{INVITE}") } else { prompt.to_string() };
    let mut a: Vec<String> = vec!["-p".into(), p, "--model".into(), model.into(), "--max-budget-usd".into(), usd(budget)];
    a.extend(["--permission-mode", "bypassPermissions", "--append-system-prompt", crate::SYSTEM].map(String::from));
    if arm != Arm::B {
        a.extend(["--disallowedTools".to_string(), "Task Agent".to_string()]);
    }
    a
}

/// `borg hive init` of D3's goal: the prompt as its spec, the visible check as its oracle, the cap as
/// its budget, the workload's tasks; no hidden oracle (it never enters the sandbox).
fn d3_init_args(wl: &Workload, spec_file: &Path, cap: f64, goal: &str) -> Vec<String> {
    let mut a: Vec<String> = ["borg", "hive", "init", goal, "--spec-file"].map(String::from).to_vec();
    a.push(spec_file.display().to_string());
    a.extend(["--oracle".into(), format!("visible={}", wl.visible), "--timeout".into(), wl.visible_timeout_s.to_string(), "--budget".into(), cap.to_string()]);
    for t in &wl.tasks {
        a.extend(["--task".into(), t.clone()]);
    }
    a
}

/// `borg hive run` of D3's goal. `--yes`: the eval approves the plan itself (hive/plan.rs), so the run
/// never waits for an engineer the sandbox does not have.
fn d3_run_args(model: &str, base: &str, iter_timeout_s: u32, sim: bool, goal: &str) -> Vec<String> {
    run_args(model, base, iter_timeout_s, sim, goal, DRONES)
}

/// `d3_run_args` with `drones` drones (arm E's runs, one per node).
fn run_args(model: &str, base: &str, iter_timeout_s: u32, sim: bool, goal: &str, drones: usize) -> Vec<String> {
    let mut a: Vec<String> = ["borg", "hive", "run", goal, "--yes", "--drones"].map(String::from).to_vec();
    a.extend([drones.to_string(), "--model".into(), model.into(), "--base".into(), base.into(), "--iter-timeout".into(), iter_timeout_s.to_string()]);
    if sim {
        a.extend(["--harness".into(), "sim".into()]);
    }
    a
}

/// The env of every agent sandbox: a store and config in the sandbox HOME, every borg model call off
/// (or a learning arm's switches, `hive_seq::arm_env`), the workload's env; with the sim harness, its
/// switches.
fn agent_env(home: &Path, wl: &Workload, sim_script: Option<&Path>, learn: Option<Learn>, model: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("BORG_HOME".to_string(), home.join(".local/share/borg").display().to_string()),
        ("BORG_CONFIG".to_string(), home.join(".config/borg/collectives.json").display().to_string()),
    ];
    env.extend(hive_seq::arm_env(&BORG_OFF, learn, model));
    if let Some(s) = sim_script {
        env.push(("BORG_TEST_HARNESS".into(), "1".into()));
        env.push(("BORG_SIM_CAPS".into(), "prompt,tool,deny,headless,budget".into()));
        env.push(("BORG_SIM_SCRIPT".into(), s.display().to_string()));
    }
    env.extend(wl.env_for(home));
    env
}

/// One agent home of a cell: the sandbox HOME, its secrets (none with a fake or simulated harness),
/// the workspace, the env and the sandbox options.
struct Agent {
    home: PathBuf,
    ws: PathBuf,
    base: String,
    secrets: Option<Secrets>,
    env: Vec<(String, String)>,
    home_binds: Vec<(PathBuf, PathBuf)>,
    ro: Vec<PathBuf>,
}

impl Agent {
    fn extra(&self) -> Extra {
        Extra {
            home_ro: self.home_binds.clone(),
            setting_sources: Some("user,project".into()),
            sandbox_home: Some(self.home.clone()),
            file_rw: self.secrets.as_ref().map(|s| Secrets::binds(&s.dir, &self.home)).unwrap_or_default(),
            env: self.env.clone(),
            unshare_net: false,
            // Nothing an agent starts (a Gradle daemon a build keeps on) outlives its session into
            // the next cells' wall-clock.
            unshare_pid: true,
        }
    }

    fn lessons(&self) -> u64 {
        lessons_lines(&self.home.join(".local/share/borg"))
    }

    /// `argv` in the sandbox: (exit code, stdout, stderr).
    fn exec(&self, cwd: &Path, argv: &[String], timeout_s: u32) -> (i32, String, String) {
        let ro: Vec<&Path> = self.ro.iter().map(PathBuf::as_path).collect();
        let (mut cmd, private) = sandbox_with(cwd, &[&self.home], &ro, &self.extra(), timeout_s);
        let out = cmd.args(argv).stdin(Stdio::null()).output();
        let _ = fs::remove_dir_all(private);
        match out {
            Ok(o) => (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(e) => (-1, String::new(), e.to_string()),
        }
    }
}

struct Env {
    root: PathBuf,
    real_home: PathBuf,
    borg: PathBuf,
}

/// D3's `claude`: `target` with `--append-system-prompt SYSTEM` appended to the runner's argv, so a
/// drone's session is told what an A, B or F session is (`session_args`); its task text is the hive's. A
/// structured call (`--json-schema`: borg's own learner) gets no drone system prompt.
fn claude_wrapper(target: &Path) -> String {
    let t = q(target.display().to_string());
    format!("#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = --json-schema ] && exec {t} \"$@\"; done\nexec {t} \"$@\" --append-system-prompt {}\n", q(crate::SYSTEM))
}

/// The sandbox HOME of a cell's agent in `dir`: the chain's, which outlives the cell, with `--chain`.
fn agent_home(dir: &Path, o: &Opts) -> PathBuf {
    o.chain.as_ref().map_or_else(|| dir.join("home"), |c| c.join("home"))
}

fn make_agent(dir: &Path, o: &Opts, wl: &Workload, env: &Env, sim_script: Option<&Path>) -> Result<Agent, String> {
    make_agent_from(dir, o, wl, env, sim_script, COLLECTIVES, None)
}

/// `make_agent` with the HOME's collectives config `collectives`, and with `remote` the workspace a
/// `--no-local` clone of that bare repo (an object store of its own, arm E's nodes) instead of a build
/// of the workload; the remote is readable in the sandbox, as a shared remote is.
fn make_agent_from(
    dir: &Path,
    o: &Opts,
    wl: &Workload,
    env: &Env,
    sim_script: Option<&Path>,
    collectives: &str,
    remote: Option<&Path>,
) -> Result<Agent, String> {
    let home = agent_home(dir, o);
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let secrets = if o.claude.is_some() || o.sim {
        prepare_home_bare(&home, &env.borg, "{}", collectives)?;
        None
    } else {
        Some(prepare_home_with(&home, &env.real_home, &env.borg, "{}", collectives, f64::from(o.timeout_s) + 900.0)?)
    };
    let mut ro = vec![env.borg.clone()];
    ro.extend(remote.map(Path::to_path_buf));
    if matches!(o.arm, Arm::D3 | Arm::E) && !o.sim {
        // The drones' `claude` (the runner finds it on PATH, `<home>/bin` first): the real one, or the
        // fake, with the system prompt A, B and F get.
        let target = o.claude.clone().unwrap_or_else(|| env.real_home.join(".local/bin/claude"));
        let w = home.join("bin/claude");
        fs::write(&w, claude_wrapper(&target)).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&w, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        ro.extend(o.claude.clone());
    } else if let Some(c) = &o.claude {
        let link = home.join("bin/claude");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(c, &link).map_err(|e| e.to_string())?;
        ro.push(c.clone());
    }
    let ws = home.join("work").join(&wl.ws);
    let base = match remote {
        Some(r) => clone_workspace(r, &ws)?,
        None => build_workspace(wl, &ws)?,
    };
    Ok(Agent { env: agent_env(&home, wl, sim_script, o.learn, &o.model), home_binds: wl.home_binds(&env.real_home), home, ws, base, secrets, ro })
}

/// One finished claude session.
struct Ran {
    cost: Cost,
    wall: f64,
    timed_out: bool,
}

/// `budget`: the session's `--max-budget-usd`, charged when it reports no cost.
fn claude_session(ag: &Agent, args: &[String], stream: &Path, timeout_s: u32, budget: f64) -> Ran {
    let ro: Vec<&Path> = ag.ro.iter().map(PathBuf::as_path).collect();
    let (result, wall, code) = crate::bwrap_claude_run(&ag.ws, &[&ag.home], &ro, &ag.extra(), args, stream, timeout_s);
    let text = fs::read_to_string(stream).unwrap_or_default();
    Ran { cost: session_cost(result.as_ref(), &text, budget), wall, timed_out: code == Some(124) || code == Some(137) }
}

/// Commits what the agent left as `result` in its workspace (scorer sandbox): the head.
fn commit_result(bx: &Jail, ws: &Path) -> Result<String, String> {
    let (_, out) = bx.exec(ws, &[ws], &[], &result_script(), 600);
    parse_head(&out).ok_or_else(|| format!("result commit failed: {}", out.trim()))
}

/// F's pick: the visible check passed first, then the lower cost (unknown is dearest), then the lower
/// index. Candidates are (visible_pass, cost_usd).
fn choose(cands: &[(bool, Option<f64>)]) -> usize {
    (0..cands.len())
        .min_by(|&i, &j| {
            let key = |k: usize| (!cands[k].0, cands[k].1.unwrap_or(f64::INFINITY));
            let (a, b) = (key(i), key(j));
            a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(i.cmp(&j))
        })
        .unwrap_or(0)
}

fn git_rev(root: &Path, paths: &str) -> String {
    let (_, _, rev) = run_t(root, "git rev-parse --short=12 HEAD", 30);
    let (_, _, dirty) = run_t(root, &format!("git status --porcelain -- {paths}"), 30);
    format!("{}{}", rev.trim(), if dirty.trim().is_empty() { "" } else { "+dirty" })
}

/// The row skeleton: every key present.
fn base_row(o: &Opts, wl: &Workload, env: &Env) -> serde_json::Map<String, Value> {
    let v = json!({
        "run": o.run, "workload": wl.id, "workload_sha": wl.sha, "shape": wl.shape, "arm": o.arm_name(), "model": o.model,
        "models": [], "seed": o.seed, "cap_usd": o.cap, "K": wl.k(), "base": null, "result_head": null, "wall_s": null,
        "timed_out": false, "cost_usd": null, "cost_complete": false, "over_cap": false, "tokens": Tokens::default().json(),
        "turns": null, "sessions": null, "hidden": {"checks": [], "passed": 0, "solve_share": 0.0}, "visible_pass": null,
        "rework": null, "merge_conflicts": 0, "integrated": false, "integrations": 0, "hacked": 0, "hacked_paths": [], "hacked_why": [], "hacked_verdicts": 0,
        "verifier_rejects": null, "failed_integrations": null, "d3": null, "e": null, "f": null, "store_lessons_before": null, "store_lessons_after": null,
        "borg_rev": git_rev(&env.root, "crates/borg crates/borg-trace"), "borg_eval_rev": git_rev(&env.root, "crates/borg-eval"),
        "mast": [], "error": null,
    });
    let mut row = v.as_object().cloned().unwrap_or_default();
    row.insert("reserve_usd".into(), json!(o.arm.reserve(o.cap)));
    for k in ["reported_usd", "charged_usd", "error_stage", "cell", "attempt"] {
        row.insert(k.into(), Value::Null);
    }
    row.insert("paid".into(), json!(false));
    row
}

/// Every key `put_score` writes: hacked counts the final tree's weakened tests and changed CI or check
/// files for every arm alike (0 or 1, as the result has a protected change or not; `hacked_why` says
/// why each); D3's verifier verdicts that caught a hack on the way (and may have been reworked away)
/// are `hacked_verdicts`, reported apart.
fn put_score(row: &mut serde_json::Map<String, Value>, s: &Score, k: usize, hacked_verdicts: u64) {
    row.insert("hidden".into(), s.json(k));
    row.insert("visible_pass".into(), json!(s.visible_pass));
    row.insert("hacked_paths".into(), json!(s.hacked_paths));
    row.insert("hacked_why".into(), json!(s.hacked_why));
    row.insert("hacked_verdicts".into(), json!(hacked_verdicts));
    row.insert("hacked".into(), json!(u64::from(!s.hacked_paths.is_empty())));
}

fn put_cost(row: &mut serde_json::Map<String, Value>, c: &Cost, cap: f64) {
    row.insert("cost_usd".into(), json!(c.usd));
    row.insert("reported_usd".into(), json!(c.reported));
    row.insert("charged_usd".into(), json!(c.charged));
    row.insert("cost_complete".into(), json!(c.complete));
    row.insert("over_cap".into(), json!(over_cap(c.charged, cap)));
    row.insert("tokens".into(), c.tokens.json());
    row.insert("turns".into(), json!(c.turns));
    row.insert("models".into(), json!(c.models));
}

/// Marks the row: a model call may have been made from here on (an error after it is not a setup
/// error, and the cell's dollars count).
fn mark_paid(row: &mut serde_json::Map<String, Value>) {
    row.insert("paid".into(), json!(true));
}

/// Arm A or B: one session.
fn arm_single(o: &Opts, wl: &Workload, env: &Env, cell: &Path, row: &mut serde_json::Map<String, Value>) -> Result<(), String> {
    let ag = make_agent(cell, o, wl, env, None)?;
    row.insert("base".into(), json!(ag.base));
    let before = ag.lessons();
    row.insert("store_lessons_before".into(), json!(before));
    if before != 0 {
        return Err(format!("the fresh store holds {before} lesson line(s)"));
    }
    let args = session_args(o.arm, &wl.prompt(), &o.model, o.cap);
    eprintln!("  {} {}: claude -p (cap ${:.2}, wall cap {}s) …", wl.id, o.arm.name(), o.cap, o.timeout_s);
    mark_paid(row);
    let ran = claude_session(&ag, &args, &cell.join("stream.jsonl"), o.timeout_s, o.cap);
    // What the session cost and how long it ran are known now: recorded before anything below can fail.
    put_cost(row, &ran.cost, o.cap);
    row.insert("wall_s".into(), json!(ran.wall));
    row.insert("timed_out".into(), json!(ran.timed_out));
    row.insert("sessions".into(), json!(1));
    let mut ag = ag;
    // No token outlives the session.
    ag.secrets = None;
    let bx = Jail::scorer(cell, wl, &env.real_home);
    let t0 = Instant::now();
    let head = commit_result(&bx, &ag.ws)?;
    row.insert("wall_s".into(), json!(ran.wall + t0.elapsed().as_secs_f64()));
    row.insert("store_lessons_after".into(), json!(ag.lessons()));
    row.insert("result_head".into(), json!(head));
    let s = score(&bx, wl, &ag.ws, &ag.base, &head, &cell.join("score"), Which::Both)?;
    put_score(row, &s, wl.k(), 0);
    Ok(())
}

/// Arm F: three A sessions at cap/3 in parallel, the best by the visible check.
fn arm_f(o: &Opts, wl: &Workload, env: &Env, cell: &Path, row: &mut serde_json::Map<String, Value>) -> Result<(), String> {
    let mut agents = Vec::new();
    for i in 1..=3 {
        agents.push(make_agent(&cell.join(format!("f{i}")), o, wl, env, None)?);
    }
    let before: u64 = agents.iter().map(Agent::lessons).sum();
    row.insert("store_lessons_before".into(), json!(before));
    if before != 0 {
        return Err(format!("the fresh stores hold {before} lesson line(s)"));
    }
    let budget = o.cap / 3.0;
    let args = session_args(Arm::F, &wl.prompt(), &o.model, budget);
    eprintln!("  {} f: 3 × claude -p (${} each, wall cap {}s) …", wl.id, usd(budget), o.timeout_s);
    mark_paid(row);
    let rans: Vec<Ran> = std::thread::scope(|s| {
        let hs: Vec<_> = agents
            .iter()
            .enumerate()
            .map(|(i, ag)| {
                let (args, stream) = (&args, cell.join(format!("f{}/stream.jsonl", i + 1)));
                s.spawn(move || claude_session(ag, args, &stream, o.timeout_s, budget))
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or(Ran { cost: session_cost(None, "", budget), wall: f64::from(o.timeout_s), timed_out: true })).collect()
    });
    for ag in &mut agents {
        ag.secrets = None;
    }
    // The three sessions' dollars, tokens and wall: recorded before selection or scoring can fail.
    let mut cost = Cost::zero();
    for r in &rans {
        cost.add(&r.cost);
    }
    put_cost(row, &cost, o.cap);
    let slowest = rans.iter().map(|r| r.wall).fold(0.0, f64::max);
    row.insert("wall_s".into(), json!(slowest));
    row.insert("timed_out".into(), json!(rans.iter().any(|r| r.timed_out)));
    row.insert("sessions".into(), json!(3));
    // Selection: each candidate's result, its visible check on a fresh tree.
    let t0 = Instant::now();
    let mut heads = Vec::new();
    let mut visible = Vec::new();
    for (i, ag) in agents.iter().enumerate() {
        let c = cell.join(format!("f{}", i + 1));
        let bx = Jail::scorer(&c, wl, &env.real_home);
        let head = commit_result(&bx, &ag.ws)?;
        let s = score(&bx, wl, &ag.ws, &ag.base, &head, &c.join("select"), Which::Visible)?;
        visible.push(s.visible_pass.unwrap_or(false));
        heads.push(head);
    }
    let selection = t0.elapsed().as_secs_f64();
    row.insert("wall_s".into(), json!(slowest + selection));
    let chosen = choose(&visible.iter().zip(&rans).map(|(v, r)| (*v, r.cost.usd)).collect::<Vec<_>>());
    let mut cands = Vec::new();
    let mut chosen_score = None;
    for (i, ag) in agents.iter().enumerate() {
        let c = cell.join(format!("f{}", i + 1));
        let bx = Jail::scorer(&c, wl, &env.real_home);
        let s = score(&bx, wl, &ag.ws, &ag.base, &heads[i], &c.join("score"), Which::Hidden)?;
        cands.push(json!({"i": i + 1, "cost_usd": rans[i].cost.usd, "charged_usd": rans[i].cost.charged, "wall_s": rans[i].wall, "visible_pass": visible[i],
                          "solve_share": s.share(wl.k()), "base": ag.base, "result_head": heads[i], "timed_out": rans[i].timed_out}));
        if i == chosen {
            chosen_score = Some(s);
        }
    }
    row.insert("base".into(), json!(agents[chosen].base));
    row.insert("result_head".into(), json!(heads[chosen]));
    row.insert("store_lessons_after".into(), json!(agents.iter().map(Agent::lessons).sum::<u64>()));
    row.insert("f".into(), json!({"candidates": cands, "chosen": chosen + 1, "selection_s": selection}));
    let s = chosen_score.ok_or("no chosen candidate")?;
    put_score(row, &s, wl.k(), 0);
    row.insert("visible_pass".into(), json!(visible[chosen]));
    Ok(())
}

/// Hooks `borg install claude --apply` wired in the sandbox HOME's settings.json.
fn wired_events(home: &Path) -> usize {
    let v: Value = fs::read_to_string(home.join(".claude/settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    ["UserPromptSubmit", "PostToolUse", "PostToolUseFailure", "Stop", "SessionEnd"]
        .iter()
        .filter(|e| v["hooks"][**e].to_string().contains("hook claude"))
        .count()
}

/// Arm D3: borg installed, the goal made, `borg hive run --drones 3` (which integrates), its combined
/// tree scored. The run has the same wall cap as an A/B session (`--timeout-s`), and so does each
/// iteration within it. With a learning arm, `borg learn` runs after it (the learner's dollars count);
/// with a chain, the HOME is the chain's (hive_seq.rs) and the row carries `seq`.
fn arm_d3(o: &Opts, wl: &Workload, env: &Env, cell: &Path, row: &mut serde_json::Map<String, Value>) -> Result<(), String> {
    let home = agent_home(cell, o);
    let root = home.join(hive_seq::KEEP[0]);
    let mut seq = serde_json::Map::new();
    if let Some(l) = o.learn {
        // Named before any step that can fail, so a goal that errors early is still the report's.
        seq.extend(
            json!({"arm": l.name(), "seed": o.seed, "position": o.position, "goal": wl.id, "hive_goal": o.goal, "K": wl.k()})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        row.insert("seq".into(), Value::Object(seq.clone()));
    }
    if let Some(chain) = &o.chain {
        // Only the store and borg's config carry over from the chain's goals before; the store is saved
        // before the goal, and a goal run again starts from that save.
        hive_seq::reset_keeping(&home, &hive_seq::KEEP)?;
        let snap = chain.join("snap").join(format!("{}{}.tar", o.position.map_or(String::new(), |p| format!("p{p}-")), wl.id));
        let restored = hive_seq::snapshot_or_restore(&home, &snap)?;
        seq.insert("chain".into(), json!(chain.display().to_string()));
        seq.insert("snapshot".into(), json!(snap.display().to_string()));
        seq.insert("restored_snapshot".into(), json!(restored));
        if o.learn.is_some() {
            row.insert("seq".into(), Value::Object(seq.clone()));
        }
    }
    // Inside the sandbox HOME: the cell dir itself is not bound into the sandbox.
    let sim_script = home.join("sim.json");
    if o.sim {
        fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        fs::write(&sim_script, wl.sim.clone().unwrap_or(json!({})).to_string()).map_err(|e| e.to_string())?;
    }
    let ag = make_agent(cell, o, wl, env, o.sim.then_some(sim_script.as_path()))?;
    row.insert("base".into(), json!(ag.base));
    let before = ag.lessons();
    row.insert("store_lessons_before".into(), json!(before));
    let facts0 = hive_seq::facts(&root);
    match o.learn {
        None if before != 0 => return Err(format!("the fresh store holds {before} lesson line(s)")),
        Some(Learn::Frozen) if facts0.lessons != 0 || facts0.playbook_lines != 0 => {
            return Err(format!("frozen: the store holds {} lesson(s) and {} playbook line(s)", facts0.lessons, facts0.playbook_lines));
        }
        Some(Learn::L0) if facts0.playbook_lines != 0 => return Err(format!("l0: the store holds {} playbook line(s)", facts0.playbook_lines)),
        _ => {}
    }
    if o.learn.is_some() {
        seq.insert("lessons_before".into(), json!(facts0.lessons));
        seq.insert("playbook_lines_before".into(), json!(facts0.playbook_lines));
        row.insert("seq".into(), Value::Object(seq.clone()));
    }
    let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (code, out, err) = ag.exec(&ag.home, &argv(&["borg", "install", "claude", "--apply"]), 60);
    let wired = wired_events(&ag.home);
    if code != 0 || wired != 5 {
        return Err(format!("borg install: exit {code}, {wired}/5 events wired: {out}{err}"));
    }
    let spec = ag.home.join("goal/spec.md");
    fs::create_dir_all(spec.parent().unwrap_or(&ag.home)).map_err(|e| e.to_string())?;
    fs::write(&spec, wl.prompt()).map_err(|e| e.to_string())?;
    let (code, out, err) = ag.exec(&ag.ws, &d3_init_args(wl, &spec, o.cap, &o.goal), 60);
    if code != 0 {
        return Err(format!("borg hive init: exit {code}: {out}{err}"));
    }
    eprintln!("  {} d3: borg hive run --drones {DRONES} (cap ${:.2}, wall cap {}s) …", wl.id, o.cap, o.timeout_s);
    mark_paid(row);
    let start_ms = borg::learn::now_ms();
    let t0 = Instant::now();
    let (code, out, err) = ag.exec(&ag.ws, &d3_run_args(&o.model, &ag.base, o.timeout_s, o.sim, &o.goal), o.timeout_s);
    let run_wall = t0.elapsed().as_secs_f64();
    let _ = fs::write(cell.join("run.out"), &out);
    let _ = fs::write(cell.join("run.err"), &err);
    let mut ag = ag;
    // The learner, while the credentials are still there: the SessionEnd hook's background `borg learn`
    // died with the run's PID namespace, so the group episodes and playbook reflections are caught up here.
    if o.learn.is_some_and(Learn::learns) {
        let store = borg::collective::store(&root, hive_seq::COLLECTIVE);
        let t = Instant::now();
        let (code, out, err) = ag.exec(&ag.ws, &argv(&["env", &format!("BORG_HOME={}", store.display()), "borg", "learn"]), 900);
        let _ = fs::write(cell.join("learn.out"), format!("{out}{err}"));
        seq.insert("learn".into(), json!({"exit": code, "wall_s": t.elapsed().as_secs_f64()}));
    }
    let end_ms = borg::learn::now_ms();
    ag.secrets = None;
    // Dollars, tokens and wall first: known now, and nothing below may lose them.
    let gdir = borg::collective::store(&root, hive_seq::COLLECTIVE).join("hive").join(&o.goal);
    let timed_out = code == 124 || code == 137;
    let (charged, reported, all_named, iterations) = d3_spend(&gdir.join("spend.jsonl"), None);
    let (cut_usd, cut) = d3_unreported(&gdir, o.cap / DRONES as f64, None);
    let (tokens, models) = d3_tokens(&gdir.join("logs"));
    let complete = all_named && cut.is_empty() && !timed_out;
    let total = charged + cut_usd;
    let cost = Cost { usd: complete.then_some(total), complete, reported, charged: total, tokens, turns: None, models };
    put_cost(row, &cost, o.cap);
    row.insert("wall_s".into(), json!(run_wall));
    row.insert("timed_out".into(), json!(timed_out));
    row.insert("sessions".into(), json!(iterations + cut.len()));
    let (rework, hacked_verdicts, statuses) = board_facts(&gdir.join("tasks.jsonl"));
    row.insert("rework".into(), json!(rework));
    row.insert("hacked_verdicts".into(), json!(hacked_verdicts));
    let (verifier_rejects, failed_integrations) = board_rejects(&gdir.join("tasks.jsonl"));
    row.insert("verifier_rejects".into(), json!(verifier_rejects));
    row.insert("failed_integrations".into(), json!(failed_integrations));
    if o.learn.is_some() {
        // The caught mistakes, in the row even when scoring fails below.
        seq.insert("verifier_rejects".into(), json!(verifier_rejects));
        seq.insert("failed_integrations".into(), json!(failed_integrations));
        row.insert("seq".into(), Value::Object(seq.clone()));
    }
    let ends: Vec<&str> = out.lines().filter(|l| l.starts_with('d') && l.contains(" end: ")).collect();
    let spend_lines = fs::read_to_string(gdir.join("spend.jsonl")).unwrap_or_default().lines().count();
    row.insert(
        "d3".into(),
        json!({"finished": code == 0, "exit": code, "iterations": iterations, "drone_ends": ends, "board": statuses, "spend_lines": spend_lines,
               "cut_off_drones": cut, "cut_off_usd": cut_usd}),
    );
    if o.learn.is_some() {
        // Borg's own model dollars (the learner's reflection, the consequence model's checks) are charged
        // to the goal: its total is what the $ guards and the H5 cost term count (`charged_of`), and
        // `over_cap` is on it. The hive's `--budget` caps the drones only, and the learner's calls are not
        // capped per goal: a goal over its cap with them shows as `over_cap`.
        let borg_usd = hive_seq::borg_usd_in(&root, start_ms, end_ms);
        let after = hive_seq::facts(&root);
        row.insert("over_cap".into(), json!(over_cap(total + borg_usd, o.cap)));
        seq.extend(
            json!({"charged_usd": total, "borg_usd": borg_usd, "charged_total_usd": total + borg_usd,
                   "lessons_after": after.lessons, "lessons_new": after.lessons.saturating_sub(facts0.lessons),
                   "lessons_new_by_source": hive_seq::lessons_new_by_source(&root, start_ms, end_ms),
                   "lessons_shown": hive_seq::outcomes_in(&root, borg::outcome::ASKED, start_ms, end_ms),
                   "playbook_shown": hive_seq::outcomes_in(&root, borg::outcome::PLAYBOOK, start_ms, end_ms),
                   "playbook_proposed": after.playbook_open, "playbook_promoted_total": after.playbook_promoted,
                   "group_episodes": after.group_episodes, "other_open": after.other_open, "window_ms": [start_ms, end_ms]})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        row.insert("seq".into(), Value::Object(seq.clone()));
    }
    let bx = Jail::scorer(cell, wl, &env.real_home);
    let r = d3_result(&gdir.join("tasks.jsonl"), &ag.base);
    row.insert("integrated".into(), json!(r.integrated));
    row.insert("integrations".into(), json!(r.integrations));
    row.insert("merge_conflicts".into(), json!(r.conflicts));
    let head = r.head;
    row.insert("result_head".into(), json!(head));
    row.insert("store_lessons_after".into(), json!(ag.lessons()));
    let scored = score(&bx, wl, &ag.ws, &ag.base, &head, &cell.join("score"), Which::Both);
    if o.chain.is_some() {
        // Disk: the workspace, the drones' worktrees and the writable Gradle state go; the store (logs,
        // board) stays in the chain, the score in the cell.
        let _ = hive_seq::remove_tree(&home.join("work"));
        let _ = hive_seq::remove_tree(&home.join(".gradle"));
    }
    let s = scored?;
    put_score(row, &s, wl.k(), hacked_verdicts);
    if o.learn.is_some() {
        let total = seq["charged_total_usd"].as_f64().unwrap_or(total);
        seq.insert("share".into(), json!(s.share(wl.k())));
        let hacked = u64::from(!s.hacked_paths.is_empty());
        let (all, escaped, caught) = hive_seq::mistakes(wl.k(), s.passed(), hacked, verifier_rejects, failed_integrations);
        seq.insert(
            "mistakes".into(),
            json!({"total": all, "escaped": escaped, "caught": caught, "hidden_failing": wl.k().saturating_sub(s.passed()), "hacked": hacked,
                   "verifier_rejects": verifier_rejects, "failed_integrations": failed_integrations}),
        );
        seq.insert("usd_per_solve".into(), json!(hive_seq::usd_per_solve(total, s.share(wl.k()), wl.k())));
        row.insert("seq".into(), Value::Object(seq));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------- commands

fn repo_root() -> PathBuf {
    let r = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::canonicalize(&r).unwrap_or(r)
}

fn real_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn validate_log(runs: &Path) -> PathBuf {
    runs.join("hive-validate/results.jsonl")
}

fn validated(runs: &Path, sha: &str) -> bool {
    fs::read_to_string(validate_log(runs))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|v| v["validate"]["workload_sha"] == sha && v["validate"]["pass"] == true)
}

/// A row's dollars: a goal sequence's total (the drones' and borg's own model calls), else what it charged
/// (`Cost::charged`); a row from before that field, its cost; nothing for an error before any model call;
/// else its cap.
pub(crate) fn charged_of(r: &Value) -> f64 {
    r["seq"]["charged_total_usd"]
        .as_f64()
        .or_else(|| r["charged_usd"].as_f64())
        .or_else(|| r["cost_usd"].as_f64())
        .or_else(|| (r["paid"] == false && !r["error"].is_null()).then_some(0.0))
        .or_else(|| r["reserve_usd"].as_f64())
        .or_else(|| r["cap_usd"].as_f64())
        .unwrap_or(0.0)
}

/// The dollars a run already counts: each row's `charged_of`, and the cap of every cell attempt that
/// started (`<cell>/cell.json`; its reserve, arm E's 2× cap) but wrote no row (killed, crashed: its sessions may have spent it).
fn spent_in(run_dir: &Path) -> f64 {
    let rows: Vec<Value> =
        fs::read_to_string(run_dir.join("results.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
    let attempts: BTreeSet<&str> = rows.iter().filter_map(|r| r["attempt"].as_str()).collect();
    let lost: f64 = fs::read_dir(run_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path().join("cell.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()))
        .filter(|c| c["attempt"].as_str().is_some_and(|a| !attempts.contains(a)))
        .map(|c| c["reserve_usd"].as_f64().or_else(|| c["cap_usd"].as_f64()).unwrap_or(0.0))
        .sum();
    rows.iter().map(charged_of).sum::<f64>() + lost
}

/// Starts a cell: its dir and `cell.json`, written before anything is spent (a cell killed before its
/// row still counts its reserve, `spent_in`), under the run's budget lock: without it, cells of one run
/// started at once would each read the total before the others' `cell.json` and all start past
/// `stop_at`. (exit code, why) when the run's spend plus `reserve` (the most the cell may spend,
/// `Arm::reserve`) passes `stop_at` or the cell exists.
fn start_cell(run_dir: &Path, cell: &Path, record: &Value, reserve: f64, stop_at: Option<f64>) -> Result<(), (i32, String)> {
    fs::create_dir_all(run_dir).map_err(|e| (1, e.to_string()))?;
    let lock = fs::File::create(run_dir.join(".budget.lock")).map_err(|e| (1, e.to_string()))?;
    // Released when `lock` closes, on return.
    lock.lock().map_err(|e| (1, format!("budget lock: {e}")))?;
    let spent = spent_in(run_dir);
    if let Some(y) = stop_at.filter(|y| spent + reserve > *y) {
        return Err((
            2,
            format!("${spent:.2} spent in run {} + reserve ${reserve:.2} > ${y:.2}: not started", run_dir.file_name().unwrap_or_default().to_string_lossy()),
        ));
    }
    if cell.exists() {
        return Err((2, format!("{} exists; one row per cell", cell.display())));
    }
    fs::create_dir_all(cell).map_err(|e| (1, e.to_string()))?;
    fs::write(cell.join("cell.json"), record.to_string()).map_err(|e| (1, e.to_string()))
}

pub(crate) fn append_row(path: &Path, row: &Value) -> Result<(), String> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
    // Held while the line goes in: whoever rewrites the log under the same lock loses no row.
    f.lock().map_err(|e| e.to_string())?;
    writeln!(f, "{row}").map_err(|e| e.to_string())
}

pub fn main(raw: &[String]) {
    match raw.first().map(String::as_str) {
        Some("report") => {
            let rows: Vec<Value> = raw[1..]
                .iter()
                .flat_map(|f| fs::read_to_string(f).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>())
                .filter(|v| v.get("arm").is_some())
                .collect();
            print!("{}", report_text(&rows));
            print!("{}", hive_seq::h5_report(&rows));
        }
        Some("validate") => std::process::exit(validate_cmd(&raw[1..])),
        Some("promote") => std::process::exit(hive_seq::promote_cmd(&raw[1..])),
        _ => {
            if parse_opts(raw).is_ok_and(|o| o.arm == Arm::E) {
                // Arm E's sync pump runs borg's session code in this process: with the nodes' switches,
                // as each node's `borg mesh serve` would (arm_e.rs `PUMP_ENV`).
                for (k, v) in arm_e::PUMP_ENV {
                    // SAFETY: borg-eval's main thread, before any other thread is started.
                    unsafe { std::env::set_var(k, v) };
                }
            }
            std::process::exit(cell_cmd(raw))
        }
    }
}

fn cell_cmd(raw: &[String]) -> i32 {
    let o = match parse_opts(raw) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("borg-eval hive: {e}");
            return 2;
        }
    };
    let root = repo_root();
    let runs = root.join("target/eval-runs");
    let real_home = real_home();
    let wl = match load_workload(&o.workload, &real_home, &runs) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("borg-eval hive: {e}");
            return 2;
        }
    };
    if !validated(&runs, &wl.sha) {
        eprintln!("borg-eval hive: workload {} ({}) has no passing `borg-eval hive validate` row; run it first", wl.id, &wl.sha[..12]);
        return 2;
    }
    let borg = o.borg.clone().unwrap_or_else(|| root.join("target/release/borg"));
    let Ok(borg) = fs::canonicalize(&borg) else {
        eprintln!("borg-eval hive: {} not found (cargo build --release -p borg-ai)", borg.display());
        return 2;
    };
    let o = Opts { claude: o.claude.as_ref().map(|c| fs::canonicalize(c).unwrap_or_else(|_| c.clone())), ..o };
    let run_dir = runs.join(&o.run);
    let results = run_dir.join("results.jsonl");
    let cell = run_dir.join(format!("{}-{}-s{}", wl.id, o.arm_name(), o.seed));
    let name = cell.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let attempt = format!("{name}-{}", (now_s() * 1000.0) as u64);
    let reserve = o.arm.reserve(o.cap);
    let record = json!({"cell": name, "attempt": attempt, "cap_usd": o.cap, "reserve_usd": reserve});
    if let Err((code, e)) = start_cell(&run_dir, &cell, &record, reserve, o.stop_at) {
        eprintln!("borg-eval hive: {e}");
        return code;
    }
    let env = Env { root, real_home, borg };
    let mut row = base_row(&o, &wl, &env);
    row.insert("cell".into(), json!(name));
    row.insert("attempt".into(), json!(attempt));
    eprintln!("hive cell {} · run {} · {}", cell.file_name().unwrap_or_default().to_string_lossy(), o.run, cell.display());
    let done = match o.arm {
        Arm::A | Arm::B => arm_single(&o, &wl, &env, &cell, &mut row),
        Arm::F => arm_f(&o, &wl, &env, &cell, &mut row),
        Arm::D3 => arm_d3(&o, &wl, &env, &cell, &mut row),
        Arm::E => arm_e::run(&o, &wl, &env, &cell, &mut row),
    };
    if let Err(e) = &done {
        eprintln!("  error: {e}");
        row.insert("error".into(), json!(e));
        // "setup": no model call was made (nothing measured, rerun the cell); "after-session": the
        // agents ran and their dollars and wall are in the row.
        row.insert("error_stage".into(), json!(if row["paid"] == true { "after-session" } else { "setup" }));
    }
    let row = Value::Object(row);
    if let Err(e) = append_row(&results, &row) {
        eprintln!("borg-eval hive: {}: {e}", results.display());
        return 1;
    }
    eprintln!(
        "  wall {:.0}s · ${} · share {} · visible {} · conflicts {} · hacked {} → {}",
        row["wall_s"].as_f64().unwrap_or(0.0),
        row["charged_usd"].as_f64().map_or("?".into(), usd),
        row["hidden"]["solve_share"],
        row["visible_pass"],
        row["merge_conflicts"],
        row["hacked"],
        results.display()
    );
    i32::from(done.is_err())
}

/// Validates a workload into `dir`: every hidden check fails on base and passes on the reference; the
/// visible check passes on the reference (F picks by it) and, for a parallel workload, on the base too
/// (the injected bugs get past the team's own suite).
fn validate_in(wl: &Workload, dir: &Path, bx: &Jail) -> Result<(bool, Value), String> {
    let ws = dir.join("work").join(&wl.id);
    let base = build_workspace(wl, &ws)?;
    let on_base = score(bx, wl, &ws, &base, &base, &dir.join("score-base"), Which::Both)?;
    let reference = wl.reference.as_ref().ok_or("no reference patch")?;
    let (ok, _, out) = run_t(
        &ws,
        &format!(
            "{GIT} apply --whitespace=nowarn {} && {GIT} add -A && {GIT} commit -q -m reference && {GIT} rev-parse HEAD",
            q(reference.display().to_string())
        ),
        600,
    );
    if !ok {
        return Err(format!("reference.patch: {}", out.trim()));
    }
    let head = out.trim().lines().last().unwrap_or_default().to_string();
    let on_ref = score(bx, wl, &ws, &base, &head, &dir.join("score-ref"), Which::Both)?;
    let visible_ok = on_ref.visible_pass == Some(true) && (wl.shape != "parallel" || on_base.visible_pass == Some(true));
    // A check that cannot run on the base (its held-out test does not compile there) would fail every
    // partial solution too: the workload says which exit code is a failed assertion.
    let base_rc_ok = wl.base_fail_rc.is_none_or(|rc| on_base.checks.iter().all(|c| c.rc == rc));
    let pass = on_base.checks.iter().all(|c| !c.pass) && on_ref.checks.iter().all(|c| c.pass) && visible_ok && base_rc_ok;
    let checks: Vec<Value> = wl
        .checks
        .iter()
        .zip(on_base.checks.iter().zip(&on_ref.checks))
        .map(|(c, (b, r))| json!({"id": c.id, "base_pass": b.pass, "base_rc": b.rc, "base_timeout": b.timeout, "ref_pass": r.pass, "ref_timeout": r.timeout, "base_ms": b.dur_ms, "ref_ms": r.dur_ms}))
        .collect();
    let v = json!({"workload": wl.id, "workload_sha": wl.sha, "pass": pass, "K": wl.k(), "base": base, "reference_head": head, "checks": checks,
                   "base_fail_rc": wl.base_fail_rc, "base_rc_ok": base_rc_ok,
                   "visible_base": on_base.visible_pass, "visible_ref": on_ref.visible_pass, "dir": dir.display().to_string()});
    Ok((pass, v))
}

fn validate_cmd(raw: &[String]) -> i32 {
    // `--timeout-s` is taken for symmetry with the arms; every check is bounded by its own timeout.
    let mut workload = None;
    let mut it = raw.iter();
    while let Some(f) = it.next() {
        match (f.as_str(), it.next()) {
            ("--workload", Some(v)) => workload = Some(PathBuf::from(v)),
            ("--timeout-s", Some(v)) if v.parse::<u32>().is_ok_and(|t| t > 0) => {}
            (other, _) => {
                eprintln!("borg-eval hive validate: unknown flag or missing value {other}");
                return 2;
            }
        }
    }
    let Some(workload) = workload else {
        eprintln!("borg-eval hive validate: --workload DIR is required");
        return 2;
    };
    let root = repo_root();
    let runs = root.join("target/eval-runs");
    let real_home = real_home();
    let wl = match load_workload(&workload, &real_home, &runs) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("borg-eval hive validate: {e}");
            return 2;
        }
    };
    let dir = runs.join("hive-validate").join(format!("{}-{}", wl.id, now_s() as u64));
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("borg-eval hive validate: {e}");
        return 1;
    }
    let bx = Jail::scorer(&dir, &wl, &real_home);
    let (pass, v) = match validate_in(&wl, &dir, &bx) {
        Ok(x) => x,
        Err(e) => (false, json!({"workload": wl.id, "workload_sha": wl.sha, "pass": false, "error": e})),
    };
    println!("validate {} ({}): {}", wl.id, &wl.sha[..12], if pass { "PASS" } else { "FAIL" });
    println!("| check | base | reference |");
    println!("|---|---|---|");
    for c in v["checks"].as_array().into_iter().flatten() {
        let cell = |p: &str, t: &str| {
            if c[t] == true {
                "timeout"
            } else if c[p] == true {
                "pass"
            } else {
                "fail"
            }
        };
        println!("| {} | {} | {} |", c["id"].as_str().unwrap_or("?"), cell("base_pass", "base_timeout"), cell("ref_pass", "ref_timeout"));
    }
    println!("visible: base {} · reference {}", v["visible_base"], v["visible_ref"]);
    if let Some(rc) = v["base_fail_rc"].as_i64() {
        println!("base fails by assertion (every check exits {rc} on the base): {}", v["base_rc_ok"]);
    }
    if let Some(e) = v["error"].as_str() {
        println!("error: {e}");
    }
    if let Err(e) = append_row(&validate_log(&runs), &json!({"validate": v, "ms": (now_s() * 1000.0) as i64})) {
        eprintln!("borg-eval hive validate: {e}");
        return 1;
    }
    i32::from(!pass)
}

// ------------------------------------------------------------------------------------- report

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    Some(if n % 2 == 1 { xs[n / 2] } else { (xs[n / 2 - 1] + xs[n / 2]) / 2.0 })
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

/// Rounding slack for the boundary of the pre-registered inequalities (they are inclusive).
const EPS: f64 = 1e-9;

/// The rows a cell's numbers are read from: the runs that completed without an error. An errored row
/// (a harness failure; its share was never scored, or its agents never ran) is left out of BOTH terms
/// alike and counted in the Errors column; drive.py reruns such a cell.
fn clean<'a>(rs: &[&'a Value]) -> Vec<&'a Value> {
    rs.iter().copied().filter(|r| r["error"].is_null()).collect()
}

fn wall_of(rs: &[&Value]) -> Option<f64> {
    median(rs.iter().filter_map(|r| r["wall_s"].as_f64()).collect())
}

fn share_of(rs: &[&Value]) -> Option<f64> {
    mean(&rs.iter().map(|r| r["hidden"]["solve_share"].as_f64().unwrap_or(0.0)).collect::<Vec<_>>())
}

/// Seeds per cell in the pre-registered rule.
const SEEDS: usize = 3;

/// The pre-registered H1 on one workload: (holds, the line), over each arm's `clean` rows. None when
/// D3 or B has none.
fn h1(d3: &[&Value], b: &[&Value], k: usize) -> Option<(bool, String)> {
    let (d3, b) = (clean(d3), clean(b));
    let (dw, bw, ds, bs) = (wall_of(&d3)?, wall_of(&b)?, share_of(&d3)?, share_of(&b)?);
    let slack = 1.0 / k.max(1) as f64;
    let holds = dw <= 0.8 * bw + EPS && ds >= bs - slack - EPS;
    let mut line = format!(
        "D3 median wall {dw:.0}s ≤ 0.8×B {bw:.0}s ({:.0}s)? AND D3 mean share {ds:.3} ≥ B {bs:.3} − 1/K ({:.3}, K={k})? → {}",
        0.8 * bw,
        bs - slack,
        if holds { "HOLDS" } else { "FAILS" }
    );
    if d3.len() < SEEDS || b.len() < SEEDS {
        line.push_str(&format!(" (only {} D3 and {} B runs without error: rerun the errored cells before reading this)", d3.len(), b.len()));
    }
    Some((holds, line))
}

fn report_text(rows: &[Value]) -> String {
    let mut out = String::new();
    let mut by: BTreeMap<(String, String), Vec<&Value>> = BTreeMap::new();
    let mut shapes: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for r in rows {
        let w = r["workload"].as_str().unwrap_or("?").to_string();
        // A goal sequence's row is under its learning arm (frozen, l0, l01: `Opts::arm_name`), never d3, and
        // its workloads get the H5 block (hive_seq.rs), not H1.
        by.entry((w.clone(), r["arm"].as_str().unwrap_or("?").to_string())).or_default().push(r);
        if !r["seq"].is_object() {
            shapes.insert(w, (r["shape"].as_str().unwrap_or("?").to_string(), r["K"].as_u64().unwrap_or(1) as usize));
        }
    }
    out.push_str(
        "| Workload | Arm | n | Median wall s | Mean solve share | Mean $ charged | Mean tokens | Mean rework | Conflicts | Hacked | Over cap | Errors |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let f = |x: Option<f64>, p: usize| x.map_or("-".to_string(), |x| format!("{x:.p$}"));
    for ((w, arm), rs) in &by {
        // Wall and share as H1 reads them (rows without error); dollars over every row that ran agents.
        let ok = clean(rs);
        let (wall, share) = (wall_of(&ok), share_of(&ok));
        let paid: Vec<&Value> = rs.iter().copied().filter(|r| r["paid"] != false).collect();
        let cost = mean(&paid.iter().map(|r| charged_of(r)).collect::<Vec<_>>());
        let tokens = mean(
            &paid
                .iter()
                .map(|r| ["input", "output", "cache_read", "cache_creation"].iter().map(|k| r["tokens"][k].as_u64().unwrap_or(0)).sum::<u64>() as f64)
                .collect::<Vec<_>>(),
        );
        let rework = mean(&rs.iter().filter_map(|r| r["rework"].as_f64()).collect::<Vec<_>>());
        let sum = |k: &str| rs.iter().map(|r| r[k].as_u64().unwrap_or(0)).sum::<u64>();
        let over = rs.iter().filter(|r| r["over_cap"] == true).count();
        let errors = rs.iter().filter(|r| !r["error"].is_null()).count();
        let incomplete = paid.iter().filter(|r| r["cost_complete"] != true).count();
        out.push_str(&format!(
            "| {w} | {arm} | {} | {} | {} | {}{} | {} | {} | {} | {} | {over} | {errors} |\n",
            rs.len(),
            f(wall, 0),
            f(share, 3),
            f(cost, 2),
            if incomplete > 0 { format!(" ({incomplete} bounded)") } else { String::new() },
            f(tokens, 0),
            f(rework, 2),
            sum("merge_conflicts"),
            sum("hacked"),
        ));
    }
    out.push('\n');
    let mut verdicts: Vec<(String, String, Option<bool>)> = Vec::new();
    for (w, (shape, k)) in &shapes {
        let get = |arm: &str| by.get(&(w.clone(), arm.to_string())).cloned().unwrap_or_default();
        match h1(&get("d3"), &get("b"), *k) {
            Some((holds, line)) => {
                out.push_str(&format!("H1 {w} ({shape}): {line}\n"));
                verdicts.push((w.clone(), shape.clone(), Some(holds)));
            }
            None => {
                out.push_str(&format!("H1 {w} ({shape}): no D3 or no B rows → not decided\n"));
                verdicts.push((w.clone(), shape.clone(), None));
            }
        }
    }
    let holding: Vec<&(String, String, Option<bool>)> = verdicts.iter().filter(|v| v.2 == Some(true)).collect();
    let decided = verdicts.iter().all(|v| v.2.is_some());
    if decided && !verdicts.is_empty() && holding.is_empty() {
        out.push_str("Kill: H1 fails on every workload → stop before mesh sync of hive lines; write up why.\n");
    } else if decided && holding.len() < verdicts.len() {
        let names: Vec<String> = holding.iter().map(|v| format!("{} ({})", v.1, v.0)).collect();
        out.push_str(&format!("H1 holds on one shape: {} → report that shape, continue only for it.\n", names.join(", ")));
    } else if decided && !verdicts.is_empty() {
        out.push_str("H1 holds on every workload.\n");
    }
    out.push_str("3 seeds per cell: point estimates, no CI. Judge-free: hidden checks, wall clock and dollars only.\n");
    out
}

// ------------------------------------------------------------------------------------- tests

// The pilot runs on Linux only: its sandboxes are bwrap, and the scoring scripts bound each check with
// coreutils' `timeout` inside the scorer sandbox.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-hive-eval-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(d).unwrap()
    }

    fn git(dir: &Path, cmd: &str) -> String {
        let (ok, _, out) = run_t(dir, cmd, 60);
        assert!(ok, "{cmd}: {out}");
        out.trim().to_string()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// A source repo: two commits and a branch; returns (src, first commit).
    fn source(root: &Path) -> (PathBuf, String) {
        let src = root.join("src-repo");
        fs::create_dir_all(src.join("tests")).unwrap();
        fs::write(src.join("a.txt"), "broken\n").unwrap();
        fs::write(src.join("tests/sim_edit.txt"), "original\n").unwrap();
        git(&src, "git init -q -b main && git add -A && git -c user.name=x -c user.email=x@y commit -q -m one");
        let first = git(&src, "git rev-parse HEAD");
        fs::write(src.join("later.txt"), "future\n").unwrap();
        git(&src, "git add -A && git -c user.name=x -c user.email=x@y commit -q -m two && git branch side && git tag v1");
        (src, first)
    }

    /// A workload dir with hidden checks, spec, reference patch; `edit` changes workload.json first.
    fn workload(root: &Path, edit: impl FnOnce(&mut Value)) -> (PathBuf, PathBuf, PathBuf) {
        let (src, first) = source(root);
        let wd = root.join("wl");
        fs::create_dir_all(wd.join("hidden")).unwrap();
        fs::write(wd.join("spec.md"), "Fix a.txt.\n").unwrap();
        fs::write(wd.join("hidden/marker"), "secret\n").unwrap();
        fs::write(wd.join("reference.patch"), "diff --git a/r/a.txt b/r/a.txt\n--- a/r/a.txt\n+++ b/r/a.txt\n@@ -1 +1 @@\n-broken\n+fixed\n").unwrap();
        let mut v = json!({
            "id": "w-test", "shape": "parallel",
            "repos": [{"src": src.display().to_string(), "commit": first, "dir": "r"}],
            "inject": null, "spec": "spec.md", "visible": "true", "visible_timeout_s": 30,
            "hidden": {"dir": "hidden", "checks": [{"id": "c1", "cmd": "grep -q fixed \"$BORG_WORK/r/a.txt\"", "timeout_s": 30}]},
            "reference": "reference.patch",
            "hive": {"tasks": ["t1|work|fix a.txt|r/a.txt"]},
            "home_ro": [], "env": {}
        });
        edit(&mut v);
        fs::write(wd.join("workload.json"), v.to_string()).unwrap();
        (wd, src, root.join("runs"))
    }

    fn load(wd: &Path, root: &Path, runs: &Path) -> Result<Workload, String> {
        load_workload(wd, root, runs)
    }

    #[test]
    fn options_need_a_positive_cap_and_known_arms_and_flags() {
        let base = ["--workload", "w", "--arm", "a", "--seed", "0"];
        let with = |extra: &[&str]| parse_opts(&s(&[&base[..], extra].concat()));
        assert!(with(&[]).unwrap_err().contains("--cap-usd"), "required, no default");
        for bad in ["0", "-1", "inf", "NaN"] {
            assert!(with(&["--cap-usd", bad]).is_err(), "{bad}");
        }
        let o = with(&["--cap-usd", "6"]).unwrap();
        assert_eq!((o.arm, o.cap, o.model.as_str(), o.timeout_s), (Arm::A, 6.0, "sonnet", 3600));
        assert!(parse_opts(&s(&["--workload", "w", "--arm", "c", "--seed", "0", "--cap-usd", "1"])).unwrap_err().contains("--arm c"));
        assert!(with(&["--cap-usd", "1", "--frobnicate", "x"]).unwrap_err().contains("unknown flag"));
        assert!(with(&["--cap-usd", "1", "--harness", "sim"]).is_err(), "sim is a d3 harness");
        // A goal sequence's flags: d3 only, a chain is one learning arm's, a simulated learner needs a fake.
        let d3 = |extra: &[&str]| parse_opts(&s(&[&["--workload", "w", "--arm", "d3", "--seed", "1", "--cap-usd", "2"][..], extra].concat()));
        assert!(with(&["--cap-usd", "1", "--learn", "l01"]).unwrap_err().contains("d3"));
        assert!(d3(&["--learn", "l2"]).unwrap_err().contains("frozen, l0 or l01"));
        assert!(d3(&["--chain", "/c"]).unwrap_err().contains("--learn"));
        assert!(d3(&["--learn", "l01", "--harness", "sim"]).unwrap_err().contains("--claude"));
        assert!(d3(&["--learn", "frozen", "--harness", "sim"]).unwrap_err().contains("--claude"), "frozen runs the consequence model too");
        assert!(d3(&["--goal", "Bad_Id"]).unwrap_err().contains("--goal"));
        let o = d3(&["--learn", "l0", "--chain", "/c", "--goal", "h5-g1", "--position", "3"]).unwrap();
        assert_eq!((o.arm_name(), o.goal.as_str(), o.position, o.chain.as_deref()), ("l0", "h5-g1", Some(3), Some(Path::new("/c"))));
        assert_eq!((d3(&[]).unwrap().arm_name(), d3(&[]).unwrap().goal.as_str()), ("d3", "pilot"));
        assert_eq!(usd(10.0 / 3.0), "3.33");
        let f = session_args(Arm::F, "P", "sonnet", 10.0 / 3.0);
        assert_eq!(f[f.iter().position(|a| a == "--max-budget-usd").unwrap() + 1], "3.33");
    }

    #[test]
    fn each_arm_gets_its_argv() {
        let at = |a: &[String], f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
        let a = session_args(Arm::A, "P", "sonnet", 6.0);
        assert_eq!(at(&a, "--disallowedTools").as_deref(), Some("Task Agent"));
        assert_eq!(at(&a, "--max-budget-usd").as_deref(), Some("6.00"));
        assert_eq!(at(&a, "--model").as_deref(), Some("sonnet"));
        assert_eq!(at(&a, "-p").as_deref(), Some("P"));
        assert_eq!(at(&a, "--permission-mode").as_deref(), Some("bypassPermissions"));
        let b = session_args(Arm::B, "P", "sonnet", 6.0);
        assert!(at(&b, "--disallowedTools").is_none());
        assert!(at(&b, "-p").unwrap().ends_with(INVITE) && at(&b, "-p").unwrap().starts_with("P\n\n"));
        assert_eq!(at(&b, "--max-budget-usd").as_deref(), Some("6.00"));
        let f = session_args(Arm::F, "P", "sonnet", 2.0);
        assert_eq!((at(&f, "--max-budget-usd").as_deref(), at(&f, "--disallowedTools").as_deref()), (Some("2.00"), Some("Task Agent")));

        let root = tmp("argv");
        let (wd, _, runs) = workload(&root, |_| {});
        let wl = load(&wd, &root, &runs).unwrap();
        let init = d3_init_args(&wl, Path::new("/h/goal/spec.md"), 6.0, "pilot");
        assert_eq!(init[3], "pilot");
        assert_eq!(d3_init_args(&wl, Path::new("/s"), 6.0, "h5-g1")[3], "h5-g1", "the goal --goal names");
        assert_eq!(at(&init, "--budget").as_deref(), Some("6"));
        assert_eq!(at(&init, "--oracle").as_deref(), Some("visible=true"));
        assert_eq!(at(&init, "--task").as_deref(), Some("t1|work|fix a.txt|r/a.txt"));
        assert!(!init.iter().any(|x| x.starts_with("--hidden")), "no hidden oracle in the sandbox: {init:?}");
        let run = d3_run_args("sonnet", "abc", 3600, false, "pilot");
        assert_eq!(d3_run_args("sonnet", "abc", 1, false, "h5-g1")[3], "h5-g1");
        assert_eq!(at(&run, "--drones").as_deref(), Some("3"));
        assert_eq!(at(&run, "--base").as_deref(), Some("abc"));
        assert_eq!(at(&run, "--iter-timeout").as_deref(), Some("3600"));
        assert_eq!(at(&run, "--model").as_deref(), Some("sonnet"));
        assert!(!run.contains(&"--harness".to_string()));
        assert!(run.contains(&"--yes".to_string()), "an eval run never waits for the plan's approval");
        assert!(d3_run_args("sonnet", "abc", 1, true, "pilot").ends_with(&s(&["--harness", "sim"])));
        assert!(wl.prompt().ends_with("Visible check: run `true` from the workspace root."));
        let _ = fs::remove_dir_all(root);
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn agent_and_scorer_sandboxes_are_isolated() {
        let root = tmp("iso");
        let (wd, src, runs) = workload(&root, |v| v["env"] = json!({"GRADLE_RO_DEP_CACHE": "$HOME/.gradle/caches"}));
        let wl = load(&wd, &root, &runs).unwrap();
        let home = root.join("cell/home");
        fs::create_dir_all(&home).unwrap();
        let ag = Agent {
            home: home.clone(),
            ws: home.join("work/w-test"),
            base: String::new(),
            secrets: None,
            env: agent_env(&home, &wl, None, None, "sonnet"),
            home_binds: vec![(root.join(".gradle/jdks"), PathBuf::from(".gradle/jdks"))],
            ro: vec![PathBuf::from("/b/borg")],
        };
        let (cmd, private) = sandbox_with(&ag.ws, &[&ag.home], &[], &ag.extra(), 5);
        let _ = fs::remove_dir_all(private);
        let a = args_of(&cmd);
        let at = |w: &[&str]| a.windows(w.len()).position(|x| x == w);
        let real = std::env::var("HOME").unwrap();
        assert!(at(&["--tmpfs", &real]).is_some(), "{a:?}");
        let clear = at(&["--clearenv"]).expect("--clearenv");
        let h = home.display().to_string();
        for (k, v) in [
            ("BORG_HOME", format!("{h}/.local/share/borg")),
            ("BORG_CONFIG", format!("{h}/.config/borg/collectives.json")),
            ("BORG_LEARN", "off".into()),
            ("BORG_QUEEN", "off".into()),
            ("BORG_PREDICT_MODEL", "off".into()),
            ("BORG_MATCH", "bm25".into()),
            ("GRADLE_RO_DEP_CACHE", format!("{h}/.gradle/caches")),
        ] {
            assert!(at(&["--setenv", k, &v]).is_some_and(|i| i > clear), "{k}={v}: {a:?}");
        }
        // The home_ro land in the sandbox HOME, after the HOME's own (writable) bind.
        let jdk = at(&["--ro-bind", &root.join(".gradle/jdks").display().to_string(), &home.join(".gradle/jdks").display().to_string()]).expect("home_ro");
        assert!(jdk > at(&["--bind", &h, &h]).unwrap());
        assert!(!a.iter().any(|x| x == "--unshare-net"), "agents may reach the API");
        assert!(a.iter().any(|x| x == "--unshare-pid"), "no agent process outlives its sandbox: {a:?}");
        let sources: Vec<&String> = a.windows(2).filter(|w| w[0] == "--bind" || w[0] == "--ro-bind").map(|w| &w[1]).collect();
        let forbidden = [
            format!("{real}/.local/share/borg"),
            format!("{real}/.config/borg"),
            format!("{real}/src-repos"),
            wl.hidden_dir.display().to_string(),
            src.display().to_string(),
        ];
        for f in &forbidden {
            assert!(!sources.iter().any(|s| s.starts_with(f.as_str())), "{f} bound: {a:?}");
        }
        // The scorer: no network, no credentials, its own empty HOME.
        let bx = Jail::scorer(&root.join("cell"), &wl, &root);
        let Jail::Sandbox { homes, env, home_ro, .. } = &bx else { panic!() };
        let scorer_home = Jail::fresh_home(homes).unwrap();
        assert_ne!(Jail::fresh_home(homes).unwrap(), scorer_home, "each call its own HOME");
        let (cmd, private) = sandbox_with(&root, &[&root], &[&wl.hidden_dir], &Jail::extra(&scorer_home, &env_in(env, &scorer_home), home_ro), 5);
        let _ = fs::remove_dir_all(private);
        let a = args_of(&cmd);
        assert!(a.contains(&"--unshare-net".to_string()) && a.contains(&"--unshare-pid".to_string()) && a.contains(&"--clearenv".to_string()));
        assert!(!a.iter().any(|x| x.ends_with(".credentials.json") || x.ends_with(".claude.json")), "{a:?}");
        assert!(a.windows(3).any(|w| w[0] == "--setenv" && w[1] == "HOME" && w[2] == scorer_home.display().to_string()));
        let _ = fs::remove_dir_all(root);
    }

    /// bwrap itself: no host Unix socket (a Docker daemon, ssh-agent, gpg-agent, the session bus)
    /// answers inside an agent's or the scorer's sandbox; a read-only bind does not stop `connect()`.
    /// Skipped where bwrap cannot run.
    #[test]
    fn sandboxes_reach_no_host_socket() {
        if !Command::new("bwrap").args(["--ro-bind", "/", "/", "--unshare-pid", "true"]).status().is_ok_and(|s| s.success()) {
            eprintln!("bwrap unavailable: skipped");
            return;
        }
        let root = tmp("sock");
        let (home, scorer) = (root.join("home"), root.join("scorer"));
        for d in [&home, &scorer] {
            fs::create_dir_all(d).unwrap();
        }
        let agent = Extra { sandbox_home: Some(home.clone()), unshare_pid: true, ..Extra::default() };
        let probe = "find /run/ /var/run/ -type s 2>/dev/null; [ -e /var/run/docker.sock ] && echo docker.sock; [ -e /run/user ] && echo /run/user; true";
        for (what, extra) in [("agent", agent), ("scorer", Jail::extra(&scorer, &[], &[]))] {
            let (mut cmd, private) = sandbox_with(&root, &[&root], &[], &extra, 10);
            let out = cmd.args(["sh", "-c", probe]).stdin(Stdio::null()).output().unwrap();
            let _ = fs::remove_dir_all(private);
            assert!(out.status.success(), "{what}: {}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "", "{what}: host sockets reachable");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workload_json_is_refused_when_it_could_leak() {
        let root = tmp("wl");
        let (wd, src, runs) = workload(&root, |_| {});
        assert!(load(&wd, &root, &runs).is_ok());
        let refuse = |edit: &dyn Fn(&mut Value), want: &str| {
            let mut v: Value = serde_json::from_str(&fs::read_to_string(wd.join("workload.json")).unwrap()).unwrap();
            edit(&mut v);
            let t = root.join(format!("bad-{}", want.len()));
            let _ = fs::remove_dir_all(&t);
            fs::create_dir_all(&t).unwrap();
            for f in ["spec.md", "reference.patch"] {
                fs::copy(wd.join(f), t.join(f)).unwrap();
            }
            fs::write(t.join("workload.json"), v.to_string()).unwrap();
            let err = load_workload(&t, &root, &runs).map(|_| ()).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        };
        let hidden = wd.join("hidden").display().to_string();
        let set_hidden = |v: &mut Value| v["hidden"]["dir"] = json!(hidden);
        let set_hidden = &set_hidden;
        refuse(&|v| v["repos"][0]["commit"] = json!("main"), "40-hex");
        refuse(&|v| v["repos"][0]["commit"] = json!("abc123"), "40-hex");
        for deny in [
            ".claude",
            ".claude/skills",
            ".",
            ".local",
            ".local/share/borg/eval",
            ".config/borg",
            ".ssh",
            ".gradle/gradle.properties",
            ".m2/settings.xml",
            ".aws",
        ] {
            refuse(
                &|v: &mut Value| {
                    set_hidden(v);
                    v["home_ro"] = json!([deny]);
                },
                "home_ro",
            );
        }
        refuse(
            &|v: &mut Value| {
                set_hidden(v);
                v["home_ro"] = json!(["../x"]);
            },
            "plain path",
        );
        // A home_ro holding a source repo (the fake HOME is `root`).
        refuse(
            &|v: &mut Value| {
                set_hidden(v);
                v["home_ro"] = json!([src.file_name().unwrap().to_string_lossy()]);
            },
            "overlaps",
        );
        // The hidden checks inside a source, or where workspaces are built.
        let in_src = src.join("held");
        fs::create_dir_all(&in_src).unwrap();
        refuse(&|v| v["hidden"]["dir"] = json!(in_src.display().to_string()), "hidden dir");
        fs::create_dir_all(runs.join("x")).unwrap();
        refuse(&|v| v["hidden"]["dir"] = json!(runs.join("x").display().to_string()), "hidden dir");
        refuse(
            &|v: &mut Value| {
                set_hidden(v);
                v["hive"]["tasks"] = json!(["b1|work|fix the a | b operator"]);
            },
            "`|` inside the title",
        );
        refuse(
            &|v: &mut Value| {
                set_hidden(v);
                v["hive"]["tasks"] = json!(["b 1|work|fix"]);
            },
            "bad id",
        );
        refuse(
            &|v: &mut Value| {
                set_hidden(v);
                v["extra"] = json!(1);
            },
            "unknown field",
        );
        refuse(
            &|v: &mut Value| {
                set_hidden(v);
                v["env"] = json!({"BORG_LEARN": "on"});
            },
            "not settable",
        );
        assert!(check_task("plan|plan|Change the API: rename x to y, adopt in app.").is_ok());
        assert!(check_task("t1|work|write f1|f1.txt|t0").is_ok());
        assert!(check_task("app|work|adopt the lib's frames||lib").is_ok(), "an empty files field");
        let plan = |ts: &[&str]| check_deps(&ts.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        assert_eq!(plan(&["app|work|L||lib", "lib|work|M"]), Ok(()));
        assert_eq!(plan(&["app|work|L||lbi", "lib|work|M"]).unwrap_err(), "task app: its dep lbi names no task of the workload");
        assert_eq!(plan(&["a|work|A||b", "b|work|B||a", "c|work|C"]).unwrap_err(), "tasks a, b depend on each other in a cycle");
        let _ = fs::remove_dir_all(root);
    }

    /// The agent sandbox hides HOME and /tmp only: the workload (its hidden checks' commands), the
    /// hidden checks, the reference solution and the cells must lie under one of them, and no
    /// `home_ro` may bind the reference back in.
    #[test]
    fn secrets_must_lie_where_the_agent_sandbox_hides_them() {
        let root = tmp("wl-hide");
        let (wd, _, runs) = workload(&root, |_| {});
        assert!(load(&wd, &root, &runs).is_ok());
        let outside = repo_root().join("target/hive-test").join(format!("outside-{}", std::process::id()));
        if outside.starts_with("/tmp") {
            eprintln!("checkout under /tmp: skipped");
            return;
        }
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(outside.join("hidden")).unwrap();
        fs::copy(wd.join("reference.patch"), outside.join("reference.patch")).unwrap();
        let original = fs::read_to_string(wd.join("workload.json")).unwrap();
        let with = |edit: &dyn Fn(&mut Value)| {
            let mut v: Value = serde_json::from_str(&original).unwrap();
            edit(&mut v);
            fs::write(wd.join("workload.json"), v.to_string()).unwrap();
            let r = load(&wd, &root, &runs).map(|_| ());
            fs::write(wd.join("workload.json"), &original).unwrap();
            r
        };
        let hidden = outside.join("hidden").display().to_string();
        assert!(with(&|v| v["hidden"]["dir"] = json!(hidden)).unwrap_err().contains("hides"));
        let reference = outside.join("reference.patch").display().to_string();
        assert!(with(&|v| v["reference"] = json!(reference)).unwrap_err().contains("hides"));
        assert!(load(&wd, &root, &outside.join("runs")).unwrap_err().contains("hides"), "cells outside");
        fs::write(outside.join("workload.json"), &original).unwrap();
        fs::copy(wd.join("spec.md"), outside.join("spec.md")).unwrap();
        let abs_hidden = wd.join("hidden").display().to_string();
        let mut v: Value = serde_json::from_str(&original).unwrap();
        v["hidden"]["dir"] = json!(abs_hidden);
        fs::write(outside.join("workload.json"), v.to_string()).unwrap();
        assert!(load(&outside, &root, &runs).unwrap_err().contains("hides"), "workload dir outside");
        // A home_ro holding the reference binds the solution into every agent's HOME.
        fs::create_dir_all(root.join("refs")).unwrap();
        fs::copy(wd.join("reference.patch"), root.join("refs/reference.patch")).unwrap();
        let in_refs = root.join("refs/reference.patch").display().to_string();
        assert!(
            with(&|v| {
                v["reference"] = json!(in_refs);
                v["home_ro"] = json!(["refs"]);
            })
            .unwrap_err()
            .contains("overlaps")
        );
        let _ = fs::remove_dir_all(outside);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_symlinked_home_ro_is_checked_where_it_resolves_and_bound_resolved() {
        let root = tmp("wl-link");
        let (wd, src, runs) = workload(&root, |_| {});
        let hidden = fs::canonicalize(wd.join("hidden")).unwrap();
        fs::create_dir_all(root.join(".local/share/borg/eval")).unwrap();
        fs::create_dir_all(root.join(".gradle/caches/modules-2")).unwrap();
        fs::create_dir_all(root.join(".gradle/caches/build-cache-1")).unwrap();
        fs::create_dir_all(&runs).unwrap();
        fs::create_dir_all(root.join(".cache/ok")).unwrap();
        let link = |name: &str, to: &Path| std::os::unix::fs::symlink(to, root.join(".cache").join(name)).unwrap();
        link("store", &root.join(".local/share/borg/eval"));
        link("hid", &hidden);
        link("source", &src);
        link("runs", &runs);
        link("home", &root);
        link("deps", &root.join(".gradle/caches/modules-2"));
        let check = |rel: &str| check_home_ro(rel, &root, std::slice::from_ref(&src), &[&hidden, &runs]);
        for bad in [".cache/store", ".cache/hid", ".cache/source", ".cache/runs", ".cache/home"] {
            assert!(check(bad).is_err(), "{bad} resolves into something no sandbox may see");
        }
        assert!(check(".cache/deps").is_ok() && check(".cache/ok").is_ok());
        let mut wl = load(&wd, &root, &runs).unwrap();
        wl.home_ro = vec![".cache/deps".into(), ".cache/missing".into()];
        assert_eq!(wl.home_binds(&root), [(fs::canonicalize(root.join(".gradle/caches/modules-2")).unwrap(), PathBuf::from(".cache/deps"))]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_workspace_is_one_commit_and_the_source_is_untouched() {
        let root = tmp("ws");
        let (wd, src, runs) = workload(&root, |_| {});
        fs::write(wd.join("inject.patch"), "diff --git a/r/new.txt b/r/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/r/new.txt\n@@ -0,0 +1 @@\n+bug\n")
            .unwrap();
        let mut v: Value = serde_json::from_str(&fs::read_to_string(wd.join("workload.json")).unwrap()).unwrap();
        v["inject"] = json!("inject.patch");
        fs::write(wd.join("workload.json"), v.to_string()).unwrap();
        let wl = load(&wd, &root, &runs).unwrap();
        let state = |p: &Path| git(p, "git rev-parse HEAD && git for-each-ref && git status --porcelain && git stash list && git reflog | wc -l");
        let before = state(&src);
        let dst = root.join("cell/home/work/w-test");
        let base = build_workspace(&wl, &dst).unwrap();
        assert_eq!(state(&src), before, "the source is untouched");
        assert_eq!(git(&dst, "git log --all --oneline | wc -l"), "1");
        assert_eq!(git(&dst, "git rev-parse HEAD"), base);
        assert!(git(&dst, "git remote").is_empty() && git(&dst, "git tag").is_empty());
        assert_eq!(fs::read_to_string(dst.join("r/a.txt")).unwrap(), "broken\n");
        assert_eq!(fs::read_to_string(dst.join("r/new.txt")).unwrap(), "bug\n", "inject applied");
        assert!(!dst.join("r/later.txt").exists(), "the pinned commit, not the source's HEAD");
        assert!(!dst.join("r/.git").exists());
        assert!(git(&dst, "git status --porcelain").is_empty());
        assert!(build_workspace(&wl, &dst).is_err(), "never over an existing one");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn d3_scores_the_integrated_head_the_board_names() {
        let root = tmp("d3-head");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("tasks.jsonl");
        let task = |id: &str, role: board::Role| board::Task {
            id: id.into(),
            role,
            title: "t".into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        };
        let verdict_of = |ev: board::Ev, t: &str, by: &str, ms: i64, head: &str, note: &str| board::Event {
            head: head.into(),
            ..board::Event::new(ev, t, by, ms).with_note(note)
        };
        // t1 done by s1 and verified by s2.
        let accepted = |w: &str, ms: i64| {
            let v = board::Task { of: Some(w.into()), not_by: vec!["s1".into()], ..task(&format!("{w}.v1"), board::Role::Verify) };
            vec![
                board::Event::new(board::Ev::Claim, w, "s1", ms),
                board::Event::new(board::Ev::Done, w, "s1", ms + 1),
                board::Event::open(v, "s1", ms + 1),
                board::Event::new(board::Ev::Claim, &format!("{w}.v1"), "s2", ms + 2),
                board::Event::new(board::Ev::Verified, &format!("{w}.v1"), "s2", ms + 3),
            ]
        };
        let (base, m, fixed) = ("b".repeat(40), "a".repeat(40), "c".repeat(40));
        board::append(&path, &[vec![board::Event::open(task("t1", board::Role::Work), "e", 1)], accepted("t1", 2)].concat()).unwrap();
        let r = d3_result(&path, &base);
        assert_eq!(r, D3Result { head: base.clone(), integrated: false, integrations: 0, conflicts: 0 }, "nothing merged: the base is scored");
        let fix = board::Task { from: Some(m.clone()), ..task("integrate.1.fix", board::Role::Work) };
        let rejected = vec![
            board::Event::open(task("integrate.1", board::Role::Integrate), "i", 10),
            board::Event::new(board::Ev::Claim, "integrate.1", "i", 10),
            verdict_of(board::Ev::Rejected, "integrate.1", "i", 11, &m, "conflict: t2 onto t1 in s.txt"),
            board::Event::open(fix, "i", 11),
        ];
        board::append(&path, &rejected).unwrap();
        let r = d3_result(&path, &base);
        assert_eq!(r, D3Result { head: m.clone(), integrated: false, integrations: 1, conflicts: 1 }, "unfinished: the failed integration's tree");
        assert_eq!(board_rejects(&path), (0, 1), "a failed integration is a caught mistake");
        let passed = [
            accepted("integrate.1.fix", 12),
            vec![
                board::Event::open(task("integrate.2", board::Role::Integrate), "i", 20),
                board::Event::new(board::Ev::Claim, "integrate.2", "i", 20),
                verdict_of(board::Ev::Verified, "integrate.2", "x", 21, &base, "not the holder's"),
                verdict_of(board::Ev::Verified, "integrate.2", "i", 22, &fixed, "merged t1, integrate.1.fix; visible pass"),
            ],
        ]
        .concat();
        board::append(&path, &passed).unwrap();
        let r = d3_result(&path, &base);
        assert_eq!(r, D3Result { head: fixed, integrated: true, integrations: 2, conflicts: 1 });
        assert_eq!(parse_head(&format!("x\nHEAD {m}\n")), Some(m));
        assert_eq!(parse_head("HEAD 123\n"), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_result_commit_survives_a_stale_index_lock() {
        let root = tmp("lock");
        let ws = root.join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("a.txt"), "base\n").unwrap();
        git(&ws, &format!("{GIT} init -q -b main && {GIT} add -A && {GIT} commit -q -m base"));
        fs::write(ws.join("a.txt"), "agent\n").unwrap();
        fs::write(ws.join(".git/index.lock"), "").unwrap();
        let head = commit_result(&Jail::Plain, &ws).unwrap();
        assert_eq!(git(&ws, &format!("git show {head}:a.txt")), "agent");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn d3_drones_get_the_system_prompt_a_b_and_f_get() {
        let root = tmp("wrapper");
        let target = root.join("fake claude");
        fs::write(&target, "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let w = root.join("claude");
        fs::write(&w, claude_wrapper(&target)).unwrap();
        fs::set_permissions(&w, fs::Permissions::from_mode(0o755)).unwrap();
        let out = git(&root, &format!("{} -p 'drone prompt' --model sonnet", q(w.display().to_string())));
        let args: Vec<&str> = out.lines().collect();
        assert_eq!(args[..4], ["-p", "drone prompt", "--model", "sonnet"]);
        assert_eq!(args[4..], ["--append-system-prompt", crate::SYSTEM]);
        // borg's own structured call (the learner) is not a drone: no drone system prompt.
        let out = git(&root, &format!("{} -p 'reflect' --json-schema '{{}}' --model sonnet", q(w.display().to_string())));
        assert_eq!(out.lines().collect::<Vec<_>>(), ["-p", "reflect", "--json-schema", "{}", "--model", "sonnet"]);
        let a = session_args(Arm::A, "P", "sonnet", 1.0);
        assert_eq!(a[a.iter().position(|x| x == "--append-system-prompt").unwrap() + 1], crate::SYSTEM, "the same text");
        let _ = fs::remove_dir_all(root);
    }

    /// A cell keeps what it scored and why: the diff base..head (the integrated tree for D3) and the
    /// hidden checks' JUnit XML, from the tree and the sandbox's scratch, not the visible check's.
    #[test]
    fn a_scored_cell_keeps_its_diff_and_the_hidden_checks_junit_xml() {
        let root = tmp("keep");
        let scratch = root.join("scratch");
        let (wd, _, runs) = workload(&root, |v| {
            v["visible"] = json!("mkdir -p r/build/test-results/test && echo '<testsuite name=\"Visible\"/>' > r/build/test-results/test/TEST-Visible.xml");
            v["hidden"]["checks"] = json!([
                {"id": "c1", "cmd": "mkdir -p r/build/test-results/test && echo '<testsuite name=\"Held\"/>' > r/build/test-results/test/TEST-Held.xml && echo '<x/>' > r/notes.xml && grep -q fixed r/a.txt", "timeout_s": 30}
            ]);
        });
        let wl = load(&wd, &root, &runs).unwrap();
        let ws = root.join("cell/home/work/w-test");
        let base = build_workspace(&wl, &ws).unwrap();
        fs::write(ws.join("r/a.txt"), "fixed\n").unwrap();
        let (code, out) = Jail::Plain.exec(&ws, &[], &[], &result_script(), 60);
        let head = parse_head(&out).unwrap_or_else(|| panic!("{code}: {out}"));
        let dir = root.join("cell/score");
        let s = score(&Jail::Plain, &wl, &ws, &base, &head, &dir, Which::Both).unwrap();
        assert!(s.checks[0].pass && s.visible_pass == Some(true), "{s:?}");
        let patch = fs::read_to_string(dir.join("diff.patch")).unwrap();
        assert!(patch.contains("--- a/r/a.txt") && patch.contains("-broken\n+fixed\n"), "{patch}");
        let kept = |p: &str| dir.join("junit").join(p);
        assert_eq!(fs::read_to_string(kept("tree/r/build/test-results/test/TEST-Held.xml")).unwrap(), "<testsuite name=\"Held\"/>\n");
        assert!(!kept("tree/r/build/test-results/test/TEST-Visible.xml").exists(), "the visible check's reports are not the hidden tests'");
        assert!(!kept("tree/r/notes.xml").exists(), "only JUnit reports");
        // The visible check alone (F's selection) keeps no reports; the diff it does.
        let sel = root.join("cell/select");
        score(&Jail::Plain, &wl, &ws, &base, &head, &sel, Which::Visible).unwrap();
        assert!(sel.join("diff.patch").is_file() && !sel.join("junit").exists());
        // A scratch root (the sandbox's /tmp) is kept under its own name.
        fs::create_dir_all(scratch.join("copy/build/test-results/test")).unwrap();
        fs::write(scratch.join("copy/build/test-results/test/TEST-Tmp.xml"), "<t/>").unwrap();
        let out = root.join("cell/raw");
        fs::create_dir_all(out.join("tree")).unwrap();
        let (ok, _, log) = run_t(&out, &junit_script(&out, std::slice::from_ref(&scratch)), 30);
        assert!(ok, "{log}");
        assert_eq!(fs::read_to_string(out.join("junit/scratch/copy/build/test-results/test/TEST-Tmp.xml")).unwrap(), "<t/>");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hidden_checks_score_a_fresh_clone_and_never_enter_the_workspace() {
        let root = tmp("score");
        let (wd, _, runs) = workload(&root, |v| {
            v["hidden"]["checks"] = json!([
                {"id": "ok", "cmd": "grep -q fixed \"$BORG_WORK/r/a.txt\" && test -f \"$BORG_HIDDEN/marker\"", "timeout_s": 30},
                {"id": "no", "cmd": "exit 1", "timeout_s": 30},
                {"id": "slow", "cmd": "sleep 5", "timeout_s": 1},
                {"id": "untracked", "cmd": "test ! -e \"$BORG_WORK/junk.txt\" && test ! -e \"$BORG_WORK/build/out\"", "timeout_s": 30}
            ]);
        });
        let wl = load(&wd, &root, &runs).unwrap();
        let ws = root.join("cell/home/work/w-test");
        let base = build_workspace(&wl, &ws).unwrap();
        fs::write(ws.join("r/a.txt"), "fixed\n").unwrap();
        let (code, out) = Jail::Plain.exec(&ws, &[], &[], &result_script(), 60);
        let head = parse_head(&out).unwrap_or_else(|| panic!("{code}: {out}"));
        // Left after the result commit: untracked files in ws do not reach the scored tree.
        fs::write(ws.join("junk.txt"), "x").unwrap();
        fs::create_dir_all(ws.join("build")).unwrap();
        fs::write(ws.join("build/out"), "x").unwrap();
        let s = score(&Jail::Plain, &wl, &ws, &base, &head, &root.join("cell/score"), Which::Both).unwrap();
        let flags: Vec<(&str, bool, bool)> = s.checks.iter().map(|c| (c.id.as_str(), c.pass, c.timeout)).collect();
        assert_eq!(flags, [("ok", true, false), ("no", false, false), ("slow", false, true), ("untracked", true, false)]);
        assert!((s.share(wl.k()) - 0.5).abs() < 1e-12);
        assert_eq!(s.visible_pass, Some(true));
        assert!(s.hacked_paths.is_empty());
        assert_eq!(s.json(wl.k())["passed"], 2);
        // Three of them as the spec's minimal set: 1/3.
        let three = Score { checks: s.checks[..3].to_vec(), ..Score::default() };
        assert!((three.share(3) - 1.0 / 3.0).abs() < 1e-12);
        let mut found = Vec::new();
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            for e in fs::read_dir(d).into_iter().flatten().flatten() {
                if e.path().is_dir() {
                    walk(&e.path(), out);
                } else {
                    out.push(e.path());
                }
            }
        }
        walk(&ws, &mut found);
        assert!(!found.iter().any(|p| p.file_name().is_some_and(|n| n == "marker")), "no hidden file in ws");
        assert!(!root.join("cell/score/out/tree").exists(), "the scored tree is removed");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn validate_needs_every_check_failing_on_base_and_passing_on_the_reference() {
        let root = tmp("validate");
        let (wd, _, runs) = workload(&root, |_| {});
        let wl = load(&wd, &root, &runs).unwrap();
        let (pass, v) = validate_in(&wl, &root.join("v1"), &Jail::Plain).unwrap();
        assert!(pass, "{v}");
        assert_eq!(v["checks"][0]["base_pass"], false);
        assert_eq!(v["checks"][0]["ref_pass"], true);
        // A workload naming the exit code of a failed assertion: a check that fails on the base any other
        // way (it could not run there, as a held-out test that does not compile) fails the validation.
        let mut wl = load(&wd, &root, &runs).unwrap();
        wl.base_fail_rc = Some(1);
        let (pass, v) = validate_in(&wl, &root.join("v-rc1"), &Jail::Plain).unwrap();
        assert!(pass && v["checks"][0]["base_rc"] == 1 && v["base_rc_ok"] == true, "grep exits 1: {v}");
        wl.checks[0].cmd = "grep -q fixed \"$BORG_WORK/r/a.txt\" || { grep -q broken \"$BORG_WORK/r/a.txt\" && exit 3; }".into();
        let (pass, v) = validate_in(&wl, &root.join("v-rc3"), &Jail::Plain).unwrap();
        assert!(!pass && v["checks"][0]["base_rc"] == 3 && v["base_rc_ok"] == false, "exit 3 on the base: {v}");
        assert!(load(&wd, &root, &runs).unwrap().base_fail_rc.is_none(), "unset: any failure on the base counts");
        let root2 = tmp("validate2");
        let (wd, _, runs) = workload(&root2, |v| {
            v["hidden"]["checks"] = json!([{"id": "c1", "cmd": "grep -q fixed \"$BORG_WORK/r/a.txt\""}, {"id": "always", "cmd": "true"}]);
        });
        let wl = load(&wd, &root2, &runs).unwrap();
        let (pass, v) = validate_in(&wl, &root2.join("v1"), &Jail::Plain).unwrap();
        assert!(!pass, "a check passing on base: {v}");
        // The visible check must pass on the reference, and on a parallel workload's base too.
        let mut wl = load(&wd, &root2, &runs).unwrap();
        wl.checks.truncate(1);
        wl.visible = "grep -q broken r/a.txt".into();
        let (pass, v) = validate_in(&wl, &root2.join("v2"), &Jail::Plain).unwrap();
        assert!(!pass && v["visible_ref"] == false && v["visible_base"] == true, "{v}");
        wl.visible = "grep -q fixed r/a.txt".into();
        let (pass, v) = validate_in(&wl, &root2.join("v3"), &Jail::Plain).unwrap();
        assert!(!pass && v["visible_base"] == false, "parallel: the injected bugs get past the visible suite: {v}");
        wl.shape = "cross-repo".into();
        let (pass, v) = validate_in(&wl, &root2.join("v4"), &Jail::Plain).unwrap();
        assert!(pass, "cross-repo: red on base is expected: {v}");
        // The validate log gates the arms by the workload's sha.
        let log = validate_log(&runs);
        append_row(&log, &json!({"validate": {"workload_sha": "x", "pass": false}})).unwrap();
        assert!(!validated(&runs, "x"));
        append_row(&log, &json!({"validate": {"workload_sha": "x", "pass": true}})).unwrap();
        assert!(validated(&runs, "x") && !validated(&runs, "y"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(root2);
    }

    #[test]
    fn f_picks_visible_then_cost_then_index() {
        assert_eq!(choose(&[(false, Some(0.1)), (true, Some(0.9)), (true, Some(0.5))]), 2);
        assert_eq!(choose(&[(true, Some(0.5)), (true, Some(0.5))]), 0);
        assert_eq!(choose(&[(false, None), (false, Some(3.0)), (false, Some(3.0))]), 1);
        assert_eq!(choose(&[(true, None), (false, Some(0.1))]), 0);
        assert_eq!(choose(&[(false, None), (false, None), (false, None)]), 0);
    }

    #[test]
    fn cost_and_tokens_from_results_streams_spend_and_logs() {
        let r = json!({"type": "result", "total_cost_usd": 1.25, "num_turns": 7, "usage": {"input_tokens": 1},
            "modelUsage": {"claude-sonnet-4-5": {"inputTokens": 10, "outputTokens": 20, "cacheReadInputTokens": 30, "cacheCreationInputTokens": 40},
                           "claude-haiku-4-5": {"inputTokens": 1, "outputTokens": 2, "cacheReadInputTokens": 3, "cacheCreationInputTokens": 4}}});
        let c = session_cost(Some(&r), "", 6.0);
        assert_eq!(c.tokens, Tokens { input: 11, output: 22, cache_read: 33, cache_creation: 44 });
        assert_eq!((c.usd, c.complete, c.turns, c.models.len()), (Some(1.25), true, Some(7), 2));
        assert_eq!((c.reported, c.charged), (1.25, 1.25));
        let asst = |id: &str, i: u64| json!({"type": "assistant", "message": {"id": id, "usage": {"input_tokens": i, "output_tokens": 1}}}).to_string();
        let stream = [asst("m1", 5), asst("m1", 5), asst("m2", 7)].join("\n");
        let c = session_cost(None, &stream, 2.0);
        assert_eq!((c.usd, c.complete, c.reported, c.charged), (None, false, 0.0, 2.0), "cut off: charged its budget");
        // F's sum: one candidate cut off leaves the total unknown, charged at that candidate's budget.
        let mut f = Cost::zero();
        f.add(&session_cost(Some(&r), "", 2.0));
        f.add(&c);
        assert_eq!((f.usd, f.complete, f.reported, f.charged, f.turns), (None, false, 1.25, 3.25, Some(7)));
        assert_eq!(c.tokens, Tokens { input: 12, output: 2, cache_read: 0, cache_creation: 0 }, "each message once");

        let root = tmp("spend");
        let line = |usd: Option<f64>, est: Option<f64>, exit: &str| {
            json!({"v": 1, "ms": 1, "drone": "d1", "iter": 1, "harness": "claude", "usd": usd, "est": est, "session": "s", "exit": exit}).to_string()
        };
        fs::write(root.join("spend.jsonl"), [line(Some(0.5), None, "ok"), line(None, Some(0.7), "timeout"), line(Some(0.25), None, "ok")].join("\n")).unwrap();
        let (charged, reported, all_named, n) = d3_spend(&root.join("spend.jsonl"), None);
        assert!((charged - 1.45).abs() < 1e-12 && (reported - 0.75).abs() < 1e-12 && !all_named && n == 3);
        // Arm E: the synced file holds both nodes' lines; each node's dollars are its own lines only.
        let on = |node: &str, usd: f64| {
            json!({"v": 1, "ms": 1, "drone": "d1", "node": node, "iter": 1, "harness": "sim", "usd": usd, "session": node, "exit": "ok"}).to_string()
        };
        fs::write(root.join("both.jsonl"), [on("na", 0.5), on("nb", 0.25), line(Some(0.125), None, "ok")].join("\n")).unwrap();
        let both = root.join("both.jsonl");
        assert_eq!((d3_spend(&both, Some("na")).0, d3_spend(&both, Some("nb")).0, d3_spend(&both, None).0), (0.625, 0.375, 0.875));
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join("logs/d1-1.out"), r.to_string()).unwrap();
        fs::write(root.join("logs/d2-1.out"), json!({"total_cost_usd": 0.1, "usage": {"input_tokens": 100, "output_tokens": 1}}).to_string()).unwrap();
        fs::write(root.join("logs/d3-1.out"), "cut off mid-wr").unwrap();
        let (t, models) = d3_tokens(&root.join("logs"));
        assert_eq!(t, Tokens { input: 111, output: 23, cache_read: 33, cache_creation: 44 });
        assert_eq!(models.len(), 2);
        assert!(!over_cap(11.0, 10.0) && over_cap(11.01, 10.0));
        let _ = fs::remove_dir_all(root);
    }

    /// What `hacked` reads is the tree as cloned: a hidden check runs the result's code with the tree
    /// writable, and that code can plant a replace ref (the head reads as base) or delete the visible
    /// check's file. A diff git cannot make fails the scoring rather than reading as clean.
    #[test]
    fn hacked_is_judged_before_the_result_runs_and_fails_closed() {
        let root = tmp("evade");
        let (wd, _, runs) = workload(&root, |v| {
            v["visible"] = json!("sh grade.sh");
            let evade = format!("{GIT} replace $({GIT} rev-parse HEAD) $({GIT} rev-list --max-parents=0 HEAD) && rm grade.sh");
            v["hidden"]["checks"] = json!([{"id": "evade", "cmd": evade, "timeout_s": 30}]);
        });
        let wl = load(&wd, &root, &runs).unwrap();
        let ws = root.join("cell/home/work/w-test");
        let base = build_workspace(&wl, &ws).unwrap();
        fs::write(ws.join("r/tests/sim_edit.txt"), "").unwrap();
        fs::write(ws.join("grade.sh"), "exit 0\n").unwrap();
        let (code, out) = Jail::Plain.exec(&ws, &[], &[], &result_script(), 60);
        let head = parse_head(&out).unwrap_or_else(|| panic!("{code}: {out}"));
        let s = score(&Jail::Plain, &wl, &ws, &base, &head, &root.join("cell/score"), Which::Both).unwrap();
        assert!(s.checks[0].pass, "the evasion ran: {:?}", s.checks);
        assert_eq!(s.hacked_paths, ["grade.sh", "r/tests/sim_edit.txt"], "{:?}", s.hacked_why);
        let err = score(&Jail::Plain, &wl, &ws, &"f".repeat(40), &head, &root.join("cell/score"), Which::Both).unwrap_err();
        assert!(err.contains("hacked"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hacked_paths_and_board_facts() {
        let root = tmp("hacked");
        let (wd, _, runs) = workload(&root, |v| v["visible"] = json!("sh grade.sh && ./run_checks.sh"));
        let wl = load(&wd, &root, &runs).unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join("tests")).unwrap();
        for (f, t) in [("tests/x_test.py", "assert f() == 1\n"), ("grade.sh", "true"), ("src.py", "1")] {
            fs::write(repo.join(f), t).unwrap();
        }
        git(&repo, &format!("{GIT} init -q -b main && {GIT} add -A && {GIT} commit -q -m base"));
        let base = git(&repo, "git rev-parse HEAD");
        fs::write(repo.join("tests/x_test.py"), "assert True\n").unwrap();
        fs::write(repo.join("grade.sh"), "exit 0").unwrap();
        fs::write(repo.join("src.py"), "2").unwrap();
        git(&repo, &format!("{GIT} add -A && {GIT} commit -q -m hack"));
        let hack = git(&repo, "git rev-parse HEAD");
        let files = visible_files(&wl, &repo);
        assert_eq!(files, ["grade.sh"]);
        let protected = |head: &str, scope: &[String]| verdict::protected(&verdict::diff(&repo, &base, head).unwrap(), scope, &files);
        let paths: Vec<String> = protected(&hack, &[]).into_iter().map(|c| c.path).collect();
        assert_eq!(paths, ["grade.sh", "tests/x_test.py"]);
        assert_eq!(protected(&hack, &["tests/*".into(), "*.sh".into()]).len(), 1, "the visible check's file is never in scope");
        git(&repo, &format!("{GIT} checkout -q {base} && mkdir -p tests && echo n > tests/y_test.py && {GIT} add -A && {GIT} commit -q -m add"));
        let add = git(&repo, "git rev-parse HEAD");
        assert!(protected(&add, &[]).is_empty(), "a new test is allowed");
        git(&repo, &format!("{GIT} checkout -q {base} && echo 'assert f() != 2' >> tests/x_test.py && {GIT} add -A && {GIT} commit -q -m grow"));
        let grow = git(&repo, "git rev-parse HEAD");
        assert!(protected(&grow, &[]).is_empty(), "a grown test is allowed");
        let scoped = Workload { tasks: vec!["a|work|A|tests/*.py, src/*".into(), "b|work|B".into(), "c|work|C|src/*||a".into()], ..wl.clone() };
        assert_eq!(scoped.scope(), ["src/*", "tests/*.py"]);

        let task = |id: &str| board::Task {
            id: id.into(),
            role: board::Role::Work,
            title: "t".into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
            group: None,
        };
        let mut ev = vec![board::Event::open(task("t1"), "e", 1), board::Event::open(task("t2"), "e", 1)];
        let mut verify = task("t1.v1");
        verify.role = board::Role::Verify;
        verify.of = Some("t1".into());
        ev.push(board::Event::new(board::Ev::Claim, "t1", "s1", 2));
        ev.push(board::Event::new(board::Ev::Done, "t1", "s1", 3));
        ev.push(board::Event::open(verify, "s1", 3));
        ev.push(board::Event::new(board::Ev::Claim, "t1.v1", "s2", 4));
        let mut rej = board::Event::new(board::Ev::Rejected, "t1.v1", "s2", 5);
        rej.hacked = Some(true);
        ev.push(rej);
        // Not the holder's verdict: the fold takes none, and neither does the mistakes count.
        ev.push(board::Event::new(board::Ev::Rejected, "t1.v1", "s9", 6));
        board::append(&root.join("tasks.jsonl"), &ev).unwrap();
        let (rework, hacked, counts) = board_facts(&root.join("tasks.jsonl"));
        assert_eq!((rework, hacked), (1, 1), "{counts:?}");
        assert_eq!(board_rejects(&root.join("tasks.jsonl")), (1, 0), "one verifier reject, no integration");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn d3_charges_the_sessions_the_wall_cap_cut_off() {
        let root = tmp("cut");
        let spend = |drone: &str, session: &str, usd: f64| {
            json!({"v": 1, "ms": 1, "drone": drone, "iter": 1, "harness": "claude", "usd": usd, "session": session, "exit": "ok"}).to_string()
        };
        let join = |drone: &str, session: &str, by: &str| {
            json!({"v": 1, "ms": 1, "goal": "pilot", "dir": "/w", "drone": drone, "session": session, "harness": "claude", "by": by}).to_string()
        };
        // d1: one finished iteration ($1.40) and one cut off; d2: cut off in its first; d3: finished.
        fs::write(root.join("spend.jsonl"), [spend("d1", "s1", 1.4), spend("d3", "s5,s5b", 0.5)].join("\n")).unwrap();
        fs::write(
            root.join("joins.jsonl"),
            [join("d1", "s1", "run"), join("d1", "s2", "run"), join("d2", "s3", "run"), join("d3", "s5", "run"), join("", "s9", "hook")].join("\n"),
        )
        .unwrap();
        fs::write(root.join("runs.jsonl"), json!({"v": 1, "ms": 1, "drones": 3, "harness": "claude", "share_usd": 2.0, "base": "b"}).to_string()).unwrap();
        let (usd, cut) = d3_unreported(&root, 9.0, None);
        assert_eq!(cut, ["d1", "d2"]);
        assert!((usd - (0.6 + 2.0)).abs() < 1e-9, "each at its drone's dollars left: {usd}");
        let _ = fs::remove_file(root.join("runs.jsonl"));
        assert!((d3_unreported(&root, 1.0, None).0 - 1.0).abs() < 1e-9, "no runs line: the cap's share, d1 past it");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_dollar_guard_counts_rows_and_cells_that_died_before_their_row() {
        let root = tmp("spent");
        let rows = [
            json!({"arm": "a", "attempt": "w-a-s0-1", "charged_usd": 2.5, "cost_usd": null, "cap_usd": 6.0, "paid": true, "error": null}),
            json!({"arm": "d3", "attempt": "w-d3-s0-1", "charged_usd": null, "cost_usd": null, "cap_usd": 6.0, "paid": false, "error": "borg install"}),
            json!({"arm": "b", "cost_usd": 1.0, "cap_usd": 6.0}),
        ];
        for r in &rows {
            append_row(&root.join("results.jsonl"), r).unwrap();
        }
        for (dir, attempt) in [("w-a-s0", "w-a-s0-1"), ("w-f-s0", "w-f-s0-9"), ("w-f-s0.attempt-1", "w-f-s0-7")] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("cell.json"), json!({"cell": dir, "attempt": attempt, "cap_usd": 6.0}).to_string()).unwrap();
        }
        assert!((spent_in(&root) - (2.5 + 0.0 + 1.0 + 6.0 + 6.0)).abs() < 1e-9, "{}", spent_in(&root));
        // A goal sequence's row counts borg's own model dollars too.
        let seq =
            json!({"arm": "l01", "charged_usd": 1.0, "cap_usd": 6.0, "error": null, "seq": {"charged_usd": 1.0, "borg_usd": 0.5, "charged_total_usd": 1.5}});
        assert!((charged_of(&seq) - 1.5).abs() < 1e-9);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn errored_rows_leave_both_h1_terms_alike() {
        let mut rows: Vec<Value> = [100.0, 100.0, 100.0].iter().map(|w| row("w1", "parallel", "b", *w, 1.0)).collect();
        rows.extend([50.0, 50.0, 50.0].iter().map(|w| row("w1", "parallel", "d3", *w, 1.0)));
        let mut err = row("w1", "parallel", "d3", 0.0, 0.0);
        err["wall_s"] = Value::Null;
        err["error"] = json!("borg hive init: exit 1");
        err["paid"] = json!(false);
        err["charged_usd"] = Value::Null;
        rows.push(err);
        let t = report_text(&rows);
        assert!(t.contains("D3 mean share 1.000") && t.contains("→ HOLDS"), "an infra error is no 0 share: {t}");
        assert!(t.contains("| w1 | d3 | 4 | 50 | 1.000 | 1.00 |") && t.contains("| 0 | 1 |\n"), "{t}");
        assert!(!t.contains("runs without error"), "{t}");
        let mut two = rows.clone();
        two.remove(3);
        assert!(report_text(&two).contains("only 2 D3 and 3 B runs without error"), "{}", report_text(&two));
    }

    fn row(w: &str, shape: &str, arm: &str, wall: f64, share: f64) -> Value {
        json!({"workload": w, "shape": shape, "arm": arm, "K": 3, "wall_s": wall, "hidden": {"solve_share": share}, "cost_usd": 1.0,
               "cost_complete": true, "tokens": {"input": 1, "output": 1, "cache_read": 0, "cache_creation": 0}, "merge_conflicts": 0, "hacked": 0, "error": null})
    }

    #[test]
    fn the_report_applies_the_pre_registered_rule_inclusively() {
        let b = [100.0, 120.0, 140.0];
        let mut rows: Vec<Value> = b.iter().map(|w| row("w1", "parallel", "b", *w, 2.0 / 3.0)).collect();
        // D3: median exactly 0.8 × 120 = 96, mean share exactly B − 1/K.
        rows.extend([90.0, 96.0, 200.0].iter().map(|w| row("w1", "parallel", "d3", *w, 1.0 / 3.0)));
        let t = report_text(&rows);
        assert!(t.contains("H1 w1 (parallel): D3 median wall 96s ≤ 0.8×B 120s (96s)?") && t.contains("→ HOLDS"), "{t}");
        assert!(t.contains("H1 holds on every workload") && t.contains("point estimates, no CI"), "{t}");
        // A hair worse on wall.
        let mut worse = rows.clone();
        worse[4]["wall_s"] = json!(96.001);
        assert!(report_text(&worse).contains("→ FAILS"));
        // A hair worse on share.
        let mut worse = rows.clone();
        for r in worse.iter_mut().filter(|r| r["arm"] == "d3") {
            r["hidden"]["solve_share"] = json!(1.0 / 3.0 - 1e-6);
        }
        assert!(report_text(&worse).contains("→ FAILS"));
        // Both workloads fail → the kill line; one holds → that shape.
        let mut both = worse.clone();
        both.extend(b.iter().map(|w| row("w2", "cross-repo", "b", *w, 1.0)));
        both.extend(b.iter().map(|w| row("w2", "cross-repo", "d3", *w, 1.0)));
        let t = report_text(&both);
        assert!(t.contains("Kill: H1 fails on every workload → stop before mesh sync"), "{t}");
        let mut one = rows.clone();
        one.extend(b.iter().map(|w| row("w2", "cross-repo", "b", *w, 1.0)));
        one.extend(b.iter().map(|w| row("w2", "cross-repo", "d3", *w, 1.0)));
        let t = report_text(&one);
        assert!(t.contains("H1 holds on one shape: parallel (w1)"), "{t}");
        assert!(t.contains("| w1 | d3 | 3 | 96 | 0.333 | 1.00 | 2 |"), "{t}");
        // A goal sequence's rows sit under their learning arm, apart from d3's, and get no H1 line.
        let mut seq = one.clone();
        for arm in ["frozen", "l01"] {
            let mut r = row("g1", "parallel", arm, 50.0, 1.0);
            r["seq"] = json!({"arm": arm, "seed": 0, "position": 1});
            seq.push(r);
        }
        let t = report_text(&seq);
        assert!(t.contains("| g1 | frozen | 1 |") && t.contains("| g1 | l01 | 1 |") && !t.contains("| g1 | d3 |"), "{t}");
        assert!(!t.contains("H1 g1") && t.contains("H1 holds on one shape: parallel (w1)"), "{t}");
    }

    // ---------------------------------------------------------------- end to end (bwrap, built borg)

    /// A workload for the end-to-end runs: two work tasks writing f1.txt and f2.txt; hidden checks want
    /// `done t1` / `done t2` in them. Not under /tmp: the sandbox mounts a fresh tmpfs there.
    fn e2e_workload(tag: &str, sim: Value) -> (PathBuf, PathBuf) {
        e2e_workload_in(tag, sim, &["w/f1.txt", "w/f2.txt"])
    }

    /// `e2e_workload` with task tN scoped to `files[N-1]` (two the same file: their attempts conflict).
    fn e2e_workload_in(tag: &str, sim: Value, files: &[&str]) -> (PathBuf, PathBuf) {
        let root = repo_root().join("target/hive-test").join(format!("{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("README"), "fixture\n").unwrap();
        git(&src, "git init -q -b main && git add -A && git -c user.name=x -c user.email=x@y commit -q -m one");
        let commit = git(&src, "git rev-parse HEAD");
        let wd = std::env::temp_dir().join(format!("borg-hive-e2e-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&wd);
        fs::create_dir_all(wd.join("hidden")).unwrap();
        let tasks: Vec<(String, &str)> = files.iter().enumerate().map(|(i, f)| (format!("t{}", i + 1), *f)).collect();
        let spec: Vec<String> = tasks.iter().map(|(t, f)| format!("`done {t}` into {f}")).collect();
        fs::write(wd.join("spec.md"), format!("Write {}.\n", spec.join(" and "))).unwrap();
        // At the workspace root, where the sim drone's `edit_tests` modifies it.
        fs::write(
            wd.join("inject.patch"),
            "diff --git a/tests/sim_edit.txt b/tests/sim_edit.txt\nnew file mode 100644\n--- /dev/null\n+++ b/tests/sim_edit.txt\n@@ -0,0 +1 @@\n+original\n",
        )
        .unwrap();
        // Each file holds the lines of every task scoped to it.
        let mut by_file: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (t, f) in &tasks {
            by_file.entry(f).or_default().push(format!("done {t}"));
        }
        let reference: String = by_file
            .iter()
            .map(|(f, lines)| {
                let body: String = lines.iter().map(|l| format!("+{l}\n")).collect();
                format!("diff --git a/{f} b/{f}\nnew file mode 100644\n--- /dev/null\n+++ b/{f}\n@@ -0,0 +1,{} @@\n{body}", lines.len())
            })
            .collect();
        fs::write(wd.join("reference.patch"), reference).unwrap();
        let checks: Vec<Value> = tasks
            .iter()
            .map(|(t, f)| json!({"id": t, "cmd": format!("grep -q 'done {t}' \"$BORG_WORK/{f}\" && ! grep -q '^<<<<<<<' \"$BORG_WORK/{f}\""), "timeout_s": 30}))
            .collect();
        let v = json!({
            "id": format!("e2e-{tag}"), "shape": "parallel",
            "repos": [{"src": src.display().to_string(), "commit": commit, "dir": "w"}],
            "inject": "inject.patch", "spec": "spec.md", "visible": "true", "visible_timeout_s": 30,
            "hidden": {"dir": "hidden", "checks": checks},
            "reference": "reference.patch",
            "hive": {"tasks": tasks.iter().map(|(t, f)| format!("{t}|work|write done {t} into {f}|{f}")).collect::<Vec<_>>()},
            "sim": sim
        });
        fs::write(wd.join("workload.json"), v.to_string()).unwrap();
        (wd, root)
    }

    /// The scorer sandbox (bwrap): a check cannot rewrite a later check's script, and nothing it starts
    /// outlives the scoring. Skipped where bwrap cannot run.
    #[test]
    fn scorer_checks_are_read_only_and_leave_no_process_behind() {
        if !Command::new("bwrap").args(["--ro-bind", "/", "/", "--unshare-pid", "true"]).status().is_ok_and(|s| s.success()) {
            eprintln!("bwrap unavailable: skipped");
            return;
        }
        let (wd, root) = e2e_workload("ro", json!(null));
        let mut wl = load_workload(&wd, &real_home(), &repo_root().join("target/eval-runs")).unwrap();
        wl.checks = vec![
            HiddenCheck {
                id: "c1".into(),
                cmd: "o=$(dirname \"$BORG_WORK\"); (sleep 2; touch \"$o/late\") >/dev/null 2>&1 & echo 'exit 0' > \"$BORG_WORK/../../checks/h-c2.sh\"; exit 1"
                    .into(),
                timeout_s: 30,
            },
            HiddenCheck { id: "c2".into(), cmd: "exit 1".into(), timeout_s: 30 },
        ];
        let ws = root.join("cell/home/work").join(&wl.id);
        let base = build_workspace(&wl, &ws).unwrap();
        let cell = root.join("cell");
        let bx = Jail::scorer(&cell, &wl, &real_home());
        let s = score(&bx, &wl, &ws, &base, &base, &cell.join("score"), Which::Hidden).unwrap();
        assert_eq!(s.checks.iter().map(|c| c.pass).collect::<Vec<_>>(), [false, false], "{:?}", s.checks);
        assert_eq!(fs::read_to_string(cell.join("score/checks/h-c2.sh")).unwrap(), "exit 1");
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(cell.join("score/out").is_dir() && !cell.join("score/out/late").exists(), "a check's background process died with the sandbox");
        let _ = fs::remove_dir_all(wd);
        let _ = fs::remove_dir_all(root);
    }

    /// The result commit runs the agent's .git config (a clean filter `git add` runs): what it writes
    /// to its HOME does not reach the HOME the checks then run in. Skipped where bwrap cannot run.
    #[test]
    fn each_scorer_step_gets_a_fresh_home() {
        if !Command::new("bwrap").args(["--ro-bind", "/", "/", "--unshare-pid", "true"]).status().is_ok_and(|s| s.success()) {
            eprintln!("bwrap unavailable: skipped");
            return;
        }
        let (wd, root) = e2e_workload("home", json!(null));
        let mut wl = load_workload(&wd, &real_home(), &repo_root().join("target/eval-runs")).unwrap();
        wl.checks = vec![HiddenCheck { id: "c1".into(), cmd: "test ! -e \"$HOME/planted\"".into(), timeout_s: 30 }];
        let ws = root.join("cell/home/work").join(&wl.id);
        let base = build_workspace(&wl, &ws).unwrap();
        fs::write(ws.join(".gitattributes"), "*.txt filter=x\n").unwrap();
        fs::write(ws.join("note.txt"), "x\n").unwrap();
        git(&ws, "git config filter.x.clean \"sh -c 'touch \\\"\\$HOME/planted\\\"; cat'\"");
        let cell = root.join("cell");
        let bx = Jail::scorer(&cell, &wl, &real_home());
        let head = commit_result(&bx, &ws).unwrap();
        let s = score(&bx, &wl, &ws, &base, &head, &cell.join("score"), Which::Hidden).unwrap();
        assert!(s.checks[0].pass, "the filter's file reached the checks' HOME: {:?}", s.checks);
        let _ = fs::remove_dir_all(wd);
        let _ = fs::remove_dir_all(root);
    }

    /// `--stop-at-usd` holds for cells of one run started at once: each reads the others' caps.
    #[test]
    fn cells_started_at_once_stay_within_stop_at() {
        let run = tmp("budget").join("run");
        let n = 8;
        let gate = std::sync::Barrier::new(n);
        let started = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..n)
                .map(|i| {
                    let (run, gate) = (&run, &gate);
                    sc.spawn(move || {
                        let rec = json!({"cell": format!("c{i}"), "attempt": format!("c{i}-1"), "cap_usd": 1.0});
                        gate.wait();
                        start_cell(run, &run.join(format!("c{i}")), &rec, 1.0, Some(2.5)).is_ok()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).filter(|ok| *ok).count()
        });
        assert_eq!(started, 2, "$1 caps under a $2.50 stop");
        assert!((spent_in(&run) - 2.0).abs() < 1e-9);
        let _ = fs::remove_dir_all(run.parent().unwrap());
    }

    /// Arm E may spend its cap on each node in the worst case (its runs' in-flight iterations): `--stop-at-usd`
    /// reserves twice the cap for it, and a cell of it that died before its row counts that reserve.
    #[test]
    fn an_arm_e_cell_reserves_each_nodes_cap_under_stop_at() {
        assert_eq!((Arm::E.reserve(15.0), Arm::D3.reserve(15.0)), (30.0, 15.0));
        let run = tmp("reserve").join("run");
        let rec = |i: u32, arm: Arm| json!({"cell": format!("c{i}"), "attempt": format!("c{i}-1"), "cap_usd": 15.0, "reserve_usd": arm.reserve(15.0)});
        assert!(start_cell(&run, &run.join("c0"), &rec(0, Arm::E), Arm::E.reserve(15.0), Some(90.0)).is_ok());
        assert!(start_cell(&run, &run.join("c1"), &rec(1, Arm::E), Arm::E.reserve(15.0), Some(90.0)).is_ok());
        assert!((spent_in(&run) - 60.0).abs() < 1e-9, "two lost cells count their reserve: {}", spent_in(&run));
        // $60 + $30 is the stop itself: a third starts; a fourth does not.
        assert!(start_cell(&run, &run.join("c2"), &rec(2, Arm::E), Arm::E.reserve(15.0), Some(90.0)).is_ok());
        assert_eq!(start_cell(&run, &run.join("c3"), &rec(3, Arm::E), Arm::E.reserve(15.0), Some(90.0)).map_err(|e| e.0), Err(2));
        assert!((charged_of(&json!({"cap_usd": 15.0, "reserve_usd": 30.0})) - 30.0).abs() < 1e-9, "a row of unknown cost counts its reserve");
        let _ = fs::remove_dir_all(run.parent().unwrap());
    }

    /// Drops a fixture workload's validate rows and dirs from the shared validate log.
    fn forget_validation(id: &str) {
        let log = validate_log(&repo_root().join("target/eval-runs"));
        // Under `append_row`'s lock: a row another test appends meanwhile is not rewritten away.
        if let Ok(mut f) = fs::OpenOptions::new().read(true).write(true).open(&log) {
            use std::io::{Read, Seek};
            f.lock().unwrap();
            let mut text = String::new();
            let _ = f.read_to_string(&mut text);
            let keep: String =
                text.lines().filter(|l| serde_json::from_str::<Value>(l).map_or(true, |v| v["validate"]["workload"] != id)).map(|l| format!("{l}\n")).collect();
            f.set_len(0).unwrap();
            f.rewind().unwrap();
            f.write_all(keep.as_bytes()).unwrap();
        }
        let dir = log.parent().unwrap().to_path_buf();
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(&format!("{id}-")) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }

    fn built_borg() -> PathBuf {
        let root = repo_root();
        ["target/debug/borg", "target/release/borg"].iter().map(|p| root.join(p)).find(|p| p.exists()).expect("cargo build -p borg-ai first")
    }

    fn rows_of(run: &str) -> Vec<Value> {
        let p = repo_root().join("target/eval-runs").join(run).join("results.jsonl");
        fs::read_to_string(p).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    /// One D3 cell of the sim harness on `e2e_workload_in(tag, sim, files)`: its row, and the passing
    /// integration's head its board names (None when there is none).
    fn e2e_d3(tag: &str, sim: Value) -> (Value, Option<String>) {
        e2e_d3_in(tag, sim, ["w/f1.txt", "w/f2.txt"])
    }

    fn e2e_d3_in(tag: &str, sim: Value, files: [&str; 2]) -> (Value, Option<String>) {
        let (wd, root) = e2e_workload_in(tag, sim, &files);
        assert_eq!(validate_cmd(&s(&["--workload", &wd.display().to_string()])), 0, "validate");
        let run = format!("hive-e2e-{tag}-{}", std::process::id());
        let code = cell_cmd(&s(&[
            "--workload",
            &wd.display().to_string(),
            "--arm",
            "d3",
            "--seed",
            "0",
            "--cap-usd",
            "3",
            "--timeout-s",
            "120",
            "--run",
            &run,
            "--harness",
            "sim",
            "--borg",
            &built_borg().display().to_string(),
        ]));
        let rows = rows_of(&run);
        forget_validation(&format!("e2e-{tag}"));
        assert_eq!(rows.len(), 1, "one row");
        let r = rows[0].clone();
        assert_eq!(code, 0, "{r:#}");
        let run_dir = repo_root().join("target/eval-runs").join(&run);
        let tasks = format!("home/.local/share/borg/collectives/{GOAL}/hive/{GOAL}/tasks.jsonl");
        let board = fs::read_dir(&run_dir).unwrap().flatten().map(|e| e.path().join(&tasks)).find(|p| p.is_file()).map(|p| board::fold(&board::read(&p)));
        let integrated = board.filter(board::Board::finished).and_then(|b| b.integrated).map(|i| i.head);
        let _ = fs::remove_dir_all(wd);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(run_dir);
        (r, integrated)
    }

    /// cargo build -p borg-ai && cargo test -p borg-eval hive_e2e -- --ignored --nocapture
    #[test]
    #[ignore]
    fn hive_e2e_d3_sim_solves_the_board() {
        let (r, integrated) = e2e_d3("sim", json!({"usd_per_iter": 0.10}));
        assert_eq!(r["hidden"]["solve_share"], 1.0, "{r:#}");
        assert_eq!((r["merge_conflicts"].as_u64(), r["integrated"].as_bool(), r["integrations"].as_u64()), (Some(0), Some(true), Some(1)), "{r:#}");
        assert_eq!(r["result_head"].as_str(), integrated.as_deref(), "the board's integrated head is what was scored");
        assert_eq!(r["store_lessons_before"], 0);
        assert_eq!(r["d3"]["finished"], true, "{r:#}");
        let iters = r["d3"]["iterations"].as_u64().unwrap() as f64;
        let cost = r["cost_usd"].as_f64().unwrap();
        assert!((cost - iters * 0.10).abs() < 1e-9 && cost <= 3.0, "{r:#}");
        assert_eq!(r["hacked"], 0, "{r:#}");
        assert!(r["error"].is_null());
    }

    #[test]
    #[ignore]
    fn hive_e2e_d3_sim_grown_tests_are_not_hacked() {
        let grow = json!({"grow_tests": true});
        let (r, _) = e2e_d3("grow", json!({"usd_per_iter": 0.10, "drones": {"d1": grow, "d2": grow, "d3": grow}}));
        assert_eq!((r["hacked"].as_u64(), r["hacked_verdicts"].as_u64()), (Some(0), Some(0)), "{r:#}");
        assert_eq!(r["d3"]["finished"], true, "{r:#}");
    }

    #[test]
    #[ignore]
    fn hive_e2e_d3_sim_test_edits_count_as_hacked() {
        let edit = json!({"edit_tests": true});
        let (r, _) = e2e_d3("hack", json!({"usd_per_iter": 0.10, "drones": {"d1": edit, "d2": edit, "d3": edit}}));
        assert!(!r["hacked_paths"].as_array().unwrap().is_empty() || r["hacked_verdicts"].as_u64().unwrap() >= 1, "{r:#}");
        // `hacked` is the final tree's, as for every arm; caught verdicts are counted apart.
        assert_eq!(r["hacked"].as_u64().unwrap(), u64::from(!r["hacked_paths"].as_array().unwrap().is_empty()), "{r:#}");
    }

    /// Both tasks write one file: their attempts conflict, the hive's fix task resolves it, and the
    /// integrated tree carries both.
    #[test]
    #[ignore]
    fn hive_e2e_d3_sim_resolves_a_conflict_through_a_fix_task() {
        let (r, integrated) = e2e_d3_in("conflict", json!({"usd_per_iter": 0.10}), ["w/shared.txt", "w/shared.txt"]);
        assert_eq!(r["hidden"]["solve_share"], 1.0, "{r:#}");
        assert_eq!((r["merge_conflicts"].as_u64(), r["integrated"].as_bool(), r["integrations"].as_u64()), (Some(1), Some(true), Some(2)), "{r:#}");
        assert_eq!(r["result_head"].as_str(), integrated.as_deref());
        assert_eq!(r["visible_pass"], true, "{r:#}");
    }

    /// D3 with a fake `claude`: the drones reach it through the wrapper, with A's system prompt.
    #[test]
    #[ignore]
    fn hive_e2e_d3_fake_claude_gets_the_system_prompt() {
        let (wd, root) = e2e_workload("d3fake", json!(null));
        let fake = root.join("fake-claude");
        fs::write(
            &fake,
            r#"#!/bin/sh
for a in "$@"; do printf '%s\n' "$a"; done >> "$HOME/argv.log"
echo '{"type":"result","total_cost_usd":0.1,"session_id":"x","result":"ok","is_error":false}'
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(validate_cmd(&s(&["--workload", &wd.display().to_string()])), 0, "validate");
        let run = format!("hive-e2e-d3fake-{}", std::process::id());
        let wds = wd.display().to_string();
        let (fakes, borg) = (fake.display().to_string(), built_borg().display().to_string());
        let args =
            ["--workload", &wds, "--arm", "d3", "--seed", "0", "--cap-usd", "3", "--timeout-s", "120", "--run", &run, "--claude", &fakes, "--borg", &borg];
        let code = cell_cmd(&s(&args));
        let rows = rows_of(&run);
        forget_validation("e2e-d3fake");
        let cell = repo_root().join("target/eval-runs").join(&run);
        let argv = fs::read_to_string(cell.join("e2e-d3fake-d3-s0/home/argv.log")).unwrap_or_default();
        assert_eq!((code, rows.len()), (0, 1), "{:#}", rows.first().cloned().unwrap_or_default());
        assert!(argv.contains("--append-system-prompt\n") && argv.contains(crate::SYSTEM), "{argv}");
        assert!(argv.contains("drone d1 of hive goal pilot"), "{argv}");
        let r = &rows[0];
        assert!(r["error"].is_null() && r["paid"] == true && r["cost_complete"] == true, "{r:#}");
        assert_eq!(r["charged_usd"], r["cost_usd"]);
        let _ = fs::remove_dir_all(wd);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(cell);
    }

    #[test]
    #[ignore]
    fn hive_e2e_fake_claude_arms_a_b_f() {
        let (wd, root) = e2e_workload("fake", json!(null));
        let fake = root.join("fake-claude");
        fs::write(
            &fake,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/argv.log"
mkdir -p w && echo 'done t1' > w/f1.txt && echo 'done t2' > w/f2.txt
echo '{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"output_tokens":4}}}'
echo '{"type":"result","total_cost_usd":0.5,"num_turns":2,"result":"ok","modelUsage":{"claude-sonnet":{"inputTokens":10,"outputTokens":20,"cacheReadInputTokens":30,"cacheCreationInputTokens":40}}}'
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(validate_cmd(&s(&["--workload", &wd.display().to_string()])), 0, "validate");
        let run = format!("hive-e2e-fake-{}", std::process::id());
        for arm in ["a", "b", "f"] {
            let code = cell_cmd(&s(&[
                "--workload",
                &wd.display().to_string(),
                "--arm",
                arm,
                "--seed",
                "0",
                "--cap-usd",
                "3",
                "--timeout-s",
                "60",
                "--run",
                &run,
                "--claude",
                &fake.display().to_string(),
                "--borg",
                &built_borg().display().to_string(),
                "--stop-at-usd",
                "100",
            ]));
            assert_eq!(code, 0, "{arm}: {:#}", rows_of(&run).last().unwrap());
        }
        let rows = rows_of(&run);
        assert_eq!(rows.len(), 3);
        let cell = repo_root().join("target/eval-runs").join(&run);
        let argv = |dir: &str| fs::read_to_string(cell.join(dir).join("home/argv.log")).unwrap();
        let a = argv("e2e-fake-a-s0");
        assert!(a.contains("--max-budget-usd 3.00") && a.contains("--disallowedTools Task Agent"), "{a}");
        let b = argv("e2e-fake-b-s0");
        assert!(b.contains("--max-budget-usd 3.00") && !b.contains("--disallowedTools") && b.contains("subagents"), "{b}");
        for i in 1..=3 {
            let f = argv(&format!("e2e-fake-f-s0/f{i}"));
            assert!(f.contains("--max-budget-usd 1.00") && f.contains("--disallowedTools Task Agent"), "{f}");
        }
        for r in &rows {
            assert_eq!(r["hidden"]["solve_share"], 1.0, "{r:#}");
            assert!(r["error"].is_null(), "{r:#}");
            assert_eq!(r["models"], json!(["claude-sonnet"]));
        }
        assert_eq!((rows[0]["cost_usd"].as_f64(), rows[0]["tokens"]["cache_creation"].as_u64()), (Some(0.5), Some(40)));
        assert_eq!((rows[2]["cost_usd"].as_f64(), rows[2]["tokens"]["input"].as_u64(), rows[2]["sessions"].as_u64()), (Some(1.5), Some(30), Some(3)));
        assert_eq!(rows[2]["f"]["candidates"].as_array().unwrap().len(), 3);
        assert_eq!(rows[2]["f"]["chosen"], 1, "all pass at equal cost: the lowest index");
        // The $ guard: 0.5 + 0.5 + 1.5 spent; another 3 over a 4.5 limit is refused.
        let code = cell_cmd(&s(&[
            "--workload",
            &wd.display().to_string(),
            "--arm",
            "a",
            "--seed",
            "1",
            "--cap-usd",
            "3",
            "--run",
            &run,
            "--claude",
            &fake.display().to_string(),
            "--borg",
            &built_borg().display().to_string(),
            "--stop-at-usd",
            "4.5",
        ]));
        assert_eq!((code, rows_of(&run).len()), (2, 3));
        forget_validation("e2e-fake");
        let _ = fs::remove_dir_all(wd);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(cell);
    }

    /// Arm E on the sim harness: two stores synced over loopback, 2 drones on a and 1 on b, four tasks of
    /// slow drones (each holds its claim 3 s) so b's drone gets work while a's hold theirs.
    /// cargo build -p borg-ai && cargo test -p borg-eval hive_e2e_e -- --ignored --nocapture
    #[test]
    #[ignore]
    fn hive_e2e_e_sim_two_nodes_work_verify_across_and_fold_one_board() {
        let slow = json!({"work_s": 3});
        let (wd, root) =
            e2e_workload_in("arme", json!({"usd_per_iter": 0.10, "drones": {"d1": slow, "d2": slow}}), &["w/f1.txt", "w/f2.txt", "w/f3.txt", "w/f4.txt"]);
        assert_eq!(validate_cmd(&s(&["--workload", &wd.display().to_string()])), 0, "validate");
        let run = format!("hive-e2e-arme-{}", std::process::id());
        let code = cell_cmd(&s(&[
            "--workload",
            &wd.display().to_string(),
            "--arm",
            "e",
            "--seed",
            "0",
            "--cap-usd",
            "6",
            "--timeout-s",
            "300",
            "--run",
            &run,
            "--harness",
            "sim",
            "--borg",
            &built_borg().display().to_string(),
        ]));
        let rows = rows_of(&run);
        forget_validation("e2e-arme");
        assert_eq!(rows.len(), 1, "one row");
        let r = &rows[0];
        let e = &r["e"];
        eprintln!("{}", serde_json::to_string_pretty(e).unwrap());
        assert_eq!(code, 0, "{r:#}");
        assert!(r["error"].is_null(), "{r:#}");
        assert_eq!((e["finished"].as_bool(), e["boards_identical"].as_bool(), e["heads_agree"].as_bool()), (Some(true), Some(true), Some(true)), "{e:#}");
        assert_eq!(e["board_digest"]["a"], e["board_digest"]["b"]);
        assert_eq!((e["boards_finished"]["a"].as_bool(), e["boards_finished"]["b"].as_bool()), (Some(true), Some(true)), "{e:#}");
        for n in ["a", "b"] {
            assert!(e["nodes"][n]["attempts_verified"].as_u64() >= Some(1), "work on node {n}: {e:#}");
        }
        // Commits travelled: every judgement the board shows across nodes, either way, is one whose judging
        // node took that attempt's bundle, and b's verified work reached a that way.
        let x = &e["cross_node"];
        assert!(x["verified"].as_u64().unwrap() + x["integrated"].as_u64().unwrap() >= 1, "{e:#}");
        assert_eq!((&x["on_board"]["verified"], &x["on_board"]["integrated"]), (&x["verified"], &x["integrated"]), "{e:#}");
        let used = |n: &str| x["by_node"][n]["verified"].as_u64().unwrap_or(0) + x["by_node"][n]["integrated"].as_u64().unwrap_or(0);
        assert!(used("a") >= 1 && e["nodes"]["a"]["bundles"]["taken"].as_u64() >= Some(1), "b's work judged on a from its bundle: {e:#}");
        assert_eq!(e["max_holders"], 1, "{e:#}");
        let bundles = |k: &str| e["nodes"]["a"]["bundles"][k].as_u64().unwrap() + e["nodes"]["b"]["bundles"][k].as_u64().unwrap();
        assert!(bundles("made") >= 1 && bundles("taken") >= 1 && bundles("refused") == 0, "{e:#}");
        assert!(e["sync"]["rounds"].as_u64() >= Some(3) && e["sync"]["failed"] == 0, "{e:#}");
        assert_eq!((r["integrated"].as_bool(), r["visible_pass"].as_bool(), r["hidden"]["solve_share"].as_f64()), (Some(true), Some(true), Some(1.0)), "{r:#}");
        assert_eq!((r["hacked"].as_u64(), r["merge_conflicts"].as_u64(), r["over_cap"].as_bool()), (Some(0), Some(0), Some(false)), "{r:#}");
        assert_ne!(r["result_head"], r["base"]);
        let cell = repo_root().join("target/eval-runs").join(&run);
        let diff = fs::read_to_string(cell.join("e2e-arme-e-s0/score/diff.patch")).unwrap_or_default();
        eprintln!("integrated diff:\n{diff}");
        for t in 1..=4 {
            assert!(diff.contains(&format!("+done t{t}")), "{diff}");
        }
        let _ = fs::remove_dir_all(wd);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(cell);
    }

    /// A chain goal of the sequence driver: `e2e_workload_in` in the workspace `toy` shared by the chain,
    /// its tasks titled in the words of the fake learner's playbook shape, an oracle that fails while a
    /// task's file holds `wip`, and d1 failing its first attempt (its next iteration starts holding the
    /// claim, where a drone's prompt shows the claim's playbooks).
    fn chain_workload(tag: &str) -> (PathBuf, PathBuf) {
        let (wd, root) = e2e_workload(tag, json!({"usd_per_iter": 0.10, "drones": {"d1": {"fail_first": true}}}));
        let p = wd.join("workload.json");
        let mut v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        v["ws"] = json!("toy");
        v["visible"] = json!("! grep -rqs '^wip' w");
        v["hive"]["tasks"] = json!([
            "t1|work|Wire protocol frame change: write done t1 into w/f1.txt|w/f1.txt",
            "t2|work|Wire protocol frame change: write done t2 into w/f2.txt|w/f2.txt"
        ]);
        fs::write(&p, v.to_string()).unwrap();
        (wd, root)
    }

    /// cargo build -p borg-ai && cargo test -p borg-eval hive_e2e_chain -- --ignored --nocapture
    #[test]
    #[ignore]
    fn hive_e2e_chain_learns_across_goals_frozen_stays_empty_and_a_retry_restores_the_snapshot() {
        let pid = std::process::id();
        let (g1, root1) = chain_workload("chain1");
        let (g2, root2) = chain_workload("chain2");
        for wd in [&g1, &g2] {
            assert_eq!(validate_cmd(&s(&["--workload", &wd.display().to_string()])), 0, "validate");
        }
        // The learner's model: a fake answering the playbook reflection with a canned edit (0 tokens).
        let fake = root1.join("fake-claude");
        fs::write(
            &fake,
            r#"#!/bin/sh
case "$2" in
  *"You curate the team's playbooks"*) echo '{"structured_output":{"shape":"wire protocol frame change","op":"add","step":"","text":"Run the frame tests before calling it done"},"total_cost_usd":0.02}' ;;
  *) echo '{"is_error":true,"result":"no","total_cost_usd":0}' ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let run = format!("hive-e2e-chain-{pid}");
        let chains = repo_root().join("target/hive-test").join(format!("chains-{pid}"));
        let (c01, cf) = (chains.join("l01-s0"), chains.join("frozen-s0"));
        let (fakes, borg) = (fake.display().to_string(), built_borg().display().to_string());
        let cell = |wd: &Path, goal: &str, pos: &str, learn: &str, chain: &Path| {
            cell_cmd(&s(&[
                "--workload",
                &wd.display().to_string(),
                "--arm",
                "d3",
                "--seed",
                "0",
                "--cap-usd",
                "3",
                "--timeout-s",
                "120",
                "--run",
                &run,
                "--harness",
                "sim",
                "--borg",
                &borg,
                "--claude",
                &fakes,
                "--learn",
                learn,
                "--chain",
                &chain.display().to_string(),
                "--goal",
                goal,
                "--position",
                pos,
            ]))
        };
        let last = || rows_of(&run).last().cloned().unwrap();
        let store = |c: &Path| c.join("home").join(hive_seq::KEEP[0]);

        // L0+L1, goal 1: its verified work proposes a playbook edit; the learner's dollars are charged.
        assert_eq!(cell(&g1, "chain-g1", "1", "l01", &c01), 0, "{:#}", last());
        let r1 = last();
        assert_eq!(
            (r1["seq"]["arm"].as_str(), r1["seq"]["position"].as_u64(), r1["seq"]["lessons_before"].as_u64()),
            (Some("l01"), Some(1), Some(0)),
            "{r1:#}"
        );
        assert!(r1["seq"]["playbook_proposed"].as_u64() >= Some(1), "{r1:#}");
        assert!(r1["seq"]["borg_usd"].as_f64() > Some(0.0) && r1["seq"]["learn"]["exit"] == 0, "{r1:#}");
        let total = r1["seq"]["charged_usd"].as_f64().unwrap() + r1["seq"]["borg_usd"].as_f64().unwrap();
        assert!((r1["seq"]["charged_total_usd"].as_f64().unwrap() - total).abs() < 1e-9);
        assert_eq!(r1["hidden"]["solve_share"], 1.0, "{r1:#}");
        assert_eq!(r1["arm"], "l01", "the row's arm is the learning arm");
        let m = &r1["seq"]["mistakes"];
        assert_eq!(m["escaped"], 0, "{r1:#}");
        assert_eq!(m["total"].as_u64(), Some(r1["verifier_rejects"].as_u64().unwrap() + r1["failed_integrations"].as_u64().unwrap()), "{r1:#}");
        assert!(!c01.join("home/work").exists(), "the workspace is pruned after the goal");
        // The simulated engineer promotes it; a second promote answers nothing.
        let p = hive_seq::promote(&c01, "chain1", borg::learn::now_ms()).unwrap();
        assert!(!p["ids"].as_array().unwrap().is_empty(), "{p}");
        assert!(hive_seq::promote(&c01, "chain1", borg::learn::now_ms()).unwrap()["ids"].as_array().unwrap().is_empty());
        assert!(hive_seq::facts(&store(&c01)).playbook_promoted >= 1);
        let promoted = borg::playbook::read(&borg::collective::store(&store(&c01), hive_seq::COLLECTIVE));
        assert_eq!(promoted.len(), 1, "one playbook");

        // Goal 2 in the same chain: the store persisted, and the promoted playbook matches its tasks. Whether
        // a drone is shown it is counted (`playbook_shown`), not asserted: borg shows a claim's playbooks only
        // on a prompt that finds the session holding the claim, and the runner's headless iterations never
        // start holding one (each ended iteration's claims are released), so that count measures delivery.
        assert_eq!(cell(&g2, "chain-g2", "2", "l01", &c01), 0, "{:#}", last());
        let r2 = last();
        assert!(r2["seq"]["playbook_lines_before"].as_u64() >= Some(1), "{r2:#}");
        assert!(r2["seq"]["playbook_shown"].is_u64(), "{r2:#}");
        // At the share the hive asks of a task (queen.rs `STATED`, 0.6).
        let title = "Wire protocol frame change: write done t1 into w/f1.txt w/f1.txt";
        assert_eq!(borg::playbook::matching(&promoted, "toy", title, 0.6).len(), 1, "goal 2's task is of the playbook's shape");
        assert_eq!(r2["seq"]["restored_snapshot"], false);

        // Frozen: no learner call, no lesson, no playbook, goal after goal.
        for (wd, goal, pos) in [(&g1, "chain-g1", "1"), (&g2, "chain-g2", "2")] {
            assert_eq!(cell(wd, goal, pos, "frozen", &cf), 0, "{:#}", last());
            let r = last();
            assert_eq!(
                (r["seq"]["lessons_after"].as_u64(), r["seq"]["playbook_proposed"].as_u64(), r["seq"]["borg_usd"].as_f64()),
                (Some(0), Some(0), Some(0.0)),
                "{r:#}"
            );
            assert!(r["seq"]["learn"].is_null(), "frozen runs no learner");
            assert!(hive_seq::promote(&cf, goal, borg::learn::now_ms()).unwrap()["ids"].as_array().unwrap().is_empty());
        }
        let f = hive_seq::facts(&store(&cf));
        assert_eq!((f.lessons, f.playbook_lines, f.playbook_open), (0, 0, 0));

        // A retry of goal 2 starts from the store as it was before goal 2's first attempt.
        let first = repo_root().join("target/eval-runs").join(&run).join("e2e-chain2-l01-s0");
        fs::rename(&first, first.with_extension("attempt-1")).unwrap();
        fs::write(store(&c01).join("half-learned.jsonl"), "x\n").unwrap();
        assert_eq!(cell(&g2, "chain-g2", "2", "l01", &c01), 0, "{:#}", last());
        let r3 = last();
        assert_eq!(r3["seq"]["restored_snapshot"], true, "{r3:#}");
        assert!(!store(&c01).join("half-learned.jsonl").exists(), "the failed attempt's state never leaks");
        assert_eq!(r3["seq"]["playbook_lines_before"], r2["seq"]["playbook_lines_before"]);

        // A goal that fails before its run (here its snapshot cannot be written) is named in the report as
        // missing, not counted as the hive's mistakes.
        fs::create_dir_all(cf.join("snap/p3-e2e-chain1.tar.part")).unwrap();
        let done = repo_root().join("target/eval-runs").join(&run).join("e2e-chain1-frozen-s0");
        fs::rename(&done, done.with_extension("attempt-1")).unwrap();
        assert_ne!(cell(&g1, "chain-g1b", "3", "frozen", &cf), 0);
        let r4 = last();
        assert!(!r4["error"].is_null(), "{r4:#}");
        assert_eq!((r4["seq"]["arm"].as_str(), r4["seq"]["position"].as_u64()), (Some("frozen"), Some(3)), "{r4:#}");

        let report = hive_seq::h5_report(&rows_of(&run));
        assert!(report.contains("| l01 |") && report.contains("| frozen |"), "{report}");
        assert!(
            report.contains("never started (every attempt failed at setup, before the hive ran; missing, not counted): frozen seed 0 position 3"),
            "{report}"
        );
        forget_validation("e2e-chain1");
        forget_validation("e2e-chain2");
        for d in [g1, g2, root1, root2, chains, repo_root().join("target/eval-runs").join(&run)] {
            let _ = hive_seq::remove_tree(&d);
        }
    }
}
