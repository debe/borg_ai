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
//!
//! An unclear verdict, or a session whose later turn ends never reach the model (the prefilter skipped
//! them, or BORG_MATCH=bm25), leaves the lesson pending: `settle` turns it into `asked_not_corrected`.
//!
//! A session counts once per lesson. If a resumed session gets both results, corrected wins.
//! memory.rs derives activation and the usefulness belief from all four kinds; `weight` below is the
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
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

/// The outcomes store next to a lessons file.
pub fn path_beside(lessons: &Path) -> std::path::PathBuf {
    lessons.with_file_name("outcomes.jsonl")
}

pub fn read(path: &Path) -> Vec<Outcome> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

pub fn append(path: &Path, o: &Outcome) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(o).unwrap())?;
    // The results count in the usage statistics; `asked` is already counted as the Stop's block.
    let kind = match o.kind.as_str() {
        CHANGED => crate::stats::CHANGED,
        IGNORED => crate::stats::IGNORED,
        CORRECTED => crate::stats::CORRECTED,
        NOT_CORRECTED => crate::stats::NOT_CORRECTED,
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
/// before `ms` and not yet recorded as corrected there: the lessons the correction `supersedes`
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
    out.retain(|l| asked_before(outcomes, session, l, ms) && !has(outcomes, CORRECTED, session, l));
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
        outcomes
            .iter()
            .any(|o| o.kind == GRADED && o.session == session && o.ms < ms && o.lessons.iter().any(|x| x == l) && !o.chosen.iter().any(|x| x == l))
    };
    out.retain(|l| applied_unchosen(l) && !asked_before(outcomes, session, l, ms) && !has(outcomes, MISSED, session, l));
    out.sort();
    out.dedup();
    out
}

/// At session end: every lesson asked in `session` that has no result yet was not corrected.
/// Sorted by lesson id so the appended lines are deterministic.
pub fn settle(outcomes: &[Outcome], session: &str, ms: i64) -> Vec<Outcome> {
    let mut asked: Vec<(&str, &str)> = outcomes
        .iter()
        .filter(|o| o.kind == ASKED && o.session == session)
        .flat_map(|o| o.lessons.iter().map(move |l| (l.as_str(), o.repo.as_str())))
        .filter(|(l, _)| !has(outcomes, CORRECTED, session, l) && !has(outcomes, NOT_CORRECTED, session, l))
        .collect();
    asked.sort();
    asked.dedup_by_key(|(l, _)| *l);
    asked
        .into_iter()
        .map(|(l, repo)| Outcome { kind: NOT_CORRECTED.into(), session: session.into(), lessons: vec![l.into()], ms, repo: repo.into(), ..Default::default() })
        .collect()
}

/// Sessions with asks that are idle at `now`: their newest ask and newest episode
/// (`last_episode`, newest episode ms per session) are at least `IDLE_MS` old. Sorted.
pub fn idle_sessions<'a>(outcomes: &'a [Outcome], last_episode: &HashMap<String, i64>, now: i64) -> Vec<&'a str> {
    let mut newest: HashMap<&str, i64> = HashMap::new();
    for o in outcomes.iter().filter(|o| o.kind == ASKED) {
        let m = newest.entry(o.session.as_str()).or_insert(o.ms);
        *m = (*m).max(o.ms);
    }
    let mut idle: Vec<&str> =
        newest.into_iter().filter(|&(s, ms)| ms.max(last_episode.get(s).copied().unwrap_or(i64::MIN)) <= now - IDLE_MS).map(|(s, _)| s).collect();
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
/// joined the lesson.
pub fn sessions(outcomes: &[Outcome]) -> HashMap<&str, HashMap<&str, (bool, i64)>> {
    let mut out: HashMap<&str, HashMap<&str, (bool, i64)>> = HashMap::new();
    for o in outcomes {
        let corrected = match o.kind.as_str() {
            CORRECTED => true,
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
