//! Who should hear a lesson: its reach.
//!
//! A correction can be an engineering concern anyone on the code should hear (the team, the default),
//! a convention of one repo (`repo`: delivered only there), or one engineer's taste in how an agent
//! works with them (`personal`: commit style, attribution, branching, tone). A personal lesson stays in
//! the collective it was learned in, in that store's local-only [`PERSONAL_FILE`] beside
//! `lessons.jsonl`: never synced (borg-mesh `SYNCED` does not list it), so it is delivered only to this
//! engineer, and only in this collective. Nothing crosses a collective boundary automatically; moving a
//! lesson elsewhere is the engineer's explicit `borg move`.
//!
//! In `lessons.jsonl` a personal lesson is only a tombstone, a newer version that withdraws one of this
//! engineer's own team lessons. A lesson another engineer is on is never withdrawn from the team.

use std::path::{Path, PathBuf};

use crate::conscience::Challenge;
use crate::learn::{Episode, Lesson, append_jsonl};

/// For a lesson of one repo only.
pub const REPO: &str = "repo";
/// For a personal lesson (in [`PERSONAL_FILE`]) and for its tombstone in `lessons.jsonl`.
pub const PERSONAL: &str = "personal";
/// This engineer's personal lessons of a store, beside its `lessons.jsonl`: the same `Lesson` lines,
/// versioned the same way, never synced and never exported.
pub const PERSONAL_FILE: &str = "personal.jsonl";

/// The personal lessons file of the store `home`.
pub fn personal_path(home: &Path) -> PathBuf {
    home.join(PERSONAL_FILE)
}

/// The latest version of each live personal lesson of the store `home` (none when the file is absent):
/// the ones `borg move` took elsewhere (their tombstone, [`MOVED`]) are gone from here.
pub fn personal_lessons(home: &Path) -> Vec<Lesson> {
    personal_all(home).into_iter().filter(|l| l.question != MOVED).collect()
}

/// The latest version of every line of the store's personal file, a move's tombstone included: what
/// "learned" counts (learn.rs `learned_episodes`), for a tombstone keeps its episodes.
pub fn personal_all(home: &Path) -> Vec<Lesson> {
    crate::learn::lessons(&personal_path(home))
}

/// The ids in the store's personal file, a move's tombstones included: what the team's synced logs
/// never name (outcome.rs `publish`, stats.rs `rollup`). Empty, and no read, when the file is absent.
pub fn personal_ids(home: &Path) -> std::collections::HashSet<String> {
    personal_all(home).into_iter().map(|l| l.id).collect()
}

/// The question of the tombstone `borg move` leaves where a lesson was (moves.rs): no text left to
/// deliver, merge into or sync on; its episodes stay, so they stay learned.
pub const MOVED: &str = "(moved to another collective)";

/// Whether `c` may be delivered in a session working in `repos` (systems.rs `work_repos`). A personal
/// lesson is delivered as a team one by the loader that reads it (conscience.rs `load_store_for`): what
/// reaches this check as personal is a tombstone.
pub fn deliverable(c: &Challenge, repos: &[String]) -> bool {
    match c.reach.as_str() {
        PERSONAL => false,
        REPO => repos.iter().any(|r| r.eq_ignore_ascii_case(&c.repo)),
        _ => true,
    }
}

/// The narrower of two reaches: a merge never widens who hears a lesson (personal > repo > team).
pub fn narrower<'a>(a: &'a str, b: &'a str) -> &'a str {
    let rank = |r: &str| match r {
        PERSONAL => 2,
        REPO => 1,
        _ => 0,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// The question a tombstone carries in `lessons.jsonl` instead of the withdrawn lesson: the lesson is
/// personal to its engineer now, kept in this store's [`PERSONAL_FILE`]. It syncs, so it says only that.
pub const PERSONAL_TOMBSTONE: &str = "(personal to its engineer)";
/// The question of the tombstones a borg before 0.7.4 wrote, when a personal lesson went to a private
/// collective: read only to find them (moves.rs doctor's repair), never written.
pub const WITHDRAWN: &str = "(personal preference, moved to a private collective)";

/// Whether every engineer on `engineers` is `me` (under this machine's aliases), and there is one: only
/// then is a lesson provably this engineer's alone. An unknown `me` (no git email) is nobody; a lesson
/// with no engineer recorded (an old one, a queen answer without author, an import) is nobody's provably.
pub fn only(engineers: &[String], me: &str) -> bool {
    only_with(engineers, me, crate::collective::aliases())
}

/// [`only`] under explicit aliases (moves.rs).
pub fn only_with(engineers: &[String], me: &str, aliases: &crate::collective::Aliases) -> bool {
    !me.trim().is_empty() && !engineers.is_empty() && engineers.iter().all(|x| aliases.same(x.trim(), me.trim()))
}

/// Whether `l` was learned from sessions or queen answers, not imported: imports are written-down team
/// knowledge, never withdrawn or re-sorted by one engineer's verdict.
fn learned_here(l: &Lesson) -> bool {
    l.source.is_empty() || l.source == "queen"
}

/// Where a learned lesson goes, in the store it was learned in (`write`).
#[derive(Debug, Clone)]
pub enum Placed {
    /// `lessons.jsonl`, as usual.
    Here(Box<Lesson>),
    /// [`PERSONAL_FILE`]. `withdraw`: a tombstone for this engineer's own team lesson of the same id,
    /// appended to `lessons.jsonl` after it.
    Personal { lesson: Box<Lesson>, withdraw: Option<Box<Lesson>> },
}

impl Placed {
    /// The lesson as it is written.
    pub fn lesson(&self) -> &Lesson {
        match self {
            Placed::Here(l) | Placed::Personal { lesson: l, .. } => l,
        }
    }
}

/// Decides where `l`, the verdict on episode `e` in a store, goes. `mine`: `l.id` is one of the store's
/// personal lessons; `team`: the store's team lesson with `l.id`, if any (a tombstone is none). Never a
/// cross-store write: a personal lesson stays in this store's [`PERSONAL_FILE`].
/// - Not personal and not mine: `lessons.jsonl`.
/// - Mine already: a new version of it, personal (a merge never widens).
/// - Personal and new: personal.
/// - Personal, merged into a team lesson only this engineer (`e.engineer`) is on: personal, and the team
///   version withdrawn.
/// - Personal, merged into a team lesson another engineer is on, or no engineer is recorded on, or that
///   was imported: the team lesson is left as it is and nothing of it is copied; the episode becomes a
///   personal lesson of its own (`own`).
pub fn place(l: &Lesson, e: &Episode, mine: bool, team: Option<&Lesson>) -> Placed {
    if !mine && l.reach != PERSONAL {
        return Placed::Here(Box::new(l.clone()));
    }
    let personal = |l: Lesson| Box::new(Lesson { reach: PERSONAL.into(), ..l });
    match team {
        Some(t) if !mine && !(only(&t.engineers, &e.engineer) && learned_here(t)) => Placed::Personal { lesson: personal(own(l, e)), withdraw: None },
        Some(_) if !mine => Placed::Personal { lesson: personal(l.clone()), withdraw: Some(Box::new(tombstone(l))) },
        _ => Placed::Personal { lesson: personal(l.clone()), withdraw: None },
    }
}

/// The episode `e` alone as a new lesson in the wording of the verdict `l`: no other engineer, episode,
/// history or supersession of the lesson it was merged into.
fn own(l: &Lesson, e: &Episode) -> Lesson {
    Lesson {
        id: format!("L{}", e.id),
        origin_ms: e.ms,
        repo: e.repo.clone(),
        question: l.question.clone(),
        situations: l.situations.clone(),
        consequence: l.consequence.clone(),
        support: 1,
        episodes: vec![e.id.clone()],
        version: 1,
        engineers: Some(e.engineer.clone()).filter(|x| !x.is_empty()).into_iter().collect(),
        area: l.area.clone(),
        ..Lesson::default()
    }
}

/// Writes `p` to the store `home`: the personal lesson before its tombstone, so a failed write leaves
/// the team lesson where it was rather than losing the lesson.
///
/// # Errors
/// When a line cannot be appended.
pub fn write(home: &Path, p: &Placed) -> std::io::Result<()> {
    match p {
        Placed::Here(l) => append_jsonl(&home.join("lessons.jsonl"), &**l),
        Placed::Personal { lesson, withdraw } => {
            append_jsonl(&personal_path(home), &**lesson)?;
            match withdraw {
                Some(t) => append_jsonl(&home.join("lessons.jsonl"), &**t),
                None => Ok(()),
            }
        }
    }
}

/// The newer version of `l` that withdraws it from `lessons.jsonl`: no text left to deliver or sync on.
/// It keeps its episodes, so they stay learned.
pub fn tombstone(l: &Lesson) -> Lesson {
    Lesson {
        version: l.version.max(1) + 1,
        question: PERSONAL_TOMBSTONE.into(),
        situations: Vec::new(),
        consequence: String::new(),
        history: Vec::new(),
        facets: Vec::new(),
        reach: PERSONAL.into(),
        ..l.clone()
    }
}

/// The answers a reach question offers (queen.rs `Kind::Reach`).
pub const TEAM_OPTION: &str = "team: everyone working on this code should hear it";
pub const PERSONAL_OPTION: &str = "personal: only for me, kept in this collective";
/// Display: the characters of a lesson's question on a line of `borg reach`'s plan, after its id and
/// before the model's reason: about one wide terminal line.
const PLAN_CHARS: usize = 110;

fn repo_option(repo: &str) -> String {
    format!("repo: only in {}", if repo.is_empty() { "the repo it was learned in" } else { repo })
}

/// What an answer to a reach question chose: `PERSONAL`, `REPO` or "" (the team).
fn verdict(text: &str) -> &'static str {
    let t = text.trim().to_lowercase();
    if t.starts_with("personal") || t.contains("private") || t.contains("only me") {
        PERSONAL
    } else if t.starts_with("repo") {
        REPO
    } else {
        ""
    }
}

/// Gives `l` (the latest version in the store `home`) the reach `to`: personal for `me` alone (kept in
/// this store's [`PERSONAL_FILE`], the team version withdrawn), a new version for repo, nothing for the
/// team. A lesson that is not provably `me`'s alone (`only`) is never re-sorted: an error, nothing written.
pub fn apply_one(home: &Path, l: &Lesson, to: &str, me: &str) -> Result<(), String> {
    match to {
        PERSONAL | REPO if !only(&l.engineers, me) => Err(format!("{} is not yours alone (another engineer or none recorded): it stays with the team", l.id)),
        PERSONAL => write(home, &Placed::Personal { lesson: Box::new(Lesson { reach: PERSONAL.into(), ..l.clone() }), withdraw: Some(Box::new(tombstone(l))) })
            .map_err(|e| e.to_string()),
        REPO => append_jsonl(&home.join("lessons.jsonl"), &Lesson { version: l.version + 1, reach: REPO.into(), ..l.clone() }).map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

/// The engineer working here: the git email of the current directory.
fn me() -> String {
    crate::learn::engineer(&std::env::current_dir().unwrap_or_default().display().to_string())
}

/// Applies the answer to a reach question (queen.rs `answer`), as the engineer working here.
pub fn answer(store: &Path, e: &crate::queen::Entry, text: &str) -> Result<(), String> {
    let id = e.evidence.first().and_then(|x| x.strip_prefix("lesson ")).ok_or("this question names no lesson")?;
    let l = crate::learn::lessons(&store.join("lessons.jsonl")).into_iter().find(|l| l.id == id).ok_or(format!("lesson {id} is not in this collective"))?;
    apply_one(store, &l, verdict(text), &me())
}

const PROMPT: &str = "A coding agent learned this lesson from an engineer's correction. Decide its reach: who should hear it. \
Ask: would a teammate working on the same code get this correction too?
- personal: how this one engineer likes the agent to work with them, not a property of the code: commit message style or \
length, AI attribution or co-author trailers, whether and when to commit, push, branch or open a PR, what to stage, how to \
report or ask, language and tone, their own tooling habits.
- repo: a convention of the repo it was learned in that other repos do not share (its branch or deploy workflow, a tool or \
command it mandates).
- team: an engineering concern about the code, design, correctness or verification that any teammate should hear.
Set confident to false when the lesson mixes these or you cannot tell. why: one short sentence.";

fn schema() -> serde_json::Value {
    serde_json::json!({"type": "object", "additionalProperties": false, "properties": {
        "reach": {"type": "string", "enum": ["team", "repo", "personal"]}, "confident": {"type": "boolean"}, "why": {"type": "string"}},
        "required": ["reach", "confident", "why"]})
}

/// One line of a reach plan: a lesson and the model's verdict on who should hear it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Verdict {
    pub id: String,
    pub repo: String,
    pub question: String,
    pub reach: String,
    pub confident: bool,
    pub why: String,
}

/// Whether a cleanup by `me` may consider `l`: learned from sessions or queen answers, still for the team,
/// and no other engineer on it (`only`): this machine never re-sorts a teammate's lesson. Imports are
/// left alone: written-down team knowledge, scoped by system already.
fn candidate(l: &Lesson, me: &str) -> bool {
    l.reach.is_empty() && learned_here(l) && only(&l.engineers, me)
}

/// The lessons of the store `home` a cleanup by `me` classifies (`candidate`).
pub fn candidates(home: &Path, me: &str) -> Vec<Lesson> {
    crate::learn::lessons(&home.join("lessons.jsonl")).into_iter().filter(|l| candidate(l, me)).collect()
}

/// A structured model call: the answer and its cost (learn.rs `ask_model`).
pub type Ask = dyn Fn(&str, &serde_json::Value) -> Option<(serde_json::Value, Option<f64>)> + Sync;

/// Classifies `lessons` with one model call each (`jobs` at a time). Returns the verdicts and the cost.
/// After `runner::MAX_FAILURES` failed calls before the first answer the model is unavailable: the
/// rest are not asked (they stay candidates for the next run).
pub fn classify(lessons: &[Lesson], jobs: usize, ask: &Ask) -> (Vec<Verdict>, f64) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let out = std::sync::Mutex::new((Vec::new(), 0.0));
    let (next, failures, answers) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| {
                loop {
                    if failures.load(Ordering::SeqCst) >= crate::runner::MAX_FAILURES && answers.load(Ordering::SeqCst) == 0 {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(l) = lessons.get(i) else { break };
                    let prompt = format!("{PROMPT}\n\nLesson (learned in repo {}):\nQuestion: {}\nConsequence: {}\n", l.repo, l.question, l.consequence);
                    let Some((v, cost)) = ask(&prompt, &schema()) else {
                        failures.fetch_add(1, Ordering::SeqCst);
                        continue;
                    };
                    answers.fetch_add(1, Ordering::SeqCst);
                    let reach = v["reach"].as_str().filter(|r| [REPO, PERSONAL].contains(r)).unwrap_or_default();
                    let verdict = Verdict {
                        id: l.id.clone(),
                        repo: l.repo.clone(),
                        question: l.question.clone(),
                        reach: reach.into(),
                        confident: v["confident"].as_bool().unwrap_or(false),
                        why: v["why"].as_str().unwrap_or_default().into(),
                    };
                    let mut g = out.lock().unwrap();
                    g.0.push(verdict);
                    g.1 += cost.unwrap_or(0.0);
                }
            });
        }
    });
    let (mut v, cost) = out.into_inner().unwrap();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    (v, cost)
}

/// What applying a plan did.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    /// Made personal: kept for this engineer in this collective, withdrawn from the team.
    pub personal: usize,
    pub scoped: usize,
    pub asked: usize,
}

/// Applies a plan to the store `home` as `me`: confident personal verdicts become personal, confident
/// repo verdicts are scoped, unsure ones become queen questions; team verdicts change nothing. A lesson
/// that is no `candidate` (now) is skipped, whatever the plan says.
pub fn apply(home: &Path, plan: &[Verdict], me: &str, now: i64) -> Result<Applied, String> {
    let lessons = crate::learn::lessons(&home.join("lessons.jsonl"));
    let mut done = Applied::default();
    for v in plan {
        let Some(l) = lessons.iter().find(|l| l.id == v.id && candidate(l, me)) else { continue };
        match (v.confident, v.reach.as_str()) {
            (true, PERSONAL) => {
                apply_one(home, l, PERSONAL, me)?;
                done.personal += 1;
            }
            (true, REPO) => {
                apply_one(home, l, REPO, me)?;
                done.scoped += 1;
            }
            (true, _) => {}
            (false, _) => {
                let key = crate::queen::decision_key(crate::queen::Kind::Reach, &l.repo, &l.id);
                let default = match v.reach.as_str() {
                    PERSONAL => PERSONAL_OPTION.to_string(),
                    REPO => repo_option(&l.repo),
                    _ => TEAM_OPTION.to_string(),
                };
                let e = crate::queen::Entry {
                    id: format!("q{}", &key[..8]),
                    decision_key: key,
                    kind: crate::queen::Kind::Reach,
                    question: format!("Who should hear this lesson? \"{}\"", crate::stats::excerpt(&l.question, crate::queen::QUOTE_CHARS)),
                    options: vec![TEAM_OPTION.into(), repo_option(&l.repo), PERSONAL_OPTION.into()],
                    default,
                    evidence: vec![format!("lesson {}", l.id), format!("model: {}", v.why)],
                    session: "reach".into(),
                    cwd: String::new(),
                    repo: l.repo.clone(),
                    engineer: l.engineers.first().cloned().unwrap_or_default(),
                    ms: now,
                    answer: None,
                    answered_by: None,
                    answered_ms: None,
                    disagreeing: Vec::new(),
                    dismissed: None,
                    dismissed_ms: None,
                    payload: None,
                };
                if crate::queen::seed(home, &e).map_err(|e| e.to_string())? {
                    done.asked += 1;
                }
            }
        }
    }
    Ok(done)
}

/// `borg reach [--plan FILE] [--apply] [--jobs N]`: the cleanup of a store's lessons by reach (module doc).
pub fn main(args: &[&str]) -> i32 {
    let flag = |name: &str| args.iter().position(|a| *a == name).and_then(|i| args.get(i + 1)).copied();
    let apply_it = args.contains(&"--apply");
    let cwd = std::env::current_dir().unwrap_or_default().display().to_string();
    let cfg = match crate::collective::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("borg reach: invalid collectives config: {e}");
            return 1;
        }
    };
    let crate::collective::Route::Collective { name, home, .. } = crate::collective::route(cfg.as_ref(), &cwd, &crate::learn::home()) else {
        eprintln!("borg reach: {cwd} belongs to no collective");
        return 1;
    };
    let plan_path = flag("--plan").map(PathBuf::from).unwrap_or_else(|| home.join("reach-plan.json"));
    if apply_it {
        let plan: Vec<Verdict> = match std::fs::read_to_string(&plan_path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            Some(p) => p,
            None => {
                eprintln!("borg reach: no plan at {} (run `borg reach` first)", plan_path.display());
                return 1;
            }
        };
        return match apply(&home, &plan, &me(), crate::learn::now_ms()) {
            Ok(a) => {
                println!(
                    "{name}: {} kept personal (for you only, in {name}), {} limited to their repo, {} queued for the queen (`borg queen`)",
                    a.personal, a.scoped, a.asked
                );
                0
            }
            Err(e) => {
                eprintln!("borg reach: {e}");
                1
            }
        };
    }
    let todo = candidates(&home, &me());
    let jobs = flag("--jobs").and_then(|j| j.parse().ok()).unwrap_or(crate::runner::JOBS);
    eprintln!("borg reach: classifying {} lessons of {name} (one model call each, {jobs} at a time)", todo.len());
    let (plan, cost) = classify(&todo, jobs, &|p: &str, s: &serde_json::Value| crate::learn::ask_model(p, s));
    for (label, pick) in [("personal: for you only, kept in this collective", PERSONAL), ("repo: deliver only in its repo", REPO)] {
        let hits: Vec<&Verdict> = plan.iter().filter(|v| v.confident && v.reach == pick).collect();
        println!("\n{label} ({})", hits.len());
        for v in hits {
            println!("  {}  {}  ({})", v.id, crate::stats::excerpt(&v.question, PLAN_CHARS), v.why);
        }
    }
    let unsure: Vec<&Verdict> = plan.iter().filter(|v| !v.confident).collect();
    println!("\nunsure: a question for the queen ({})", unsure.len());
    for v in unsure {
        println!("  {}  [{}?]  {}", v.id, if v.reach.is_empty() { "team" } else { &v.reach }, crate::stats::excerpt(&v.question, PLAN_CHARS));
    }
    let team = plan.iter().filter(|v| v.confident && v.reach.is_empty()).count();
    println!("\nteam: unchanged ({team}); {} of {} classified, ${cost:.2}", plan.len(), todo.len());
    match std::fs::write(&plan_path, serde_json::to_string_pretty(&plan).unwrap_or_default()) {
        Ok(()) => {
            println!("plan: {} (apply with `borg reach --apply --plan {}`)", plan_path.display(), plan_path.display());
            0
        }
        Err(e) => {
            eprintln!("borg reach: cannot write {}: {e}", plan_path.display());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(reach: &str, repo: &str) -> Challenge {
        Challenge { id: "L".into(), reach: reach.into(), repo: repo.into(), ..Default::default() }
    }

    #[test]
    fn personal_is_never_delivered_and_repo_only_in_its_repo() {
        let here = ["app".to_string()];
        assert!(deliverable(&lesson("", "other"), &here), "team lessons go everywhere");
        assert!(!deliverable(&lesson(PERSONAL, "app"), &here));
        assert!(deliverable(&lesson(REPO, "App"), &here));
        assert!(!deliverable(&lesson(REPO, "other"), &here));
        assert!(!deliverable(&lesson(REPO, "app"), &[]), "unknown repo: a repo convention stays out");
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-reach-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn learned(id: &str, version: u32, reach: &str) -> Lesson {
        Lesson { id: id.into(), question: "never add a co-author trailer?".into(), consequence: "c".into(), version, reach: reach.into(), ..Default::default() }
    }

    fn by(l: Lesson, engineers: &[&str]) -> Lesson {
        Lesson { engineers: engineers.iter().map(|e| e.to_string()).collect(), ..l }
    }

    fn episode(id: &str, engineer: &str) -> Episode {
        Episode { id: id.into(), ms: 7, repo: "app".into(), engineer: engineer.into(), ..Default::default() }
    }

    fn latest(path: &Path, id: &str) -> Option<Lesson> {
        crate::learn::lessons(path).into_iter().find(|l| l.id == id)
    }

    #[test]
    fn a_personal_lesson_stays_in_its_store_and_only_its_own_engineer_withdraws_a_team_one() {
        let base = tmp("place");
        let (team, other) = (crate::collective::store(&base, "work"), crate::collective::store(&base, "private"));
        std::fs::create_dir_all(&team).unwrap();
        let (lessons, personal) = (team.join("lessons.jsonl"), personal_path(&team));
        // Brand new: kept in this store's personal file, nothing in lessons.jsonl, nothing elsewhere.
        let p = place(&learned("L1", 1, PERSONAL), &episode("e1", "ann@x"), false, None);
        write(&team, &p).unwrap();
        assert!(crate::learn::lessons(&lessons).is_empty());
        assert_eq!(latest(&personal, "L1").map(|l| (l.version, l.reach)), Some((1, PERSONAL.to_string())));
        assert!(!other.exists(), "never written to another collective");
        // Merged into a team lesson only ann is on: withdrawn there, kept in her personal file.
        let mine = by(learned("L2", 2, ""), &["ann@x"]);
        append_jsonl(&lessons, &mine).unwrap();
        let p = place(&by(learned("L2", 3, PERSONAL), &["ann@x"]), &episode("e2", "ann@x"), false, Some(&mine));
        write(&team, &p).unwrap();
        let t = latest(&lessons, "L2").unwrap();
        assert_eq!((t.version, t.reach.as_str(), t.question.as_str(), t.consequence.as_str()), (4, PERSONAL, PERSONAL_TOMBSTONE, ""));
        assert_eq!(latest(&personal, "L2").map(|l| l.version), Some(3));
        let c = Challenge { id: t.id.clone(), reach: t.reach.clone(), ..Default::default() };
        assert!(!deliverable(&c, &["app".into()]), "the tombstone is never delivered");
        // Merged into a team lesson bob is on too: the team lesson is untouched and not copied; ann's
        // episode becomes a personal lesson of its own.
        let shared = by(learned("L3", 1, ""), &["bob@x"]);
        append_jsonl(&lessons, &shared).unwrap();
        let before = std::fs::read_to_string(&lessons).unwrap();
        let p = place(&by(learned("L3", 2, PERSONAL), &["bob@x", "ann@x"]), &episode("e3", "ann@x"), false, Some(&shared));
        write(&team, &p).unwrap();
        assert_eq!(std::fs::read_to_string(&lessons).unwrap(), before, "the teammate's lesson is untouched");
        assert!(latest(&personal, "L3").is_none(), "no personal copy of it");
        let own = latest(&personal, "Le3").unwrap();
        assert_eq!((own.engineers, own.episodes, own.support), (vec!["ann@x".to_string()], vec!["e3".to_string()], 1));
        // An unknown engineer is nobody: never withdraws a lesson someone is on.
        assert!(matches!(place(&by(learned("L2", 3, PERSONAL), &["ann@x"]), &episode("e4", ""), false, Some(&mine)), Placed::Personal { withdraw: None, .. }));
        // Nobody recorded on a team lesson, or an imported one: not provably ann's, never withdrawn; her
        // episode becomes a personal lesson of its own.
        let nobody = learned("L4", 1, "");
        let imported = Lesson { source: "import".into(), ..by(learned("L5", 1, ""), &["ann@x"]) };
        for t in [&nobody, &imported] {
            let p = place(&Lesson { id: t.id.clone(), ..learned("x", 2, PERSONAL) }, &episode("e7", "ann@x"), false, Some(t));
            assert!(matches!(&p, Placed::Personal { lesson, withdraw: None } if lesson.id == "Le7"), "{}: {p:?}", t.id);
        }
        // A merge into a personal lesson stays personal, whatever the verdict; a team one stays here.
        assert!(
            matches!(place(&learned("L1", 2, ""), &episode("e5", "ann@x"), true, None), Placed::Personal { lesson, withdraw: None } if lesson.reach == PERSONAL)
        );
        assert!(matches!(place(&learned("L9", 1, ""), &episode("e6", "ann@x"), false, None), Placed::Here(l) if l.reach.is_empty()));
    }

    #[test]
    fn a_plan_makes_personal_scopes_and_asks_and_answers_apply_it() {
        let base = tmp("plan");
        let team = crate::collective::store(&base, "work");
        std::fs::create_dir_all(&team).unwrap();
        for (id, q) in [("La", "no co-author trailer?"), ("Lb", "stage branch refspec?"), ("Lc", "lock order?"), ("Ld", "report style?")] {
            let l = Lesson { id: id.into(), question: q.into(), repo: "app".into(), version: 1, engineers: vec!["ann@x".into()], ..Default::default() };
            append_jsonl(&team.join("lessons.jsonl"), &l).unwrap();
        }
        let fake = |p: &str, _: &serde_json::Value| {
            let p = p.split("Question: ").nth(1).unwrap_or_default();
            let (reach, confident) = if p.contains("trailer") {
                ("personal", true)
            } else if p.contains("refspec") {
                ("repo", true)
            } else if p.contains("report") {
                ("personal", false)
            } else {
                ("team", true)
            };
            Some((serde_json::json!({"reach": reach, "confident": confident, "why": "w"}), Some(0.01)))
        };
        let (plan, cost) = classify(&candidates(&team, "ann@x"), 2, &fake);
        assert_eq!((plan.len(), (cost * 100.0).round()), (4, 4.0));
        let done = apply(&team, &plan, "ann@x", 1000).unwrap();
        assert_eq!(done, Applied { personal: 1, scoped: 1, asked: 1 });
        let lessons = team.join("lessons.jsonl");
        assert_eq!(latest(&lessons, "La").unwrap().reach, PERSONAL);
        assert_eq!(latest(&personal_path(&team), "La").unwrap().question, "no co-author trailer?", "kept in this store");
        assert_eq!(latest(&lessons, "Lb").unwrap().reach, REPO);
        assert_eq!(latest(&lessons, "Lc").unwrap().version, 1, "team: untouched");
        assert!(candidates(&team, "ann@x").iter().all(|l| l.id == "Lc" || l.id == "Ld"), "personal and scoped ones are no candidates any more");
        // The unsure one became a reach question; answering "personal" (option 3) applies it.
        let q = crate::queen::entries(&team).into_iter().find(|e| e.kind == crate::queen::Kind::Reach).unwrap();
        assert_eq!(q.options.len(), 3);
        let l = latest(&lessons, "Ld").unwrap();
        apply_one(&team, &l, verdict(&q.options[2]), "ann@x").unwrap();
        assert_eq!(latest(&lessons, "Ld").unwrap().reach, PERSONAL);
        assert_eq!((verdict(&q.options[0]), verdict(&q.options[1])), ("", REPO));
    }

    #[test]
    fn reach_never_considers_a_lesson_another_engineer_is_on() {
        let base = tmp("teammate");
        let team = crate::collective::store(&base, "work");
        let lessons = team.join("lessons.jsonl");
        for (id, engineers) in [("Lmine", vec!["ann@x"]), ("Lbob", vec!["bob@x"]), ("Lboth", vec!["ann@x", "bob@x"]), ("Lnone", vec![])] {
            append_jsonl(&lessons, &by(learned(id, 1, ""), &engineers)).unwrap();
        }
        let ids = |me: &str| {
            let mut v = candidates(&team, me).into_iter().map(|l| l.id).collect::<Vec<_>>();
            v.sort();
            v
        };
        assert_eq!(ids("ann@x"), ["Lmine"], "a lesson nobody is recorded on is not provably ann's");
        assert!(ids("").is_empty(), "an unknown engineer is nobody");
        // A plan naming the teammate's lessons (stale or hand-edited) touches none of them.
        let plan: Vec<Verdict> = ["Lbob", "Lboth"]
            .iter()
            .map(|id| Verdict { id: id.to_string(), repo: String::new(), question: String::new(), reach: PERSONAL.into(), confident: true, why: String::new() })
            .collect();
        let before = std::fs::read_to_string(&lessons).unwrap();
        assert_eq!(apply(&team, &plan, "ann@x", 1).unwrap(), Applied::default());
        assert_eq!(std::fs::read_to_string(&lessons).unwrap(), before);
        assert!(!personal_path(&team).exists());
        // A queen answer on one is refused, nothing written: personal or one repo alike.
        for to in [PERSONAL, REPO] {
            for id in ["Lboth", "Lnone"] {
                assert!(apply_one(&team, &latest(&lessons, id).unwrap(), to, "ann@x").is_err(), "{id} {to}");
            }
        }
        assert_eq!(std::fs::read_to_string(&lessons).unwrap(), before);
    }

    /// The guard: a model that never answers is asked `runner::MAX_FAILURES` times, not once per lesson.
    #[test]
    fn a_dead_model_is_asked_only_until_the_failure_guard() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let lessons: Vec<Lesson> = (0..10).map(|i| Lesson { id: format!("L{i}"), ..Lesson::default() }).collect();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let dead = move |_: &str, _: &serde_json::Value| {
            counted.fetch_add(1, Ordering::SeqCst);
            None
        };
        assert!(classify(&lessons, 1, &dead).0.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), crate::runner::MAX_FAILURES);
        // One answer first: the model is there, a later failure stops nothing.
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let flaky = move |_: &str, _: &serde_json::Value| {
            (counted.fetch_add(1, Ordering::SeqCst) == 0).then(|| (serde_json::json!({"reach": "team", "confident": true, "why": "w"}), None))
        };
        assert_eq!(classify(&lessons, 1, &flaky).0.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), lessons.len(), "every lesson asked");
    }

    #[test]
    fn merges_never_widen() {
        assert_eq!(narrower("", REPO), REPO);
        assert_eq!(narrower(PERSONAL, ""), PERSONAL);
        assert_eq!(narrower(REPO, PERSONAL), PERSONAL);
        assert_eq!(narrower("", ""), "");
    }
}
