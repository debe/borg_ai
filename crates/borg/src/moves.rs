//! Crossing a collective boundary, only ever on the engineer's word: `borg move` and `borg correct`.
//!
//! Collectives never mix on their own (collective.rs): a session reads and writes only the store its
//! repo routes to. But a lesson can land in the wrong collective, a repo can change collectives, and
//! one session can correct things that belong to two collectives. Each of those is an explicit act
//! the engineer triggers, through the harness skill or a terminal:
//!
//! - **`borg move <lesson-id> --to <collective>`** appends the lesson's latest version to the target
//!   store as its next version and withdraws it from the source with a tombstone ([`MOVED`]) that keeps
//!   its episodes, so the source's own "done" rule never learns them again. The target notes the move
//!   in its local [`PLACED_FILE`] (never synced: the source's name never reaches the target's team), so
//!   `borg doctor` never calls the lesson misplaced. A lesson another engineer is on stays in a team
//!   store as it is (copied, never hidden from their team); a local store's copy is always withdrawn,
//!   since no team reads it. A lesson in the source's `personal.jsonl` moves to the target's;
//!   `--personal` puts a lesson there. Moving it again changes nothing.
//! - **`borg move <repo-path> --to <collective>`** moves the episodes of that repo the stores hold but
//!   have not learned into a lesson (with their not-a-lesson marks) to the target store, deduped there,
//!   when the repo routes to the target now. Without `--from` only stores that hold them because the
//!   repo routes there (learn.rs `held`) give them up. Episodes already learned stay with their lessons;
//!   `borg move <id>` moves a lesson.
//! - **`borg correct --collective <name> <words>`** records a correction of the current session as an
//!   episode in that collective's store (`via` [`VIA`]), learned there at the session's end like any
//!   other. One command per collective the correction concerns, the session's own included: the raw
//!   words the hooks captured in the session's collective are set aside, so each collective learns only
//!   what was said for it. Into another collective goes only what the engineer said: no task, proposal
//!   or email of the session's collective.
//!
//! Both are dry runs where they change stores (`--apply` writes), and take each store's locks (in path
//! order, never one twice). `borg doctor` lists what may need a move ([`doctor`]): evidence held in a
//! store its repo no longer routes to, lessons that name another collective's repo, and the repair of
//! lessons a borg before 0.7.4 withdrew to a private collective. It only shows the commands; it never
//! moves anything.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::collective::{self, Aliases, Config, Route, Share};
use crate::harness::Level;
use crate::learn::{self, Episode, Lesson};
use crate::reach;

/// A store's lessons only its engineer hears, beside `lessons.jsonl`; never synced (reach.rs).
const PERSONAL_FILE: &str = reach::PERSONAL_FILE;
/// A store's team lessons.
const LESSONS_FILE: &str = "lessons.jsonl";
/// A store's episodes, and the ones learning judged no lesson.
const EPISODES_FILE: &str = "episodes.jsonl";
const SKIPPED_FILE: &str = "episodes-not-lessons.txt";

/// The question of the tombstone a move leaves in the source store: no text left to deliver or sync.
pub const MOVED: &str = reach::MOVED;
/// A store's record of the lessons `borg move` placed in it (`{"id", "from", "ms"}` lines): placed by
/// the engineer, so never "misplaced?". Local: borg-mesh `SYNCED` does not list it.
pub const PLACED_FILE: &str = "placed.jsonl";
/// `Episode::via` of a correction `borg correct` recorded for a named collective: placed by the
/// engineer, so no routing check ever holds it back or calls it misplaced (learn.rs `CORRECT`).
pub const VIA: &str = learn::CORRECT;
/// The most characters of a correction `borg correct` records. Guard: bounds the episode as the hook
/// bounds a captured correction (learn.rs `EXCERPT_CHARS`): derived, the same bound.
pub const CORRECTION_CHARS: usize = learn::EXCERPT_CHARS;
/// How often a repo move re-reads a source's episodes when an append landed while it rewrote them.
/// Guard: a store written without pause never stalls the move; a second run moves what is left.
const REWRITE_TRIES: usize = 3;

/// One collective's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub name: String,
    pub home: PathBuf,
    pub share: Share,
}

/// Every collective's store under the root store `base` (without a config, "default" at `base`).
#[must_use]
pub fn stores(cfg: Option<&Config>, base: &Path) -> Vec<Store> {
    match cfg {
        None => vec![Store { name: "default".into(), home: base.to_path_buf(), share: Share::Team }],
        Some(c) => c.collectives.iter().map(|e| Store { name: e.name.clone(), home: collective::store(base, &e.name), share: e.share }).collect(),
    }
}

/// The store of the collective `name`.
///
/// # Errors
/// An unknown or off collective, with the list of collectives.
pub fn named(cfg: Option<&Config>, base: &Path, name: &str) -> Result<Store, String> {
    let all = stores(cfg, base);
    all.iter().find(|s| s.name == name).cloned().ok_or_else(|| {
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        let what = if name == "off" { "off is no collective".to_string() } else { format!("no collective {name:?}") };
        format!("{what}; the collectives are: {} (borg collective, borg status)", names.join(", "))
    })
}

/// The store `cwd` routes to.
fn routed(cfg: Option<&Config>, base: &Path, cwd: &str) -> Result<Store, String> {
    match collective::route(cfg, cwd, base) {
        Route::Collective { name, home, share } => Ok(Store { name, home, share }),
        Route::Off => Err(format!("{cwd} belongs to no collective: name the source with --from <collective>")),
    }
}

/// Whether every engineer on `l` is `me` (and there is one): only then may a team store lose it
/// (reach.rs `only`, the one rule for both).
fn mine(l: &Lesson, me: &str, aliases: &Aliases) -> bool {
    reach::only_with(&l.engineers, me, aliases)
}

/// The engineers on `l` other than `me`.
fn others(l: &Lesson, me: &str, aliases: &Aliases) -> Vec<String> {
    l.engineers.iter().filter(|e| me.is_empty() || !aliases.same(e, me)).map(|e| aliases.display(e)).collect()
}

/// Who a lesson that is not only `me`'s belongs to, for a line of the doctor.
fn whose(l: &Lesson, me: &str, aliases: &Aliases) -> String {
    match others(l, me, aliases) {
        o if o.is_empty() => "no recorded engineer".into(),
        o => o.join(", "),
    }
}

/// Whether `l`, the latest version of its id in `file`, withdraws it: in `lessons.jsonl` a tombstone
/// (reach.rs `PERSONAL` marks one); in [`PERSONAL_FILE`], where every lesson is personal, the tombstone
/// a move leaves there ([`MOVED`]).
fn withdrawn(file: &str, l: &Lesson) -> bool {
    match file {
        PERSONAL_FILE => l.question == MOVED,
        _ => l.reach == reach::PERSONAL,
    }
}

/// The latest version of `id` in `file`, whatever it is.
fn latest(file: &Path, id: &str) -> Option<Lesson> {
    learn::lessons(file).into_iter().find(|l| l.id == id)
}

/// Runs `f` holding the locks of `files` (live.rs `hold`), taken in path order so two moves in
/// opposite directions never wait on each other; each file once.
fn with_locks<T>(files: &[PathBuf], f: impl FnOnce() -> T) -> T {
    let mut sorted: Vec<&PathBuf> = files.iter().collect();
    sorted.sort();
    sorted.dedup();
    let held: Vec<_> = sorted.into_iter().map(|p| crate::live::hold(p)).collect();
    let out = f();
    drop(held);
    out
}

/// The ids of the lessons `borg move` placed in the store `home`, with the collective each came from.
fn placed(home: &Path) -> HashSet<(String, String)> {
    let line = |v: Value| Some((v["id"].as_str()?.to_string(), v["from"].as_str().unwrap_or_default().to_string()));
    read_lines(&home.join(PLACED_FILE)).lines().filter_map(|l| serde_json::from_str(l).ok()).filter_map(line).collect()
}

/// What moving a lesson does.
#[derive(Debug, Clone)]
pub struct LessonMove {
    pub id: String,
    pub from: Store,
    pub to: Store,
    /// The file it is read from and the one it is appended to (`lessons.jsonl` or [`PERSONAL_FILE`]).
    pub from_file: &'static str,
    pub to_file: &'static str,
    /// The source's latest version, and the line the target gets (its next version).
    pub source_version: u32,
    pub copy: Lesson,
    /// The target holds the lesson as the source has it already (a move applied before): no copy.
    pub already: bool,
    /// The source's tombstone; None when another engineer is on the lesson in a team store, or the
    /// source holds one already.
    pub tombstone: Option<Lesson>,
    /// The other engineers on it.
    pub others: Vec<String>,
}

impl LessonMove {
    /// What the move changes, one line each.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let version = self.copy.version;
        let reach = match self.copy.reach.as_str() {
            _ if self.to_file == PERSONAL_FILE => "personal: heard by this engineer only",
            "" => "team",
            r => r,
        };
        let mut out = vec![format!("move lesson {} (v{} in {}'s {}) to {}", self.id, self.source_version, self.from.name, self.from_file, self.to.name)];
        out.push(match self.already {
            true => format!("  {}: holds v{version} of it in {} already, as {} has it: nothing appended", self.to.name, self.to_file, self.from.name),
            false => format!("  {}: append v{version} to {}, reach {reach}", self.to.name, self.to_file),
        });
        out.push(match &self.tombstone {
            Some(t) => {
                format!("  {}: append v{} tombstone {MOVED:?} to {}, keeping its {} episode(s)", self.from.name, t.version, self.from_file, t.episodes.len())
            }
            None if self.already && self.others.is_empty() => format!("  {}: withdrawn there already", self.from.name),
            None => {
                format!("  {}: kept as it is: another engineer is on it ({}); copied, never hidden from their team", self.from.name, self.others.join(", "))
            }
        });
        out
    }
}

/// Plans moving lesson `id` from `from` to `to` (`personal`: into the target's [`PERSONAL_FILE`]);
/// `me` is this engineer.
///
/// # Errors
/// The same store, or no live lesson `id` in the source.
pub fn plan_lesson(from: &Store, to: &Store, id: &str, personal: bool, me: &str, aliases: &Aliases) -> Result<LessonMove, String> {
    if from.home == to.home {
        return Err(format!("lesson {id} is in {} already", to.name));
    }
    // The version this engineer has: a personal one over the team's (learn.rs `store_lessons`).
    let live = |home: &Path, file: &'static str| latest(&home.join(file), id).filter(|l| !withdrawn(file, l)).map(|l| (file, l));
    let Some((from_file, src)) = live(&from.home, PERSONAL_FILE).or_else(|| live(&from.home, LESSONS_FILE)) else {
        let moved = [PERSONAL_FILE, LESSONS_FILE].iter().find_map(|f| latest(&from.home.join(f), id).filter(|l| l.question == MOVED).map(|l| (*f, l)));
        let there = live(&to.home, PERSONAL_FILE).or_else(|| live(&to.home, LESSONS_FILE));
        // Moved before: the source holds a move's tombstone and the target the lesson. Nothing to do.
        if let (Some((from_file, t)), Some((to_file, copy))) = (moved, there) {
            return Ok(LessonMove {
                id: id.to_string(),
                from: from.clone(),
                to: to.clone(),
                from_file,
                to_file,
                source_version: t.version,
                copy,
                already: true,
                tombstone: None,
                others: Vec::new(),
            });
        }
        let gone = latest(&from.home.join(LESSONS_FILE), id).is_some();
        return Err(match gone {
            true => format!("lesson {id} is withdrawn in {} (a tombstone): name the collective holding its text with --from", from.name),
            false => format!("no lesson {id} in {} ({}): name its collective with --from", from.name, from.home.display()),
        });
    };
    let to_file = if personal || from_file == PERSONAL_FILE { PERSONAL_FILE } else { LESSONS_FILE };
    let target = latest(&to.home.join(to_file), id);
    let held = target.as_ref().map_or(0, |l| l.version);
    // Applied before (a teammate's lesson stays in its source): the target's live version says what the
    // source does, so a second move appends nothing.
    let already = target
        .filter(|l| !withdrawn(to_file, l))
        .is_some_and(|t| (&t.question, &t.consequence, &t.situations, &t.episodes) == (&src.question, &src.consequence, &src.situations, &src.episodes));
    let reach = match to_file {
        PERSONAL_FILE => reach::PERSONAL.to_string(),
        _ if src.reach == reach::PERSONAL => String::new(),
        _ => src.reach.clone(),
    };
    let copy = match already {
        true => latest(&to.home.join(to_file), id).unwrap_or_default(),
        false => Lesson { version: held.max(src.version) + 1, reach, ..src.clone() },
    };
    // A local store and personal lessons are no team's: only a team store keeps a teammate's lesson.
    let tombstone = (from.share == Share::Local || from_file == PERSONAL_FILE || mine(&src, me, aliases)).then(|| Lesson {
        version: src.version + 1,
        question: MOVED.into(),
        situations: Vec::new(),
        consequence: String::new(),
        history: Vec::new(),
        facets: Vec::new(),
        reach: reach::PERSONAL.into(),
        ..src.clone()
    });
    Ok(LessonMove {
        id: id.to_string(),
        from: from.clone(),
        to: to.clone(),
        from_file,
        to_file,
        source_version: src.version,
        copy,
        already,
        tombstone,
        others: others(&src, me, aliases),
    })
}

/// Moves lesson `id` (`plan_lesson`), with `apply` writing it under both stores' `lessons.jsonl` locks,
/// planned again under them; the dry run writes nothing.
///
/// # Errors
/// What `plan_lesson` refuses, or a write that failed.
pub fn move_lesson(from: &Store, to: &Store, id: &str, personal: bool, me: &str, aliases: &Aliases, apply: bool) -> Result<LessonMove, String> {
    if !apply || from.home == to.home {
        return plan_lesson(from, to, id, personal, me, aliases);
    }
    let fail = |e: std::io::Error| format!("cannot write: {e}");
    // The locks live beside the files: a store not written yet gets its directory first.
    for home in [&from.home, &to.home] {
        std::fs::create_dir_all(home).map_err(fail)?;
    }
    with_locks(&[from.home.join(LESSONS_FILE), to.home.join(LESSONS_FILE)], || {
        let m = plan_lesson(from, to, id, personal, me, aliases)?;
        if !m.already {
            learn::append_jsonl(&to.home.join(m.to_file), &m.copy).map_err(fail)?;
        }
        if !placed(&to.home).contains(&(id.to_string(), from.name.clone())) {
            let note = serde_json::json!({"id": id, "from": from.name, "ms": learn::now_ms()});
            learn::append_jsonl(&to.home.join(PLACED_FILE), &note).map_err(fail)?;
        }
        if let Some(t) = &m.tombstone {
            learn::append_jsonl(&from.home.join(m.from_file), t).map_err(fail)?;
        }
        Ok(m)
    })
}

/// What moving a repo's evidence takes from one store.
#[derive(Debug, Clone)]
pub struct RepoFrom {
    pub store: Store,
    /// The episodes it gives the target, and which of them learning judged no lesson there.
    pub moved: Vec<Episode>,
    pub skipped: Vec<String>,
    /// Of `moved`, the ones the target holds already: removed here, not appended twice.
    pub held: usize,
    /// The repo's episodes that stay: learned into a lesson here, or recorded here by `borg correct`.
    pub stay: usize,
    /// Where the source's `episodes.jsonl` was copied before the move rewrote it (applied only).
    pub backup: Option<PathBuf>,
}

/// What moving a repo's evidence does.
#[derive(Debug, Clone)]
pub struct RepoMove {
    pub repo: String,
    pub to: Store,
    pub from: Vec<RepoFrom>,
}

impl RepoMove {
    /// What the move changes, one line each.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let mut out = vec![format!("move the evidence of {} to {}", self.repo, self.to.name)];
        for f in &self.from {
            if !f.moved.is_empty() {
                out.push(format!(
                    "  {}: {} episode(s) to {}'s {EPISODES_FILE} ({} held there already, {} judged no lesson)",
                    f.store.name,
                    f.moved.len(),
                    self.to.name,
                    f.held,
                    f.skipped.len()
                ));
            }
            if let Some(b) = &f.backup {
                out.push(format!("  {}: its episodes before the move are kept in {}", f.store.name, b.display()));
            }
            if f.stay > 0 {
                out.push(format!("  {}: {} episode(s) stay with their lessons or were placed there by borg correct", f.store.name, f.stay));
            }
        }
        if self.from.iter().all(|f| f.moved.is_empty()) {
            out.push(format!("  nothing to move: no store but {} holds waiting episodes of {}", self.to.name, self.repo));
        }
        out
    }
}

/// The ids of the episodes `home`'s lessons (team and personal) were learned from.
fn learned(home: &Path) -> HashSet<String> {
    learn::learned_episodes(home)
}

fn read_lines(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn skipped_ids(home: &Path) -> HashSet<String> {
    read_lines(&home.join(SKIPPED_FILE)).lines().map(str::to_string).collect()
}

/// The repo's episodes `from` gives `to` (pure on the stores' files).
fn plan_repo_from(from: &Store, to: &Store, repo: &str) -> RepoFrom {
    let done = learned(&from.home);
    let skipped = skipped_ids(&from.home);
    let known = learn::Known::read(&to.home);
    let mut f = RepoFrom { store: from.clone(), moved: Vec::new(), skipped: Vec::new(), held: 0, stay: 0, backup: None };
    let episodes = read_lines(&from.home.join(EPISODES_FILE));
    for e in episodes.lines().filter_map(|l| serde_json::from_str::<Episode>(l).ok()).filter(|e| e.repo == repo) {
        if done.contains(&e.id) || e.via == VIA {
            f.stay += 1;
            continue;
        }
        if known.holds(&e) {
            f.held += 1;
        }
        if skipped.contains(&e.id) {
            f.skipped.push(e.id.clone());
        }
        f.moved.push(e);
    }
    f
}

/// Writes one source's part of a repo move: the target gets what it does not hold, then the source
/// loses the moved episodes (a rename of its rewritten file, done again if an append landed meanwhile),
/// its file as it was kept beside it first. Returns that copy.
fn apply_repo_from(f: &RepoFrom, to: &Store) -> std::io::Result<PathBuf> {
    let mut known = learn::Known::read(&to.home);
    let target_skipped = skipped_ids(&to.home);
    std::fs::create_dir_all(&to.home)?;
    for e in &f.moved {
        if !known.holds(e) {
            learn::append_jsonl(&to.home.join(EPISODES_FILE), e)?;
            known.add(e);
        }
    }
    for id in f.skipped.iter().filter(|id| !target_skipped.contains(*id)) {
        learn::append_line(&to.home.join(SKIPPED_FILE), id)?;
    }
    let gone: HashSet<&str> = f.moved.iter().map(|e| e.id.as_str()).collect();
    let path = f.store.home.join(EPISODES_FILE);
    let tmp = f.store.home.join(format!("{EPISODES_FILE}.moving"));
    let backup = f.store.home.join(format!("{EPISODES_FILE}.before-move-{}", learn::now_ms()));
    for _ in 0..REWRITE_TRIES {
        let before = read_lines(&path);
        let kept: String =
            before.lines().filter(|l| serde_json::from_str::<Episode>(l).map_or(true, |e| !gone.contains(e.id.as_str()))).map(|l| format!("{l}\n")).collect();
        std::fs::write(&tmp, kept)?;
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) == before.len() as u64 {
            std::fs::write(&backup, &before)?;
            return std::fs::rename(&tmp, &path).map(|()| backup);
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(std::io::Error::other(format!("{} kept changing; the episodes are in {} too, run the move again", path.display(), to.name)))
}

/// Moves the waiting episodes of `repo` (a repo name, as episodes carry it) from `sources` to `to`, one
/// source at a time under both stores' `episodes.jsonl` and `lessons.jsonl` locks (so a running
/// `borg learn` on either finishes first and never learns an episode as it leaves); the dry run writes
/// nothing.
///
/// # Errors
/// A write that failed.
pub fn move_repo(sources: &[Store], to: &Store, repo: &str, apply: bool) -> Result<RepoMove, String> {
    let mut out = RepoMove { repo: repo.to_string(), to: to.clone(), from: Vec::new() };
    for s in sources.iter().filter(|s| s.home != to.home) {
        let f = match apply {
            false => plan_repo_from(s, to, repo),
            true => {
                let locks: Vec<PathBuf> = [&s.home, &to.home].iter().flat_map(|h| [h.join(EPISODES_FILE), h.join(LESSONS_FILE)]).collect();
                std::fs::create_dir_all(&to.home).map_err(|e| format!("{}: {e}", to.name))?;
                with_locks(&locks, || {
                    let mut f = plan_repo_from(s, to, repo);
                    if !f.moved.is_empty() {
                        f.backup = Some(apply_repo_from(&f, to)?);
                    }
                    Ok::<_, std::io::Error>(f)
                })
                .map_err(|e| format!("{}: {e}", s.name))?
            }
        };
        if !f.moved.is_empty() || f.stay > 0 {
            out.from.push(f);
        }
    }
    Ok(out)
}

/// The stores a `borg move <repo-path> --to <to>` takes `repo`'s evidence from when no `--from` names
/// one: those that hold it waiting only because the repo routes to `to` now (learn.rs `held`). A store
/// that may hold another repo of that name, or this one by right, is never a source unasked.
#[must_use]
pub fn repo_sources(cfg: Option<&Config>, base: &Path, repo: &str, to: &Store) -> Vec<Store> {
    let routes_to = |r: &Route| matches!(r, Route::Collective { home, .. } if *home == to.home);
    stores(cfg, base).into_iter().filter(|s| learn::held(&s.home, cfg).iter().any(|h| h.repo == repo && routes_to(&h.route))).collect()
}

/// One correction `borg correct` records.
#[derive(Debug, Clone, Default)]
pub struct Said {
    pub session: String,
    /// The repo it is about, when the engineer names one; empty otherwise (the session's repo belongs
    /// to another collective).
    pub repo: String,
    pub engineer: String,
    /// The session's task and the agent's last proposal, as far as they are known.
    pub task: String,
    pub agent: String,
    pub words: String,
    pub ms: i64,
}

/// Records `s` as a pushback episode in the store `home`, once (learn.rs `record_new_in`, which also
/// counts it); None when the store holds it already.
///
/// # Errors
/// When the episode cannot be appended.
pub fn record_correction(home: &Path, s: &Said) -> std::io::Result<Option<Episode>> {
    let e = Episode {
        id: format!("{VIA}-{}-{}", s.session.chars().take(8).collect::<String>(), s.ms),
        ms: s.ms,
        repo: s.repo.clone(),
        session: s.session.clone(),
        task: s.task.clone(),
        agent: s.agent.clone(),
        rejected_tool: None,
        correction: s.words.chars().take(CORRECTION_CHARS).collect(),
        engineer: s.engineer.clone(),
        via: VIA.into(),
        ..Episode::default()
    };
    Ok(learn::record_new_in(home, vec![e], |_| {})?.pop())
}

// ---------------------------------------------------------------- doctor

/// `s` as one shell word.
fn q(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./~:@+=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `borg doctor`'s checks of where lessons and evidence sit (see the module doc); `me` is this
/// engineer's git email. Nothing without a config: one store holds everything.
#[must_use]
pub fn doctor(cfg: Option<&Config>, base: &Path, me: &str) -> Vec<(Level, String)> {
    let Some(c) = cfg else { return Vec::new() };
    doctor_with(c, base, me, &c.aliases())
}

/// [`doctor`] under explicit aliases.
#[must_use]
pub fn doctor_with(cfg: &Config, base: &Path, me: &str, aliases: &Aliases) -> Vec<(Level, String)> {
    let stores = stores(Some(cfg), base);
    let mut out = Vec::new();
    let home_of = |name: &str| stores.iter().find(|s| s.name == name);
    let mut routes: BTreeMap<String, Option<Route>> = BTreeMap::new();
    let mut route_of = |repo: &str| routes.entry(repo.to_string()).or_insert_with(|| collective::route_repo(cfg, repo, base)).clone();
    let path_of = |repo: &str| collective::places_of(cfg, repo).into_iter().next().map_or_else(|| repo.to_string(), |p| p.display().to_string());
    // Held evidence: what `borg learn` holds in a store because its repo routes elsewhere now (learn.rs `held`).
    for s in &stores {
        for h in learn::held(&s.home, Some(cfg)) {
            let n = h.episodes.len();
            out.push(match &h.route {
                Route::Collective { name: to, .. } => (
                    Level::Warn,
                    format!(
                        "evidence: {n} waiting episode(s) of {} are in {}, but {} routes to {to} now: borg move {} --from {} --to {}",
                        h.repo,
                        s.name,
                        h.repo,
                        q(&path_of(&h.repo)),
                        q(&s.name),
                        q(to)
                    ),
                ),
                Route::Off => (
                    Level::Warn,
                    format!(
                        "evidence: {n} waiting episode(s) of {} are in {}, but {} routes off now; they wait unlearned (route it to a collective in the config, then borg doctor names the move)",
                        h.repo, s.name, h.repo
                    ),
                ),
            });
        }
    }
    // Whose lesson a line is: unknowable without this engineer's email. Then nothing is classified,
    // for a wrong guess would suggest publishing a personal lesson to a team.
    let known = !me.trim().is_empty();
    if !known {
        out.push((
            Level::Warn,
            "collectives: cannot tell whose lessons these are (no git user.email here): run borg doctor in a repo with git user.email set".into(),
        ));
    }
    // The repair of withdrawn lessons: a team tombstone of a borg before 0.7.4 (reach.rs `WITHDRAWN`)
    // whose text a local store holds goes back where it was learned: for the team when another engineer
    // is on it and the local copy learned nothing of its own, into that collective's personal lessons
    // when it is this engineer's. A tombstone no local store holds the text of has nothing to restore
    // here (a teammate's machine sees every synced one).
    let local: Vec<&Store> = stores.iter().filter(|s| s.share == Share::Local).collect();
    let live = |s: &Store, id: &str| [LESSONS_FILE, PERSONAL_FILE].iter().find_map(|f| latest(&s.home.join(f), id).filter(|l| !withdrawn(f, l)));
    let mut repaired: HashSet<(String, String)> = HashSet::new();
    for y in stores.iter().filter(|s| s.share == Share::Team && known) {
        for t in learn::lessons(&y.home.join(LESSONS_FILE)).into_iter().filter(|l| l.question == reach::WITHDRAWN) {
            if live(y, &t.id).is_some() {
                continue; // kept in this collective's personal lessons
            }
            let Some((p, held)) = local.iter().find_map(|p| live(p, &t.id).map(|l| (*p, l))) else { continue };
            repaired.insert((p.name.clone(), t.id.clone()));
            // The holder's version counts: what the local store learned into it is its engineer's.
            let both: Vec<String> = t.engineers.iter().chain(&held.engineers).cloned().collect();
            let own = reach::only_with(&both, me, aliases);
            let learned_there = held.episodes.iter().any(|e| !t.episodes.contains(e));
            let item = match (own, learned_there) {
                (true, _) => format!(
                    "repair: your lesson {} was withdrawn from {} as personal; keep it personal there: borg move {} --from {} --to {} --personal",
                    t.id,
                    y.name,
                    t.id,
                    q(&p.name),
                    q(&y.name)
                ),
                (false, false) => format!(
                    "repair: lesson {} of {} was withdrawn from {} as personal; restore it for the team: borg move {} --from {} --to {}",
                    t.id,
                    whose(&t, me, aliases),
                    y.name,
                    t.id,
                    q(&p.name),
                    q(&y.name)
                ),
                (false, true) => format!(
                    "repair: lesson {} of {} was withdrawn from {} as personal, and its copy in {} learned more there since: no move restores it as the team had it; keep it personal there with borg move {} --from {} --to {} --personal, or teach the team again",
                    t.id,
                    whose(&Lesson { engineers: both, ..t.clone() }, me, aliases),
                    y.name,
                    p.name,
                    t.id,
                    q(&p.name),
                    q(&y.name)
                ),
            };
            out.push((Level::Warn, item));
        }
    }
    // The safety net: a team lesson whose repo routes to another collective (shown, never acted on).
    // In a local store it came from that collective (a personal placement), so it goes back as personal.
    // Never one the engineer placed: moved there (`PLACED_FILE`), copied from there to where its repo
    // routes, or learned from a `borg correct` alone.
    let placed_in: BTreeMap<&str, HashSet<(String, String)>> = stores.iter().map(|s| (s.name.as_str(), placed(&s.home))).collect();
    for s in &stores {
        let here = &placed_in[s.name.as_str()];
        let named: HashSet<String> = read_lines(&s.home.join(EPISODES_FILE))
            .lines()
            .filter_map(|l| serde_json::from_str::<Episode>(l).ok())
            .filter(|e| e.via == VIA)
            .map(|e| e.id)
            .collect();
        for l in learn::lessons(&s.home.join(LESSONS_FILE)).into_iter().filter(|l| !withdrawn(LESSONS_FILE, l)) {
            let by_name = !l.episodes.is_empty() && l.episodes.iter().all(|e| named.contains(e));
            if repaired.contains(&(s.name.clone(), l.id.clone())) || here.iter().any(|(id, _)| *id == l.id) || by_name {
                continue;
            }
            let Some(Route::Collective { name: to, .. }) = route_of(&l.repo) else { continue };
            let Some(y) = home_of(&to).filter(|y| y.name != s.name) else { continue };
            if placed_in[y.name.as_str()].contains(&(l.id.clone(), s.name.clone())) {
                continue; // copied there already
            }
            let item = match s.share == Share::Local && y.share == Share::Team {
                true if !known => continue,
                true if mine(&l, me, aliases) => format!(
                    "repair: lesson {} in {} was learned in {to} ({}): keep it personal there: borg move {} --from {} --to {} --personal",
                    l.id,
                    s.name,
                    l.repo,
                    l.id,
                    q(&s.name),
                    q(&to)
                ),
                _ => format!(
                    "misplaced? lesson {} in {} names {}, which routes to {to}: borg move {} --from {} --to {}",
                    l.id,
                    s.name,
                    l.repo,
                    l.id,
                    q(&s.name),
                    q(&to)
                ),
            };
            out.push((Level::Warn, item));
        }
    }
    if out.is_empty() {
        out.push((Level::Ok, "collectives: every lesson and waiting episode sits in the collective its repo routes to".into()));
    } else if out.iter().any(|(_, t)| t.contains("borg move ")) {
        out.push((Level::Ok, "collectives: the moves above are suggestions; borg move shows each as a dry run and changes nothing without --apply".into()));
    }
    out
}

// ---------------------------------------------------------------- CLI

/// `borg move`'s flags (completions.rs).
pub const MOVE_FLAGS: &[&str] = &["--to", "--from", "--dir", "--personal", "--apply"];
/// `borg correct`'s flags (completions.rs).
pub const CORRECT_FLAGS: &[&str] = &["--collective", "--session", "--repo", "--dir"];

const MOVE_USAGE: &str = "usage: borg move <lesson-id|repo-path> --to <collective> [--from <collective>] [--dir DIR] [--personal] [--apply]
  a lesson: appended to the target as its next version, withdrawn from the source unless another engineer is on it
  a repo path: its waiting episodes move to the target store
  a dry run without --apply";

const CORRECT_USAGE: &str = "usage: borg correct --collective <name> [--session ID] [--repo NAME] [--dir DIR] <words…>
  records a correction of this session in that collective's store; learned there like any other";

/// A command line: each flag's value, the flags without one, and the other words.
type Parsed<'a> = (BTreeMap<String, &'a str>, BTreeSet<String>, Vec<&'a str>);

/// The value after each flag of `with_value`, the `bare` flags given, and the arguments that are
/// neither flags nor their values.
fn parse<'a>(args: &[&'a str], with_value: &[&str], bare: &[&str]) -> Result<Parsed<'a>, String> {
    let (mut values, mut flags, mut words) = (BTreeMap::new(), BTreeSet::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if with_value.contains(&a) {
            let v = args.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
            values.insert(a.to_string(), *v);
            i += 2;
            continue;
        }
        if bare.contains(&a) {
            flags.insert(a.to_string());
        } else if a.starts_with("--") {
            return Err(format!("unknown flag {a}"));
        } else {
            words.push(a);
        }
        i += 1;
    }
    Ok((values, flags, words))
}

/// The directory a command runs for: `--dir`, else the current one, absolute (routing compares path
/// text, as hooks' cwd is absolute).
fn cwd_of(dir: Option<&str>) -> String {
    let d = dir.map_or_else(|| std::env::current_dir().unwrap_or_default(), PathBuf::from);
    std::path::absolute(&d).unwrap_or(d).display().to_string()
}

/// Whether a `borg move` argument names a repo (a path) rather than a lesson id.
fn is_path(arg: &str) -> bool {
    arg.contains('/') || arg.starts_with('.') || arg.starts_with('~') || Path::new(arg).is_dir()
}

/// `borg move …`; returns the exit code.
pub fn main_move(args: &[&str]) -> i32 {
    let (values, flags, words) = match parse(args, &["--to", "--from", "--dir"], &["--personal", "--apply"]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("borg move: {e}\n{MOVE_USAGE}");
            return 2;
        }
    };
    let ([what], Some(to)) = (words.as_slice(), values.get("--to")) else {
        eprintln!("{MOVE_USAGE}");
        return 2;
    };
    let cfg = match collective::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("borg move: invalid collectives config: {e}");
            return 1;
        }
    };
    let base = collective::root_of(&learn::home());
    let apply = flags.contains("--apply");
    let cwd = cwd_of(values.get("--dir").copied());
    let resolve = |name: &str| named(cfg.as_ref(), &base, name);
    let target = match resolve(to) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("borg move: {e}");
            return 2;
        }
    };
    let result = if is_path(what) {
        let path = std::path::absolute(collective::expand(what)).unwrap_or_else(|_| PathBuf::from(what));
        let repo = crate::hook::repo_of(&path.display().to_string());
        // Evidence moves only where its repo routes now: anywhere else learning would hold it again.
        let sources = match collective::route(cfg.as_ref(), &path.display().to_string(), &base) {
            Route::Collective { home, .. } if home == target.home => match values.get("--from") {
                Some(f) => resolve(f).map(|s| vec![s]),
                None => Ok(repo_sources(cfg.as_ref(), &base, &repo, &target)),
            },
            Route::Collective { name, .. } => Err(format!(
                "{} routes to {name}, not {to}: its evidence would wait unlearned in {to}; route it to {to} in the collectives config first (borg collective)",
                path.display()
            )),
            Route::Off => Err(format!("{} routes to no collective: route it to {to} in the collectives config first (borg collective)", path.display())),
        };
        sources.and_then(|s| move_repo(&s, &target, &repo, apply)).map(|m| m.describe())
    } else {
        let source = match values.get("--from") {
            Some(f) => resolve(f),
            None => routed(cfg.as_ref(), &base, &cwd),
        };
        let aliases = cfg.as_ref().map(Config::aliases).unwrap_or_default();
        let me = learn::engineer(&cwd);
        source.and_then(|s| move_lesson(&s, &target, what, flags.contains("--personal"), &me, &aliases, apply)).map(|m| m.describe())
    };
    match result {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
            println!("{}", if apply { "moved" } else { "dry run: nothing changed; run it again with --apply" });
            0
        }
        Err(e) => {
            eprintln!("borg move: {e}");
            1
        }
    }
}

/// The session's task as the hooks kept it (hook.rs, `<state>/<session>.task`), else `fallback`.
fn task_of(session: &str, fallback: &str) -> String {
    std::fs::read_to_string(crate::hook::state_file(session).with_extension("task"))
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// The agent's last proposal in `session` on `harness`, its last `learn::PROPOSAL_CHARS` characters,
/// where the harness writes a transcript borg can read; empty otherwise.
fn proposal_of(harness: &str, session: &str) -> String {
    let Some(h) = crate::harness::by_name(harness) else { return String::new() };
    if h.caps().transcript != crate::harness::Transcript::Claude {
        return String::new();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let Some(log) = h.session_log(&home, session, &Value::Null) else { return String::new() };
    let t = crate::harness::claude::last_assistant_full(&log);
    let n = t.chars().count();
    t.chars().skip(n.saturating_sub(learn::PROPOSAL_CHARS)).collect()
}

/// This engineer as the collective of `target` knows them: the git email in the repo `repo` there
/// (collective.rs `places_of`), else in the collective's first path on disk (git's global email outside
/// a repo); empty when none is known. Never the email of a repo of another collective.
fn engineer_in(cfg: Option<&Config>, base: &Path, target: &Store, repo: &str) -> String {
    let Some(c) = cfg else { return String::new() };
    let routes_there = |p: &Path| matches!(collective::route(Some(c), &p.display().to_string(), base), Route::Collective { home, .. } if home == target.home);
    let paths = c.collectives.iter().filter(|x| x.name == target.name).flat_map(|x| x.paths.iter().map(|p| collective::expand(p)));
    collective::places_of(c, repo)
        .into_iter()
        .chain(paths)
        .find(|p| p.is_dir() && routes_there(p))
        .map_or_else(String::new, |p| learn::engineer(&p.display().to_string()))
}

/// Sets aside, in the session's own store `here`, the engineer's latest prompt the hooks recorded as a
/// correction (hook.rs `record`: `#borg-episode <id>` in the session's state file): a `borg correct`
/// restates it for each collective it concerns, so its raw words are not learned in this collective
/// besides. Only the latest prompt's episode (newer than the session's task, which the hook rewrites on
/// every other prompt), and only while unlearned. Marked as settled (`episodes-not-lessons.txt`) under
/// the store's learning lock; the episode stays, so transcript capture never records it again.
/// Returns its id.
///
/// # Errors
/// When the mark cannot be written.
pub fn set_aside(here: &Path, session: &str) -> std::io::Result<Option<String>> {
    let state = crate::hook::state_file(session);
    let last = read_lines(&state).lines().rev().find_map(|l| l.strip_prefix("#borg-episode ").map(|id| id.trim().to_string()));
    let Some(id) = last else { return Ok(None) };
    let ms_of = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let task_ms = std::fs::metadata(state.with_extension("task")).and_then(|m| m.modified()).map_or(0, ms_of);
    let episodes = read_lines(&here.join(EPISODES_FILE));
    let Some(e) = episodes.lines().filter_map(|l| serde_json::from_str::<Episode>(l).ok()).find(|e| e.id == id && e.session == session) else {
        return Ok(None);
    };
    if e.ms < task_ms {
        return Ok(None); // a later prompt was no correction: this episode is not what is restated
    }
    crate::live::locked(&here.join(LESSONS_FILE), || {
        if learned(here).contains(&id) || skipped_ids(here).contains(&id) {
            return Ok(None);
        }
        learn::append_line(&here.join(SKIPPED_FILE), &id).map(|()| Some(id.clone()))
    })
}

/// The session's list of stores `borg correct` recorded into (`<state>/<session>.correct`), which its
/// end-of-session `borg learn` learns too (main.rs `learn_cmd`).
fn corrected_file(session: &str) -> PathBuf {
    crate::hook::state_file(session).with_extension("correct")
}

/// The stores `borg correct` recorded a correction of `session` into.
#[must_use]
pub fn corrected_homes(session: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = read_lines(&corrected_file(session)).lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect();
    out.dedup();
    out
}

/// `msg::caller`'s error, with the flag of `borg correct` that names the session.
fn session_hint(e: &str) -> String {
    e.replace("pass --from <session>", "name it with --session ID")
}

/// `borg correct …`; returns the exit code.
pub fn main_correct(args: &[&str]) -> i32 {
    let (values, _, words) = match parse(args, &["--collective", "--session", "--repo", "--dir"], &[]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("borg correct: {e}\n{CORRECT_USAGE}");
            return 2;
        }
    };
    let said = words.join(" ").trim().to_string();
    let Some(name) = values.get("--collective").filter(|_| !said.is_empty()) else {
        eprintln!("{CORRECT_USAGE}");
        return 2;
    };
    let cfg = match collective::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("borg correct: invalid collectives config: {e}");
            return 1;
        }
    };
    let base = collective::root_of(&learn::home());
    let target = match named(cfg.as_ref(), &base, name) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("borg correct: {e}");
            return 2;
        }
    };
    if !learn::learning() {
        println!("learning is off (BORG_LEARN=off): nothing recorded");
        return 0;
    }
    let cwd = cwd_of(values.get("--dir").copied());
    // The session lives where its own repo routes: its presence, task and drones are there.
    let here = routed(cfg.as_ref(), &base, &cwd).ok();
    let now = learn::now_ms();
    let drones = here.as_ref().map(|h| crate::top::drones(&h.home, false, now)).unwrap_or_default();
    let session = match values.get("--session") {
        Some(s) => Some((*s).to_string()),
        None => match crate::msg::caller(&drones) {
            Ok(c) => c.map(|(s, _)| s),
            Err(e) => {
                eprintln!("borg correct: {}", session_hint(&e));
                return 1;
            }
        },
    };
    let Some(session) = session else {
        eprintln!("borg correct: no session calls this; name it with --session ID");
        return 1;
    };
    let repo = values.get("--repo").map_or_else(String::new, |r| (*r).to_string());
    // Into another collective goes only what the engineer said: the session's task, the agent's
    // proposal and the email of its repo belong to the session's collective.
    let own = here.as_ref().is_some_and(|h| h.home == target.home);
    let drone = drones.iter().find(|d| d.node.is_empty() && d.p.session == session).filter(|_| own);
    let s = Said {
        session: session.clone(),
        engineer: if own { learn::engineer(&cwd) } else { engineer_in(cfg.as_ref(), &base, &target, &repo) },
        repo,
        task: if own { task_of(&session, drone.map_or("", |d| d.p.prompt.as_str())) } else { String::new() },
        agent: drone.map_or_else(String::new, |d| proposal_of(&d.p.harness, &session)),
        words: said,
        ms: now,
    };
    let recorded = match record_correction(&target.home, &s) {
        Ok(Some(e)) => format!("recorded in {}: episode {}; learned there when this session ends", target.name, e.id),
        Ok(None) => format!("{} holds this correction of the session already: nothing recorded", target.name),
        Err(e) => {
            eprintln!("borg correct: cannot record it in {}: {e}", target.name);
            return 1;
        }
    };
    println!("{recorded}");
    let path = corrected_file(&session);
    if !corrected_homes(&session).contains(&target.home) {
        let _ = learn::append_line(&path, &target.home.display().to_string());
    }
    if let Some(h) = &here {
        match set_aside(&h.home, &session) {
            Ok(Some(id)) if own => println!("set aside in {}: the words as the hooks captured them (episode {id}); this restatement stands for them", h.name),
            Ok(Some(id)) => println!(
                "set aside in {}: the words as the hooks captured them (episode {id}); what belongs to {} needs its own borg correct --collective {}",
                h.name, h.name, h.name
            ),
            Ok(None) => {}
            Err(e) => eprintln!("borg correct: cannot set aside the captured words in {}: {e}", h.name),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-moves-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn store(root: &Path, name: &str, share: Share) -> Store {
        let home = root.join(name);
        std::fs::create_dir_all(&home).unwrap();
        Store { name: name.into(), home, share }
    }

    fn lesson(id: &str, version: u32, engineers: &[&str], episodes: &[&str]) -> Lesson {
        Lesson {
            id: id.into(),
            repo: "app".into(),
            question: format!("Does {id} hold?"),
            consequence: "it breaks".into(),
            version,
            engineers: engineers.iter().map(|e| (*e).to_string()).collect(),
            episodes: episodes.iter().map(|e| (*e).to_string()).collect(),
            ..Lesson::default()
        }
    }

    fn write(path: &Path, lines: &[Value]) {
        std::fs::write(path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    }

    fn lines(path: &Path) -> Vec<Value> {
        read_lines(path).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    fn episode(id: &str, repo: &str) -> Episode {
        Episode { id: id.into(), ms: 1, repo: repo.into(), session: format!("s-{id}"), correction: format!("words of {id}"), ..Episode::default() }
    }

    const ME: &str = "me@x";

    #[test]
    fn a_dry_run_changes_nothing() {
        let root = temp("dry");
        let (a, b) = (store(&root, "corp", Share::Team), store(&root, "oss", Share::Team));
        learn::append_jsonl(&a.home.join(LESSONS_FILE), &lesson("L1", 2, &[ME], &["e1"])).unwrap();
        let before = read_lines(&a.home.join(LESSONS_FILE));
        let m = move_lesson(&a, &b, "L1", false, ME, &Aliases::default(), false).unwrap();
        assert!(m.describe().iter().any(|l| l.contains("append v3 to lessons.jsonl")), "{:?}", m.describe());
        assert_eq!(read_lines(&a.home.join(LESSONS_FILE)), before);
        assert!(!b.home.join(LESSONS_FILE).exists(), "the target is untouched");
    }

    #[test]
    fn applying_moves_an_own_lesson_and_leaves_a_tombstone_that_keeps_its_episodes() {
        let root = temp("own");
        let (a, b) = (store(&root, "corp", Share::Team), store(&root, "oss", Share::Team));
        learn::append_jsonl(&a.home.join(LESSONS_FILE), &lesson("L1", 2, &["Me@X"], &["e1", "e2"])).unwrap();
        move_lesson(&a, &b, "L1", false, ME, &Aliases::default(), true).unwrap();
        let got = lines(&b.home.join(LESSONS_FILE));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["version"], 3, "the target's next version, above the source's");
        assert!(got[0].get("moved_from").is_none(), "the source's name never goes into the target's synced lessons");
        assert_eq!(placed(&b.home), [("L1".to_string(), "corp".to_string())].into_iter().collect(), "noted in the target's local placed file");
        assert_eq!(got[0]["question"], "Does L1 hold?");
        let src = latest(&a.home.join(LESSONS_FILE), "L1").unwrap();
        assert_eq!((src.version, src.question.as_str(), src.reach.as_str()), (3, MOVED, reach::PERSONAL));
        assert_eq!(learned(&a.home), ["e1", "e2"].iter().map(|s| (*s).to_string()).collect(), "the source's done rule still sees the episodes");
        // Moving it again changes nothing, and says so.
        let (before_a, before_b) = (read_lines(&a.home.join(LESSONS_FILE)), read_lines(&b.home.join(LESSONS_FILE)));
        let again = move_lesson(&a, &b, "L1", false, ME, &Aliases::default(), true).unwrap();
        assert!(again.already && again.describe().iter().any(|l| l.contains("already")), "{:?}", again.describe());
        assert_eq!((read_lines(&a.home.join(LESSONS_FILE)), read_lines(&b.home.join(LESSONS_FILE))), (before_a, before_b));
    }

    #[test]
    fn a_move_into_a_store_not_written_yet_holds_its_lock() {
        let root = temp("fresh");
        let a = store(&root, "corp", Share::Team);
        let b = Store { name: "oss".into(), home: root.join("oss"), share: Share::Team };
        learn::append_jsonl(&a.home.join(LESSONS_FILE), &lesson("L1", 1, &[ME], &["e1"])).unwrap();
        move_lesson(&a, &b, "L1", false, ME, &Aliases::default(), true).unwrap();
        assert!(b.home.join("lessons.jsonl.lock").exists(), "the target's lock was taken, not skipped for a missing directory");
    }

    #[test]
    fn a_teammates_lesson_is_copied_and_stays_in_its_team_store() {
        let root = temp("teammate");
        let (a, b) = (store(&root, "corp", Share::Team), store(&root, "oss", Share::Team));
        learn::append_jsonl(&a.home.join(LESSONS_FILE), &lesson("L2", 1, &[ME, "bo@x"], &["e1"])).unwrap();
        let m = move_lesson(&a, &b, "L2", false, ME, &Aliases::default(), true).unwrap();
        assert!(m.tombstone.is_none());
        assert!(m.describe().iter().any(|l| l.contains("another engineer is on it (bo@x)")), "{:?}", m.describe());
        assert_eq!(latest(&a.home.join(LESSONS_FILE), "L2").unwrap().question, "Does L2 hold?", "never hidden from the team");
        assert_eq!(lines(&b.home.join(LESSONS_FILE))[0]["version"], 2);
        // A second apply copies nothing more.
        assert!(move_lesson(&a, &b, "L2", false, ME, &Aliases::default(), true).unwrap().already);
        assert_eq!(lines(&b.home.join(LESSONS_FILE)).len(), 1, "one copy, however often it is applied");
        // Nobody known: not provably own, so not withdrawn either.
        learn::append_jsonl(&a.home.join(LESSONS_FILE), &lesson("L3", 1, &[], &["e3"])).unwrap();
        assert!(plan_lesson(&a, &b, "L3", false, ME, &Aliases::default()).unwrap().tombstone.is_none());
    }

    #[test]
    fn a_local_stores_copy_is_withdrawn_and_restored_above_a_team_tombstone() {
        let root = temp("restore");
        let (team, private) = (store(&root, "corp", Share::Team), store(&root, "mine", Share::Local));
        let l = lesson("L4", 1, &["bo@x"], &["e1"]);
        learn::append_jsonl(&team.home.join(LESSONS_FILE), &l).unwrap();
        learn::append_jsonl(&team.home.join(LESSONS_FILE), &reach::tombstone(&l)).unwrap();
        learn::append_jsonl(&private.home.join(LESSONS_FILE), &Lesson { version: 1, ..l.clone() }).unwrap();
        move_lesson(&private, &team, "L4", false, ME, &Aliases::default(), true).unwrap();
        let back = latest(&team.home.join(LESSONS_FILE), "L4").unwrap();
        assert_eq!((back.version, back.question.as_str(), back.reach.as_str()), (3, "Does L4 hold?", ""), "team reach, above the tombstone");
        assert!(withdrawn(LESSONS_FILE, &latest(&private.home.join(LESSONS_FILE), "L4").unwrap()), "the private copy is withdrawn");
    }

    #[test]
    fn personal_moves_into_the_targets_personal_lessons() {
        let root = temp("personal");
        let (team, private) = (store(&root, "corp", Share::Team), store(&root, "mine", Share::Local));
        learn::append_jsonl(&private.home.join(LESSONS_FILE), &lesson("L5", 1, &[ME], &["e1"])).unwrap();
        move_lesson(&private, &team, "L5", true, ME, &Aliases::default(), true).unwrap();
        assert!(!team.home.join(LESSONS_FILE).exists(), "nothing in the team's synced lessons");
        let got = lines(&team.home.join(PERSONAL_FILE));
        assert_eq!((got[0]["id"].as_str(), got[0]["reach"].as_str()), (Some("L5"), Some(reach::PERSONAL)));
        assert!(placed(&team.home).contains(&("L5".to_string(), "mine".to_string())));
        // A personal lesson (reach.rs: personal reach in its file) moves within its file kind, and its
        // source keeps a tombstone even with a teammate's email on it: no team reads that file.
        let other = store(&root, "oss", Share::Team);
        learn::append_jsonl(&team.home.join(PERSONAL_FILE), &Lesson { reach: reach::PERSONAL.into(), ..lesson("L6", 1, &["bo@x"], &["e6"]) }).unwrap();
        move_lesson(&team, &other, "L5", false, ME, &Aliases::default(), true).unwrap();
        move_lesson(&team, &other, "L6", false, ME, &Aliases::default(), true).unwrap();
        assert!(other.home.join(PERSONAL_FILE).exists() && !other.home.join(LESSONS_FILE).exists());
        for id in ["L5", "L6"] {
            let t = latest(&team.home.join(PERSONAL_FILE), id).unwrap();
            assert!(withdrawn(PERSONAL_FILE, &t), "{id}: {t:?}");
            assert!(!withdrawn(PERSONAL_FILE, &latest(&other.home.join(PERSONAL_FILE), id).unwrap()), "{id} is live in the target");
        }
    }

    #[test]
    fn a_repos_waiting_evidence_moves_and_dedupes_at_the_target() {
        let root = temp("repo");
        let (a, b, c) = (store(&root, "mine", Share::Local), store(&root, "oss", Share::Team), store(&root, "corp", Share::Team));
        let eps = [episode("w1", "lib"), episode("w2", "lib"), episode("d1", "lib"), episode("o1", "app"), episode("s1", "lib")];
        write(&a.home.join(EPISODES_FILE), &eps.iter().map(|e| serde_json::to_value(e).unwrap()).collect::<Vec<_>>());
        learn::append_jsonl(&a.home.join(LESSONS_FILE), &lesson("Ld1", 1, &[ME], &["d1"])).unwrap();
        learn::append_line(&a.home.join(SKIPPED_FILE), "s1").unwrap();
        learn::append_jsonl(&b.home.join(EPISODES_FILE), &episode("w2", "lib")).unwrap();
        learn::append_jsonl(&c.home.join(EPISODES_FILE), &Episode { via: VIA.into(), ..episode("c1", "lib") }).unwrap();
        let sources = [a.clone(), b.clone(), c.clone()];
        let dry = move_repo(&sources, &b, "lib", false).unwrap();
        assert_eq!(dry.from[0].moved.len(), 3);
        assert_eq!(lines(&a.home.join(EPISODES_FILE)).len(), 5, "the dry run changes nothing");
        let m = move_repo(&sources, &b, "lib", true).unwrap();
        assert_eq!((m.from[0].held, m.from[0].stay, m.from[0].skipped.clone()), (1, 1, vec!["s1".to_string()]));
        let ids = |s: &Store| lines(&s.home.join(EPISODES_FILE)).iter().map(|v| v["id"].as_str().unwrap_or_default().to_string()).collect::<Vec<_>>();
        assert_eq!(ids(&b), ["w2", "w1", "s1"], "deduped by id at the target");
        assert_eq!(ids(&a), ["d1", "o1"], "learned and other repos' episodes stay");
        assert_eq!(ids(&c), ["c1"], "an episode placed by borg correct stays");
        assert!(skipped_ids(&b.home).contains("s1"), "its not-a-lesson mark moves along");
        assert!(move_repo(&sources, &b, "lib", true).unwrap().from.iter().all(|f| f.moved.is_empty()), "a second run moves nothing");
    }

    #[test]
    fn a_correction_lands_only_in_the_named_store_and_is_held_once() {
        let root = temp("correct");
        let (a, b) = (store(&root, "corp", Share::Team), store(&root, "oss", Share::Team));
        let s = Said { session: "sess-1234".into(), engineer: ME.into(), task: "t".into(), words: "use the upstream parser".into(), ms: 7, ..Said::default() };
        let e = record_correction(&b.home, &s).unwrap().unwrap();
        assert_eq!((e.via.as_str(), e.rejected_tool.is_none(), e.kind.as_str()), (VIA, true, ""), "a pushback, marked as placed");
        assert_eq!(lines(&b.home.join(EPISODES_FILE)).len(), 1);
        assert!(!a.home.join(EPISODES_FILE).exists());
        assert!(record_correction(&b.home, &Said { ms: 8, ..s }).unwrap().is_none(), "the same words of the session once");
        let cfg: Config = serde_json::from_str(r#"{"default": "off", "collectives": [{"name": "corp", "paths": []}, {"name": "oss", "paths": []}]}"#).unwrap();
        let err = named(Some(&cfg), &root, "nope").unwrap_err();
        assert!(err.contains("corp, oss"), "{err}");
        assert!(named(Some(&cfg), &root, "off").unwrap_err().starts_with("off is no collective"));
    }

    #[test]
    fn doctor_lists_held_evidence_misplaced_lessons_and_the_repair_with_commands() {
        let root = temp("doctor");
        let base = root.join("base");
        for d in ["w/app", "o/lib"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let cfg: Config = serde_json::from_value(serde_json::json!({"default": "off", "collectives": [
            {"name": "corp", "paths": [root.join("w")], "share": "team"},
            {"name": "oss", "paths": [root.join("o")], "share": "team"},
            {"name": "mine", "paths": [root.join("p")], "share": "local"}]}))
        .unwrap();
        let at = |name: &str| stores(Some(&cfg), &base).into_iter().find(|s| s.name == name).unwrap();
        let (corp, oss, mine_) = (at("corp"), at("oss"), at("mine"));
        // Held: two waiting episodes of lib in corp; one learned, one placed by borg correct.
        for e in [episode("h1", "lib"), episode("h2", "lib"), episode("h3", "lib"), Episode { via: VIA.into(), ..episode("h4", "lib") }] {
            learn::append_jsonl(&corp.home.join(EPISODES_FILE), &e).unwrap();
        }
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &Lesson { repo: "app".into(), ..lesson("Lh", 1, &[ME], &["h3"]) }).unwrap();
        // Misplaced: a lesson of lib in corp. Left alone: one moved there on purpose, one learned from a
        // borg correct alone.
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &Lesson { repo: "lib".into(), ..lesson("Lm", 1, &[ME], &[]) }).unwrap();
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &Lesson { repo: "lib".into(), ..lesson("Lp", 1, &[ME], &[]) }).unwrap();
        learn::append_jsonl(&corp.home.join(PLACED_FILE), &serde_json::json!({"id": "Lp", "from": "oss", "ms": 1})).unwrap();
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &Lesson { repo: "lib".into(), ..lesson("Lc", 1, &[ME], &["h4"]) }).unwrap();
        // Repair: a teammate's and an own lesson withdrawn from corp, their text in the private store;
        // and an own corp-repo lesson the private store learned itself.
        for l in [lesson("Lt", 1, &["bo@x"], &["t1"]), lesson("Lo", 1, &[ME], &["o1"]), lesson("Ln", 1, &[], &["n1"])] {
            learn::append_jsonl(&corp.home.join(LESSONS_FILE), &l).unwrap();
            learn::append_jsonl(&corp.home.join(LESSONS_FILE), &tombstone_old(&l)).unwrap();
            learn::append_jsonl(&mine_.home.join(LESSONS_FILE), &l).unwrap();
        }
        // No engineer on the team's, but the private copy is this engineer's and learned a private
        // episode since: never published to the team as it is.
        let lw = lesson("Lw", 1, &[], &["w1"]);
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &lw).unwrap();
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &tombstone_old(&lw)).unwrap();
        learn::append_jsonl(&mine_.home.join(LESSONS_FILE), &lesson("Lw", 2, &[ME], &["w1", "private1"])).unwrap();
        // A teammate's own tombstone, synced from their machine, no text here: nothing to act on.
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &tombstone_old(&lesson("Ls", 1, &["bo@x"], &["s1"]))).unwrap();
        learn::append_jsonl(&mine_.home.join(LESSONS_FILE), &lesson("Lr", 3, &[ME], &["r1"])).unwrap();
        let out = doctor_with(&cfg, &base, ME, &Aliases::default());
        let text: Vec<&str> = out.iter().map(|(_, t)| t.as_str()).collect();
        let has = |s: &str| text.iter().any(|t| t.contains(s));
        assert!(
            has(&format!(
                "2 waiting episode(s) of lib are in corp, but lib routes to oss now: borg move {} --from corp --to oss",
                root.join("o/lib").display()
            )),
            "{text:#?}"
        );
        assert!(has("lesson Lm in corp names lib, which routes to oss: borg move Lm --from corp --to oss"), "{text:#?}");
        assert!(!has("Lp ") && !has("Lc "), "placed by the engineer, no suggestion: {text:#?}");
        assert!(has("lesson Lt of bo@x was withdrawn from corp as personal; restore it for the team: borg move Lt --from mine --to corp"), "{text:#?}");
        assert!(has("your lesson Lo was withdrawn from corp as personal; keep it personal there: borg move Lo --from mine --to corp --personal"), "{text:#?}");
        assert!(has("lesson Lr in mine was learned in corp (app): keep it personal there: borg move Lr --from mine --to corp --personal"), "{text:#?}");
        assert!(
            has("lesson Ln of no recorded engineer was withdrawn from corp as personal; restore it for the team: borg move Ln --from mine --to corp"),
            "{text:#?}"
        );
        assert!(has("your lesson Lw was withdrawn from corp as personal; keep it personal there: borg move Lw --from mine --to corp --personal"), "{text:#?}");
        assert!(!has("Lw --from mine --to corp\n") && !text.iter().any(|t| t.ends_with("Lw --from mine --to corp")), "{text:#?}");
        assert!(!has(" Ls "), "a teammate's synced tombstone is no item here: {text:#?}");
        assert_eq!(text.iter().filter(|t| t.contains(" Lt ")).count(), 1, "one item per lesson: {text:#?}");
        // Without this engineer's email nothing is classified: one warning instead of guesses.
        let blind = doctor_with(&cfg, &base, "", &Aliases::default());
        assert!(blind.iter().any(|(_, t)| t.contains("cannot tell whose lessons these are")), "{blind:#?}");
        assert!(!blind.iter().any(|(_, t)| t.starts_with("repair") || t.contains("--from mine")), "nothing private suggested for a team: {blind:#?}");
        // After the repair the items are gone.
        move_lesson(&mine_, &corp, "Lt", false, ME, &Aliases::default(), true).unwrap();
        move_lesson(&mine_, &corp, "Ln", false, ME, &Aliases::default(), true).unwrap();
        for id in ["Lo", "Lr", "Lw"] {
            move_lesson(&mine_, &corp, id, true, ME, &Aliases::default(), true).unwrap();
        }
        assert!(reach::personal_lessons(&corp.home).iter().any(|l| l.id == "Lw"), "kept for this engineer in corp");
        assert!(!read_lines(&corp.home.join(LESSONS_FILE)).contains("private1"), "nothing the private store learned reaches the team");
        // A teammate's lesson of lib copied to oss stays in corp, and is no suggestion any more.
        learn::append_jsonl(&corp.home.join(LESSONS_FILE), &Lesson { repo: "lib".into(), ..lesson("Lb", 1, &[ME, "bo@x"], &[]) }).unwrap();
        move_lesson(&corp, &oss, "Lb", false, ME, &Aliases::default(), true).unwrap();
        move_lesson(&corp, &oss, "Lm", false, ME, &Aliases::default(), true).unwrap();
        let after = doctor_with(&cfg, &base, ME, &Aliases::default());
        assert!(!after.iter().any(|(_, t)| t.starts_with("repair") || t.starts_with("misplaced")), "{after:#?}");
    }

    /// A tombstone as a borg before 0.7.4 wrote it (reach.rs `WITHDRAWN`).
    fn tombstone_old(l: &Lesson) -> Lesson {
        Lesson { question: reach::WITHDRAWN.into(), ..reach::tombstone(l) }
    }

    #[test]
    fn a_repos_evidence_leaves_only_stores_that_hold_it_for_the_target() {
        let root = temp("sources");
        let base = root.join("base");
        for d in ["w/group/api", "o/api", "o/lib"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let cfg: Config = serde_json::from_value(serde_json::json!({"default": "off", "collectives": [
            {"name": "corp", "paths": [root.join("w")], "share": "team"},
            {"name": "oss", "paths": [root.join("o")], "share": "team"}]}))
        .unwrap();
        let at = |name: &str| stores(Some(&cfg), &base).into_iter().find(|s| s.name == name).unwrap();
        let (corp, oss) = (at("corp"), at("oss"));
        // corp waits on episodes of its own api (a namesake of oss's) and of lib, which routes to oss.
        for e in [episode("a1", "api"), episode("l1", "lib")] {
            learn::append_jsonl(&corp.home.join(EPISODES_FILE), &e).unwrap();
        }
        assert!(repo_sources(Some(&cfg), &base, "api", &oss).is_empty(), "corp may hold api by right: never a source unasked");
        assert_eq!(repo_sources(Some(&cfg), &base, "lib", &oss), std::slice::from_ref(&corp));
        let m = move_repo(&repo_sources(Some(&cfg), &base, "api", &oss), &oss, "api", true).unwrap();
        assert!(m.from.is_empty());
        assert_eq!(lines(&corp.home.join(EPISODES_FILE)).len(), 2, "corp keeps its api evidence");
        let m = move_repo(&repo_sources(Some(&cfg), &base, "lib", &oss), &oss, "lib", true).unwrap();
        let backup = m.from[0].backup.clone().unwrap();
        assert!(read_lines(&backup).contains("\"l1\""), "the source's log as it was is kept");
        assert!(m.describe().iter().any(|l| l.contains(&backup.display().to_string())));
    }

    #[test]
    fn a_repo_move_waits_for_a_learn_run_on_its_source() {
        let root = temp("learn-lock");
        let (a, b) = (store(&root, "mine", Share::Local), store(&root, "oss", Share::Team));
        learn::append_jsonl(&a.home.join(EPISODES_FILE), &episode("w1", "lib")).unwrap();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        // `borg learn` holds the store's lessons.jsonl lock while it reads and learns the episodes.
        let learner = {
            let lessons = a.home.join(LESSONS_FILE);
            std::thread::spawn(move || {
                crate::live::locked(&lessons, || {
                    held_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                })
            })
        };
        held_rx.recv().unwrap();
        let mover = {
            let (a, b) = (a.clone(), b.clone());
            std::thread::spawn(move || move_repo(&[a], &b, "lib", true).unwrap())
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(lines(&a.home.join(EPISODES_FILE)).len(), 1, "the move waits while learning runs");
        go_tx.send(()).unwrap();
        learner.join().unwrap();
        assert_eq!(mover.join().unwrap().from[0].moved.len(), 1);
        assert!(lines(&a.home.join(EPISODES_FILE)).is_empty());
    }

    #[test]
    fn the_ambiguous_session_error_names_the_flag_correct_takes() {
        let e = crate::msg::pick(vec![("s1".into(), "claude"), ("s2".into(), "codex")], &[], &[]).unwrap_err();
        assert!(e.contains("--from"), "msg's own wording: {e}");
        let h = session_hint(&e);
        assert!(h.contains("--session ID") && !h.contains("--from"), "{h}");
    }
}
