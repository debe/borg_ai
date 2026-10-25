//! A hive goal sequence for `borg-eval hive` (tasks/todo.md "H5: does the hive learn?", the pre-registered
//! rule; docs/design/hive.md "The learning curve"): learning arms, a store that persists across the goals of a
//! chain, the simulated engineer who promotes queued playbook edits between goals, and the H5 report.
//!
//! - Arms (`--learn`): `frozen` (every learning switch off), `l0` (lessons on, playbooks off), `l01` (lessons
//!   and playbooks on). Every arm runs borg's delivery as shipped (`arm_env`, `ON`), its model calls charged
//!   to the goal, so the arms differ only in learning. Every arm also gets `BORG_QUEEN_ASK=off`, so drones
//!   are never handed queue questions: only the simulated engineer answers them, between goals.
//! - Chain (`--chain DIR`): one sandbox HOME per (arm, seed) at `<chain>/home`, so absolute paths in the store
//!   stay the same from goal to goal. Before each goal everything in it but the store (`.local/share/borg`)
//!   and borg's config is removed, and the store is saved to `<chain>/snap/<goal>.tar`; a goal that runs
//!   again (a retry) first gets its store back from that snapshot, so a failed attempt's half-learned state
//!   never leaks into the next one.
//! - `hive promote --chain DIR --after GOAL`: answers every open playbook question of the chain's store with
//!   "promote" as `sim-engineer@eval` (0 model calls) and logs it to `<chain>/promoted.jsonl`; other
//!   questions stay open and are counted.
//! - Report: rows with a `seq` object get the H5 block (`h5_report`), the amended rule: mistakes per goal =
//!   hidden checks failing on the integrated tree + hacked (escaped) + verifier rejects + failed
//!   integrations (caught). H5 holds iff over positions 5-8 (paired goals of l01 and frozen) l01's mean
//!   mistakes per goal <= 0.75 x frozen's AND l01's escaped mistakes <= frozen's AND l01's charged $ <= 2 x
//!   frozen's. Positions 1-4 of the two arms not within each other's +-25% are flagged; the trend over k is
//!   reported per arm; the $/solve ratio r of the superseded rule is a secondary line.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The collective every hive cell's workspace routes to (hive.rs `COLLECTIVES`).
pub(crate) const COLLECTIVE: &str = "pilot";
/// Who answers the chain's playbook questions: the declared deviation, a simulated engineer.
pub(crate) const SIM_ENGINEER: &str = "sim-engineer@eval";
/// What a chain's HOME keeps from goal to goal (HOME-relative).
pub(crate) const KEEP: [&str; 2] = [".local/share/borg", ".config/borg"];

/// A learning arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Learn {
    Frozen,
    L0,
    L01,
}

impl Learn {
    pub(crate) fn parse(s: &str) -> Option<Learn> {
        match s {
            "frozen" => Some(Learn::Frozen),
            "l0" => Some(Learn::L0),
            "l01" => Some(Learn::L01),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Learn::Frozen => "frozen",
            Learn::L0 => "l0",
            Learn::L01 => "l01",
        }
    }

    /// The arm writes lessons (and runs `borg learn` after each goal).
    pub(crate) fn learns(self) -> bool {
        self != Learn::Frozen
    }
}

/// The pilot's overrides (hive.rs `BORG_OFF`) every learning arm drops: borg runs with its defaults there, so
/// "everything on" is borg as shipped (the consequence model decides at the turn end and during the turn,
/// vector recall, decision-to-fix links), not the pilot's word-matching-only delivery at the turn end.
pub(crate) const ON: [&str; 7] = ["BORG_LEARN", "BORG_LEARN_MODEL", "BORG_PREDICT_MODEL", "BORG_MATCH", "BORG_MODE", "BORG_VECTORS", "BORG_REGRESS"];

/// The learning switches frozen turns off (tasks/todo.md H5: the four pre-registered ones, and the rework
/// reflection of amendment 2, which borg reads where it is built).
pub(crate) const LEARNING_OFF: [&str; 5] = ["BORG_LEARN", "BORG_PLAYBOOKS", "BORG_GROUP_REFLECT", "BORG_HIVE_CREDIT", "BORG_REWORK_REFLECT"];

/// The env of an arm. Without one: `off` (every borg model call off), as in the pilot. Every learning arm
/// runs the same delivery: borg's defaults (the `ON` overrides of `off` dropped), borg's model calls
/// (learner, consequence model) on the arm's model (`claude:<model>`) and charged to the goal from their
/// events (`borg_usd_in`), and `BORG_QUEEN_ASK=off`, so drones are never handed queue questions: only the
/// simulated engineer answers them, between goals. The arms differ only in learning: l01 everything on, l0
/// without playbooks (`BORG_PLAYBOOKS=off`), frozen with every `LEARNING_OFF` switch off.
pub(crate) fn arm_env(off: &[(&str, &str)], learn: Option<Learn>, model: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = off.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let Some(l) = learn else { return env };
    let set = |env: &mut Vec<(String, String)>, k: &str, v: &str| {
        env.retain(|(x, _)| x != k);
        env.push((k.to_string(), v.to_string()));
    };
    env.retain(|(k, _)| !ON.contains(&k.as_str()));
    let spec = format!("claude:{model}");
    set(&mut env, "BORG_LEARN_MODEL", &spec);
    set(&mut env, "BORG_PREDICT_MODEL", &spec);
    set(&mut env, "BORG_QUEEN_ASK", "off");
    match l {
        Learn::Frozen => {
            for k in LEARNING_OFF {
                set(&mut env, k, "off");
            }
        }
        Learn::L0 => set(&mut env, "BORG_PLAYBOOKS", "off"),
        Learn::L01 => {}
    }
    env
}

// ------------------------------------------------------------------------------------- chain HOME

/// Removes everything under `home` except the `keep` paths (HOME-relative) and the directories leading to
/// them. Read-only build caches are made writable first.
pub(crate) fn reset_keeping(home: &Path, keep: &[&str]) -> Result<(), String> {
    let keep: Vec<PathBuf> = keep.iter().map(|k| home.join(k)).collect();
    fn walk(dir: &Path, keep: &[PathBuf]) -> Result<(), String> {
        for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
            let p = e.path();
            if keep.contains(&p) {
                continue;
            }
            let is_dir = fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
            if is_dir && keep.iter().any(|k| k.starts_with(&p)) {
                walk(&p, keep)?;
            } else if is_dir {
                remove_tree(&p)?;
            } else {
                fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        Ok(())
    }
    if !home.exists() {
        return Ok(());
    }
    walk(home, &keep)
}

/// `rm -rf` that also removes read-only trees (Gradle's and Go's caches).
pub(crate) fn remove_tree(p: &Path) -> Result<(), String> {
    if fs::remove_dir_all(p).is_ok() || !p.exists() {
        return Ok(());
    }
    fn writable(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let Ok(m) = fs::symlink_metadata(p) else { return };
        if m.is_dir() {
            let _ = fs::set_permissions(p, fs::Permissions::from_mode(m.permissions().mode() | 0o700));
            for e in fs::read_dir(p).into_iter().flatten().flatten() {
                writable(&e.path());
            }
        }
    }
    writable(p);
    fs::remove_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Before a goal: saves the store (`<home>/.local/share/borg`) to `snap`; when `snap` exists (the goal ran
/// before and is run again), restores the store from it instead. Returns whether it restored.
pub(crate) fn snapshot_or_restore(home: &Path, snap: &Path) -> Result<bool, String> {
    let store = home.join(KEEP[0]);
    if let Some(d) = snap.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let run = |args: &[&str]| -> Result<(), String> {
        let out = std::process::Command::new("tar").args(args).output().map_err(|e| format!("tar: {e}"))?;
        match out.status.success() {
            true => Ok(()),
            false => Err(format!("tar {args:?}: {}", String::from_utf8_lossy(&out.stderr).trim())),
        }
    };
    let h = home.display().to_string();
    if snap.exists() {
        remove_tree(&store)?;
        run(&["-x", "-f", &snap.display().to_string(), "-C", &h])?;
        return Ok(true);
    }
    fs::create_dir_all(&store).map_err(|e| e.to_string())?;
    let tmp = snap.with_extension("tar.part");
    run(&["-c", "-f", &tmp.display().to_string(), "-C", &h, KEEP[0]])?;
    fs::rename(&tmp, snap).map_err(|e| e.to_string())?;
    Ok(false)
}

// ------------------------------------------------------------------------------------- store facts

fn jsonl(p: &Path) -> Vec<Value> {
    fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Every file named `name` under `root`.
fn files_named(root: &Path, name: &str) -> Vec<PathBuf> {
    fn walk(d: &Path, name: &str, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false) {
                walk(&p, name, out);
            } else if p.file_name().is_some_and(|f| f == name) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, name, &mut out);
    out.sort();
    out
}

/// What a store (`root`: `<HOME>/.local/share/borg`, every collective under it) holds of learning.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Facts {
    /// Distinct lesson ids.
    pub(crate) lessons: usize,
    /// Lines of `playbooks.jsonl`, and those promoted (`ok_by`).
    pub(crate) playbook_lines: usize,
    pub(crate) playbook_promoted: usize,
    /// Group reflection episodes (`group-<goal>.<task>`, hive/group.rs `episode_id`).
    pub(crate) group_episodes: usize,
    /// Open queen questions of kind playbook, and of any other kind.
    pub(crate) playbook_open: usize,
    pub(crate) other_open: usize,
}

pub(crate) fn facts(root: &Path) -> Facts {
    let ids: BTreeSet<String> = files_named(root, "lessons.jsonl").iter().flat_map(|p| jsonl(p)).filter_map(|l| l["id"].as_str().map(String::from)).collect();
    let pb: Vec<Value> = files_named(root, borg::playbook::FILE).iter().flat_map(|p| jsonl(p)).collect();
    let episodes =
        files_named(root, "episodes.jsonl").iter().flat_map(|p| jsonl(p)).filter(|e| e["id"].as_str().is_some_and(|i| i.starts_with("group-"))).count();
    let (mut playbook_open, mut other_open) = (0, 0);
    for q in files_named(root, "queue.jsonl") {
        let Some(store) = q.parent().and_then(Path::parent) else { continue };
        for e in borg::queen::open(store) {
            match e.kind == borg::queen::Kind::Playbook {
                true => playbook_open += 1,
                false => other_open += 1,
            }
        }
    }
    Facts {
        lessons: ids.len(),
        playbook_lines: pb.len(),
        playbook_promoted: pb.iter().filter(|e| e["ok_by"].as_str().is_some_and(|s| !s.is_empty())).count(),
        group_episodes: episodes,
        playbook_open,
        other_open,
    }
}

/// Outcome records of `kind` with `from <= ms <= to`, over every `outcomes.jsonl` under `root`.
pub(crate) fn outcomes_in(root: &Path, kind: &str, from: i64, to: i64) -> usize {
    files_named(root, "outcomes.jsonl")
        .iter()
        .flat_map(|p| jsonl(p))
        .filter(|o| o["kind"] == kind && o["ms"].as_i64().is_some_and(|m| from <= m && m <= to))
        .count()
}

/// Borg's own model dollars (not in the drones' spend): `usd` over the events of borg's model calls with
/// `from <= ms <= to`, over every `stats/events.jsonl` under `root`: the learner's and the assimilator's
/// (`model_call`) and the consequence model's at the turn end and during the turn (`check`, `live_check`).
pub(crate) fn borg_usd_in(root: &Path, from: i64, to: i64) -> f64 {
    let kinds = [borg::stats::MODEL_CALL, borg::stats::CHECK, borg::stats::LIVE_CHECK];
    files_named(root, "events.jsonl")
        .iter()
        .filter(|p| p.parent().is_some_and(|d| d.ends_with("stats")))
        .flat_map(|p| jsonl(p))
        .filter(|e| kinds.iter().any(|k| e["kind"] == *k) && e["ms"].as_i64().is_some_and(|m| from <= m && m <= to))
        .filter_map(|e| e["usd"].as_f64())
        // Not `sum()`: an empty f64 sum is -0.0, which a row would print as such.
        .fold(0.0, |a, b| a + b)
}

/// New lessons by where they came from (`why` of borg's `lesson_new` events with `from <= ms <= to`: group,
/// session, veto, regret, review, personal), over every `stats/events.jsonl` under `root`.
pub(crate) fn lessons_new_by_source(root: &Path, from: i64, to: i64) -> BTreeMap<String, u64> {
    let mut by = BTreeMap::new();
    for e in files_named(root, "events.jsonl")
        .iter()
        .filter(|p| p.parent().is_some_and(|d| d.ends_with("stats")))
        .flat_map(|p| jsonl(p))
        .filter(|e| e["kind"] == borg::stats::LESSON_NEW && e["ms"].as_i64().is_some_and(|m| from <= m && m <= to))
    {
        let why = e["why"].as_str().filter(|w| !w.is_empty()).unwrap_or("unknown");
        *by.entry(why.to_string()).or_insert(0) += 1;
    }
    by
}

/// Mistakes of one goal (tasks/todo.md H5 amendment): escaped = hidden checks failing on the integrated
/// tree + hacked (the scored tree has a protected change); caught = verifier rejects + failed
/// integrations. (total, escaped, caught).
pub(crate) fn mistakes(k: usize, passed: usize, hacked: u64, verifier_rejects: u64, failed_integrations: u64) -> (u64, u64, u64) {
    let escaped = k.saturating_sub(passed) as u64 + hacked;
    let caught = verifier_rejects + failed_integrations;
    (escaped + caught, escaped, caught)
}

/// $ per solve of a goal: its dollars over its hidden solve share, an unsolved goal at share 1/K.
pub(crate) fn usd_per_solve(usd: f64, share: f64, k: usize) -> f64 {
    usd / share.max(1.0 / k.max(1) as f64)
}

// ------------------------------------------------------------------------------------- promote

/// `borg-eval hive promote --chain DIR --after GOAL`: the simulated engineer promotes every open playbook
/// question of the chain's store and logs it; idempotent (only open questions are answered).
pub(crate) fn promote_cmd(raw: &[String]) -> i32 {
    let (mut chain, mut after) = (None, None);
    let mut it = raw.iter();
    while let Some(f) = it.next() {
        match (f.as_str(), it.next()) {
            ("--chain", Some(v)) => chain = Some(PathBuf::from(v)),
            ("--after", Some(v)) => after = Some(v.clone()),
            (other, _) => {
                eprintln!("borg-eval hive promote: unknown flag or missing value {other}");
                return 2;
            }
        }
    }
    let (Some(chain), Some(after)) = (chain, after) else {
        eprintln!("borg-eval hive promote: --chain DIR and --after GOAL are required");
        return 2;
    };
    match promote(&chain, &after, borg::learn::now_ms()) {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            eprintln!("borg-eval hive promote: {e}");
            1
        }
    }
}

/// Promotes the open playbook questions of `<chain>/home`'s collective store; appends `{after, ids,
/// other_open, ms}` to `<chain>/promoted.jsonl` and returns it.
pub(crate) fn promote(chain: &Path, after: &str, now: i64) -> Result<Value, String> {
    let root = chain.join("home").join(KEEP[0]);
    let store = borg::collective::store(&root, COLLECTIVE);
    let open = borg::queen::open(&store);
    let mut ids = Vec::new();
    for e in open.iter().filter(|e| e.kind == borg::queen::Kind::Playbook) {
        borg::queen::answer(&store, &e.id, "promote", SIM_ENGINEER, now)?;
        ids.push(e.id.clone());
    }
    let other_open = open.iter().filter(|e| e.kind != borg::queen::Kind::Playbook).count();
    let v = json!({"after": after, "ids": ids, "other_open": other_open, "ms": now});
    crate::hive::append_row(&chain.join("promoted.jsonl"), &v)?;
    Ok(v)
}

// ------------------------------------------------------------------------------------- report

/// Rounding slack for the pre-registered inequalities (inclusive).
const EPS: f64 = 1e-9;
/// Positions: 1-4 early, 5-8 late (the 8 main goals), 9-10 the held-out goals (transfer).
const EARLY: [u64; 4] = [1, 2, 3, 4];
const LATE: [u64; 4] = [5, 6, 7, 8];
const TRANSFER: [u64; 2] = [9, 10];
pub(crate) const ARMS: [&str; 3] = ["frozen", "l0", "l01"];

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

/// One goal of a chain as the report reads it.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    arm: String,
    seed: u64,
    position: u64,
    goal: String,
    /// Mistakes (`mistakes`): all, and those that escaped to the integrated tree.
    mistakes: f64,
    escaped: f64,
    /// Charged dollars of every attempt at the goal: the drones' and borg's own model calls.
    usd: f64,
    usd_per_solve: f64,
    share: f64,
    /// No attempt ended without an error: the goal counts as failed (all K checks escaped, share 0).
    failed: bool,
}

/// The rows by goal: (arm, seed, position) -> its attempts' rows, in order.
fn by_goal(rows: &[Value]) -> BTreeMap<(String, u64, u64), Vec<&Value>> {
    let mut by: BTreeMap<(String, u64, u64), Vec<&Value>> = BTreeMap::new();
    for r in rows {
        let s = &r["seq"];
        let (Some(arm), Some(seed), Some(pos)) = (s["arm"].as_str(), s["seed"].as_u64(), s["position"].as_u64()) else { continue };
        by.entry((arm.to_string(), seed, pos)).or_default().push(r);
    }
    by
}

fn goal_of(rs: &[&Value]) -> String {
    rs.iter().find_map(|r| r["seq"]["goal"].as_str()).unwrap_or("?").to_string()
}

/// A goal no attempt of which started its hive run: every row errored before anything was paid (a setup
/// failure of the harness: snapshot, sandbox, install, init). Not the hive's mistake: the goal is missing.
fn never_started(rs: &[&Value]) -> bool {
    rs.iter().all(|r| r["paid"] == false && !r["error"].is_null())
}

/// The goals that never started (`never_started`): (arm, seed, position, goal).
fn unstarted(rows: &[Value]) -> Vec<(String, u64, u64, String)> {
    by_goal(rows).into_iter().filter(|(_, rs)| never_started(rs)).map(|((a, s, p), rs)| (a, s, p, goal_of(&rs))).collect()
}

/// The rows' goals, one per (arm, seed, position) whose hive run started (`never_started` goals are
/// missing). A goal's dollars are every attempt's (`charged_of`); its mistakes and share are its clean
/// row's (the last one), or, when every attempt errored, those of a failed goal: all K hidden checks
/// failing (K escaped mistakes), share 0.
fn cells(rows: &[Value]) -> Vec<Cell> {
    let mut out = Vec::new();
    for ((arm, seed, position), rs) in by_goal(rows) {
        if never_started(&rs) {
            continue;
        }
        let usd: f64 = rs.iter().map(|r| crate::hive::charged_of(r)).sum();
        let clean = rs.iter().rev().find(|r| r["error"].is_null() && r["seq"]["mistakes"]["total"].is_number());
        let k = rs.iter().find_map(|r| r["seq"]["K"].as_u64().or_else(|| r["K"].as_u64())).unwrap_or(1).max(1) as usize;
        let goal = goal_of(&rs);
        let (mistakes, escaped, share, failed) = match clean {
            Some(r) => {
                let m = &r["seq"]["mistakes"];
                (m["total"].as_f64().unwrap_or(0.0), m["escaped"].as_f64().unwrap_or(0.0), r["hidden"]["solve_share"].as_f64().unwrap_or(0.0), false)
            }
            None => (k as f64, k as f64, 0.0, true),
        };
        out.push(Cell { arm, seed, position, goal, mistakes, escaped, usd, usd_per_solve: usd_per_solve(usd, share, k), share, failed });
    }
    out
}

/// The cells of `a` and `b` at `positions` that both arms have (same seed, same position): the gate
/// compares like with like, so a goal one arm lost leaves the other arm's same goal out too.
fn paired<'a>(cs: &'a [Cell], a: &str, b: &str, positions: &[u64]) -> Vec<(&'a Cell, &'a Cell)> {
    let at = |arm: &str, c: &Cell| cs.iter().find(|x| x.arm == arm && x.seed == c.seed && x.position == c.position);
    cs.iter().filter(|c| c.arm == a && positions.contains(&c.position)).filter_map(|c| at(b, c).map(|o| (c, o))).collect()
}

/// The H5 gate (tasks/todo.md H5 amendment) over the paired positions 5-8 of l01 and frozen.
#[derive(Debug, Clone, PartialEq)]
struct Gate {
    pairs: usize,
    /// Mean mistakes per goal, escaped mistakes summed, charged $ summed: (l01, frozen).
    mistakes: (f64, f64),
    escaped: (f64, f64),
    usd: (f64, f64),
    holds: bool,
    /// Frozen's mean mistakes per goal reach `FLOOR`: there are mistakes for learning to remove.
    informative: bool,
}

/// The least mean mistakes per goal frozen must make over positions 5-8 for the gate to say anything
/// (tasks/todo.md H5 amendment 3): below it, "not informative", never a pass.
const FLOOR: f64 = 0.5;

/// H5 holds iff l01's mean mistakes per goal <= 0.75 x frozen's AND l01's escaped mistakes <= frozen's
/// AND l01's charged $ <= 2 x frozen's (inclusive); None without a pair.
fn gate(cs: &[Cell]) -> Option<Gate> {
    let p = paired(cs, "l01", "frozen", &LATE);
    if p.is_empty() {
        return None;
    }
    let sum = |f: &dyn Fn(&(&Cell, &Cell)) -> f64| p.iter().map(f).sum::<f64>();
    let n = p.len() as f64;
    let mistakes = (sum(&|(a, _)| a.mistakes) / n, sum(&|(_, b)| b.mistakes) / n);
    let escaped = (sum(&|(a, _)| a.escaped), sum(&|(_, b)| b.escaped));
    let usd = (sum(&|(a, _)| a.usd), sum(&|(_, b)| b.usd));
    let holds = mistakes.0 <= 0.75 * mistakes.1 + EPS && escaped.0 <= escaped.1 + EPS && usd.0 <= 2.0 * usd.1 + EPS;
    Some(Gate { pairs: p.len(), mistakes, escaped, usd, holds, informative: mistakes.1 + EPS >= FLOOR })
}

/// A value of l01 and of frozen.
type Both = (f64, f64);

/// Within each other's +-25%: |a - b| <= 0.25 x min(a, b).
fn within_25(a: f64, b: f64) -> bool {
    (a - b).abs() <= 0.25 * a.min(b) + EPS
}

/// The sanity check (on mistakes; $ reported beside it): positions 1-4 of l01 and frozen (paired), mean
/// mistakes and mean charged $ per goal;
/// (pairs, (mistakes l01, frozen), (usd l01, frozen)), None without a pair.
fn early(cs: &[Cell]) -> Option<(usize, Both, Both)> {
    let p = paired(cs, "l01", "frozen", &EARLY);
    let n = p.len() as f64;
    (!p.is_empty()).then(|| {
        let m = (p.iter().map(|(a, _)| a.mistakes).sum::<f64>() / n, p.iter().map(|(_, b)| b.mistakes).sum::<f64>() / n);
        let u = (p.iter().map(|(a, _)| a.usd).sum::<f64>() / n, p.iter().map(|(_, b)| b.usd).sum::<f64>() / n);
        (p.len(), m, u)
    })
}

/// The trend over k of one arm: mean mistakes per position (1-10, those it has), and the least-squares
/// slope over positions 1-8 (None with fewer than two positions).
fn trend(cs: &[Cell], arm: &str) -> (Vec<(u64, f64)>, Option<f64>) {
    let by: Vec<(u64, f64)> =
        (1..=10).filter_map(|p| mean(&cs.iter().filter(|c| c.arm == arm && c.position == p).map(|c| c.mistakes).collect::<Vec<_>>()).map(|m| (p, m))).collect();
    let main: Vec<(f64, f64)> = by.iter().filter(|(p, _)| *p <= 8).map(|(p, m)| (*p as f64, *m)).collect();
    let slope = (main.len() >= 2).then(|| {
        let n = main.len() as f64;
        let (mx, my) = (main.iter().map(|x| x.0).sum::<f64>() / n, main.iter().map(|x| x.1).sum::<f64>() / n);
        main.iter().map(|(x, y)| (x - mx) * (y - my)).sum::<f64>() / main.iter().map(|(x, _)| (x - mx).powi(2)).sum::<f64>()
    });
    (by, slope)
}

/// r of one seed: (mean $/solve at 5-8) / (mean at 1-4); None when either half has no goal or the early mean
/// is 0. Secondary (the superseded gate's ratio).
fn ratio(cs: &[&Cell]) -> Option<f64> {
    let at = |ps: &[u64]| mean(&cs.iter().filter(|c| ps.contains(&c.position)).map(|c| c.usd_per_solve).collect::<Vec<_>>());
    let (e, l) = (at(&EARLY)?, at(&LATE)?);
    (e > 0.0).then(|| l / e)
}

/// r(arm) over `cs`: the mean of the seeds' ratios, and the seeds it averaged.
fn r_of(cs: &[Cell], arm: &str) -> (Option<f64>, Vec<u64>) {
    let seeds: BTreeSet<u64> = cs.iter().filter(|c| c.arm == arm).map(|c| c.seed).collect();
    let mut rs = Vec::new();
    let mut used = Vec::new();
    for s in seeds {
        let mine: Vec<&Cell> = cs.iter().filter(|c| c.arm == arm && c.seed == s).collect();
        if let Some(r) = ratio(&mine) {
            rs.push(r);
            used.push(s);
        }
    }
    (mean(&rs), used)
}

/// Mean of `f` over `arm`'s cells at `positions`.
fn mean_at(cs: &[Cell], arm: &str, positions: &[u64], f: impl Fn(&Cell) -> f64) -> Option<f64> {
    mean(&cs.iter().filter(|c| c.arm == arm && positions.contains(&c.position)).map(f).collect::<Vec<_>>())
}

/// A small deterministic generator for the bootstrap (xorshift64*).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % n.max(1)
    }
}

/// Bootstrap over seeds x goals: seeds resampled with replacement, and within each resampled seed its early
/// and late goals resampled within their half (arms alike, by the same draws). The 2.5/97.5 percentiles of
/// `stat` over the draws where it is defined; None when it never is.
fn bootstrap(cs: &[Cell], n: usize, stat: impl Fn(&[Cell]) -> Option<f64>) -> Option<(f64, f64)> {
    let seeds: Vec<u64> = cs.iter().map(|c| c.seed).collect::<BTreeSet<_>>().into_iter().collect();
    if seeds.is_empty() {
        return None;
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut xs = Vec::new();
    for _ in 0..n {
        let mut sample = Vec::new();
        for (i, _) in seeds.iter().enumerate() {
            let s = seeds[rng.below(seeds.len())];
            // (drawn position, the slot it fills): 4 draws from the early half fill 1-4, 4 from the late 5-8.
            let mut draws: Vec<(u64, u64)> = Vec::new();
            for (half, slot0) in [(&EARLY, 1), (&LATE, 5)] {
                for j in 0..half.len() as u64 {
                    draws.push((half[rng.below(half.len())], slot0 + j));
                }
            }
            for arm in ARMS {
                for (from, to) in &draws {
                    if let Some(c) = cs.iter().find(|c| c.arm == arm && c.seed == s && c.position == *from) {
                        sample.push(Cell { seed: i as u64, position: *to, ..c.clone() });
                    }
                }
            }
        }
        xs.extend(stat(&sample));
    }
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    let at = |q: f64| xs[((xs.len() - 1) as f64 * q).round() as usize];
    Some((at(0.025), at(0.975)))
}

/// The H5 block for the rows with a `seq` object (empty when none has one).
pub(crate) fn h5_report(rows: &[Value]) -> String {
    let seq: Vec<Value> = rows.iter().filter(|r| r["seq"].is_object()).cloned().collect();
    if seq.is_empty() {
        return String::new();
    }
    let cs = cells(&seq);
    let f = |x: Option<f64>, p: usize| x.map_or("-".to_string(), |x| format!("{x:.p$}"));
    let mut out = String::from("\n## H5: does the hive learn? (pre-registered, tasks/todo.md, amended: mistakes primary, $ a guard)\n\n");
    out.push_str(
        "| Arm | goals | failed | mistakes/goal 1-4 | mistakes/goal 5-8 | escaped/goal 5-8 | $/goal 1-4 | $/goal 5-8 | mean share 1-8 | mistakes/goal 9-10 | share 9-10 | lessons new/goal | playbook edits proposed/goal | rework | hacked |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for arm in ARMS {
        let n = cs.iter().filter(|c| c.arm == arm).count();
        if n == 0 {
            continue;
        }
        let at = |ps: &[u64], g: fn(&Cell) -> f64| mean_at(&cs, arm, ps, g);
        let of_arm: Vec<&Value> = seq.iter().filter(|r| r["seq"]["arm"] == arm && r["error"].is_null()).collect();
        let per_goal = |k: &str| mean(&of_arm.iter().filter_map(|r| r["seq"][k].as_f64()).collect::<Vec<_>>());
        let sum = |k: &str| of_arm.iter().map(|r| r[k].as_u64().unwrap_or(0)).sum::<u64>();
        let main: Vec<u64> = EARLY.iter().chain(&LATE).copied().collect();
        out.push_str(&format!(
            "| {arm} | {n} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cs.iter().filter(|c| c.arm == arm && c.failed).count(),
            f(at(&EARLY, |c| c.mistakes), 2),
            f(at(&LATE, |c| c.mistakes), 2),
            f(at(&LATE, |c| c.escaped), 2),
            f(at(&EARLY, |c| c.usd), 2),
            f(at(&LATE, |c| c.usd), 2),
            f(at(&main, |c| c.share), 3),
            f(at(&TRANSFER, |c| c.mistakes), 2),
            f(at(&TRANSFER, |c| c.share), 3),
            f(per_goal("lessons_new"), 2),
            f(per_goal("playbook_proposed"), 2),
            sum("rework"),
            sum("hacked"),
        ));
    }
    out.push('\n');
    match gate(&cs) {
        Some(g) => out.push_str(&format!(
            "H5 (positions 5-8, {} goal pair(s) l01/frozen): mistakes/goal l01 {:.3} <= 0.75 x frozen {:.3} ({:.3})? AND escaped l01 {:.0} <= frozen {:.0}? AND $ l01 {:.2} <= 2 x frozen {:.2} ({:.2})? -> {}\n",
            g.pairs,
            g.mistakes.0,
            g.mistakes.1,
            0.75 * g.mistakes.1,
            g.escaped.0,
            g.escaped.1,
            g.usd.0,
            g.usd.1,
            2.0 * g.usd.1,
            if !g.informative {
                "NOT INFORMATIVE (frozen makes fewer than 0.5 mistakes per goal: no headroom on mistakes)"
            } else if g.holds {
                "HOLDS"
            } else {
                "FAILS (kill: stop at L0, write up why)"
            }
        )),
        None => out.push_str("H5: no position 5-8 goal that both l01 and frozen have yet -> not decided\n"),
    }
    if let Some(g) = gate(&cs)
        && g.pairs < 12
    {
        out.push_str(&format!("FLAG: the gate read {} of 12 goal pairs (3 seeds x positions 5-8); the rest are missing below.\n", g.pairs));
    }
    if let Some((n, m, u)) = early(&cs) {
        out.push_str(&format!(
            "Sanity (positions 1-4, {n} pair(s)): mistakes/goal l01 {:.3} vs frozen {:.3}: {} ($/goal l01 {:.2} vs frozen {:.2}, reported)\n",
            m.0,
            m.1,
            if within_25(m.0, m.1) { "within each other's +-25%" } else { "FLAG: not within each other's +-25%: the arms differ before learning could help" },
            u.0,
            u.1,
        ));
    }
    for arm in ARMS {
        let (by, slope) = trend(&cs, arm);
        if by.is_empty() {
            continue;
        }
        let pts: Vec<String> = by.iter().map(|(p, m)| format!("{p}:{m:.2}")).collect();
        out.push_str(&format!("Trend {arm}: mistakes/goal by position {}; slope over 1-8 {}/position\n", pts.join(" "), f(slope, 3)));
    }
    for arm in ARMS {
        let mut by: BTreeMap<String, u64> = BTreeMap::new();
        for r in seq.iter().filter(|r| r["seq"]["arm"] == arm && r["error"].is_null()) {
            for (k, v) in r["seq"]["lessons_new_by_source"].as_object().into_iter().flatten() {
                *by.entry(k.clone()).or_insert(0) += v.as_u64().unwrap_or(0);
            }
        }
        if !by.is_empty() {
            let parts: Vec<String> = by.iter().map(|(k, v)| format!("{k} {v}")).collect();
            out.push_str(&format!("Lessons new by source {arm}: {}\n", parts.join(", ")));
        }
    }
    if let (Some(m0), Some(m01)) = (mean_at(&cs, "l0", &LATE, |c| c.mistakes), mean_at(&cs, "l01", &LATE, |c| c.mistakes)) {
        out.push_str(&format!("L0 vs L0+L1 (do playbooks add?): mistakes/goal 5-8 l0 {m0:.3}, l01 {m01:.3}, difference {:.3}\n", m01 - m0));
    }
    let late_ratio = |s: &[Cell]| gate(s).filter(|g| g.mistakes.1 > 0.0).map(|g| g.mistakes.0 / g.mistakes.1);
    if let Some((a, b)) = bootstrap(&cs, 2000, late_ratio) {
        out.push_str(&format!("Bootstrap over seeds x goals (2000 draws): mistakes/goal 5-8 l01 / frozen 95% [{a:.3}, {b:.3}] (gate: <= 0.75)\n"));
    }
    // Secondary: the $/solve ratio of the superseded gate, reported only.
    let (r01, rf) = (r_of(&cs, "l01").0, r_of(&cs, "frozen").0);
    if r01.is_some() || rf.is_some() {
        out.push_str(&format!(
            "Secondary, not the gate: r = $/solve 5-8 / 1-4 (mean over seeds): l01 {}, l0 {}, frozen {}",
            f(r01, 3),
            f(r_of(&cs, "l0").0, 3),
            f(rf, 3)
        ));
        match rf.filter(|rf| !(0.85 - EPS..=1.15 + EPS).contains(rf)) {
            Some(_) => out.push_str("; r(frozen) is outside [0.85, 1.15]: the rotation did not balance cost\n"),
            None => out.push('\n'),
        }
    }
    let seeds: BTreeSet<u64> = cs.iter().map(|c| c.seed).collect();
    let unstarted = unstarted(&seq);
    for arm in ARMS {
        for s in &seeds {
            let have: BTreeSet<u64> = cs.iter().filter(|c| c.arm == arm && c.seed == *s).map(|c| c.position).collect();
            let gaps: Vec<u64> = EARLY.iter().chain(&LATE).filter(|p| !have.contains(p)).copied().collect();
            if !have.is_empty() && !gaps.is_empty() {
                out.push_str(&format!("{arm} seed {s}: positions {gaps:?} missing\n"));
            }
        }
    }
    for (arm, seed, position, goal) in &unstarted {
        out.push_str(&format!(
            "never started (every attempt failed at setup, before the hive ran; missing, not counted): {arm} seed {seed} position {position} {goal}\n"
        ));
    }
    for c in cs.iter().filter(|c| c.failed) {
        out.push_str(&format!(
            "failed goal (every attempt errored; counted as {:.0} escaped mistakes, share 0, ${:.2}): {} seed {} position {} {}\n",
            c.escaped, c.usd, c.arm, c.seed, c.position, c.goal
        ));
    }
    out.push_str(
        "Mistakes/goal = hidden checks failing on the integrated tree + hacked (escaped) + verifier rejects + failed integrations (caught). 3 seeds: point estimates, no CI claim; the bootstrap is reported, not a gate. $ includes borg's own model calls.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-hive-seq-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn get<'a>(env: &'a [(String, String)], k: &str) -> Option<&'a str> {
        env.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str())
    }

    const OFF: [(&str, &str); 6] = [
        ("BORG_LEARN", "off"),
        ("BORG_LEARN_MODEL", "off"),
        ("BORG_QUEEN", "off"),
        ("BORG_MODE", "hook"),
        ("BORG_MATCH", "bm25"),
        ("BORG_PREDICT_MODEL", "off"),
    ];

    #[test]
    fn the_arms_share_borgs_delivery_as_shipped_and_differ_only_in_learning() {
        assert_eq!(arm_env(&OFF, None, "sonnet").len(), OFF.len(), "no arm: the pilot's env as it was");
        assert_eq!(get(&arm_env(&OFF, None, "sonnet"), "BORG_QUEEN_ASK"), None);
        let frozen = arm_env(&OFF, Some(Learn::Frozen), "sonnet");
        let l0 = arm_env(&OFF, Some(Learn::L0), "sonnet");
        let l01 = arm_env(&OFF, Some(Learn::L01), "sonnet");
        for env in [&frozen, &l0, &l01] {
            assert_eq!(get(env, "BORG_QUEEN_ASK"), Some("off"), "only the simulated engineer answers the queue");
            assert_eq!(get(env, "BORG_QUEEN"), Some("off"), "the queen's turn-end call stays at borg's default");
            assert_eq!(env.iter().filter(|(k, _)| k == "BORG_QUEEN_ASK").count(), 1);
            for k in ["BORG_MODE", "BORG_MATCH", "BORG_VECTORS", "BORG_REGRESS"] {
                assert_eq!(get(env, k), None, "{k}: borg's default");
            }
            assert_eq!((get(env, "BORG_LEARN_MODEL"), get(env, "BORG_PREDICT_MODEL")), (Some("claude:sonnet"), Some("claude:sonnet")));
        }
        for k in LEARNING_OFF {
            assert_eq!(get(&frozen, k), Some("off"), "frozen {k}");
        }
        assert_eq!(get(&l0, "BORG_PLAYBOOKS"), Some("off"));
        for k in LEARNING_OFF {
            assert_eq!(get(&l01, k), None, "l01 {k}: on");
        }
        // Beyond the learning switches, the three envs are the same.
        let strip = |env: &[(String, String)]| {
            let mut v: Vec<(String, String)> = env.iter().filter(|(k, _)| !LEARNING_OFF.contains(&k.as_str())).cloned().collect();
            v.sort();
            v
        };
        assert_eq!(strip(&frozen), strip(&l01));
        assert_eq!(strip(&l0), strip(&l01));
        assert_eq!((Learn::parse("l01"), Learn::parse("x")), (Some(Learn::L01), None));
        assert!(Learn::L0.learns() && !Learn::Frozen.learns());
    }

    #[test]
    fn a_chain_home_keeps_only_the_store_and_a_retry_gets_the_snapshot_back() {
        let home = tmp("reset");
        for f in [
            ".local/share/borg/lessons.jsonl",
            ".local/share/other/x",
            ".local/bin/y",
            ".config/borg/collectives.json",
            ".config/gh/hosts",
            "work/app/a.txt",
            ".claude/settings.json",
            "top.txt",
        ] {
            let p = home.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        // A read-only build cache (Gradle's) goes too.
        let ro = home.join(".gradle/caches/ro");
        fs::create_dir_all(&ro).unwrap();
        fs::write(ro.join("f"), "x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o500)).unwrap();
        reset_keeping(&home, &KEEP).unwrap();
        let mut left: Vec<String> = Vec::new();
        for e in fs::read_dir(&home).unwrap().flatten() {
            left.push(e.file_name().to_string_lossy().into_owned());
        }
        left.sort();
        assert_eq!(left, [".config", ".local"]);
        assert!(home.join(".local/share/borg/lessons.jsonl").exists() && home.join(".config/borg/collectives.json").exists());
        assert!(!home.join(".local/share/other").exists() && !home.join(".local/bin").exists() && !home.join(".config/gh").exists());

        // Snapshot before the goal; the attempt writes; the retry restores the snapshot.
        let snap = home.join("../snap-test/p1-g1.tar");
        assert!(!snapshot_or_restore(&home, &snap).unwrap(), "first time: saved");
        fs::write(home.join(".local/share/borg/lessons.jsonl"), "half-learned\n").unwrap();
        fs::write(home.join(".local/share/borg/new.jsonl"), "x\n").unwrap();
        assert!(snapshot_or_restore(&home, &snap).unwrap(), "again: restored");
        assert_eq!(fs::read_to_string(home.join(".local/share/borg/lessons.jsonl")).unwrap(), "x");
        assert!(!home.join(".local/share/borg/new.jsonl").exists());
        let _ = fs::remove_dir_all(home.join("../snap-test"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn store_facts_count_lessons_playbooks_episodes_and_windows() {
        let root = tmp("facts");
        let c = borg::collective::store(&root, COLLECTIVE);
        fs::create_dir_all(c.join("stats")).unwrap();
        fs::write(c.join("lessons.jsonl"), "{\"id\":\"a\"}\n{\"id\":\"a\",\"v\":2}\n{\"id\":\"b\"}\n").unwrap();
        fs::write(c.join("playbooks.jsonl"), "{\"pb\":\"p\",\"ok_by\":\"q1\"}\n{\"pb\":\"p\"}\n").unwrap();
        fs::write(c.join("episodes.jsonl"), "{\"id\":\"group-g.t1\"}\n{\"id\":\"e1\"}\n").unwrap();
        fs::write(c.join("outcomes.jsonl"), "{\"kind\":\"playbook\",\"ms\":5}\n{\"kind\":\"playbook\",\"ms\":50}\n{\"kind\":\"asked\",\"ms\":6}\n").unwrap();
        fs::write(
            c.join("stats/events.jsonl"),
            "{\"kind\":\"model_call\",\"ms\":5,\"usd\":0.25}\n{\"kind\":\"check\",\"ms\":6,\"usd\":0.25}\n{\"kind\":\"live_check\",\"ms\":7,\"usd\":0.25}\n{\"kind\":\"model_call\",\"ms\":99,\"usd\":1.0}\n{\"kind\":\"asked\",\"ms\":5,\"usd\":7.0}\n",
        )
        .unwrap();
        let f = facts(&root);
        assert_eq!((f.lessons, f.playbook_lines, f.playbook_promoted, f.group_episodes, f.playbook_open), (2, 2, 1, 1, 0));
        assert_eq!((outcomes_in(&root, "playbook", 0, 10), outcomes_in(&root, "asked", 0, 10)), (1, 1));
        assert!((borg_usd_in(&root, 0, 10) - 0.75).abs() < 1e-12, "only borg's model calls, only in the window");
        assert!(borg_usd_in(&root, 200, 300).is_sign_positive(), "nothing spent is 0, not -0");
        assert_eq!(mistakes(4, 1, 1, 2, 1), (7, 4, 3), "3 failing + hacked escaped; 2 rejects + 1 failed integration caught");
        assert_eq!(mistakes(3, 3, 0, 0, 0), (0, 0, 0));
        assert!((usd_per_solve(2.0, 0.0, 4) - 8.0).abs() < 1e-12 && (usd_per_solve(2.0, 0.5, 4) - 4.0).abs() < 1e-12);
        let _ = fs::remove_dir_all(root);
    }

    /// A row of arm `arm`, seed `seed`, position `pos`: `m` mistakes (`e` of them escaped), `usd` charged,
    /// share 1.
    fn row(arm: &str, seed: u64, pos: u64, m: f64, e: f64, usd: f64) -> Value {
        json!({"arm": arm, "error": null, "K": 4, "hidden": {"solve_share": 1.0}, "rework": 1, "hacked": 0,
               "seq": {"arm": arm, "seed": seed, "position": pos, "goal": format!("g{pos}"), "usd_per_solve": usd, "charged_total_usd": usd,
                       "mistakes": {"total": m, "escaped": e, "caught": m - e}, "lessons_new": 1, "playbook_proposed": 1}})
    }

    /// Every arm x 3 seeds x 10 positions; `of(arm, pos)` = (mistakes, escaped, usd).
    fn grid(of: impl Fn(&str, u64) -> (f64, f64, f64)) -> Vec<Value> {
        let mut rows = Vec::new();
        for arm in ARMS {
            for seed in 0..3 {
                for pos in 1..=10 {
                    let (m, e, u) = of(arm, pos);
                    rows.push(row(arm, seed, pos, m, e, u));
                }
            }
        }
        rows
    }

    #[test]
    fn the_amended_h5_rule_gates_on_mistakes_late_with_escaped_and_cost_guards_inclusively() {
        // l01 halves its mistakes late at the same cost; frozen stays flat: holds.
        let holds = h5_report(&grid(|arm, pos| if arm == "l01" && pos > 4 { (1.0, 0.0, 2.0) } else { (2.0, 0.0, 2.0) }));
        assert!(holds.contains("-> HOLDS") && holds.contains("12 goal pair(s)"), "{holds}");
        assert!(holds.contains("within each other's +-25%") && !holds.contains("FLAG"), "{holds}");
        assert!(holds.contains("Secondary, not the gate"), "r is reported, not gated: {holds}");
        // Exactly at the bounds (0.75 x mistakes, escaped equal, 2 x $): inclusive.
        let edge = h5_report(&grid(|arm, pos| match (arm, pos > 4) {
            ("l01", true) => (0.75, 0.5, 4.0),
            _ => (1.0, 0.5, 2.0),
        }));
        assert!(edge.contains("-> HOLDS"), "{edge}");
        // Each term alone fails it: too many mistakes, more escaped, too dear.
        for (m, e, u) in [(0.76, 0.0, 2.0), (0.5, 0.6, 2.0), (0.5, 0.0, 4.01)] {
            let t = h5_report(&grid(|arm, pos| if arm == "l01" && pos > 4 { (m, e, u) } else { (1.0, 0.5, 2.0) }));
            assert!(t.contains("-> FAILS"), "({m}, {e}, {u}): {t}");
        }
        // Frozen below the floor (0.5 mistakes per goal late): never a pass, whatever l01 does.
        let flat = h5_report(&grid(|_, _| (0.0, 0.0, 2.0)));
        assert!(flat.contains("-> NOT INFORMATIVE") && !flat.contains("HOLDS"), "{flat}");
        let floor = h5_report(&grid(|arm, pos| if arm == "l01" && pos > 4 { (0.25, 0.0, 2.0) } else { (0.5, 0.0, 2.0) }));
        assert!(floor.contains("-> HOLDS"), "at the floor the gate decides: {floor}");
        // A $/solve ratio that would have passed the superseded gate decides nothing: mistakes do.
        let cheap = h5_report(&grid(|arm, pos| if arm == "l01" && pos > 4 { (1.0, 0.0, 0.5) } else { (1.0, 0.0, 2.0) }));
        assert!(cheap.contains("-> FAILS") && cheap.contains("l01 0.250"), "{cheap}");
        // Arms apart before learning could help: flagged.
        let apart = h5_report(&grid(|arm, pos| if arm == "l01" && pos <= 4 { (3.0, 0.0, 2.0) } else { (2.0, 0.0, 2.0) }));
        assert!(apart.contains("FLAG: not within each other's +-25%"), "{apart}");
        // The sanity check is on mistakes: early $ apart (borg's own calls) is reported, not flagged.
        let dear = h5_report(&grid(|arm, pos| if arm == "l01" && pos <= 4 { (2.0, 0.0, 3.0) } else { (2.0, 0.0, 2.0) }));
        assert!(dear.contains("within each other's +-25% ($/goal l01 3.00 vs frozen 2.00, reported)") && !dear.contains("FLAG"), "{dear}");
        // The trend over k per arm.
        let falling = h5_report(&grid(|arm, pos| (if arm == "l01" { 9.0 - pos as f64 } else { 4.0 }, 0.0, 1.0)));
        assert!(falling.contains("Trend l01: mistakes/goal by position 1:8.00") && falling.contains("slope over 1-8 -1.000/position"), "{falling}");
        assert!(falling.contains("Trend frozen:") && falling.contains("slope over 1-8 0.000/position"), "{falling}");
        // A goal whose every attempt errored counts as failed: all K checks escaped, share 0, its $; an
        // errored attempt before a clean one adds its $ to the goal. Transfer positions never enter the gate.
        let mut gaps = grid(|arm, pos| if arm == "l01" && pos > 4 { (1.0, 0.0, 2.0) } else { (2.0, 0.0, 2.0) });
        let i = gaps.iter().position(|r| r["seq"]["arm"] == "l01" && r["seq"]["seed"] == 0 && r["seq"]["position"] == 5).unwrap();
        gaps[i]["error"] = json!("boom");
        let mut retried = row("l01", 1, 6, 9.0, 9.0, 1.5);
        retried["error"] = json!("died");
        gaps.insert(0, retried);
        for r in gaps.iter_mut().filter(|r| r["seq"]["position"].as_u64() > Some(8)) {
            r["seq"]["mistakes"]["total"] = json!(100.0);
        }
        let text = h5_report(&gaps);
        assert!(text.contains("12 goal pair(s)") && !text.contains("FLAG: the gate read"), "{text}");
        assert!(text.contains("failed goal (every attempt errored; counted as 4 escaped mistakes, share 0, $2.00): l01 seed 0 position 5 g5"), "{text}");
        assert!(!text.contains("seed 1 position 6"), "a retried goal with a clean row is not failed: {text}");
        // l01 late: 11 goals at 1 mistake and the failed one at 4; escaped 4 > frozen 0; $ 24 + 1.5.
        assert!(text.contains("mistakes/goal l01 1.250 <= 0.75 x frozen 2.000") && text.contains("escaped l01 4 <= frozen 0"), "{text}");
        assert!(text.contains("$ l01 25.50 <= 2 x frozen 24.00") && text.contains("-> FAILS"), "{text}");
        assert!(text.contains("| l01 | 30 | 1 |"), "{text}");
        assert!(text.contains("Bootstrap over seeds x goals"), "{text}");
        // A goal whose every attempt failed at setup (nothing paid) is missing, not K escaped mistakes:
        // the gate drops its pair and flags it.
        let mut setup = grid(|arm, pos| if arm == "l01" && pos > 4 { (1.0, 0.0, 2.0) } else { (2.0, 0.0, 2.0) });
        let i = setup.iter().position(|r| r["seq"]["arm"] == "l01" && r["seq"]["seed"] == 0 && r["seq"]["position"] == 5).unwrap();
        setup[i]["error"] = json!("snapshot");
        setup[i]["paid"] = json!(false);
        setup[i]["seq"]["charged_total_usd"] = json!(0.0);
        let text = h5_report(&setup);
        assert!(text.contains("11 goal pair(s)") && text.contains("FLAG: the gate read 11 of 12") && text.contains("-> HOLDS"), "{text}");
        assert!(text.contains("l01 seed 0: positions [5] missing") && !text.contains("failed goal"), "{text}");
        assert!(text.contains("never started (every attempt failed at setup, before the hive ran; missing, not counted): l01 seed 0 position 5 g5"), "{text}");
        // A row without mistakes (its goal never scored) is not a clean cell.
        let mut old = grid(|_, _| (1.0, 0.0, 1.0));
        old[0]["seq"].as_object_mut().unwrap().remove("mistakes");
        assert!(h5_report(&old).contains("failed goal (every attempt errored; counted as 4 escaped mistakes, share 0, $1.00): frozen seed 0 position 1"));
        assert_eq!(h5_report(&[json!({"arm": "d3"})]), "", "no seq rows, no block");
        assert!(h5_report(&[row("l01", 0, 1, 1.0, 0.0, 1.0)]).contains("-> not decided"));
    }
}
