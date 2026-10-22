//! Who should hear a lesson: its reach.
//!
//! A correction can be an engineering concern anyone on the code should hear (the team, the default),
//! a convention of one repo (`repo`: delivered only there), or one engineer's taste in how an agent
//! works with them (`personal`: commit style, attribution, branching, tone). Personal lessons belong in
//! that engineer's private collective, never in a team one: they would steer everyone's drones. In a
//! team store a personal lesson is only a tombstone, a newer version that withdraws it; the full text
//! lives in the private store, which never syncs.

use std::path::{Path, PathBuf};

use crate::conscience::Challenge;
use crate::learn::{Lesson, append_jsonl};

/// For a lesson of one repo only.
pub const REPO: &str = "repo";
/// For a tombstone in a team store: withdrawn to an engineer's private collective.
pub const PERSONAL: &str = "personal";

/// Whether `c` may be delivered in a session working in `repos` (systems.rs `work_repos`).
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

/// The question a tombstone carries in the team store instead of the withdrawn lesson.
pub const WITHDRAWN: &str = "(personal preference, moved to a private collective)";

/// Where a lesson ended up.
#[derive(Debug, Clone)]
pub enum Placed {
    /// Write this to the store it was learned in, as usual (a personal lesson learned in the private
    /// collective itself loses its marker: everything there is personal).
    Here(Box<Lesson>),
    /// Personal: copied to this private store (and withdrawn from the team store if it was there).
    Private(PathBuf),
    /// Personal, but this machine has no private collective to keep it: dropped, never shared.
    Dropped,
}

/// Decides where `l`, learned in the store `home`, belongs, and writes it there if that is not `home`.
pub fn place(home: &Path, l: &Lesson) -> Placed {
    if l.reach != PERSONAL {
        return Placed::Here(Box::new(l.clone()));
    }
    place_with(home, l, crate::collective::load().ok().flatten().as_ref())
}

/// `place` under an explicit config.
pub fn place_with(home: &Path, l: &Lesson, cfg: Option<&crate::collective::Config>) -> Placed {
    if l.reach != PERSONAL {
        return Placed::Here(Box::new(l.clone()));
    }
    match crate::collective::personal_store(cfg, home) {
        // Learned in the private collective itself: everything there is personal already.
        Some(p) if p == home => Placed::Here(Box::new(Lesson { reach: String::new(), ..l.clone() })),
        Some(p) => {
            if move_to(home, &p, l).is_ok() {
                Placed::Private(p)
            } else {
                Placed::Dropped
            }
        }
        None => {
            // A merge into a team lesson that turned personal still withdraws it.
            if l.version > 1 {
                let _ = append_jsonl(&home.join("lessons.jsonl"), &tombstone(l));
            }
            Placed::Dropped
        }
    }
}

/// Copies `l` to the private store `private` (as team-free text, `reach` empty) and, if the team store
/// `team` holds an earlier version, withdraws it there with a tombstone.
pub fn move_to(team: &Path, private: &Path, l: &Lesson) -> std::io::Result<()> {
    let held = crate::learn::lessons(&private.join("lessons.jsonl")).into_iter().find(|x| x.id == l.id).map_or(0, |x| x.version);
    let copy = Lesson { version: held + 1, reach: String::new(), ..l.clone() };
    std::fs::create_dir_all(private)?;
    append_jsonl(&private.join("lessons.jsonl"), &copy)?;
    if crate::learn::lessons(&team.join("lessons.jsonl")).iter().any(|x| x.id == l.id) {
        append_jsonl(&team.join("lessons.jsonl"), &tombstone(l))?;
    }
    Ok(())
}

/// The newer version of `l` that withdraws it from a team store: no text left to deliver or sync on.
pub fn tombstone(l: &Lesson) -> Lesson {
    Lesson {
        version: l.version.max(1) + 1,
        question: WITHDRAWN.into(),
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
pub const PERSONAL_OPTION: &str = "personal: move it to my private collective";

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

/// Gives `l` (the latest version in the store `home`) the reach `to`: moved out for personal, a new
/// version for repo, nothing for the team.
pub fn apply_one(home: &Path, l: &Lesson, to: &str, cfg: Option<&crate::collective::Config>) -> Result<(), String> {
    match to {
        PERSONAL => {
            let private = crate::collective::personal_store(cfg, home)
                .filter(|p| p != home)
                .ok_or("no private collective to move it to: add one with \"share\": \"local\" (or name it in \"personal\")")?;
            move_to(home, &private, l).map_err(|e| e.to_string())
        }
        REPO => append_jsonl(&home.join("lessons.jsonl"), &Lesson { version: l.version + 1, reach: REPO.into(), ..l.clone() }).map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

/// Applies the answer to a reach question (queen.rs `answer`).
pub fn answer(store: &Path, e: &crate::queen::Entry, text: &str) -> Result<(), String> {
    let id = e.evidence.first().and_then(|x| x.strip_prefix("lesson ")).ok_or("this question names no lesson")?;
    let l = crate::learn::lessons(&store.join("lessons.jsonl")).into_iter().find(|l| l.id == id).ok_or(format!("lesson {id} is not in this collective"))?;
    apply_one(store, &l, verdict(text), crate::collective::load().ok().flatten().as_ref())
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

/// The lessons a cleanup classifies: learned from sessions or queen answers, still for the team.
/// Imports are left alone: written-down team knowledge, scoped by system already.
pub fn candidates(home: &Path) -> Vec<Lesson> {
    crate::learn::lessons(&home.join("lessons.jsonl")).into_iter().filter(|l| l.reach.is_empty() && (l.source.is_empty() || l.source == "queen")).collect()
}

/// A structured model call: the answer and its cost (learn.rs `ask_model`).
pub type Ask = dyn Fn(&str, &serde_json::Value) -> Option<(serde_json::Value, Option<f64>)> + Sync;

/// Classifies `lessons` with one model call each (`jobs` at a time). Returns the verdicts and the cost.
pub fn classify(lessons: &[Lesson], jobs: usize, ask: &Ask) -> (Vec<Verdict>, f64) {
    let out = std::sync::Mutex::new((Vec::new(), 0.0));
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(l) = lessons.get(i) else { break };
                    let prompt = format!("{PROMPT}\n\nLesson (learned in repo {}):\nQuestion: {}\nConsequence: {}\n", l.repo, l.question, l.consequence);
                    let Some((v, cost)) = ask(&prompt, &schema()) else { continue };
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
    pub moved: usize,
    pub scoped: usize,
    pub asked: usize,
}

/// Applies a plan to the store `home`: confident personal verdicts move, confident repo verdicts are
/// scoped, unsure ones become queen questions; team verdicts change nothing.
pub fn apply(home: &Path, plan: &[Verdict], cfg: Option<&crate::collective::Config>, now: i64) -> Result<Applied, String> {
    let lessons = crate::learn::lessons(&home.join("lessons.jsonl"));
    let mut done = Applied::default();
    for v in plan {
        let Some(l) = lessons.iter().find(|l| l.id == v.id && l.reach.is_empty()) else { continue };
        match (v.confident, v.reach.as_str()) {
            (true, PERSONAL) => {
                apply_one(home, l, PERSONAL, cfg)?;
                done.moved += 1;
            }
            (true, REPO) => {
                apply_one(home, l, REPO, cfg)?;
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
                    question: format!("Who should hear this lesson? \"{}\"", crate::stats::excerpt(&l.question, 300)),
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
        return match apply(&home, &plan, cfg.as_ref(), crate::learn::now_ms()) {
            Ok(a) => {
                println!(
                    "{name}: {} moved to the private collective, {} limited to their repo, {} queued for the queen (`borg queen`)",
                    a.moved, a.scoped, a.asked
                );
                0
            }
            Err(e) => {
                eprintln!("borg reach: {e}");
                1
            }
        };
    }
    let todo = candidates(&home);
    let jobs = flag("--jobs").and_then(|j| j.parse().ok()).unwrap_or(4);
    eprintln!("borg reach: classifying {} lessons of {name} (one model call each, {jobs} at a time)", todo.len());
    let (plan, cost) = classify(&todo, jobs, &|p: &str, s: &serde_json::Value| crate::learn::ask_model(p, s));
    for (label, pick) in [("personal: move to the private collective", PERSONAL), ("repo: deliver only in its repo", REPO)] {
        let hits: Vec<&Verdict> = plan.iter().filter(|v| v.confident && v.reach == pick).collect();
        println!("\n{label} ({})", hits.len());
        for v in hits {
            println!("  {}  {}  ({})", v.id, crate::stats::excerpt(&v.question, 110), v.why);
        }
    }
    let unsure: Vec<&Verdict> = plan.iter().filter(|v| !v.confident).collect();
    println!("\nunsure: a question for the queen ({})", unsure.len());
    for v in unsure {
        println!("  {}  [{}?]  {}", v.id, if v.reach.is_empty() { "team" } else { &v.reach }, crate::stats::excerpt(&v.question, 110));
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

    fn config(local: &[&str]) -> crate::collective::Config {
        let mut collectives = vec![serde_json::json!({"name": "work", "paths": ["/w"], "share": "team"})];
        collectives.extend(local.iter().map(|n| serde_json::json!({"name": n, "paths": [format!("/{n}")], "share": "local"})));
        serde_json::from_value(serde_json::json!({"collectives": collectives})).unwrap()
    }

    fn learned(id: &str, version: u32, reach: &str) -> Lesson {
        Lesson { id: id.into(), question: "never add a co-author trailer?".into(), consequence: "c".into(), version, reach: reach.into(), ..Default::default() }
    }

    #[test]
    fn a_personal_lesson_goes_to_the_private_store_and_is_withdrawn_from_the_team() {
        let base = tmp("move");
        let cfg = config(&["private"]);
        let (team, private) = (crate::collective::store(&base, "work"), crate::collective::store(&base, "private"));
        std::fs::create_dir_all(&team).unwrap();
        // Brand new: copied to private, nothing written to the team store.
        assert!(matches!(place_with(&team, &learned("L1", 1, PERSONAL), Some(&cfg)), Placed::Private(p) if p == private));
        assert!(crate::learn::lessons(&team.join("lessons.jsonl")).is_empty());
        let copy = &crate::learn::lessons(&private.join("lessons.jsonl"))[0];
        assert_eq!((copy.id.as_str(), copy.reach.as_str(), copy.version), ("L1", "", 1));
        // A team lesson that turns out personal: the team store keeps only a newer tombstone.
        append_jsonl(&team.join("lessons.jsonl"), &learned("L2", 3, "")).unwrap();
        move_to(&team, &private, &learned("L2", 3, "")).unwrap();
        let t = crate::learn::lessons(&team.join("lessons.jsonl")).into_iter().find(|l| l.id == "L2").unwrap();
        assert_eq!((t.version, t.reach.as_str(), t.question.as_str(), t.consequence.as_str()), (4, PERSONAL, WITHDRAWN, ""));
        let c = Challenge { id: t.id.clone(), reach: t.reach.clone(), ..Default::default() };
        assert!(!deliverable(&c, &["app".into()]), "the tombstone is never delivered");
    }

    #[test]
    fn without_a_private_collective_personal_lessons_are_dropped_not_shared() {
        let base = tmp("drop");
        let team = crate::collective::store(&base, "work");
        std::fs::create_dir_all(&team).unwrap();
        assert!(matches!(place_with(&team, &learned("L1", 1, PERSONAL), Some(&config(&[]))), Placed::Dropped));
        assert!(matches!(place_with(&team, &learned("L1", 1, PERSONAL), Some(&config(&["a", "b"]))), Placed::Dropped), "two local ones and no key: none");
        assert!(crate::learn::lessons(&team.join("lessons.jsonl")).is_empty());
        assert!(matches!(place_with(&team, &learned("L2", 1, ""), Some(&config(&[]))), Placed::Here(l) if l.reach.is_empty()));
    }

    #[test]
    fn learned_inside_the_private_collective_it_stays_there() {
        let base = tmp("here");
        let private = crate::collective::store(&base, "private");
        let kept = place_with(&private, &learned("L1", 1, PERSONAL), Some(&config(&["private"])));
        assert!(matches!(kept, Placed::Here(l) if l.reach.is_empty()), "kept, and deliverable there");
    }

    #[test]
    fn a_plan_moves_scopes_and_asks_and_answers_apply_the_move() {
        let base = tmp("plan");
        let cfg = config(&["private"]);
        let (team, private) = (crate::collective::store(&base, "work"), crate::collective::store(&base, "private"));
        std::fs::create_dir_all(&team).unwrap();
        for (id, q) in [("La", "no co-author trailer?"), ("Lb", "stage branch refspec?"), ("Lc", "lock order?"), ("Ld", "report style?")] {
            append_jsonl(&team.join("lessons.jsonl"), &Lesson { id: id.into(), question: q.into(), repo: "app".into(), version: 1, ..Default::default() })
                .unwrap();
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
        let (plan, cost) = classify(&candidates(&team), 2, &fake);
        assert_eq!((plan.len(), (cost * 100.0).round()), (4, 4.0));
        let done = apply(&team, &plan, Some(&cfg), 1000).unwrap();
        assert_eq!(done, Applied { moved: 1, scoped: 1, asked: 1 });
        let latest = |h: &Path, id: &str| crate::learn::lessons(&h.join("lessons.jsonl")).into_iter().find(|l| l.id == id);
        assert_eq!(latest(&team, "La").unwrap().reach, PERSONAL);
        assert_eq!(latest(&private, "La").unwrap().question, "no co-author trailer?");
        assert_eq!(latest(&team, "Lb").unwrap().reach, REPO);
        assert_eq!(latest(&team, "Lc").unwrap().version, 1, "team: untouched");
        assert!(candidates(&team).iter().all(|l| l.id == "Lc" || l.id == "Ld"), "moved and scoped ones are no candidates any more");
        // The unsure one became a reach question; answering "personal" (option 3) moves it.
        let q = crate::queen::entries(&team).into_iter().find(|e| e.kind == crate::queen::Kind::Reach).unwrap();
        assert_eq!(q.options.len(), 3);
        let l = latest(&team, "Ld").unwrap();
        apply_one(&team, &l, verdict(&q.options[2]), Some(&cfg)).unwrap();
        assert_eq!(latest(&team, "Ld").unwrap().reach, PERSONAL);
        assert_eq!((verdict(&q.options[0]), verdict(&q.options[1])), ("", REPO));
    }

    #[test]
    fn merges_never_widen() {
        assert_eq!(narrower("", REPO), REPO);
        assert_eq!(narrower(PERSONAL, ""), PERSONAL);
        assert_eq!(narrower(REPO, PERSONAL), PERSONAL);
        assert_eq!(narrower("", ""), "");
    }
}
