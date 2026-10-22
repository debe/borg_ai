//! Trust per engineer × area, earned from outcomes (PLAN.md "Assimilation", step 4).
//!
//! Corrections are claims, not facts: engineers reverse themselves (in one real case the same day),
//! and weak engineers exist. So a lesson's pull on the conscience depends on how its authors' earlier
//! claims in the same area held up. Everything here is deterministic, costs 0 tokens and reads only
//! the routed collective's own store (`lessons.jsonl` and the `outcomes.jsonl` beside it): a private
//! collective's evidence never moves trust in a work one, and an off repo has none.
//!
//! **Local and shared evidence**: `followed` and `corrected` come from this machine's `outcomes.jsonl`
//! plus the teammates' results the mesh syncs as `outcomes-team.jsonl` (outcome.rs `read_team`: hashed
//! sessions, each counted once), unless BORG_SHARE_OUTCOMES=off; `superseded` comes from the lessons and
//! is shared. Teammates converge on the same trust once synced; results not yet synced, or kept local
//! with BORG_SHARE_OUTCOMES=off, still make it differ, and `borg trust` says so.
//!
//! **Engineers** are canonical (collective.rs "Engineers"): emails compare case-insensitively and
//! the emails an `engineers` entry lists are one engineer, mapped when lessons are read. The
//! aliases are this machine's config, so a peer without them splits that engineer's trust again.
//!
//! **Area**: every lesson carries one of [`AREAS`], set by the same model call that generalises its
//! episode (learn.rs); lessons from before areas existed, or with an unknown area, are "other".
//!
//! **Evidence** per (engineer, area): for each lesson, each of its engineers (git user.email
//! recorded at capture), in the lesson's area, gets (an engineer who joined the lesson by a later
//! merge only from sessions after they joined, so merging into a well-followed lesson inherits none
//! of its earned trust)
//! - `followed`, +1 per session: the conscience asked it and no correction of the same concern
//!   followed (outcome.rs `asked_not_corrected`);
//! - `corrected`, −1 per session: asked, and the engineer still corrected the same concern
//!   (`asked_then_corrected`): the claim did not change what the drone did;
//! - `superseded`, −2 once, when the supersession is settled: a later lesson replaced or contradicted
//!   it (`supersedes`) and is the same engineer reversing himself, or a queen answer, or the
//!   successor has been followed at least once. An unsettled contradiction by another engineer is a
//!   claim against a claim and costs nobody trust yet.
//!
//! **Formula** (Laplace's rule of succession, a Beta(1, 1) prior):
//!
//! trust = (1 + followed) / (2 + followed + corrected + 2 · superseded)
//!
//! No evidence gives the neutral 0.5; the value stays strictly inside (0, 1), so one event can never
//! make an engineer infallible or silence them. A superseded claim counts as two failures: being
//! reversed is direct evidence the claim was wrong, while a correction after asking may also mean
//! the question was too generic. One followed session gives 2/3, one corrected 1/3, one reversal 1/4.
//!
//! A lesson's trust is the mean of its authors' trust in its area (0.5 without known authors). The
//! conscience multiplies BM25 relevance by trust / 0.5 on top of the outcome weight. A superseded
//! lesson is hidden ([`successors`]) on a self-reversal, a queen answer, or when its successor is at
//! least as trusted; the successor inherits its relevance. Otherwise both stay live and the
//! conscience renders them as one disagreement with each side's trust.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::conscience::{self, Challenge};
use crate::outcome::{self, Outcome};
use crate::{collective, learn};

/// The kinds of concern a lesson can be about.
pub const AREAS: [&str; 12] = [
    "concurrency",
    "spec-conformance",
    "reuse-existing",
    "architecture-boundary",
    "verification",
    "minimality",
    "protocol-correctness",
    "data-source",
    "security",
    "dependency-version",
    "scope",
    OTHER,
];

pub const OTHER: &str = "other";

/// Trust without evidence.
pub const NEUTRAL: f64 = 0.5;

/// The area `raw` names (case, spaces and underscores ignored), "other" for anything else.
pub fn area(raw: &str) -> &'static str {
    let a = raw.trim().to_lowercase().replace(['_', ' '], "-");
    AREAS.iter().find(|x| **x == a).copied().unwrap_or(OTHER)
}

/// What one engineer's claims in one area led to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Evidence {
    pub followed: u32,
    pub corrected: u32,
    pub superseded: u32,
}

impl Evidence {
    /// (1 + followed) / (2 + followed + corrected + 2 · superseded); see the module docs.
    pub fn trust(&self) -> f64 {
        let (f, c, s) = (self.followed as f64, self.corrected as f64, self.superseded as f64);
        (1.0 + f) / (2.0 + f + c + 2.0 * s)
    }

    fn add(&mut self, o: Evidence) {
        self.followed += o.followed;
        self.corrected += o.corrected;
        self.superseded += o.superseded;
    }
}

/// Trust of every (engineer, area) that authored a lesson in one store.
#[derive(Debug, Default)]
pub struct Trust {
    evidence: BTreeMap<(String, String), Evidence>,
}

impl Trust {
    /// From a store's lessons (latest versions) and its outcomes.
    pub fn compute(lessons: &[Challenge], outcomes: &[Outcome]) -> Trust {
        let sessions = outcome::sessions(outcomes);
        let followed = |id: &str| sessions.get(id).is_some_and(|ss| outcome::tally(ss.values(), i64::MIN).0 > 0);
        let replaced: std::collections::HashSet<&str> =
            edges(lessons).into_iter().filter(|(x, y)| decisive(x, y) || followed(&x.id)).map(|(_, y)| y.id.as_str()).collect();
        let mut evidence: BTreeMap<(String, String), Evidence> = BTreeMap::new();
        for c in lessons {
            let superseded = u32::from(replaced.contains(c.id.as_str()));
            let mut authors: Vec<&str> = c.engineers.iter().map(String::as_str).filter(|x| !x.is_empty()).collect();
            authors.sort();
            authors.dedup();
            for a in authors {
                // Joined by a later merge: only sessions after that count (legacy lessons: all).
                let since = c.joined.get(a).copied().unwrap_or(i64::MIN);
                let (followed, corrected) = sessions.get(c.id.as_str()).map_or((0, 0), |ss| outcome::tally(ss.values(), since));
                let e = Evidence { followed, corrected, superseded };
                evidence.entry((a.to_string(), area(&c.area).to_string())).or_default().add(e);
            }
        }
        Trust { evidence }
    }

    /// `engineer`'s trust in `area`; neutral when they have no lesson there.
    pub fn of(&self, engineer: &str, area_name: &str) -> f64 {
        self.evidence.get(&(engineer.to_string(), area(area_name).to_string())).map_or(NEUTRAL, Evidence::trust)
    }

    /// Mean trust of a lesson's authors in its area; neutral without known authors.
    pub fn lesson(&self, c: &Challenge) -> f64 {
        let authors: Vec<&String> = c.engineers.iter().filter(|e| !e.is_empty()).collect();
        match authors.len() {
            0 => NEUTRAL,
            n => authors.iter().map(|e| self.of(e, &c.area)).sum::<f64>() / n as f64,
        }
    }

    /// Every (engineer, area) with its evidence, sorted by engineer then area.
    pub fn rows(&self) -> impl Iterator<Item = (&str, &str, Evidence)> {
        self.evidence.iter().map(|((e, a), x)| (e.as_str(), a.as_str(), *x))
    }

    /// Whether any (engineer, area) has evidence beyond the neutral prior.
    pub fn has_evidence(&self) -> bool {
        self.evidence.values().any(|e| *e != Evidence::default())
    }
}

/// The trust of the store at `home` (its lessons and outcomes, nothing else), engineers under this
/// machine's aliases.
pub fn of_store(home: &Path) -> Trust {
    of_store_with(home, collective::aliases())
}

/// `of_store` under explicit aliases.
pub fn of_store_with(home: &Path, aliases: &collective::Aliases) -> Trust {
    let lessons = home.join("lessons.jsonl");
    Trust::compute(&conscience::load_unweighted_with(&lessons, None, aliases), &outcome::read_team(&lessons))
}

/// Valid supersession edges (successor, replaced). "X supersedes Y" counts only if Y does not itself
/// (transitively) supersede X: in a cycle the collective disagrees with itself, so nobody's claim is
/// hidden or penalised and the conscience renders the disagreement instead. Valid edges are acyclic.
fn edges(lessons: &[Challenge]) -> Vec<(&Challenge, &Challenge)> {
    if lessons.iter().all(|c| c.supersedes.is_empty()) {
        return Vec::new();
    }
    let by: HashMap<&str, &Challenge> = lessons.iter().map(|c| (c.id.as_str(), c)).collect();
    let declared: HashMap<&str, Vec<&str>> =
        lessons.iter().map(|c| (c.id.as_str(), c.supersedes.iter().map(String::as_str).filter(|y| *y != c.id && by.contains_key(y)).collect())).collect();
    let reaches = |from: &str, to: &str| -> bool {
        let mut stack = vec![from];
        let mut seen = std::collections::HashSet::new();
        while let Some(x) = stack.pop() {
            if x == to {
                return true;
            }
            if seen.insert(x) {
                stack.extend(declared.get(x).into_iter().flatten().copied());
            }
        }
        false
    };
    lessons.iter().flat_map(|x| declared[x.id.as_str()].iter().filter(|y| !reaches(y, &x.id)).map(|y| (x, by[y])).collect::<Vec<_>>()).collect()
}

/// A supersession that settles the question by itself: an author reversing their own lesson, or a
/// queen answer (the engineer asked decided).
fn decisive(x: &Challenge, y: &Challenge) -> bool {
    x.source == "queen" || x.engineers.iter().any(|e| !e.is_empty() && y.engineers.contains(e))
}

/// Every hidden lesson's live successor (the end of its chain of hiding supersessions): hidden
/// lessons are not asked, the successor is asked in their place. X hides Y when the supersession is
/// `decisive` or X's trust (`Challenge::trust`) is at least Y's; otherwise both stay live and the
/// conscience renders them as a disagreement. Per hidden lesson the newest successor wins.
pub fn successors(lessons: &[Challenge]) -> HashMap<String, String> {
    let mut direct: HashMap<&str, &Challenge> = HashMap::new();
    for (x, y) in edges(lessons) {
        if !(decisive(x, y) || x.trust >= y.trust) {
            continue;
        }
        let newer = |o: &&Challenge| (x.origin_ms, x.id.as_str()) > (o.origin_ms, o.id.as_str());
        if direct.get(y.id.as_str()).is_none_or(newer) {
            direct.insert(&y.id, x);
        }
    }
    direct
        .keys()
        .map(|&y| {
            let mut s = direct[y];
            for _ in 0..lessons.len() {
                match direct.get(s.id.as_str()) {
                    Some(next) => s = next,
                    None => break,
                }
            }
            (y.to_string(), s.id.clone())
        })
        .collect()
}

/// One line for `borg status`.
pub fn summary(home: &Path) -> String {
    summary_with(home, collective::aliases())
}

/// `summary` under explicit aliases.
pub fn summary_with(home: &Path, aliases: &collective::Aliases) -> String {
    let t = of_store_with(home, aliases);
    let mut rows: Vec<(&str, &str, Evidence)> = t.rows().filter(|(_, _, e)| *e != Evidence::default()).collect();
    if rows.is_empty() {
        let engineers: std::collections::BTreeSet<&str> = t.rows().map(|(e, _, _)| e).collect();
        return format!("no evidence yet ({} engineers neutral at 0.50)", engineers.len());
    }
    rows.sort_by(|a, b| b.2.trust().total_cmp(&a.2.trust()).then(a.0.cmp(b.0)).then(a.1.cmp(b.1)));
    let (top, low) = (rows[0], rows[rows.len() - 1]);
    let mut s = format!("highest {} in {} {:.2}", top.0, top.1, top.2.trust());
    if rows.len() > 1 {
        s.push_str(&format!(", lowest {} in {} {:.2}", low.0, low.1, low.2.trust()));
    }
    format!("{s} ({} with evidence; borg trust)", rows.len())
}

/// The table `borg trust` prints for one store.
pub fn table(t: &Trust) -> String {
    let rows: Vec<(&str, &str, Evidence)> = t.rows().collect();
    if rows.is_empty() {
        return "no lessons with known engineers yet\n".into();
    }
    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max("engineer".len());
    let a = AREAS.iter().map(|x| x.len()).max().unwrap_or(0);
    let mut s = format!("{:<w$}  {:<a$}  trust  followed  corrected  superseded\n", "engineer", "area");
    for (e, ar, x) in rows {
        s.push_str(&format!("{e:<w$}  {ar:<a$}  {:.2}   {:>8}  {:>9}  {:>10}\n", x.trust(), x.followed, x.corrected, x.superseded));
    }
    s
}

/// `borg trust [--dir DIR]`: trust per engineer × area of the collective DIR (default: the current
/// directory) routes to. Off and invalid configs show nothing.
pub fn main(args: &[&str]) -> i32 {
    let mut cwd = std::env::current_dir().unwrap_or_default().display().to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (*a, it.next()) {
            ("--dir", Some(d)) => cwd = d.to_string(),
            _ => {
                eprintln!("usage: borg trust [--dir DIR]");
                return 2;
            }
        }
    }
    match collective::load().map(|cfg| collective::route(cfg.as_ref(), &cwd, &learn::home())) {
        Ok(collective::Route::Collective { name, home, .. }) => {
            println!("collective {name} ({})", home.display());
            println!("trust = (1 + followed) / (2 + followed + corrected + 2 x superseded); 0.50 = no evidence");
            match crate::outcome::on() {
                true => println!("followed/corrected: sessions on this machine and the team's synced outcomes; superseded: shared lessons\n"),
                false => println!("followed/corrected: sessions on this machine only (BORG_SHARE_OUTCOMES=off); superseded: shared lessons\n"),
            }
            print!("{}", table(&of_store(&home)));
            0
        }
        Ok(collective::Route::Off) => {
            println!("off: {cwd} belongs to no collective; no trust is kept here");
            0
        }
        Err(e) => {
            eprintln!("borg trust: invalid collectives config ({e}); everything is off");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{ASKED, CORRECTED, NOT_CORRECTED};

    fn lesson(id: &str, ms: i64, area: &str, engineers: &[&str], supersedes: &[&str]) -> Challenge {
        Challenge {
            id: id.into(),
            origin_ms: ms,
            question: format!("question {id}"),
            area: area.into(),
            engineers: engineers.iter().map(|e| e.to_string()).collect(),
            supersedes: supersedes.iter().map(|e| e.to_string()).collect(),
            ..Default::default()
        }
    }

    fn o(kind: &str, session: &str, id: &str) -> Outcome {
        Outcome { kind: kind.into(), session: session.into(), lessons: vec![id.into()], ms: 1, repo: "r".into(), ..Default::default() }
    }

    #[test]
    fn area_parsing_and_fallback() {
        assert_eq!(area("concurrency"), "concurrency");
        assert_eq!(area(" Spec_Conformance "), "spec-conformance");
        assert_eq!(area("reuse existing"), "reuse-existing");
        assert_eq!(area(""), OTHER);
        assert_eq!(area("vibes"), OTHER);
        // A stored lesson without an area reads as "other".
        let c: Challenge = serde_json::from_str(r#"{"id":"a","origin_ms":1,"question":"q"}"#).unwrap();
        assert_eq!(area(&c.area), OTHER);
    }

    #[test]
    fn neutral_without_evidence_and_laplace_smoothing() {
        assert_eq!(Evidence::default().trust(), NEUTRAL);
        let t = Trust::compute(&[lesson("a", 1, "concurrency", &["ann@x"], &[])], &[]);
        assert_eq!(t.of("ann@x", "concurrency"), NEUTRAL);
        assert_eq!(t.of("nobody@x", "security"), NEUTRAL);
        assert!(!t.has_evidence());
        let e = |f, c, s| Evidence { followed: f, corrected: c, superseded: s }.trust();
        assert!((e(1, 0, 0) - 2.0 / 3.0).abs() < 1e-12);
        assert!((e(0, 1, 0) - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(e(0, 0, 1), 0.25);
        assert!(e(1000, 0, 0) < 1.0 && e(0, 1000, 1000) > 0.0, "strictly inside (0, 1)");
    }

    #[test]
    fn followed_raises_trust_in_that_area_only() {
        let ls = [lesson("a", 1, "concurrency", &["ann@x", "bob@x"], &[])];
        let outs = [o(ASKED, "s1", "a"), o(NOT_CORRECTED, "s1", "a"), o(ASKED, "s2", "a"), o(NOT_CORRECTED, "s2", "a")];
        let t = Trust::compute(&ls, &outs);
        assert_eq!(t.of("ann@x", "concurrency"), 0.75);
        assert_eq!(t.of("bob@x", "concurrency"), 0.75);
        assert_eq!(t.of("ann@x", "security"), NEUTRAL);
    }

    #[test]
    fn asked_and_still_corrected_lowers_trust() {
        let ls = [lesson("a", 1, "scope", &["ann@x"], &[])];
        // Corrected in s1 (counts once despite the later settle), followed in s2.
        let outs = [o(ASKED, "s1", "a"), o(CORRECTED, "s1", "a"), o(NOT_CORRECTED, "s1", "a"), o(ASKED, "s2", "a"), o(NOT_CORRECTED, "s2", "a")];
        let t = Trust::compute(&ls, &outs);
        assert_eq!(t.rows().next().unwrap().2, Evidence { followed: 1, corrected: 1, superseded: 0 });
        assert_eq!(t.of("ann@x", "scope"), 0.5);
        let t = Trust::compute(&ls, &[o(CORRECTED, "s1", "a")]);
        assert!((t.of("ann@x", "scope") - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn superseded_lowers_its_authors_even_when_they_reverse_themselves() {
        // bob's lesson is replaced by ann's; ann later reverses her own lesson in another area.
        let ls = [
            lesson("b1", 1, "concurrency", &["bob@x"], &[]),
            lesson("a1", 2, "concurrency", &["ann@x"], &["b1"]),
            lesson("a2", 3, "security", &["ann@x"], &[]),
            lesson("a3", 4, "security", &["ann@x"], &["a2"]),
        ];
        let t = Trust::compute(&ls, &[]);
        assert_eq!(t.of("bob@x", "concurrency"), NEUTRAL, "another engineer's contradiction is unsettled: no penalty yet");
        assert_eq!(t.of("ann@x", "security"), 0.25, "a self-reversal counts against the reversed lesson's author at once");
        // Once ann's successor is followed, the supersession is settled and bob pays for it.
        let t = Trust::compute(&ls, &[o(NOT_CORRECTED, "s1", "a1")]);
        assert_eq!(t.of("bob@x", "concurrency"), 0.25);
        assert!((t.of("ann@x", "concurrency") - 2.0 / 3.0).abs() < 1e-12, "replacing a claim is no evidence for the replacer, being followed is");
        assert_eq!(successors(&ls), HashMap::from([("b1".to_string(), "a1".to_string()), ("a2".to_string(), "a3".to_string())]));
    }

    #[test]
    fn chains_resolve_and_cycles_hide_nobody() {
        let chain = [lesson("a", 1, "", &["x@x"], &[]), lesson("b", 2, "", &["y@x"], &["a"]), lesson("c", 3, "", &["z@x"], &["b"])];
        let s = successors(&chain);
        assert_eq!((s["a"].as_str(), s["b"].as_str(), s.len()), ("c", "c", 2));
        // Mutual supersession: unresolved disagreement, nobody is hidden or penalised.
        let cycle = [lesson("a", 1, "", &["x@x"], &["b"]), lesson("b", 2, "", &["y@x"], &["a"]), lesson("c", 3, "", &[], &["missing", "c"])];
        assert!(successors(&cycle).is_empty());
        assert!(!Trust::compute(&cycle, &[]).has_evidence());
    }

    #[test]
    fn a_later_coauthor_earns_only_from_sessions_after_joining() {
        // ann's lesson was followed in 20 sessions; bob merged an agreeing correction in at ms 100.
        let mut l = lesson("a", 1, "concurrency", &["ann@x", "bob@x"], &[]);
        l.joined.insert("bob@x".into(), 100);
        let mut outs: Vec<Outcome> = (0..20).map(|i| Outcome { ms: 10 + i, ..o(NOT_CORRECTED, &format!("s{i}"), "a") }).collect();
        // His own correction marked the lesson corrected at the moment he joined: not his evidence either.
        outs.push(Outcome { ms: 100, ..o(CORRECTED, "j", "a") });
        let t = Trust::compute(std::slice::from_ref(&l), &outs);
        assert!((t.of("ann@x", "concurrency") - 21.0 / 23.0).abs() < 1e-12);
        assert_eq!(t.of("bob@x", "concurrency"), NEUTRAL, "no inherited trust");
        outs.push(Outcome { ms: 200, ..o(NOT_CORRECTED, "later", "a") });
        assert!((Trust::compute(&[l], &outs).of("bob@x", "concurrency") - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn a_less_trusted_contradiction_hides_nothing_self_reversals_and_queen_answers_do() {
        let mut ann = lesson("ann", 1, "concurrency", &["ann@x"], &[]);
        let mut bob = lesson("bob", 2, "concurrency", &["bob@x"], &["ann"]);
        (ann.trust, bob.trust) = (0.9, 0.2);
        assert!(successors(&[ann.clone(), bob.clone()]).is_empty(), "both stay live");
        bob.trust = 0.9;
        assert_eq!(successors(&[ann.clone(), bob.clone()])["ann"], "bob", "at least as trusted: hides");
        bob.trust = 0.2;
        let mut queen = lesson("q", 3, "concurrency", &["lead@x"], &["ann"]);
        (queen.source, queen.trust) = ("queen".into(), 0.1);
        assert_eq!(successors(&[ann.clone(), queen.clone()])["ann"], "q");
        let own = Challenge { trust: 0.1, ..lesson("own", 3, "concurrency", &["ann@x"], &["ann"]) };
        assert_eq!(successors(&[ann.clone(), own])["ann"], "own");
        // The queen answer settles the penalty without any outcome.
        assert_eq!(Trust::compute(&[ann, queen], &[]).of("ann@x", "concurrency"), 0.25);
    }

    #[test]
    fn lesson_trust_is_the_mean_of_its_authors_in_its_area() {
        let ls = [lesson("a", 1, "concurrency", &["ann@x"], &[]), lesson("b", 2, "concurrency", &["bob@x"], &[])];
        let t = Trust::compute(&ls, &[o(NOT_CORRECTED, "s", "a"), o(CORRECTED, "s", "b")]);
        let both = lesson("c", 3, "concurrency", &["ann@x", "bob@x"], &[]);
        assert!((t.lesson(&both) - (2.0 / 3.0 + 1.0 / 3.0) / 2.0).abs() < 1e-12);
        assert_eq!(t.lesson(&lesson("d", 4, "concurrency", &[], &[])), NEUTRAL);
        assert_eq!(t.lesson(&lesson("e", 4, "security", &["ann@x"], &[])), NEUTRAL);
    }

    fn store(name: &str, lessons: &[serde_json::Value], outcomes: &[Outcome]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("borg-trust-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lessons.jsonl"), lessons.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        for x in outcomes {
            outcome::append(&dir.join("outcomes.jsonl"), x).unwrap();
        }
        dir
    }

    fn dana() -> collective::Aliases {
        let cfg: collective::Config =
            serde_json::from_str(r#"{"engineers": [{"name": "dana", "emails": ["Dana.Example@corp.example", "da@home.example"]}]}"#).unwrap();
        cfg.aliases()
    }

    #[test]
    fn an_engineers_emails_share_one_trust() {
        use serde_json::json;
        let l = |id: &str, e: &[&str]| json!({"id": id, "origin_ms": 1, "question": "q", "area": "concurrency", "engineers": e});
        let dir = store(
            "aliases",
            &[l("a", &["Dana.Example@corp.example"]), l("b", &["da@home.example"]), l("c", &["ann@x"])],
            &[o(NOT_CORRECTED, "s1", "a"), o(NOT_CORRECTED, "s2", "b"), o(CORRECTED, "s3", "c")],
        );
        // Absent config: unchanged, two engineers with one followed session each.
        let none = collective::Aliases::default();
        let t = of_store_with(&dir, &none);
        let rows: Vec<(&str, Evidence)> = t.rows().map(|(e, _, x)| (e, x)).collect();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!((t.of("da@home.example", "concurrency") - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(summary_with(&dir, &none), "highest da@home.example in concurrency 0.67, lowest ann@x in concurrency 0.33 (3 with evidence; borg trust)");
        // Listed as one engineer: one row, both sessions.
        let t = of_store_with(&dir, &dana());
        assert_eq!(t.rows().map(|(e, _, x)| (e.to_string(), x.followed)).collect::<Vec<_>>(), [("ann@x".into(), 0), ("dana".to_string(), 2)]);
        assert_eq!(t.of("dana", "concurrency"), 0.75);
        assert!(table(&t).contains("dana") && !table(&t).contains("corp.example"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn status_counts_an_engineers_emails_once() {
        use serde_json::json;
        let l = |id: &str, e: &[&str]| json!({"id": id, "origin_ms": 1, "question": "q", "engineers": e});
        let dir = store("status", &[l("a", &["Dana.Example@corp.example"]), l("b", &["da@home.example", "DA@home.example"])], &[]);
        assert_eq!(summary_with(&dir, &collective::Aliases::default()), "no evidence yet (2 engineers neutral at 0.50)", "case alone never splits");
        assert_eq!(summary_with(&dir, &dana()), "no evidence yet (1 engineers neutral at 0.50)");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn aliases_settle_self_reversals_and_joins() {
        use serde_json::json;
        // dana reverses her own lesson from her other email: settled at once, like any self-reversal.
        let dir = store(
            "reversal",
            &[
                json!({"id": "a", "origin_ms": 1, "question": "q", "area": "security", "engineers": ["Dana.Example@corp.example"]}),
                json!({"id": "b", "origin_ms": 2, "question": "q", "area": "security", "engineers": ["da@home.example"], "supersedes": ["a"]}),
                // His other email merged into his own lesson later: still the first contributor.
                json!({"id": "c", "origin_ms": 3, "question": "q", "area": "scope", "engineers": ["Dana.Example@corp.example", "da@home.example"],
                    "joined": {"da@home.example": 100}}),
            ],
            &[Outcome { ms: 10, ..o(NOT_CORRECTED, "early", "c") }],
        );
        let t = of_store_with(&dir, &collective::Aliases::default());
        assert_eq!(t.of("Dana.Example@corp.example".to_lowercase().as_str(), "security"), NEUTRAL, "two engineers: an unsettled contradiction");
        assert_eq!(t.of("da@home.example", "scope"), NEUTRAL, "joined after the session");
        let t = of_store_with(&dir, &dana());
        assert_eq!(t.of("dana", "security"), 0.25, "one engineer reversing himself");
        assert!((t.of("dana", "scope") - 2.0 / 3.0).abs() < 1e-12, "the session counts: he was there from the start");
        let lessons = conscience::load_with(&dir.join("lessons.jsonl"), None, &dana());
        assert_eq!(lessons[2].engineers, ["dana"]);
        assert!(lessons[2].joined.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn table_and_summary_read_one_store() {
        let dir = std::env::temp_dir().join(format!("borg-trust-table-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(summary(&dir).starts_with("no evidence yet (0 engineers"));
        let line =
            |id: &str, area: &str, e: &str| format!("{}\n", serde_json::json!({"id": id, "origin_ms": 1, "question": "q", "area": area, "engineers": [e]}));
        std::fs::write(dir.join("lessons.jsonl"), line("a", "concurrency", "ann@x") + &line("b", "scope", "bob@x")).unwrap();
        for x in [o(NOT_CORRECTED, "s", "a"), o(CORRECTED, "s", "b")] {
            outcome::append(&dir.join("outcomes.jsonl"), &x).unwrap();
        }
        let t = of_store(&dir);
        let text = table(&t);
        assert!(text.lines().nth(1).unwrap().starts_with("ann@x     concurrency"), "{text}");
        assert!(text.contains("0.67") && text.contains("0.33"), "{text}");
        assert_eq!(summary(&dir), "highest ann@x in concurrency 0.67, lowest bob@x in scope 0.33 (2 with evidence; borg trust)");
        let _ = std::fs::remove_dir_all(dir);
    }
}
