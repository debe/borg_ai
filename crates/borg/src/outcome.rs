//! What happened after the conscience asked a lesson: the signal for down-weighting lessons
//! that are generic or do not help, and for trust per engineer × area (trust.rs).
//!
//! `$BORG_HOME/outcomes.jsonl` is append-only and deterministic, one record per line:
//! - `asked`: the conscience asked these lessons at a stop (hook, 0 tokens);
//! - `asked_then_corrected`: after lesson X was asked, the engineer still made a correction in the
//!   same session that `borg learn` merged into X (same concern): asking did not prevent it;
//! - `asked_not_corrected`: the session ended, or has been idle for `IDLE_MS`, and no later
//!   correction was merged into X. The idle rule keeps both kinds coming from every `borg learn`
//!   run: without it, a session whose `end` hook never ran could only ever add corrected counts.
//!
//! A correction that contradicts a lesson asked earlier in the session (`supersedes`) also marks that
//! lesson `asked_then_corrected`; when the conscience rendered asked lessons as a disagreement (the
//! `asked` record's `groups`), a correction siding with one of them marks the other sides, not the
//! one it agreed with (`contradicted`).
//!
//! - `asked_then_changed` (hook, no extra call): at a later Stop of the session, the consequence model's
//!   call (predict.rs) also judged, by meaning, that the drone changed its work on the lesson's point
//!   and that the change avoids the consequence: a use, the deposit that keeps a lesson active
//!   (memory.rs). The drone's `<id>: applies|skip` is evidence in that prompt, never a label alone;
//! - `asked_ignored` (hook): the same judge found the drone changed nothing on that point.
//! - `vetoed` (`borg no`, veto.rs, 0 tokens): the engineer said no to the session's work; every lesson
//!   shown in the session before it is credited, the strongest negative result.
//!
//! - `regretted` (`borg learn`, 0 tokens): a regret episode (a later fix of a decision, or a review finding;
//!   learn/regret.rs) generalised into a lesson blames the lessons asked in the deciding session before the
//!   decision (`regretted`): asking did not prevent it. `credit` carries each lesson's share of the blame;
//!   memory.rs weighs it below a correction. Deliberately not read by trust.rs (`sessions`), by `weight`
//!   (the legacy single-number view) or by attention.rs: a regret is about a lesson, not about an
//!   engineer following it.
//!
//! - `verified`, `rejected`, `hacked` (a hive verdict, hive/verdict.rs, 0 tokens): a drone's task in a
//!   hive goal was judged, by its verifier or, when the goal has one, by the hidden oracle
//!   (hive/hidden.rs); `hacked` is a done whose diff weakened tests or changed CI, or whose drone
//!   touched the oracle's files. Every lesson shown to the author's session while it held the claim is
//!   credited, once per `cause` (`<goal>/<verify task>`). memory.rs weighs them; like `regretted` they
//!   are deliberately not read by trust.rs (`sessions`), `weight` or attention.rs: a verdict is about
//!   the drone's task, not about an engineer following a lesson. Counted once per verdict as `task_*`
//!   (stats.rs), shared with `c` = the tag of its cause since the hive syncs (`SHARED_KINDS`).
//!
//! An unclear verdict, or a session whose later turn ends never reach the model (the prefilter skipped
//! them, or BORG_MATCH=bm25), leaves the lesson pending: `settle` turns it into `asked_not_corrected`.
//!
//! `asked` and `graded` records carry `p`, each lesson's probability of being asked under the policy in
//! force at that check (its propensity), for off-policy evaluation of another policy from this log. An
//! `asked` record from a Thompson slot draw (the bm25 and non-graded paths) also carries that draw's
//! pool (`pool`, `base`, `scoped`): every competitor with its score before the usefulness factor, so
//! `borg ope` (ope.rs) can recompute the propensity under other belief weights. Local only: the team
//! projection drops them.
//!
//! **Team outcomes.** The raw log stays on this machine (it holds session ids and repos). `publish`
//! appends a redacted, deterministic projection of the results the team may see (`SHARED_KINDS`) to
//! `outcomes-team.jsonl` (`TEAM_FILE`), which the mesh syncs (borg-mesh lines.rs `SYNCED`): kind,
//! lesson ids, time, propensity, channel, the hashed session and repo (`tag`), a regret's hashed cause
//! and credit, and the node, never raw sessions, repos, causes, groups, grades or text. Only the belief readers opt in to the team view
//! (`read_team`): memory and trust in the conscience (conscience.rs `load_as`), `borg trust`, `borg
//! memory` and borg-web's lessons. learn.rs (it would settle remote sessions), attention.rs (labels and
//! the explore counter are this node's policy), stats.rs (rollups sync their own counts), install.rs and
//! the hook's pending/judge read this machine's log only. BORG_SHARE_OUTCOMES=off publishes and reads
//! nothing of the team's.
//!
//! A session counts once per lesson. If a resumed session gets both results, corrected wins.
//! memory.rs derives activation and the usefulness belief from these results; `weight` below is the
//! earlier, single-number view (kept for reporting and for comparison in the R1 replay).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const ASKED: &str = "asked";
pub const CORRECTED: &str = "asked_then_corrected";
pub const NOT_CORRECTED: &str = "asked_not_corrected";
pub const CHANGED: &str = "asked_then_changed";
pub const IGNORED: &str = "asked_ignored";
/// One graded check: every lesson the model graded as applying (`lessons`, in its order, with
/// `grades` and `ranks`), and which of them the check chose to ask (`chosen`). The denominator for
/// learning how many to ask: a lesson that applied but was not chosen can still turn out needed.
pub const GRADED: &str = "graded";
/// A lesson a graded check of the session found applying but did not ask, which a later correction
/// in that session then merged into or superseded: a question that was needed and not asked (`missed`).
pub const MISSED: &str = "applied_then_corrected";
/// The engineer vetoed the session's work (`borg no`, veto.rs): every lesson shown in the session
/// before it is credited. The strongest negative result.
pub const VETOED: &str = "vetoed";
/// A lesson asked in a session before a decision that a later fix (or a review finding) showed was
/// wrong (learn.rs `credit_regrets`): asking did not prevent it. `cause` names the regret episode and
/// `credit` carries the share of the blame, (0, 1]. Weaker than a correction (memory.rs `REGRET`).
pub const REGRETTED: &str = "regretted";

/// A hive verdict on a task (hive/verdict.rs): passed, rejected by its verifier (or failed by the
/// hidden oracle), or hacked (protected files changed, or the oracle touched). Each credits the lessons
/// shown in the author's claim, once per (`cause`, lesson).
pub const VERIFIED: &str = "verified";
pub const REJECTED: &str = "rejected";
pub const HACKED: &str = "hacked";

/// The playbook versions shown to a session (playbook.rs; `lessons`: each `Playbook::key`). Local, never
/// shared, and no result: like `asked` it is what the session's later results credit, the veto
/// (veto.rs `shown_between`), the regret (`regretted`) and a hive verdict, so a playbook version gets the
/// usefulness belief a lesson gets (memory.rs `derive`, keyed by the version).
pub const PLAYBOOK: &str = "playbook";

/// Whether `kind` is a hive verdict's.
pub fn is_verdict(kind: &str) -> bool {
    matches!(kind, VERIFIED | REJECTED | HACKED)
}

/// Whether a record of `kind` shows its `lessons` to its session: an `asked` lesson or a shown playbook
/// version, what later results of the session credit.
pub fn shows(kind: &str) -> bool {
    matches!(kind, ASKED | PLAYBOOK)
}

/// A session without asks or episodes for this long is treated as ended (24h).
pub const IDLE_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub kind: String,
    pub session: String,
    pub lessons: Vec<String>,
    pub ms: i64,
    pub repo: String,
    /// `asked` only: the asked lessons rendered as one disagreement (conscience.rs `disagreements`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Vec<String>>,
    /// How the lessons were asked (`asked` and the judge's results): `stop` (a turn-end block) or `live`
    /// (a live check during the turn, live.rs). Empty in records from before delivery modes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    /// `graded` and `asked`: each lesson's grade (3 certain, 2 likely, 1 worth a thought) and its rank in
    /// the model's applies (0 first), aligned with `lessons`, and how many lessons the check graded as
    /// applying; on `asked`, 0 where no check of the session graded the lesson. For learning how many to
    /// ask (attention.rs reads them from `graded`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grades: Vec<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranks: Vec<u16>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub applied: u16,
    /// `graded` only: the lessons the check chose to ask.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
    /// `graded` only: the check explored, asking every apply up to `attention::RAIL` whatever the cut,
    /// so its outcomes label the cut (attention.rs).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub explore: bool,
    /// `regretted`: the regret episode (`R…`/`V…` id); a verdict kind: `<goal>/<verify task>`, the
    /// attempt judged. The idempotence key with the lesson.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cause: String,
    /// `regretted` only: the lesson's share of the regret, (0, 1].
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub credit: f64,
    /// `asked` and `graded`: per lesson (aligned with `lessons`), the probability that the policy in force
    /// at that check asked it: 1 or 0 under the fixed graded rule, the explore mixture under the learned
    /// cut (attention.rs `propensities`), the share of Thompson draws that give it a slot in
    /// `budget`/`widen` (conscience.rs `propensity`). Rounded to 4 decimals (`round_p`). Empty in records
    /// from before propensities and when unknown (BORG_ONLY, a state file from an older binary). For
    /// off-policy evaluation (hive.md "Credit assignment").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p: Vec<f64>,
    /// `asked` from a Thompson slot draw only (conscience.rs `budget_pool`/`widen_pool`, hook.rs
    /// `#borg-pool`): the pool the check drew from, in ranking order, the asked lessons included; each
    /// one's score without its usefulness factor (conscience.rs `pool_base`, unrounded so a replay is
    /// exact); and whether it is scoped. Aligned with each other, not with `lessons`. Empty for graded
    /// and live asks. Local: never shared (`share`). For `borg ope` (ope.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pool: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scoped: Vec<bool>,
}

/// A propensity rounded to 4 decimals, so lines stay deterministic and short.
pub fn round_p(p: f64) -> f64 {
    (p * 1e4).round() / 1e4
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

fn is_zero_f(v: &f64) -> bool {
    *v == 0.0
}

/// The outcomes store next to a lessons file.
pub fn path_beside(lessons: &Path) -> std::path::PathBuf {
    lessons.with_file_name("outcomes.jsonl")
}

/// The redacted outcomes the team sees, at the store root beside `outcomes.jsonl` (module doc).
pub const TEAM_FILE: &str = "outcomes-team.jsonl";
/// Beside `TEAM_FILE`, never synced: the length of `outcomes.jsonl` at the last `publish`.
pub const PUBLISHED: &str = "outcomes-team.published";

/// The kinds `publish` shares. A whitelist: `graded` and any later kind stay local until deliberately
/// added here.
///
/// Closed under memory.rs `derive`: every kind that changes a lesson's result for a session, or its
/// belief, travels, so a peer folds the same memory. `vetoed` outranks the session's `changed` (shared
/// alone, a lesson vetoed here would gain a use and spread there); `regretted` carries its `credit` and
/// its hashed `cause` (`Shared::c`), the key derive dedupes regrets by. `graded` is the asking policy's
/// denominator, not a result, and never moves the activity clock (`memory::Clock` counts shared kinds).
///
/// Unguarded: any member can write a line under any session tag (tags are public in this file), so a
/// forged `asked_then_corrected` or `vetoed` overrides a teammate's session on every node but that
/// teammate's, and a few forged `asked_then_changed` with fresh tags and two repo tags consolidate a
/// lesson. `node` is self-declared. The mesh admits only members' signed lines (borg-mesh origin.rs),
/// so this is the trusted-member model; the deferred guards (a session tag bound to its first signer,
/// consolidation from at least two signers) would close it.
///
/// The hive verdicts (`verified`, `rejected`, `hacked`) are shared since the hive lines sync (hive.md
/// C4), with `c` = the tag of their cause (`<goal>/<verify task>`), so memory derives one verdict per
/// (lesson, attempt) from a local record and a peer's copy alike. A verdict is a self-reported reward,
/// so `team_view` binds a peer's verdict line to the signed board: it counts only for an attempt whose
/// board verdict took effect on this node (hive/verdict.rs `judged`), and only of a kind that verdict
/// allows (`hacked` for a hacked one; else the kind it credits, or either of verified and rejected for a
/// goal with a hidden oracle, whose score decides, then only from the node the work was done on), only
/// under the attempt author's session tag, and only for lessons the team's records show that session
/// was given. A forged verdict then needs a forged, signed board verdict too, which is auditable and
/// quarantined on revocation. What remains is the trusted-member
/// model: a member can still forge both, counted once per (lesson, attempt), but attempts are mintable;
/// the k-distinct-nodes rule and a trust-weighted median stay deferred. Hidden-oracle results stay
/// private: a goal with one credits on its runner's node only, so a remote author of such a goal's
/// attempt is not credited yet (deferred). They never move the activity clock (`memory::Clock`).
pub const SHARED_KINDS: [&str; 11] = [ASKED, CHANGED, IGNORED, CORRECTED, NOT_CORRECTED, MISSED, VETOED, REGRETTED, VERIFIED, REJECTED, HACKED];

/// A session or repo as the team sees it: the first 32 hex digits of blake3("borg-outcome-v1\0" + s).
/// Unsalted on purpose: the same session hashes to the same tag on every node, so a node can drop its
/// own published copies (`read_team`).
pub fn tag(s: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-outcome-v1\0");
    h.update(s.as_bytes());
    h.finalize().to_hex()[..32].to_string()
}

/// `tag`, keeping an empty repo empty.
fn tag_repo(repo: &str) -> String {
    if repo.is_empty() { String::new() } else { tag(repo) }
}

/// One line of `TEAM_FILE`. Never carries `id` and `version` together (the mesh would settle it as a
/// versioned record).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Shared {
    pub kind: String,
    /// `tag` of the session.
    pub s: String,
    pub lessons: Vec<String>,
    pub ms: i64,
    /// The publishing node (stats.rs `node`), self-declared: dedup does not depend on it.
    pub node: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p: Vec<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    /// `tag` of the repo: memory.rs counts distinct repos to consolidate a lesson.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r: String,
    /// `tag` of a `regretted` record's cause (the regret episode) or a verdict's (`<goal>/<verify task>`):
    /// memory.rs counts each regret, and each verdict per attempt, once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub c: String,
    /// A `regretted` record's share of the blame.
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub credit: f64,
}

/// `o` as the team sees it, or None for a kind not in `SHARED_KINDS`.
pub fn share(o: &Outcome, node: &str) -> Option<Shared> {
    SHARED_KINDS.contains(&o.kind.as_str()).then(|| Shared {
        kind: o.kind.clone(),
        s: tag(&o.session),
        lessons: o.lessons.clone(),
        ms: o.ms,
        node: node.into(),
        p: o.p.clone(),
        channel: o.channel.clone(),
        r: tag_repo(&o.repo),
        c: tag_repo(&o.cause),
        credit: o.credit,
    })
}

/// Team outcomes on: `BORG_SHARE_OUTCOMES` is not `off`.
pub fn on() -> bool {
    std::env::var("BORG_SHARE_OUTCOMES").as_deref() != Ok("off")
}

/// Appends to `TEAM_FILE` in `home` the shared lines of `home`'s `outcomes.jsonl` it lacks; returns
/// how many. The lines are deterministic, so a re-run appends nothing; two runs at once may append the
/// same line twice, which the mesh's union and the per-session count make harmless. Nothing when off.
/// A local log no longer than at the last publish (`PUBLISHED`, its length then) is not read again: the
/// log only grows, so nothing new can be in it.
pub fn publish(home: &Path) -> usize {
    let local = home.join("outcomes.jsonl");
    let team = home.join(TEAM_FILE);
    let mark = home.join(PUBLISHED);
    let len = std::fs::metadata(&local).map_or(0, |m| m.len());
    if !on() || len == 0 {
        return 0;
    }
    let published = std::fs::read_to_string(&mark).ok().and_then(|t| t.trim().parse::<u64>().ok());
    if team.exists() && published == Some(len) {
        return 0;
    }
    let mine = std::fs::read_to_string(&local).unwrap_or_default();
    let node = crate::stats::node(home);
    let text = std::fs::read_to_string(&team).unwrap_or_default();
    let mut have: std::collections::HashSet<String> = text.lines().map(str::to_string).collect();
    let mut out = String::new();
    let records = mine.lines().filter_map(|l| serde_json::from_str::<Outcome>(l).ok());
    for line in records.filter_map(|o| share(&o, &node)).filter_map(|x| serde_json::to_string(&x).ok()) {
        if have.insert(line.clone()) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        let _ = std::fs::write(&mark, mine.len().to_string());
        return 0;
    }
    use std::io::Write;
    let open = text.as_bytes().last().is_some_and(|c| *c != b'\n');
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&team)
        .and_then(|mut f| f.write_all(format!("{}{out}", if open { "\n" } else { "" }).as_bytes()));
    if written.is_err() {
        return 0;
    }
    let _ = std::fs::write(&mark, mine.len().to_string());
    out.lines().count()
}

/// The belief view of the outcomes beside `lessons`: this machine's log plus, unless
/// BORG_SHARE_OUTCOMES=off, the team's (`team_view`).
pub fn read_team(lessons: &Path) -> Vec<Outcome> {
    team_view(lessons, on())
}

/// Off: this machine's log as is. On: its records with each repo and cause as its `tag` (memory.rs only
/// compares them for distinctness, so beliefs are the same and comparable with the team's), then the
/// team's lines that count (`team_parts`). A remote session then counts once per lesson like a local
/// one (memory.rs `derive`, `sessions`). learn.rs and verdict.rs key on raw causes: they read this
/// machine's log (`read`).
pub fn team_view(lessons: &Path, on: bool) -> Vec<Outcome> {
    let (mut out, team) = team_parts(lessons, on);
    if on {
        for o in &mut out {
            o.repo = tag_repo(&o.repo);
            o.cause = tag_repo(&o.cause);
        }
    }
    out.extend(team);
    out
}

/// This machine's records as written, and (on) every `TEAM_FILE` line of a shared kind whose session
/// is not one of this machine's (that drops this node's own published copies, whatever its node id),
/// once per (kind, session, lessons, ms, cause): one read of each file, so the two never disagree on
/// where the local records end (ope.rs reads both). A kind this binary does not share (a newer node's)
/// is ignored, and so is a verdict the board does not back (`SHARED_KINDS`; hive/verdict.rs `judged`,
/// read only when the team file holds a verdict): its attempt was judged that way here, the line's
/// session is the attempt's author, each lesson it names is one the team's records show that session
/// was given, and with a hidden oracle the line comes from the node the work was done on.
pub fn team_parts(lessons: &Path, on: bool) -> (Vec<Outcome>, Vec<Outcome>) {
    let local = read(&path_beside(lessons));
    if !on {
        return (local, Vec::new());
    }
    let mine: std::collections::HashSet<String> = local.iter().map(|o| tag(&o.session)).collect();
    let text = std::fs::read_to_string(lessons.with_file_name(TEAM_FILE)).unwrap_or_default();
    let lines: Vec<Shared> = text.lines().filter_map(|l| serde_json::from_str::<Shared>(l).ok()).collect();
    // The lessons each team session's records name: a verdict credits only lessons its author was given.
    let mut given: std::collections::HashMap<&str, std::collections::HashSet<&str>> = std::collections::HashMap::new();
    for x in lines.iter().filter(|x| !is_verdict(&x.kind)) {
        given.entry(x.s.as_str()).or_default().extend(x.lessons.iter().map(String::as_str));
    }
    let mut seen: std::collections::HashSet<(String, String, Vec<String>, i64, String)> = std::collections::HashSet::new();
    let judged = std::cell::OnceCell::new();
    let home = lessons.parent().unwrap_or(Path::new("."));
    let backed = |x: &Shared| {
        judged.get_or_init(|| crate::hive::verdict::judged(home)).get(&x.c).is_some_and(|a: &crate::hive::verdict::Allowed| {
            a.allows(&x.kind)
                && x.s == a.author
                && (!a.held_out || x.node == a.node)
                && x.lessons.iter().all(|l| given.get(x.s.as_str()).is_some_and(|g| g.contains(l.as_str())))
        })
    };
    let mut team = Vec::new();
    for x in lines.iter() {
        if !SHARED_KINDS.contains(&x.kind.as_str()) || mine.contains(&x.s) || !seen.insert((x.kind.clone(), x.s.clone(), x.lessons.clone(), x.ms, x.c.clone()))
        {
            continue;
        }
        if is_verdict(&x.kind) && !backed(x) {
            continue;
        }
        team.push(Outcome {
            kind: x.kind.clone(),
            session: x.s.clone(),
            lessons: x.lessons.clone(),
            ms: x.ms,
            repo: x.r.clone(),
            p: x.p.clone(),
            channel: x.channel.clone(),
            cause: x.c.clone(),
            credit: x.credit,
            ..Default::default()
        });
    }
    (local, team)
}

pub fn read(path: &Path) -> Vec<Outcome> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Appends `o` in one write (learn.rs `append_line`: concurrent writers never tear a record).
pub fn append(path: &Path, o: &Outcome) -> std::io::Result<()> {
    crate::learn::append_jsonl(path, o)?;
    // The results count in the usage statistics; `asked` is already counted as the Stop's block, a
    // `vetoed` result as the `veto` that credited it (veto.rs), and a hive verdict's as the one
    // `task_*` event of its verdict (hive/verdict.rs).
    let kind = match o.kind.as_str() {
        CHANGED => crate::stats::CHANGED,
        IGNORED => crate::stats::IGNORED,
        CORRECTED => crate::stats::CORRECTED,
        NOT_CORRECTED => crate::stats::NOT_CORRECTED,
        REGRETTED => crate::stats::REGRETTED,
        _ => return Ok(()),
    };
    if let Some(home) = path.parent() {
        crate::stats::count(home, kind, &o.session, |e| {
            e.repo = o.repo.clone();
            e.lessons = o.lessons.clone();
            e.channel = o.channel.clone();
        });
    }
    Ok(())
}

/// Did the conscience ask `lesson` in `session` before `ms`?
pub fn asked_before(outcomes: &[Outcome], session: &str, lesson: &str, ms: i64) -> bool {
    outcomes.iter().any(|o| o.kind == ASKED && o.session == session && o.ms < ms && o.lessons.iter().any(|l| l == lesson))
}

/// Is a result (`kind`) already recorded for `lesson` in `session`? Keeps `borg learn` idempotent.
pub fn has(outcomes: &[Outcome], kind: &str, session: &str, lesson: &str) -> bool {
    outcomes.iter().any(|o| o.kind == kind && o.session == session && o.lessons.iter().any(|l| l == lesson))
}

/// The lessons a correction at `ms` in `session` shows were asked in vain, each asked in `session`
/// before `ms` and not yet recorded as corrected or vetoed there (a veto is the stronger result): the lessons the correction `supersedes`
/// (it contradicts them), and `target`, the lesson it was merged into (the same concern came up
/// again). When `target` was asked as one side of a rendered disagreement, the engineer sided with
/// it: the other sides of that disagreement were corrected, not `target`. Sorted, unique.
pub fn contradicted(outcomes: &[Outcome], session: &str, ms: i64, target: Option<&str>, supersedes: &[String]) -> Vec<String> {
    let mut out: Vec<String> = supersedes.to_vec();
    if let Some(t) = target {
        let sides: Vec<String> = outcomes
            .iter()
            .filter(|o| o.kind == ASKED && o.session == session && o.ms < ms)
            .flat_map(|o| &o.groups)
            .filter(|g| g.iter().any(|x| x == t))
            .flatten()
            .filter(|x| *x != t)
            .cloned()
            .collect();
        match sides.is_empty() {
            true => out.push(t.to_string()),
            false => out.extend(sides),
        }
    }
    out.retain(|l| asked_before(outcomes, session, l, ms) && !has(outcomes, CORRECTED, session, l) && !has(outcomes, VETOED, session, l));
    out.sort();
    out.dedup();
    out
}

/// The lessons a correction at `ms` in `session` shows were needed and not asked: of the lessons it
/// `supersedes` and `target` (the lesson it was merged into), those a graded check of the session found
/// applying before `ms` without choosing them, never asked in the session before `ms`, and not yet
/// recorded as missed there. Sorted, unique.
pub fn missed(outcomes: &[Outcome], session: &str, ms: i64, target: Option<&str>, supersedes: &[String]) -> Vec<String> {
    let mut out: Vec<String> = supersedes.iter().cloned().chain(target.map(str::to_string)).collect();
    let applied_unchosen = |l: &str| {
        outcomes.iter().any(|o| o.kind == GRADED && o.session == session && o.ms < ms && o.lessons.iter().any(|x| x == l) && !o.chosen.iter().any(|x| x == l))
    };
    out.retain(|l| applied_unchosen(l) && !asked_before(outcomes, session, l, ms) && !has(outcomes, MISSED, session, l));
    out.sort();
    out.dedup();
    out
}

/// Has `lesson` been credited for the regret `cause` already (in any session)?
fn credited(outcomes: &[Outcome], cause: &str, lesson: &str) -> bool {
    outcomes.iter().any(|o| o.kind == REGRETTED && o.cause == cause && o.lessons.iter().any(|l| l == lesson))
}

/// The lessons a regret blames: those asked in `session` (the deciding one) before `before_ms`, without
/// a correction or a veto there already (both outrank regretted, as in memory.rs) and not yet credited
/// for `cause`. When some of `target` (the lesson the regret was folded into, and the lessons that lesson
/// supersedes) were asked, they share the blame alone: the same concern came up again, and when all of
/// them already took a correction or a veto for it, nobody else is blamed. Otherwise every asked lesson
/// shares it equally (the Shapley value of a symmetric game). A lesson a graded check found applying but
/// did not ask was never shown, so it is not blamed. One record per lesson, sorted, at `ms`,
/// with `credit = strength / n`; n counts the blamed lessons before the already-credited ones are left
/// out, so a re-run writes nothing and the shares never change. Every record's session has an earlier
/// `asked` record, so it never moves memory.rs's `Clock`.
///
/// The playbook versions shown in the session before the decision (`PLAYBOOK` records) share the same
/// regret among themselves, the same way, apart from the lessons: a shown playbook never dilutes a
/// lesson's share, nor a lesson a playbook's.
#[allow(clippy::too_many_arguments)]
pub fn regretted(outcomes: &[Outcome], session: &str, before_ms: i64, cause: &str, target: &[String], strength: f64, ms: i64, repo: &str) -> Vec<Outcome> {
    if session.is_empty() || before_ms <= 0 || strength <= 0.0 {
        return Vec::new();
    }
    let mut out: Vec<Outcome> =
        [ASKED, PLAYBOOK].iter().flat_map(|kind| blamed(outcomes, kind, session, before_ms, cause, target, strength, ms, repo)).collect();
    out.sort_by(|a, b| a.lessons.cmp(&b.lessons));
    out
}

/// One pool of `regretted`: the ids the records of `kind` showed in `session` before `before_ms`.
#[allow(clippy::too_many_arguments)]
fn blamed(outcomes: &[Outcome], kind: &str, session: &str, before_ms: i64, cause: &str, target: &[String], strength: f64, ms: i64, repo: &str) -> Vec<Outcome> {
    let mut asked: Vec<&str> =
        outcomes.iter().filter(|o| o.kind == kind && o.session == session && o.ms < before_ms).flat_map(|o| o.lessons.iter().map(String::as_str)).collect();
    asked.sort_unstable();
    asked.dedup();
    if asked.iter().any(|l| target.iter().any(|t| t == l)) {
        asked.retain(|l| target.iter().any(|t| t == l));
    }
    asked.retain(|l| !has(outcomes, CORRECTED, session, l) && !has(outcomes, VETOED, session, l));
    let credit = (strength / asked.len().max(1) as f64).min(1.0);
    asked
        .into_iter()
        .filter(|l| !credited(outcomes, cause, l))
        .map(|l| Outcome {
            kind: REGRETTED.into(),
            session: session.into(),
            lessons: vec![l.into()],
            ms,
            repo: repo.into(),
            cause: cause.into(),
            credit,
            ..Default::default()
        })
        .collect()
}

/// The verdict records a hive verdict (`kind`, one of `is_verdict`) on the attempt `cause` writes for
/// `lessons` (shown in `author`'s claim): one per lesson, sorted, skipping a lesson that already has a
/// verdict record for `cause` (whatever its kind), so a re-run or a later score writes nothing.
pub fn verdicts(outcomes: &[Outcome], kind: &str, author: &str, lessons: &[String], cause: &str, ms: i64, repo: &str) -> Vec<Outcome> {
    if !is_verdict(kind) || author.is_empty() || cause.is_empty() {
        return Vec::new();
    }
    let done = |l: &str| outcomes.iter().any(|o| is_verdict(&o.kind) && o.cause == cause && o.lessons.iter().any(|x| x == l));
    let mut ids: Vec<&str> = lessons.iter().map(String::as_str).filter(|l| !l.is_empty()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .filter(|l| !done(l))
        .map(|l| Outcome {
            kind: kind.into(),
            session: author.into(),
            lessons: vec![l.into()],
            ms,
            repo: repo.into(),
            cause: cause.into(),
            ..Default::default()
        })
        .collect()
}

/// At session end: every lesson asked in `session` that has no result yet was not corrected. A
/// vetoed lesson already has its result.
/// Sorted by lesson id so the appended lines are deterministic.
/// One pass over the log, whatever the number of asks.
pub fn settle(outcomes: &[Outcome], session: &str, ms: i64) -> Vec<Outcome> {
    let mine = outcomes.iter().filter(|o| o.session == session);
    let resolved: std::collections::HashSet<&str> = mine.clone().filter(|o| resolves(&o.kind)).flat_map(|o| o.lessons.iter().map(String::as_str)).collect();
    let mut asked: Vec<(&str, &str)> = mine
        .filter(|o| o.kind == ASKED)
        .flat_map(|o| o.lessons.iter().map(move |l| (l.as_str(), o.repo.as_str())))
        .filter(|(l, _)| !resolved.contains(l))
        .collect();
    asked.sort();
    asked.dedup_by_key(|(l, _)| *l);
    asked
        .into_iter()
        .map(|(l, repo)| Outcome { kind: NOT_CORRECTED.into(), session: session.into(), lessons: vec![l.into()], ms, repo: repo.into(), ..Default::default() })
        .collect()
}

/// Does a `kind` record end an ask's wait for its result (`settle`)?
fn resolves(kind: &str) -> bool {
    matches!(kind, CORRECTED | NOT_CORRECTED | VETOED)
}

/// Sessions with asks that are idle at `now`: their newest ask and newest episode
/// (`last_episode`, newest episode ms per session) are at least `IDLE_MS` old. Sorted. Only sessions
/// with an ask still without its result, so `borg learn` (under the store lock) settles what is new,
/// not every session it ever settled: one pass over the log.
pub fn idle_sessions<'a>(outcomes: &'a [Outcome], last_episode: &HashMap<String, i64>, now: i64) -> Vec<&'a str> {
    let mut newest: HashMap<&str, i64> = HashMap::new();
    let (mut asked, mut resolved) = (std::collections::HashSet::new(), std::collections::HashSet::new());
    for o in outcomes {
        let into = match o.kind.as_str() {
            ASKED => &mut asked,
            k if resolves(k) => &mut resolved,
            _ => continue,
        };
        into.extend(o.lessons.iter().map(|l| (o.session.as_str(), l.as_str())));
        if o.kind == ASKED {
            let m = newest.entry(o.session.as_str()).or_insert(o.ms);
            *m = (*m).max(o.ms);
        }
    }
    let open: std::collections::HashSet<&str> = asked.difference(&resolved).map(|(s, _)| *s).collect();
    let mut idle: Vec<&str> = newest
        .into_iter()
        .filter(|&(s, ms)| open.contains(s) && ms.max(last_episode.get(s).copied().unwrap_or(i64::MIN)) <= now - IDLE_MS)
        .map(|(s, _)| s)
        .collect();
    idle.sort();
    idle
}

/// Usefulness of a lesson from its asked outcomes:
///
/// w = (1 + not_corrected) / (1 + not_corrected + 2 * corrected)
///
/// The +1 is a neutral prior, so a lesson without outcomes keeps w = 1 and a single bad session
/// cannot zero it. A correction of the same concern right after asking is strong evidence the
/// question did not help (too generic, or it does not change what the drone does), while a
/// session without correction is weak evidence (the engineer may simply not have looked), so a
/// correction counts twice. w stays in (0, 1]: outcomes only demote, BM25 relevance still decides
/// what is on topic. One corrected session gives 1/3; one corrected and one not gives 1/2.
pub fn weight(not_corrected: u32, corrected: u32) -> f64 {
    let (n, c) = (not_corrected as f64, corrected as f64);
    (1.0 + n) / (1.0 + n + 2.0 * c)
}

/// The weight of every lesson that has outcomes. Each (session, lesson) counts once; corrected wins.
pub fn weights(outcomes: &[Outcome]) -> HashMap<String, f64> {
    counts(outcomes).into_iter().map(|(l, (n, c))| (l, weight(n, c))).collect()
}

/// Per lesson with results: (sessions not corrected, sessions corrected). Each (session, lesson)
/// counts once; corrected wins. Used by `weights`.
pub fn counts(outcomes: &[Outcome]) -> HashMap<String, (u32, u32)> {
    sessions(outcomes).into_iter().map(|(l, ss)| (l.to_string(), tally(ss.values(), i64::MIN))).collect()
}

/// Per lesson, per session with a result: (corrected, ms of the session's earliest result for the
/// lesson). Corrected wins over a later (resumed) settle. trust.rs filters these by when an engineer
/// joined the lesson. A veto (`vetoed`) counts as a correction here, so trust and `weight` see it too
/// (as heavy as a correction; only memory.rs weighs it above one).
pub fn sessions(outcomes: &[Outcome]) -> HashMap<&str, HashMap<&str, (bool, i64)>> {
    let mut out: HashMap<&str, HashMap<&str, (bool, i64)>> = HashMap::new();
    for o in outcomes {
        let corrected = match o.kind.as_str() {
            CORRECTED | VETOED => true,
            NOT_CORRECTED => false,
            _ => continue,
        };
        for l in &o.lessons {
            let r = out.entry(l.as_str()).or_default().entry(o.session.as_str()).or_insert((corrected, o.ms));
            r.0 |= corrected;
            r.1 = r.1.min(o.ms);
        }
    }
    out
}

/// (sessions not corrected, sessions corrected) among `sessions` results whose earliest result is
/// after `since`.
pub fn tally<'a>(sessions: impl IntoIterator<Item = &'a (bool, i64)>, since: i64) -> (u32, u32) {
    sessions.into_iter().filter(|(_, ms)| *ms > since).fold((0, 0), |(n, c), (corrected, _)| match corrected {
        true => (n, c + 1),
        false => (n + 1, c),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(kind: &str, session: &str, lessons: &[&str], ms: i64) -> Outcome {
        Outcome {
            kind: kind.into(),
            session: session.into(),
            lessons: lessons.iter().map(|l| l.to_string()).collect(),
            ms,
            repo: "r".into(),
            ..Default::default()
        }
    }

    #[test]
    fn missed_is_an_applied_unasked_lesson_a_later_correction_lands_on() {
        let graded = |ms: i64, chosen: &[&str]| Outcome {
            kind: GRADED.into(),
            session: "s".into(),
            lessons: vec!["A".into(), "B".into()],
            ms,
            chosen: chosen.iter().map(|x| x.to_string()).collect(),
            ..Default::default()
        };
        let none: Vec<String> = Vec::new();
        let log = vec![graded(10, &["B"])];
        assert_eq!(missed(&log, "s", 20, Some("A"), &none), ["A"]);
        assert_eq!(missed(&log, "s", 20, None, &["A".into(), "B".into()]), ["A"], "B was chosen");
        assert!(missed(&log, "s", 5, Some("A"), &none).is_empty(), "a correction before the check");
        assert!(missed(&log, "t", 20, Some("A"), &none).is_empty(), "another session");
        assert!(missed(&log, "s", 20, Some("C"), &none).is_empty(), "never applied");
        let mut asked = log.clone();
        asked.push(o(ASKED, "s", &["A"], 15));
        assert!(missed(&asked, "s", 20, Some("A"), &none).is_empty(), "asked later in the session: not a miss");
        let mut done = log.clone();
        done.push(o(MISSED, "s", &["A"], 18));
        assert!(missed(&done, "s", 20, Some("A"), &none).is_empty(), "recorded once");
    }

    #[test]
    fn weight_formula() {
        assert_eq!(weight(0, 0), 1.0);
        assert_eq!(weight(5, 0), 1.0);
        assert!((weight(0, 1) - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(weight(1, 1), 0.5);
        assert!(weight(0, 3) < weight(0, 1));
    }

    #[test]
    fn weights_count_each_session_once_and_corrected_wins() {
        let all = vec![
            o(ASKED, "s1", &["A", "B"], 10),
            o(CORRECTED, "s1", &["A"], 20),
            o(CORRECTED, "s1", &["A"], 30),     // second correction in the same session: still one
            o(NOT_CORRECTED, "s1", &["A"], 40), // resumed session settled later: corrected wins
            o(NOT_CORRECTED, "s1", &["B"], 40),
            o(NOT_CORRECTED, "s2", &["A"], 50),
        ];
        let w = weights(&all);
        assert_eq!(w["A"], weight(1, 1));
        assert_eq!(w["B"], 1.0);
        assert!(!w.contains_key("C"));
    }

    #[test]
    fn settle_marks_only_unresolved_asks_of_this_session() {
        let all = vec![o(ASKED, "s1", &["B", "A"], 10), o(ASKED, "s1", &["A", "C"], 15), o(CORRECTED, "s1", &["A"], 20), o(ASKED, "s2", &["D"], 10)];
        let settled = settle(&all, "s1", 99);
        assert_eq!(settled, vec![o(NOT_CORRECTED, "s1", &["B"], 99), o(NOT_CORRECTED, "s1", &["C"], 99)]);
        assert!(asked_before(&all, "s1", "A", 11) && !asked_before(&all, "s1", "C", 15) && !asked_before(&all, "s2", "A", 99));
    }

    #[test]
    fn contradicted_marks_superseded_asks_and_the_other_sides_of_a_disagreement() {
        let mut ask = o(ASKED, "s1", &["A", "B", "C", "D"], 10);
        ask.groups = vec![vec!["A".into(), "B".into()]];
        let all = vec![ask, o(ASKED, "s1", &["E"], 30), o(CORRECTED, "s1", &["D"], 15)];
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // A plain merge into C: C was asked in vain.
        assert_eq!(contradicted(&all, "s1", 20, Some("C"), &[]), ["C"]);
        // A correction that sides with A in the rendered A/B disagreement: B was wrong, A was right.
        assert_eq!(contradicted(&all, "s1", 20, Some("A"), &[]), ["B"]);
        // A new lesson superseding C and E: only C was asked before it; D is already corrected.
        assert_eq!(contradicted(&all, "s1", 20, None, &ids(&["C", "E", "D", "Z"])), ["C"]);
        assert!(contradicted(&all, "s2", 20, Some("C"), &ids(&["A"])).is_empty(), "another session asked nothing");
        let mut vetoed = all.clone();
        vetoed.push(o(VETOED, "s1", &["C"], 12));
        assert!(contradicted(&vetoed, "s1", 20, Some("C"), &[]).is_empty(), "a vetoed lesson already has the stronger result");
    }

    #[test]
    fn a_vetoed_lesson_is_not_settled_and_counts_as_corrected() {
        let all = vec![o(ASKED, "s1", &["A", "B"], 10), o(VETOED, "s1", &["A"], 20)];
        assert_eq!(settle(&all, "s1", 99), vec![o(NOT_CORRECTED, "s1", &["B"], 99)]);
        assert_eq!(sessions(&all)["A"]["s1"], (true, 20));
        assert_eq!(weights(&all)["A"], weight(0, 1));
    }

    #[test]
    fn sessions_keep_the_earliest_result_and_tally_filters_by_it() {
        let all =
            vec![o(NOT_CORRECTED, "s1", &["A"], 10), o(CORRECTED, "s2", &["A"], 20), o(NOT_CORRECTED, "s2", &["A"], 25), o(NOT_CORRECTED, "s3", &["A"], 30)];
        let s = sessions(&all);
        assert_eq!(s["A"]["s2"], (true, 20));
        assert_eq!(tally(s["A"].values(), i64::MIN), (2, 1));
        assert_eq!(tally(s["A"].values(), 20), (1, 0));
    }

    #[test]
    fn idle_sessions_need_old_asks_and_old_episodes() {
        let all = vec![o(ASKED, "old", &["A"], 0), o(ASKED, "busy", &["A"], 0), o(ASKED, "fresh", &["A"], 50), o(CORRECTED, "x", &["A"], 0)];
        let episodes = HashMap::from([("busy".to_string(), 60)]);
        assert_eq!(idle_sessions(&all, &episodes, IDLE_MS + 10), ["old"]);
        assert_eq!(idle_sessions(&all, &episodes, IDLE_MS + 60), ["busy", "fresh", "old"]);
    }

    #[test]
    fn idle_sessions_skip_settled_ones() {
        let all = vec![
            o(ASKED, "done", &["A", "B"], 0),
            o(NOT_CORRECTED, "done", &["A"], 1),
            o(CORRECTED, "done", &["B"], 1),
            o(ASKED, "vetoed", &["A"], 0),
            o(VETOED, "vetoed", &["A"], 1),
            o(ASKED, "half", &["A", "B"], 0),
            o(NOT_CORRECTED, "half", &["A"], 1),
        ];
        assert_eq!(idle_sessions(&all, &HashMap::new(), IDLE_MS + 10), ["half"], "every ask of the others has its result");
    }

    #[test]
    fn regretted_blames_asks_before_the_decision_shared_or_by_target() {
        let mut graded = o(GRADED, "s", &["G"], 5);
        graded.chosen = vec![];
        let log = vec![graded, o(ASKED, "s", &["B", "A"], 10), o(ASKED, "s", &["C"], 30), o(ASKED, "t", &["D"], 10)];
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let blame = |log: &[Outcome], target: &[String]| regretted(log, "s", 20, "R1", target, 0.5, 99, "r");
        let got = blame(&log, &[]);
        let want = |l: &str, credit: f64| Outcome { cause: "R1".into(), credit, ..o(REGRETTED, "s", &[l], 99) };
        assert_eq!(got, vec![want("A", 0.25), want("B", 0.25)], "shared; C was asked after the decision, G never shown");
        assert_eq!(blame(&log, &ids(&["B", "Z"])), vec![want("B", 0.5)], "the merge target takes it all");
        assert_eq!(blame(&log, &ids(&["Z"])), got, "a target not asked: shared");
        let mut corrected = log.clone();
        corrected.push(o(CORRECTED, "s", &["B"], 15));
        assert_eq!(blame(&corrected, &[]), vec![want("A", 0.5)], "corrected outranks regretted");
        let mut vetoed = log.clone();
        vetoed.push(o(VETOED, "s", &["B"], 15));
        assert_eq!(blame(&vetoed, &[]), vec![want("A", 0.5)], "vetoed outranks regretted");
        assert!(blame(&corrected, &ids(&["B"])).is_empty(), "the target took it as a correction: nobody else is blamed");
        assert!(blame(&vetoed, &ids(&["B", "Z"])).is_empty(), "the target took it as a veto: nobody else is blamed");
        assert_eq!(blame(&corrected, &ids(&["A", "B"])), vec![want("A", 0.5)], "the uncorrected target keeps its share");
        let mut again = log.clone();
        again.extend(got.clone());
        assert!(blame(&again, &[]).is_empty(), "credited once per cause");
        let mut half = log.clone();
        half.push(got[0].clone());
        assert_eq!(blame(&half, &[]), vec![want("B", 0.25)], "a partial run completes with the same share");
        assert_eq!(regretted(&again, "s", 20, "R2", &[], 0.5, 99, "r").len(), 2, "another regret blames again");
        assert!(regretted(&log, "u", 20, "R1", &[], 0.5, 99, "r").is_empty(), "another session asked nothing");
        assert!(regretted(&log, "", 20, "R1", &[], 0.5, 99, "r").is_empty());
        assert!(regretted(&log, "s", 0, "R1", &[], 0.5, 99, "r").is_empty(), "no decision time");
        assert!(regretted(&log, "s", 20, "R1", &[], 0.0, 99, "r").is_empty(), "not strong");
        // A playbook version shown before the decision shares the regret apart: the lessons keep theirs.
        let mut shown = log.clone();
        shown.push(o(PLAYBOOK, "s", &["P1@1.aaaaaa"], 12));
        assert_eq!(blame(&shown, &[]), vec![want("A", 0.25), want("B", 0.25), want("P1@1.aaaaaa", 0.5)]);
        // An old line without cause and credit still reads; a regretted line round-trips.
        let old: Outcome = serde_json::from_str(r#"{"kind":"asked","session":"s","lessons":["A"],"ms":1,"repo":"r"}"#).unwrap();
        assert_eq!(old, o(ASKED, "s", &["A"], 1));
        assert!(!serde_json::to_string(&old).unwrap().contains("credit"));
        let line = serde_json::to_string(&got[0]).unwrap();
        assert!(line.contains(r#""cause":"R1","credit":0.25"#), "{line}");
        assert_eq!(serde_json::from_str::<Outcome>(&line).unwrap(), got[0]);
    }

    #[test]
    fn verdicts_credit_each_lesson_once_per_attempt_and_share_their_cause_tag() {
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let first = verdicts(&[], HACKED, "s", &ids(&["B", "A", "B", ""]), "g/t1.v1", 9, "r");
        let want = |l: &str| Outcome { cause: "g/t1.v1".into(), ..o(HACKED, "s", &[l], 9) };
        assert_eq!(first, vec![want("A"), want("B")], "sorted, unique, one per lesson");
        assert!(verdicts(&first, HACKED, "s", &ids(&["A", "B"]), "g/t1.v1", 10, "r").is_empty(), "a re-run writes nothing");
        assert!(verdicts(&first, VERIFIED, "s", &ids(&["A"]), "g/t1.v1", 10, "r").is_empty(), "a later score of the same attempt neither");
        assert_eq!(verdicts(&first, VERIFIED, "s", &ids(&["A"]), "g/t1.v2", 10, "r").len(), 1, "the next attempt is credited");
        assert!(verdicts(&[], VETOED, "s", &ids(&["A"]), "c", 1, "r").is_empty() && verdicts(&[], VERIFIED, "", &ids(&["A"]), "c", 1, "r").is_empty());
        for kind in [VERIFIED, REJECTED, HACKED] {
            let x = share(&Outcome { cause: "g/t1.v1".into(), ..o(kind, "s", &["A"], 1) }, "n1").expect("shared");
            assert!(is_verdict(kind) && x.c == tag("g/t1.v1"), "{kind} is shared with its cause's tag");
        }
        assert!(!is_verdict(VETOED) && !is_verdict(REGRETTED));
        // Not a session result: trust and `weight` never see a verdict, and settling still settles.
        assert!(sessions(&first).is_empty() && weights(&first).is_empty());
        let mut log = first.clone();
        log.push(o(ASKED, "s", &["A"], 5));
        assert_eq!(settle(&log, "s", 99), vec![o(NOT_CORRECTED, "s", &["A"], 99)]);
        log.push(o(NOT_CORRECTED, "t", &["A"], 7));
        let trust = |log: &[Outcome]| sessions(log).into_iter().map(|(l, m)| (l.to_string(), m.len())).collect::<Vec<_>>();
        assert_eq!(trust(&log), trust(&log[first.len()..]), "trust reads the same without the verdicts");
    }

    #[test]
    fn a_forged_team_verdict_is_ignored_and_a_local_one_counts_no_stat() {
        let home = tmp("verdict-team");
        let forged = Shared { kind: HACKED.into(), s: "x".into(), lessons: vec!["L1".into()], ms: 5, node: "n2".into(), ..Default::default() };
        lines(&home.join(TEAM_FILE), &[serde_json::to_string(&forged).unwrap()]);
        assert!(team_view(&home.join("lessons.jsonl"), true).is_empty(), "a verdict on an attempt no board here judged is dropped");
        let p = home.join("outcomes.jsonl");
        append(&p, &Outcome { cause: "g/t1.v1".into(), ..o(HACKED, "s", &["L1"], 5) }).unwrap();
        assert_eq!(read(&p).len(), 1);
        assert!(crate::stats::events(&home).is_empty(), "counted as task_hacked by its verdict, not per record");
        let _ = std::fs::remove_dir_all(home);
    }

    /// The board-binding guard: a peer's verdict line counts only for an attempt this node's board
    /// judged, of a kind that verdict allows, once per (lesson, attempt) with a local record of it.
    #[test]
    fn team_verdicts_count_once_with_their_cause_and_only_for_a_judged_attempt() {
        use crate::hive::{Ev, Event, Role, Task, board};
        let home = tmp("verdict-guard");
        let g = crate::hive::Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: vec![],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
            held_out: false,
        };
        crate::hive::goal::create(&home, &g).unwrap();
        let work = |id: &str| Task {
            id: id.into(),
            role: Role::Work,
            title: id.into(),
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
        let check = |id: &str| Task { id: format!("{id}.v1"), role: Role::Verify, of: Some(id.into()), not_by: vec!["s1".into()], ..work(id) };
        let verdict = |ev: Ev, t: &str, credit: &str| Event { credit: credit.into(), ..Event::new(ev, t, "sv", 40) };
        let mut lines = Vec::new();
        for t in ["t1", "t2"] {
            lines.extend([
                Event::open(work(t), "engineer:e", 1),
                Event { node: "n1".into(), ..Event::new(Ev::Claim, t, "s1", 10) },
                Event { node: "n1".into(), ..Event::new(Ev::Done, t, "s1", 20) },
                Event::open(check(t), "s1", 20),
                Event::new(Ev::Claim, &format!("{t}.v1"), "sv", 30),
            ]);
        }
        lines.extend([verdict(Ev::Verified, "t1.v1", VERIFIED), verdict(Ev::Rejected, "t2.v1", REJECTED)]);
        board::append(&crate::hive::tasks_path(&home, "g"), &lines).unwrap();
        let team = |kind: &str, s: &str, cause: &str, node: &str| {
            serde_json::to_string(&Shared {
                kind: kind.into(),
                s: s.into(),
                lessons: vec!["L1".into()],
                ms: 40,
                node: node.into(),
                c: tag(cause),
                ..Default::default()
            })
            .unwrap()
        };
        let ls = [crate::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let mem = |h: &Path| crate::memory::derive(&team_view(&h.join("lessons.jsonl"), true), &ls, None).remove("L1").unwrap();
        // The author's shared ask of L1: what a verdict on its attempt may credit.
        let asked =
            serde_json::to_string(&Shared { kind: ASKED.into(), s: tag("s1"), lessons: vec!["L1".into()], ms: 15, node: "n2".into(), ..Default::default() })
                .unwrap();
        let lines_at = |home: &Path, v: &[String]| lines_at(home, &[v, std::slice::from_ref(&asked)].concat());
        // A peer's verified, and the same line from a second node's copy: one count.
        lines_at(&home, &[team(VERIFIED, &tag("s1"), "g/t1.v1", "n2"), team(VERIFIED, &tag("s1"), "g/t1.v1", "n3")]);
        assert_eq!((mem(&home).verified, mem(&home).rejected, mem(&home).hacked), (1, 0, 0));
        // A judged cause is no licence: the line's session must be the attempt's author, and each lesson
        // it names one the team's records show the author was given.
        let any_session = team(VERIFIED, &tag("anything"), "g/t1.v1", "n2");
        let more = serde_json::to_string(&Shared {
            lessons: vec!["L1".into(), "L7".into()],
            ..serde_json::from_str(&team(VERIFIED, &tag("s1"), "g/t1.v1", "n2")).unwrap()
        })
        .unwrap();
        lines_at(&home, &[any_session, more]);
        let view = team_view(&home.join("lessons.jsonl"), true);
        assert!(!view.iter().any(|o| is_verdict(&o.kind)), "{view:?}");
        // A verified whose cause no board here judged, a hacked of a verdict that was not hacked, a
        // verified of an attempt the board rejected: all dropped.
        lines_at(
            &home,
            &[
                team(VERIFIED, &tag("s1"), "g/t1.v1", "n2"),
                team(VERIFIED, &tag("s9"), "g/t9.v1", "n2"),
                team(HACKED, &tag("s1"), "g/t1.v1", "n2"),
                team(VERIFIED, &tag("s1"), "g/t2.v1", "n2"),
                team(REJECTED, &tag("s1"), "g/t2.v1", "n2"),
            ],
        );
        let m = mem(&home);
        assert_eq!((m.verified, m.rejected, m.hacked), (1, 1, 0), "{m:?}");
        // With a hidden oracle either kind is allowed, but only from the node the work was done on.
        std::fs::remove_file(crate::hive::goal_path(&home, "g")).unwrap();
        crate::hive::goal::create(&home, &crate::hive::Goal { held_out: true, ..g.clone() }).unwrap();
        lines_at(&home, &[team(REJECTED, &tag("s1"), "g/t1.v1", "n2")]);
        assert_eq!(mem(&home).rejected, 0, "the work was done on n1, not on n2");
        lines_at(&home, &[team(REJECTED, &tag("s1"), "g/t1.v1", "n1")]);
        assert_eq!(mem(&home).rejected, 1);
        std::fs::remove_file(crate::hive::goal_path(&home, "g")).unwrap();
        crate::hive::goal::create(&home, &g).unwrap();
        // This node's own record of the attempt and a peer's copy of it under another session tag: one
        // count per (lesson, attempt), the local cause compared as its tag.
        append(&home.join("outcomes.jsonl"), &Outcome { cause: "g/t1.v1".into(), ..o(VERIFIED, "s1", &["L1"], 40) }).unwrap();
        lines_at(&home, &[team(VERIFIED, &tag("s1-copy"), "g/t1.v1", "n2")]);
        let view = team_view(&home.join("lessons.jsonl"), true);
        assert!(view.iter().any(|o| o.session == "s1" && o.cause == tag("g/t1.v1")), "local causes compare as tags");
        assert_eq!(mem(&home).verified, 1);
        let _ = std::fs::remove_dir_all(home);
    }

    fn lines_at(home: &Path, v: &[String]) {
        lines(&home.join(TEAM_FILE), v);
    }

    #[test]
    fn a_line_from_before_propensities_reads_and_writes_back_unchanged() {
        let line = r#"{"kind":"asked","session":"s","lessons":["A","B"],"ms":1,"repo":"r","channel":"stop","grades":[3,1],"ranks":[0,1],"applied":2}"#;
        let o: Outcome = serde_json::from_str(line).unwrap();
        assert!(o.p.is_empty());
        assert_eq!(serde_json::to_string(&o).unwrap(), line);
        let with = Outcome { p: vec![round_p(1.0), round_p(1.0 / 3.0)], ..o };
        assert!(serde_json::to_string(&with).unwrap().ends_with(r#""applied":2,"p":[1.0,0.3333]}"#));
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-outcome-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn lines(path: &Path, v: &[String]) {
        std::fs::write(path, v.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    }

    fn changed(team: &[Outcome], lesson: &str) -> u32 {
        let ls = [crate::conscience::Challenge { id: lesson.into(), ..Default::default() }];
        crate::memory::derive(team, &ls, None)[lesson].changed
    }

    #[test]
    fn share_redacts() {
        let mut a = o(ASKED, "session-raw", &["A"], 5);
        a.repo = "secret-repo".into();
        a.groups = vec![vec!["A".into(), "B".into()]];
        a.grades = vec![3];
        a.ranks = vec![0];
        a.p = vec![0.5];
        let x = share(&a, "n1").unwrap();
        let line = serde_json::to_string(&x).unwrap();
        for raw in ["session-raw", "secret-repo", "groups", "grades", "ranks", "\"id\"", "version"] {
            assert!(!line.contains(raw), "{raw} in {line}");
        }
        assert_eq!((x.s, x.r, x.p, x.node), (tag("session-raw"), tag("secret-repo"), vec![0.5], "n1".to_string()));
        assert_eq!(tag("s").len(), 32);
        for kind in [GRADED, "veto"] {
            assert!(share(&o(kind, "s", &["A"], 1), "n1").is_none(), "{kind} is not shared");
        }
        let regret = share(&Outcome { cause: "episode-raw".into(), credit: 0.25, ..o(REGRETTED, "s", &["A"], 1) }, "n1").unwrap();
        let line = serde_json::to_string(&regret).unwrap();
        assert!(!line.contains("episode-raw") && line.contains(&tag("episode-raw")) && line.contains(r#""credit":0.25"#), "{line}");
    }

    #[test]
    fn pool_fields_stay_local() {
        let a = Outcome { pool: vec!["A".into(), "B".into()], base: vec![1.25, 0.5], scoped: vec![true, false], p: vec![0.5], ..o(ASKED, "s", &["A"], 5) };
        let line = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Outcome>(&line).unwrap(), a, "round trip");
        let shared = serde_json::to_string(&share(&a, "n1").unwrap()).unwrap();
        for field in ["pool", "base", "scoped"] {
            assert!(line.contains(field) && !shared.contains(field), "{field}: {shared}");
        }
        assert!(!serde_json::to_string(&o(ASKED, "s", &["A"], 5)).unwrap().contains("pool"), "empty: not written");
    }

    #[test]
    fn publish_is_idempotent() {
        let home = tmp("publish");
        assert_eq!(publish(&home), 0, "no outcomes, nothing to publish");
        let mut asked = o(ASKED, "s1", &["A"], 1);
        asked.p = vec![1.0];
        lines(&home.join("outcomes.jsonl"), &[asked, o(GRADED, "s1", &["A"], 1), o(CHANGED, "s1", &["A"], 2)].map(|x| serde_json::to_string(&x).unwrap()));
        assert_eq!(publish(&home), 2, "graded stays local");
        let first = std::fs::read_to_string(home.join(TEAM_FILE)).unwrap();
        assert_eq!(publish(&home), 0);
        assert_eq!(std::fs::read_to_string(home.join(TEAM_FILE)).unwrap(), first);
        assert!(first.contains(r#""p":[1.0]"#) && !first.contains("\"s1\""), "{first}");
        // Unchanged since the last publish: the log is not read again; grown: only the new line goes.
        assert_eq!(std::fs::read_to_string(home.join(PUBLISHED)).unwrap(), std::fs::metadata(home.join("outcomes.jsonl")).unwrap().len().to_string());
        std::fs::OpenOptions::new()
            .append(true)
            .open(home.join("outcomes.jsonl"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{}\n", serde_json::to_string(&o(IGNORED, "s2", &["A"], 3)).unwrap()).as_bytes()))
            .unwrap();
        assert_eq!(publish(&home), 1);
        assert_eq!(publish(&home), 0);
        // A team file gone (a fresh mesh) is published again in full.
        std::fs::remove_file(home.join(TEAM_FILE)).unwrap();
        assert_eq!(publish(&home), 3);
        let _ = std::fs::remove_dir_all(home);
    }

    /// A store whose local log has s1 changed L1, and whose team file holds that result as published
    /// by this node, a remote session's change of L1, the same remote line from another node's copy,
    /// and a kind this binary does not share.
    fn team_store(name: &str, shuffle: bool) -> std::path::PathBuf {
        let home = tmp(name);
        let mut local = o(CHANGED, "s1", &["L1"], 10);
        local.repo = "app".into();
        lines(&home.join("outcomes.jsonl"), &[serde_json::to_string(&local).unwrap()]);
        let remote = |kind: &str, s: &str, node: &str| {
            serde_json::to_string(&Shared {
                kind: kind.into(),
                s: s.into(),
                lessons: vec!["L1".into()],
                ms: 20,
                node: node.into(),
                r: tag("web"),
                ..Default::default()
            })
            .unwrap()
        };
        let mut team = vec![
            serde_json::to_string(&share(&local, "me").unwrap()).unwrap(),
            remote(CHANGED, "x", "n2"),
            remote(CHANGED, "x", "n3"),
            remote(HACKED, "y", "n2"),
            remote("veto", "z", "n2"),
        ];
        if shuffle {
            team.reverse();
            team.swap(0, 2);
        }
        lines(&home.join(TEAM_FILE), &team);
        home
    }

    #[test]
    fn read_team_drops_own_copies_and_keeps_remote() {
        let home = team_store("team", false);
        let team = team_view(&home.join("lessons.jsonl"), true);
        assert_eq!(changed(&team, "L1"), 2, "{team:?}");
        assert_eq!(team.iter().filter(|o| o.session == "x").count(), 1, "one remote line per (kind, session, lessons, ms)");
        assert!(team.iter().all(|o| o.kind != HACKED && o.kind != "veto"), "unshared kinds, and verdicts no board here judged, are ignored");
        assert!(team.iter().all(|o| o.repo != "app"), "local repos compare as tags");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_team_is_order_independent() {
        let (a, b) = (team_store("order-a", false), team_store("order-b", true));
        let ls = [crate::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let m = |h: &Path| crate::memory::derive(&team_view(&h.join("lessons.jsonl"), true), &ls, None);
        assert_eq!(m(&a), m(&b));
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    /// A node's whole fold reaches its peers: a veto that outranks the session's change, a regret's
    /// credit, and a session that only graded (no tick on either node). A peer holding only the team file
    /// derives the same memory as the node that recorded it.
    #[test]
    fn a_peer_derives_the_same_memory_from_the_team_file() {
        let (a, b) = (tmp("fold-a"), tmp("fold-b"));
        let regret = Outcome { cause: "episode-1".into(), credit: 0.5, ..o(REGRETTED, "s2", &["L1"], 40) };
        let log = [
            o(GRADED, "s0", &["L1"], 5),
            o(ASKED, "s1", &["L1"], 10),
            o(CHANGED, "s1", &["L1"], 20),
            o(VETOED, "s1", &["L1"], 30),
            o(ASKED, "s2", &["L1"], 35),
            regret,
        ];
        lines(&a.join("outcomes.jsonl"), &log.map(|x| serde_json::to_string(&x).unwrap()));
        publish(&a);
        std::fs::copy(a.join(TEAM_FILE), b.join(TEAM_FILE)).unwrap();
        let ls = [crate::conscience::Challenge { id: "L1".into(), origin_ms: 1, ..Default::default() }];
        let m = |h: &Path| crate::memory::derive(&team_view(&h.join("lessons.jsonl"), true), &ls, None);
        let (here, there) = (m(&a), m(&b));
        assert_eq!((here["L1"].vetoed, here["L1"].changed, here["L1"].regretted), (1, 0, 0.5), "{here:?}");
        assert_eq!(here, there);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    /// The unguarded surface `SHARED_KINDS` documents, pinned until the deferred guards land: a remote
    /// line under one of this node's session tags is dropped here, whatever its kind, while a peer
    /// without that session takes it, and the stronger result wins there.
    #[test]
    fn a_remote_line_under_a_local_tag_counts_on_peers_only() {
        let (victim, peer) = (tmp("forged-victim"), tmp("forged-peer"));
        let own = o(CHANGED, "s1", &["L1"], 10);
        let forged = Shared { kind: CORRECTED.into(), s: tag("s1"), lessons: vec!["L1".into()], ms: 11, node: "n2".into(), ..Default::default() };
        let team = [serde_json::to_string(&share(&own, "me").unwrap()).unwrap(), serde_json::to_string(&forged).unwrap()];
        lines(&victim.join("outcomes.jsonl"), &[serde_json::to_string(&own).unwrap()]);
        lines(&victim.join(TEAM_FILE), &team);
        lines(&peer.join(TEAM_FILE), &team);
        let ls = [crate::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let m = |h: &Path| crate::memory::derive(&team_view(&h.join("lessons.jsonl"), true), &ls, None).remove("L1").unwrap();
        assert_eq!((m(&victim).changed, m(&victim).corrected), (1, 0), "the victim never sees the override");
        assert_eq!((m(&peer).changed, m(&peer).corrected), (0, 1), "a peer counts the forged correction");
        let _ = std::fs::remove_dir_all(victim);
        let _ = std::fs::remove_dir_all(peer);
    }

    #[test]
    fn off_keeps_beliefs_local() {
        let home = team_store("off", false);
        let local = team_view(&home.join("lessons.jsonl"), false);
        assert_eq!(local, read(&home.join("outcomes.jsonl")));
        assert_eq!(changed(&local, "L1"), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    /// Several writers appending to one log at once (a hive's drones on one store): every record
    /// reads back, none torn into `{A}{B}\n\n`.
    #[test]
    fn concurrent_appends_keep_every_record() {
        let home = tmp("concurrent");
        let p = home.join("outcomes.jsonl");
        let (writers, each) = (8, 2000);
        std::thread::scope(|scope| {
            for w in 0..writers {
                let p = &p;
                scope.spawn(move || (0..each).for_each(|i| append(p, &o(ASKED, &format!("s{w}"), &["A"], i)).unwrap()));
            }
        });
        assert_eq!(read(&p).len(), writers * each as usize);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn append_and_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("borg-outcome-{}", std::process::id()));
        let p = dir.join("outcomes.jsonl");
        let _ = std::fs::remove_file(&p);
        append(&p, &o(ASKED, "s", &["A"], 1)).unwrap();
        append(&p, &o(CORRECTED, "s", &["A"], 2)).unwrap();
        assert_eq!(read(&p), vec![o(ASKED, "s", &["A"], 1), o(CORRECTED, "s", &["A"], 2)]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
