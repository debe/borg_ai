//! Transcript capture: the engineer's corrections and answers read from each harness's own transcript,
//! the durable record, beside the hooks' live capture. A hook can go quiet when a harness changes (Claude
//! Code 2.1.312 stopped sending its `PostToolUseFailure` for a rejected tool call, while its transcript
//! still names the engineer's words); the transcript cannot.
//!
//! - **Turn end** ([`turn_end`], the `stop` hook): Claude Code (the hook's `transcript_path`), omp (the
//!   session file found by its id) and Codex (the rollout found by its thread id). Only what the turn
//!   added is read: `<state>/<session>.swept` keeps the byte offset swept to, a hash of the
//!   [`HASHED`] bytes before it and where the reader stood ([`Carry`]: the task, the drone's last text,
//!   a denial waiting for its reason, the tool calls still waiting for their result), so a call and its
//!   result read in different turns still pair. When the file did not grow, the turn costs one stat. A
//!   file that shrank or changed before the offset is read again (dedupe keeps that safe).
//! - **Session end** ([`session_end`], `borg learn --session`, before learning): the whole transcript
//!   on every harness, opencode through its CLI export (only here, never in a hook).
//! - **Idle sessions** ([`idle`], every `borg learn`): a session whose transcript grew since its last
//!   sweep and has been quiet for `outcome::IDLE_MS` is swept, and its episodes learned (learn.rs `quiet`):
//!   a session never ended no longer keeps them waiting. Only sessions swept once have the `.swept` it
//!   looks for; one never swept is read at its end or by `borg assimilate`.
//!
//! Episodes are only appended, through learn.rs's one dedupe ([`Known`]: the same id, the same words in
//! the same session, a prompt moments apart), so the hook's capture and this one never double; nothing
//! here settles an outcome (a session may still be open). `BORG_TRANSCRIPT_CAPTURE=off` turns all of it
//! off. [`doctor`] compares what the transcripts say happened with what Borg holds.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::assimilate::{self, Carry, Found};
use crate::collective::{self, Config, Route};
use crate::harness::{self, Harness, Level};
use crate::learn::{self, Episode, Known};

/// Whether transcript capture runs: learning is on and `BORG_TRANSCRIPT_CAPTURE` is not `off`.
pub fn on() -> bool {
    learn::learning() && std::env::var("BORG_TRANSCRIPT_CAPTURE").as_deref() != Ok("off")
}

/// Method: the bytes before the swept offset whose hash says the file is still the one swept (a
/// transcript rewritten in place, or another file at the same path, differs there).
const HASHED: u64 = 64;
/// Guard: what a turn-end sweep reads of a transcript it has no valid offset for (the first turn after an
/// upgrade, a file rewritten), its last bytes from a line start. Bounds a hook's read of a long session;
/// what lies before is read at the session's end or by the idle sweep, which read from the start.
const FIRST_READ: u64 = 4 << 20;

/// `<state>/<session>.swept`: where a session's transcript was swept to.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Swept {
    session: String,
    harness: String,
    path: PathBuf,
    offset: u64,
    /// The hash ([`hash`]) of the [`HASHED`] bytes before `offset`.
    before: String,
    carry: Carry,
}

fn swept_file(state: &Path, session: &str) -> PathBuf {
    state.join(format!("{}.swept", crate::notes::safe(session)))
}

fn load(file: &Path) -> Option<Swept> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

/// Written whole and renamed into place: a reader never sees half of it.
fn save(file: &Path, st: &Swept) {
    let tmp = file.with_extension("swept.tmp");
    if serde_json::to_vec(st).ok().is_some_and(|b| std::fs::write(&tmp, b).is_ok()) {
        let _ = std::fs::rename(&tmp, file);
    }
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_string()
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

#[cfg(test)]
thread_local! {
    /// Each file system call a sweep makes on a transcript (a stat, an open), for the cost tests.
    static TOUCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn touched() {
    #[cfg(test)]
    TOUCHES.with(|t| t.set(t.get() + 1));
}

// ---------------------------------------------------------------- reading what was added

/// Where a read starts when the swept offset cannot be used.
#[derive(Clone, Copy)]
enum Restart {
    /// The transcript's last [`FIRST_READ`] bytes (a hook).
    Tail,
    /// Its start (a session's end, the idle sweep, which run in the background).
    Start,
}

/// The complete lines a read gave, and where they end (with the hash of the bytes before that).
struct Read {
    lines: Vec<Value>,
    end: u64,
    end_hash: String,
}

/// The complete lines of `f` from `from` on (from the first line start after it when `align`), checked
/// first against `before`, the hash of the bytes before `from`: None when they differ (the file changed).
fn read_from(f: &mut std::fs::File, from: u64, before: Option<&str>, align: bool) -> Option<Read> {
    let start = from - from.min(HASHED);
    let mut buf = Vec::new();
    f.seek(SeekFrom::Start(start)).ok()?;
    f.read_to_end(&mut buf).ok()?;
    let pre = usize::try_from(from - start).ok()?;
    if buf.len() < pre {
        return None; // the file shrank under us since its size was taken: the next sweep restarts
    }
    if before.is_some_and(|b| hash(&buf[..pre]) != b) {
        return None;
    }
    let data = &buf[pre..];
    let skip = if align && from > 0 { data.iter().position(|b| *b == b'\n').map_or(data.len(), |i| i + 1) } else { 0 };
    let body = &data[skip..];
    let whole = body.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let lines = body[..whole].split(|b| *b == b'\n').filter_map(|l| serde_json::from_slice(l).ok()).collect();
    let end = from + (skip + whole) as u64;
    let upto = usize::try_from(end - start).ok()?;
    let end_hash = hash(&buf[upto - upto.min(HASHED as usize)..upto]);
    Some(Read { lines, end, end_hash })
}

fn size(path: &Path) -> Option<u64> {
    touched();
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// Whether `st` was swept to the end of `path`, `len` bytes long: nothing to read.
fn swept_to_end(st: &Swept, path: &Path, len: u64) -> bool {
    st.path == path && st.offset == len && len > 0
}

/// What `path` (`len` bytes) added since `st` (all of it, or its tail, when `st` cannot be used), with
/// the reader's state to go on from.
fn added(st: &Swept, path: &Path, len: u64, restart: Restart) -> Option<(Read, Carry)> {
    let same = st.path == path && st.offset > 0 && st.offset <= len;
    touched();
    let mut f = std::fs::File::open(path).ok()?;
    if same && let Some(r) = read_from(&mut f, st.offset, Some(&st.before), false) {
        return Some((r, st.carry.clone()));
    }
    let from = match restart {
        Restart::Tail => len.saturating_sub(FIRST_READ),
        Restart::Start => 0,
    };
    read_from(&mut f, from, None, true).map(|r| (r, Carry::default()))
}

// ---------------------------------------------------------------- one session's sweep

/// Session `session`'s `.swept` in `state`, if it is this harness's.
fn swept_of(h: &dyn Harness, file: &Path, session: &str) -> Swept {
    load(file).filter(|s| s.harness == h.name() && s.session == session).unwrap_or_default()
}

/// The transcript to sweep and its size: the one swept before while it is there (one stat), else where
/// `locate` finds it.
fn transcript(st: &Swept, locate: &dyn Fn() -> Option<PathBuf>) -> Option<(PathBuf, u64)> {
    if !st.path.as_os_str().is_empty()
        && let Some(len) = size(&st.path)
    {
        return Some((st.path.clone(), len));
    }
    let path = locate()?;
    size(&path).map(|len| (path, len))
}

/// Sweeps session `session`'s transcript of harness `h` (the one swept before, else where `locate`
/// finds it) from the offset swept to, or from where `restart` says when there is none or it no longer
/// holds (`whole`: from the start, whatever was swept). When it did not grow that costs one stat (and
/// the read of the small `.swept`); else the read runs under the lock of the `.swept` file in `state`.
/// Its new episodes go to `place` ([`record`]: the store their cwd routes to); the stores that got one.
fn sweep(h: &dyn Harness, state: &Path, session: &str, locate: &dyn Fn() -> Option<PathBuf>, how: (Restart, bool), place: Place) -> BTreeSet<PathBuf> {
    let (restart, whole) = how;
    let file = swept_file(state, session);
    if !whole {
        let st = swept_of(h, &file, session);
        if transcript(&st, locate).is_none_or(|(path, len)| swept_to_end(&st, &path, len)) {
            return BTreeSet::new();
        }
    }
    let _ = std::fs::create_dir_all(state);
    crate::live::locked(&file, || {
        // Again under the lock: a sweep that ran meanwhile moved the offset.
        let st = swept_of(h, &file, session);
        let Some((path, len)) = transcript(&st, locate) else { return BTreeSet::new() };
        let st = if whole { Swept::default() } else { st };
        if swept_to_end(&st, &path, len) {
            return BTreeSet::new();
        }
        let Some((read, carry)) = added(&st, &path, len, restart) else { return BTreeSet::new() };
        let Some((found, carry)) = assimilate::log_lines(h, &path, session, carry, read.lines) else { return BTreeSet::new() };
        let got = if found.is_empty() { BTreeSet::new() } else { place(found) };
        save(&file, &Swept { session: session.into(), harness: h.name().into(), path, offset: read.end, before: read.end_hash, carry });
        got
    })
}

/// Where a sweep's episodes go: the stores that got one.
type Place<'a> = &'a dyn Fn(Vec<Found>) -> BTreeSet<PathBuf>;

/// Appends each episode to the store its cwd routes to (assimilate.rs `place`), unless that store holds
/// it already ([`Known`]), and counts it as the hook would; the stores that got one. The collectives
/// config is read only here, once there is something to route.
fn record(found: Vec<Found>) -> BTreeSet<PathBuf> {
    let Ok(cfg) = collective::load() else { return BTreeSet::new() };
    let base = collective::root_of(&learn::home());
    record_in(found, cfg.as_ref(), &base)
}

fn record_in(found: Vec<Found>, cfg: Option<&Config>, base: &Path) -> BTreeSet<PathBuf> {
    let mut known: HashMap<PathBuf, Known> = HashMap::new();
    let mut engineers = HashMap::new();
    let mut got = BTreeSet::new();
    for f in found {
        let Some(mut p) = assimilate::place(cfg, base, f) else { continue };
        let k = known.entry(p.home.clone()).or_insert_with(|| Known::read(&p.home));
        if k.holds(&p.found.episode) {
            continue;
        }
        assimilate::complete(&mut p.found, &mut engineers);
        if learn::record_new_in(&p.home, k, &p.found.episode).unwrap_or(false) {
            assimilate::count_veto(&p.home, &p.found.episode);
            got.insert(p.home);
        }
    }
    got
}

// ---------------------------------------------------------------- the three moments

/// The `stop` hook: what this turn added to the session's transcript, its episodes appended. One stat
/// when it added nothing. Never settles anything, and fails open: a reader's or the sweep's panic never
/// takes the rest of the stop hook down with it.
pub fn turn_end(h: &dyn Harness, session: &str, input: &Value) {
    if !on() {
        return;
    }
    let home = user_home();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sweep(h, &crate::notes::state_dir(), session, &|| h.session_log(&home, session, input), (Restart::Tail, false), &record)
    }));
}

/// The state directories `.swept` files may be in: `BORG_STATE` when set (as every hook sets it, to the
/// collective its repo routes to), each collective's store's `state/` and the root store's. All of them,
/// so a `borg learn` one session's hook spawned also reaches the idle sessions of other collectives.
fn state_dirs(cfg: Option<&Config>, base: &Path) -> Vec<PathBuf> {
    let set = std::env::var_os("BORG_STATE").filter(|s| !s.is_empty()).map(PathBuf::from);
    let homes = crate::install::collectives(cfg, base).into_iter().filter_map(|r| match r {
        Route::Collective { home, .. } => Some(home.join("state")),
        Route::Off => None,
    });
    set.into_iter().chain(std::iter::once(base.join("state"))).chain(homes).collect::<BTreeSet<_>>().into_iter().collect()
}

/// A session's end (`borg learn --session`, before it learns): the whole transcript of `session` on its
/// harness (`BORG_HARNESS`, as its hook names it; else every harness's), opencode's through its CLI
/// export. The stores that got episodes, so they are learned.
pub fn session_end(session: &str, cfg: Option<&Config>, base: &Path) -> BTreeSet<PathBuf> {
    let hs: Vec<&dyn Harness> = match harness::named_current() {
        Some(h) => vec![h],
        None => harness::visible().collect(),
    };
    let home = user_home();
    // The `.swept` already there (a manual run finds it in its collective's state), else beside the
    // store's other state (`BORG_STATE`, as the hook sets it, else the store's `state/`, as learn.rs reads it).
    let fallback = || std::env::var_os("BORG_STATE").filter(|s| !s.is_empty()).map_or_else(|| learn::home().join("state"), PathBuf::from);
    let state = state_dirs(cfg, base).into_iter().find(|d| swept_file(d, session).exists()).unwrap_or_else(fallback);
    let here = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    let ends = Ends {
        locate: &|h| h.session_log(&home, session, &Value::Null),
        export: &|h| assimilate::exported(h, session, &here),
        place: &|found| record_in(found, cfg, base),
    };
    end_of(&hs, session, &state, &ends)
}

/// How a session's end finds its transcript ([`Harness::session_log`]), or exports it (assimilate.rs
/// `exported`: opencode), and where its episodes go.
struct Ends<'a> {
    locate: &'a dyn Fn(&dyn Harness) -> Option<PathBuf>,
    export: &'a dyn Fn(&dyn Harness) -> Option<Vec<Found>>,
    place: Place<'a>,
}

/// [`session_end`] on the harnesses `hs`, its `.swept` in `state`.
fn end_of(hs: &[&dyn Harness], session: &str, state: &Path, e: &Ends) -> BTreeSet<PathBuf> {
    let mut got = BTreeSet::new();
    for &h in hs {
        if let Some(found) = (e.export)(h) {
            got.extend((e.place)(found));
            continue;
        }
        got.extend(sweep(h, state, session, &|| (e.locate)(h), (Restart::Start, true), e.place));
    }
    got
}

/// Every `borg learn`: each swept session whose transcript changed since its last sweep and has been
/// quiet for `outcome::IDLE_MS` at `now` is swept on (a session left open never sends its end). The
/// stores that got episodes. A `.swept` whose transcript is gone is removed.
///
/// Only a session swept at least once (a turn end, a session end) has a `.swept` to find. A session
/// never swept (open since before 0.7.2, or one whose transcript a stop could not locate) is read at its
/// end, or by `borg assimilate`: finding every such transcript here would backfill old history on every
/// machine's first `borg learn`, one model call per episode, without the cost being stated first.
pub fn idle(cfg: Option<&Config>, base: &Path, now: i64) -> BTreeSet<PathBuf> {
    idle_in(&state_dirs(cfg, base), now, &|found| record_in(found, cfg, base))
}

/// [`idle`] over the state directories `dirs`.
fn idle_in(dirs: &[PathBuf], now: i64, place: Place) -> BTreeSet<PathBuf> {
    let mut got = BTreeSet::new();
    for dir in dirs {
        let files = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "swept"));
        for file in files {
            let Some(st) = load(&file) else { continue };
            let Some(h) = harness::by_name(&st.harness) else { continue };
            let Ok(meta) = std::fs::metadata(&st.path) else {
                let _ = std::fs::remove_file(&file);
                continue;
            };
            let quiet = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|d| d.as_millis() as i64 <= now - crate::outcome::IDLE_MS);
            if quiet && meta.len() != st.offset {
                got.extend(sweep(h, dir, &st.session, &|| None, (Restart::Start, false), place));
            }
        }
    }
    got
}

// ---------------------------------------------------------------- borg doctor

/// Method: how far back the doctor compares transcripts with what Borg holds: two weeks covers a
/// harness update a few days old and the sessions on either side of it.
pub const DOCTOR_DAYS: i64 = 14;
/// Guard: the transcripts the doctor reads per harness, the most recently written; a note says when
/// there were more.
const DOCTOR_FILES: usize = 200;
/// Method: what "far fewer" captured means: under half of what the transcripts name. A capture that
/// works holds about all of them (an answer with nothing picked or typed is no episode); one that broke
/// holds few or none.
const CAPTURED_SHARE: f64 = 0.5;

/// What the stores hold of one session in the window.
#[derive(Debug, Default)]
struct Held {
    /// Rejected tool calls the hook captured, and the ones read from a transcript.
    hook: usize,
    transcript: usize,
    answers: usize,
    /// The first rejection read from a transcript (its time and harness version).
    first_transcript: Option<(i64, String)>,
}

/// What every store holds per session, episodes at or after `since`.
fn held(cfg: Option<&Config>, base: &Path, since: i64) -> HashMap<String, Held> {
    let mut out: HashMap<String, Held> = HashMap::new();
    for r in crate::install::collectives(cfg, base) {
        let Route::Collective { home, .. } = r else { continue };
        let text = std::fs::read_to_string(home.join("episodes.jsonl")).unwrap_or_default();
        for e in text.lines().filter_map(|l| serde_json::from_str::<Episode>(l).ok()).filter(|e| e.ms >= since) {
            let h = out.entry(e.session.clone()).or_default();
            match (e.via.as_str(), &e.rejected_tool, e.harness.is_empty()) {
                (learn::ANSWER, ..) => h.answers += 1,
                ("", Some(_), true) => h.hook += 1,
                ("", Some(_), false) => {
                    h.transcript += 1;
                    if h.first_transcript.as_ref().is_none_or(|(ms, _)| e.ms < *ms) {
                        h.first_transcript = Some((e.ms, e.harness_version.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// One harness version's tally over the window: the sessions whose newest mark (else line) it wrote
/// (assimilate.rs `Marks::version`), so a capture that broke with a harness update is not hidden by the
/// healthy captures under the version before it.
#[derive(Debug, Default)]
struct Tally {
    sessions: usize,
    /// Marks in the transcripts, and of them the ones Borg holds (per session, at most its marks).
    rejections: usize,
    rejections_held: usize,
    answers: usize,
    answers_held: usize,
    /// The first mark of a session holding fewer than its marks, per kind: its time and harness version.
    first_lost_rejection: Option<(i64, String)>,
    first_lost_answer: Option<(i64, String)>,
    hook: usize,
    transcript: usize,
    first_transcript: Option<(i64, String)>,
    /// The newest mark's time: the versions are told in the order they were in use.
    last: i64,
}

fn earliest(slot: &mut Option<(i64, String)>, at: Option<&(i64, String)>) {
    if let Some(at) = at
        && slot.as_ref().is_none_or(|(ms, _)| at.0 < *ms)
    {
        *slot = Some(at.clone());
    }
}

impl Tally {
    fn add(&mut self, m: &assimilate::Marks, h: Option<&Held>) {
        let none = Held::default();
        let h = h.unwrap_or(&none);
        self.sessions += 1;
        let rejections_held = (h.hook + h.transcript).min(m.rejections.len());
        let answers_held = h.answers.min(m.answers.len());
        if rejections_held < m.rejections.len() {
            earliest(&mut self.first_lost_rejection, m.rejections.iter().min());
        }
        if answers_held < m.answers.len() {
            earliest(&mut self.first_lost_answer, m.answers.iter().min());
        }
        (self.rejections, self.rejections_held) = (self.rejections + m.rejections.len(), self.rejections_held + rejections_held);
        (self.answers, self.answers_held) = (self.answers + m.answers.len(), self.answers_held + answers_held);
        (self.hook, self.transcript) = (self.hook + h.hook, self.transcript + h.transcript);
        earliest(&mut self.first_transcript, h.first_transcript.as_ref());
        self.last = m.rejections.iter().chain(&m.answers).map(|(ms, _)| *ms).fold(self.last, i64::max);
    }
}

fn far_fewer(held: usize, marks: usize) -> bool {
    marks > 0 && (held as f64) < marks as f64 * CAPTURED_SHARE
}

/// `<name> <version>` (the version where the transcript names one).
fn named(h: &dyn Harness, version: &str) -> String {
    if version.is_empty() { h.name().to_string() } else { format!("{} {version}", h.name()) }
}

/// The checks of one harness's tallies, one per harness version (`versions`); `capture_on` when
/// transcript capture runs ([`on`]).
fn verdicts(h: &dyn Harness, versions: &BTreeMap<String, Tally>, note: &str, capture_on: bool) -> Vec<(Level, String)> {
    let mut out = Vec::new();
    let mut warned = false;
    let mut ordered: Vec<(&String, &Tally)> = versions.iter().collect();
    ordered.sort_by_key(|(_, t)| t.last);
    for (version, t) in ordered {
        let lost = |what: &str, marks: usize, held: usize, first: &Option<(i64, String)>| {
            let day = crate::stats::day(first.as_ref().map_or(0, |(ms, _)| *ms));
            format!(
                "{}: {marks} {what} in its transcripts since {day}, {held} captured{note}: transcript capture recovers them (borg assimilate --since {day} to recover the older ones)",
                named(h, version)
            )
        };
        if far_fewer(t.rejections_held, t.rejections) {
            out.push((Level::Warn, lost("rejections with the engineer's words", t.rejections, t.rejections_held, &t.first_lost_rejection)));
            warned = true;
        }
        if far_fewer(t.answers_held, t.answers) {
            out.push((Level::Warn, lost("answers to the drone's questions", t.answers, t.answers_held, &t.first_lost_answer)));
            warned = true;
        }
        // The hook went quiet while the transcript still names rejections: the harness changed what it
        // tells the hook. Only news while transcript capture covers it; a warning when it does not.
        if h.caps().deny_capture && t.hook == 0 && t.transcript > 0 {
            let covered = capture_on && !far_fewer(t.rejections_held, t.rejections);
            let day = crate::stats::day(t.first_transcript.as_ref().map_or(0, |(ms, _)| *ms));
            let tail = if covered { "transcript capture covers it" } else { "only transcript capture recovers them, and it is off (BORG_TRANSCRIPT_CAPTURE)" };
            out.push((
                if covered { Level::Ok } else { Level::Warn },
                format!(
                    "{}: its hook captured no rejected tool call since {day}, its transcript {}: the harness stopped telling the hook ({tail})",
                    named(h, version),
                    t.transcript
                ),
            ));
            warned |= !covered;
        }
    }
    let sum = |f: fn(&Tally) -> usize| versions.values().map(f).sum::<usize>();
    let (rejections, answers) = (sum(|t| t.rejections), sum(|t| t.answers));
    if !warned && rejections + answers > 0 {
        out.push((
            Level::Ok,
            format!(
                "{}: capture holds {} of {rejections} rejections and {} of {answers} answers in {} sessions' transcripts of the last {DOCTOR_DAYS} days{note}",
                h.name(),
                sum(|t| t.rejections_held),
                sum(|t| t.answers_held),
                sum(|t| t.sessions)
            ),
        ));
    }
    out
}

fn modified_ms(p: &Path) -> i64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// `borg doctor`: per harness, the rejections with words and the answers its transcripts of the last
/// [`DOCTOR_DAYS`] days name (assimilate.rs `marks`, raw lines, the replay aside) against the episodes
/// Borg holds of those sessions (off repos aside). WARN when it holds none or far fewer, naming the
/// harness version and since when; WARN when its hook captured no rejection while its transcript did
/// (the Claude Code 2.1.312 signature). Bounded: transcripts written in the window only, the newest
/// [`DOCTOR_FILES`] per harness, read line by line. `home` is `$HOME`.
pub fn doctor(cfg: Option<&Config>, base: &Path, home: &Path, now: i64) -> Vec<(Level, String)> {
    if !learn::learning() {
        return vec![(Level::Ok, "capture: learning is off (BORG_LEARN=off), nothing is captured".into())];
    }
    let mut out = doctor_with(cfg, base, now, on(), &|h| assimilate::logs(h, home));
    if !on() {
        out.push((Level::Warn, "capture: BORG_TRANSCRIPT_CAPTURE=off: corrections reach Borg through the hooks alone".into()));
    }
    out
}

/// [`doctor`]'s per-harness checks, `logs` listing each harness's transcripts; `capture_on` when
/// transcript capture runs.
fn doctor_with(cfg: Option<&Config>, base: &Path, now: i64, capture_on: bool, logs: &dyn Fn(&dyn Harness) -> Option<Vec<PathBuf>>) -> Vec<(Level, String)> {
    let since = now - DOCTOR_DAYS * crate::clock::DAY_MS;
    let held = held(cfg, base, since);
    let mut out = Vec::new();
    for h in harness::visible() {
        let Some(files) = logs(h) else { continue };
        let mut recent: Vec<(i64, PathBuf)> = files.into_iter().map(|f| (modified_ms(&f), f)).filter(|(ms, _)| *ms >= since).collect();
        recent.sort_by_key(|(ms, _)| std::cmp::Reverse(*ms));
        let note = if recent.len() > DOCTOR_FILES { format!(" (the newest {DOCTOR_FILES} of {} transcripts read)", recent.len()) } else { String::new() };
        let mut versions: BTreeMap<String, Tally> = BTreeMap::new();
        for (_, f) in recent.iter().take(DOCTOR_FILES) {
            let Some(mut m) = assimilate::marks(h, f) else { continue };
            if matches!(collective::route(cfg, &m.cwd, base), Route::Off) {
                continue;
            }
            m.rejections.retain(|(ms, _)| *ms >= since);
            m.answers.retain(|(ms, _)| *ms >= since);
            versions.entry(m.version.clone()).or_default().add(&m, held.get(&m.session));
        }
        out.extend(verdicts(h, &versions, &note, capture_on));
    }
    out
}

/// `borg status`: a store's captured corrections and answers per source, each harness's transcript
/// (`<harness> transcript`), a veto said without one (`borg no`) and the rest (`hook or older import`:
/// the hooks name no harness, nor did `borg assimilate` before 0.7.2), each with its count and newest time.
pub fn sources(home: &Path) -> BTreeMap<String, (usize, i64)> {
    let mut out: BTreeMap<String, (usize, i64)> = BTreeMap::new();
    let text = std::fs::read_to_string(home.join("episodes.jsonl")).unwrap_or_default();
    for e in text.lines().filter_map(|l| serde_json::from_str::<Episode>(l).ok()) {
        // A regret or a review finding (notes/, review_fix.rs) is no capture of the engineer's words.
        let key = match (e.harness.is_empty(), e.kind.as_str()) {
            (false, _) => format!("{} transcript", e.harness),
            (true, crate::veto::KIND) => "borg no".to_string(),
            (true, "") => "hook or older import".to_string(),
            (true, _) => continue,
        };
        let s = out.entry(key).or_default();
        (s.0, s.1) = (s.0 + 1, s.1.max(e.ms));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::claude::Claude;

    const DEV: &str = r#"{ "default": "off", "collectives": [ { "name": "dev", "paths": ["/home/dev/work"] } ] }"#;
    const CLAUDE_SESSION: &str = "7f3c2a10-5b6d-4e8f-9a01-23456789abcd";

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-sweep-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)
    }

    fn claude_fixture() -> PathBuf {
        fixture(&format!("claude/transcripts/-home-dev-work-relay/{CLAUDE_SESSION}.jsonl"))
    }

    fn stored(base: &Path) -> Vec<Episode> {
        let text = std::fs::read_to_string(base.join("collectives/dev/episodes.jsonl")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn ids(eps: &[Episode]) -> Vec<String> {
        eps.iter().map(|e| e.id.clone()).collect()
    }

    /// The transcript's lines, each with its newline.
    fn lines_of(p: &Path) -> Vec<Vec<u8>> {
        std::fs::read(p).unwrap().split_inclusive(|b| *b == b'\n').map(<[u8]>::to_vec).collect()
    }

    fn grow(t: &Path, bytes: &[u8]) {
        use std::io::Write as _;
        std::fs::OpenOptions::new().create(true).append(true).open(t).unwrap().write_all(bytes).unwrap();
    }

    fn touches() -> usize {
        TOUCHES.with(std::cell::Cell::get)
    }

    #[test]
    fn a_growing_transcript_gives_each_episode_once_and_a_call_pairs_with_its_result_across_turns() {
        let root = temp("grow");
        let (base, state, t) = (root.join("home"), root.join("state"), root.join("t.jsonl"));
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
        let turn = || sweep(&Claude, &state, CLAUDE_SESSION, &|| Some(t.clone()), (Restart::Tail, false), &place);
        let lines = lines_of(&claude_fixture());
        // Turn 1 ends after the rejected Edit's call, before its result.
        grow(&t, &lines[..2].concat());
        assert!(turn().is_empty());
        // Turn 2: its result, and half of the next line (still being written).
        grow(&t, &lines[2]);
        grow(&t, &lines[3][..40]);
        assert_eq!(turn().len(), 1);
        let first = stored(&base);
        assert_eq!(ids(&first), ["7f3c2a10-Ttoolu_01RelayEdit"]);
        assert!(first[0].rejected_tool.as_deref().is_some_and(|t| t.starts_with("Edit ")), "the call read a turn before pairs: {first:?}");
        assert_eq!(first[0].task, "Add a retry to the relay delivery");
        // The rest, then a turn that added nothing.
        grow(&t, &lines[3][40..]);
        grow(&t, &lines[4..].concat());
        turn();
        turn();
        let all = stored(&base);
        let whole: Vec<Episode> = assimilate::claude_session(&claude_fixture()).into_iter().map(|f| f.episode).collect();
        assert_eq!(ids(&all), ids(&whole), "each once, the transcript's ids");
        assert!(all.iter().all(|e| e.repo == "relay" && e.harness == Claude.name() && e.harness_version == "2.1.312"));
        // Nothing changed: one stat, the transcript never opened.
        let before = touches();
        assert!(turn().is_empty());
        assert_eq!(touches() - before, 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn swept_line_by_line_every_harness_gives_what_one_read_of_the_whole_gives() {
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        for (session, h, path) in fixtures().into_iter().filter(|(.., p)| p.extension().is_some_and(|x| x == "jsonl")) {
            let root = temp(&format!("lines-{}", h.name()));
            let (base, state, t) = (root.join("home"), root.join("state"), root.join("t.jsonl"));
            let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
            for line in lines_of(&path) {
                grow(&t, &line);
                sweep(h, &state, session, &|| Some(t.clone()), (Restart::Tail, false), &place);
            }
            let all: Vec<Value> = std::fs::read_to_string(&path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
            let (whole, _) = assimilate::log_lines(h, &path, session, Carry::default(), all).unwrap();
            let shape = |e: &Episode| (e.id.clone(), e.task.clone(), e.agent.clone(), e.rejected_tool.clone(), e.correction.clone(), e.via.clone());
            let got: Vec<_> = stored(&base).iter().map(shape).collect();
            let want: Vec<_> = whole.iter().map(|f| shape(&f.episode)).collect();
            assert!(!want.is_empty());
            assert_eq!(got, want, "{}", h.name());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn a_truncated_or_rewritten_transcript_is_read_again_safely() {
        let root = temp("truncate");
        let (base, state, t) = (root.join("home"), root.join("state"), root.join("t.jsonl"));
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
        let turn = || sweep(&Claude, &state, CLAUDE_SESSION, &|| Some(t.clone()), (Restart::Tail, false), &place);
        std::fs::copy(claude_fixture(), &t).unwrap();
        turn();
        assert_eq!(stored(&base).len(), 3);
        // Shrunk: read again from the start, nothing doubles.
        let lines = lines_of(&claude_fixture());
        std::fs::write(&t, lines[..3].concat()).unwrap();
        assert!(turn().is_empty());
        assert_eq!(stored(&base).len(), 3);
        // Rewritten, longer, other bytes before the offset (another rejection): read again, the new one once.
        let other = String::from_utf8(lines.concat())
            .unwrap()
            .replace("toolu_01RelayEdit", "toolu_01RelayEdit2")
            .replace("a fixed sleep hammers", "a fixed sleep floods");
        std::fs::write(&t, format!("{other}\n")).unwrap();
        assert_eq!(turn().len(), 1);
        let eps = stored(&base);
        assert_eq!(eps.len(), 4);
        assert_eq!(eps[3].id, "7f3c2a10-Ttoolu_01RelayEdit2");
        turn();
        assert_eq!(stored(&base).len(), 4);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_long_omp_sessions_first_tail_read_knows_its_cwd_from_the_session_record() {
        let root = temp("omp-tail");
        let (base, state, t) = (root.join("home"), root.join("state"), root.join("t.jsonl"));
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
        let omp = fixture("omp/sessions/-home-dev-work-relay/2026-10-28T10-00-00-000Z_omp-relay-1.jsonl");
        let lines = lines_of(&omp);
        // The session record names the cwd; then more than a first read's worth of other lines.
        grow(&t, &lines[0]);
        let pad = format!("{}\n", serde_json::json!({"type": "custom", "data": "x".repeat(64 << 10)}));
        for _ in 0..=(FIRST_READ >> 16) {
            grow(&t, pad.as_bytes());
        }
        grow(&t, &lines[1..].concat());
        let turn = || sweep(&harness::omp::Omp, &state, "omp-relay-1", &|| Some(t.clone()), (Restart::Tail, false), &place);
        assert_eq!(turn(), BTreeSet::from([base.join("collectives/dev")]), "routed by the record's cwd");
        assert_eq!(stored(&base).len(), 4);
        assert_eq!(load(&swept_file(&state, "omp-relay-1")).unwrap().carry.cwd, "/home/dev/work/relay", "later turns go on from it");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_transcript_that_shrank_under_a_read_gives_nothing_and_no_panic() {
        let root = temp("shrank");
        let t = root.join("t.jsonl");
        std::fs::write(&t, b"{}\n").unwrap();
        let mut f = std::fs::File::open(&t).unwrap();
        assert!(read_from(&mut f, FIRST_READ, None, true).is_none());
        assert!(read_from(&mut f, 100, Some("x"), false).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// Every harness's fixture session, ended: (session, the harness whose transcript or export it is).
    fn fixtures() -> Vec<(&'static str, &'static dyn Harness, PathBuf)> {
        vec![
            (CLAUDE_SESSION, &Claude, claude_fixture()),
            ("omp-relay-1", &harness::omp::Omp, fixture("omp/sessions/-home-dev-work-relay/2026-10-28T10-00-00-000Z_omp-relay-1.jsonl")),
            (
                "0199e0aa-1111-7222-8333-444455556666",
                &harness::codex::Codex,
                fixture("codex/sessions/2026/10/28/rollout-2026-10-28T11-00-00-0199e0aa-1111-7222-8333-444455556666.jsonl"),
            ),
            ("ses_0c1a", &harness::opencode::Opencode, fixture("opencode/v2/session-export-question.json")),
        ]
    }

    /// Ends every fixture session on every harness; the stores that got episodes.
    fn end_all(state: &Path, place: Place) -> BTreeSet<PathBuf> {
        let hs: Vec<&dyn Harness> = harness::visible().collect();
        let mut got = BTreeSet::new();
        for (session, owner, path) in fixtures() {
            let mine = |h: &dyn Harness| h.name() == owner.name();
            let export = |h: &dyn Harness| {
                (mine(h) && path.extension().is_some_and(|x| x == "json"))
                    .then(|| assimilate::opencode_session(&serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()))
            };
            let ends = Ends { locate: &|h| mine(h).then(|| path.clone()), export: &export, place };
            got.extend(end_of(&hs, session, state, &ends));
        }
        got
    }

    #[test]
    fn a_sessions_end_reads_its_whole_transcript_on_every_harness_once() {
        let root = temp("end");
        let (base, state) = (root.join("home"), root.join("state"));
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
        assert_eq!(end_all(&state, &place), BTreeSet::from([base.join("collectives/dev")]));
        let eps = stored(&base);
        let per = |h: &str| eps.iter().filter(|e| e.harness == h).count();
        assert_eq!((per("claude"), per("omp"), per("opencode"), per("codex")), (3, 4, 2, 1), "{:?}", ids(&eps));
        assert_eq!(eps.iter().filter(|e| e.via == learn::ANSWER).count(), 7);
        // Ended again (a resumed session): nothing doubles.
        assert!(end_all(&state, &place).is_empty());
        assert_eq!(stored(&base).len(), 10);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_idle_sessions_new_lines_are_swept_and_learned() {
        let root = temp("idle");
        let (base, state, t) = (root.join("home"), root.join("state"), root.join("t.jsonl"));
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
        let lines = lines_of(&claude_fixture());
        // The last turn end swept the first prompt; then the engineer rejected a call, answered twice, and left.
        grow(&t, &lines[..1].concat());
        sweep(&Claude, &state, CLAUDE_SESSION, &|| Some(t.clone()), (Restart::Tail, false), &place);
        grow(&t, &lines[1..].concat());
        let now = learn::now_ms();
        assert!(idle_in(std::slice::from_ref(&state), now, &place).is_empty(), "written just now: not idle yet");
        let later = now + crate::outcome::IDLE_MS + 60_000;
        let store = base.join("collectives/dev");
        assert_eq!(idle_in(std::slice::from_ref(&state), later, &place), BTreeSet::from([store.clone()]));
        assert_eq!(stored(&base).len(), 3);
        assert!(idle_in(std::slice::from_ref(&state), later, &place).is_empty(), "swept to its end");
        // Another session's end learns them: this one is quiet (learn.rs `quiet`).
        let ask =
            |_: &str| Some(serde_json::json!({"is_lesson": true, "question": "Q?", "situations": [], "consequence": "c", "same_as": "", "supersedes": []}));
        let (new, merged, waiting) = learn::learn_in(&store, Some("another"), &ask, true, later);
        assert_eq!((new + merged, waiting), (3, 0));
        // A transcript that is gone takes its `.swept` with it.
        std::fs::remove_file(&t).unwrap();
        idle_in(std::slice::from_ref(&state), later, &place);
        assert!(!swept_file(&state, CLAUDE_SESSION).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_doctor_warns_on_marks_without_episodes_and_on_a_silent_hook() {
        let root = temp("doctor");
        let (base, state) = (root.join("home"), root.join("state"));
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
        let t = root.join(format!("{CLAUDE_SESSION}.jsonl"));
        std::fs::copy(claude_fixture(), &t).unwrap();
        let logs = |h: &dyn Harness| (h.name() == Claude.name()).then(|| vec![t.clone()]);
        let now = borg_trace::parse_ts("2026-10-29T00:00:00Z");
        let doctor = || doctor_with(Some(&cfg), &base, now, true, &logs);
        let warns = |c: &[(Level, String)]| c.iter().filter(|(l, _)| *l == Level::Warn).map(|(_, m)| m.clone()).collect::<Vec<_>>();
        // Nothing captured: both kinds warn, with the version and the day.
        let w = warns(&doctor());
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].starts_with("claude 2.1.312: 1 rejections with the engineer's words in its transcripts since 2026-10-28, 0 captured"), "{w:?}");
        assert!(w[1].starts_with("claude 2.1.312: 2 answers to the drone's questions in its transcripts since 2026-10-28, 0 captured"), "{w:?}");
        // The hook captured the rejection, the session's end the rest: all held, ok.
        let store = base.join("collectives/dev");
        std::fs::create_dir_all(&store).unwrap();
        let hook = Episode {
            id: "7f3c2a10-1793178071000".into(),
            ms: borg_trace::parse_ts("2026-10-28T09:01:11.000Z"),
            session: CLAUDE_SESSION.into(),
            rejected_tool: Some("Edit {}".into()),
            correction: "use exponential backoff with jitter, a fixed sleep hammers the broker".into(),
            ..Episode::default()
        };
        learn::record_episode_in(&store, &hook).unwrap();
        let ends = Ends { locate: &|h| (h.name() == Claude.name()).then(|| t.clone()), export: &|_| None, place: &place };
        end_of(&[&Claude], CLAUDE_SESSION, &state, &ends);
        let ok = doctor();
        assert!(warns(&ok).is_empty(), "{ok:?}");
        assert!(
            ok.iter().any(|(l, m)| *l == Level::Ok && m.starts_with("claude: capture holds 1 of 1 rejections and 2 of 2 answers in 1 sessions'")),
            "{ok:?}"
        );
        // The 2.1.312 signature: the hook captured none, the transcript did. Transcript capture covers
        // it: news, no warning (the backfilled state is clean); with capture off it warns.
        let _ = std::fs::remove_dir_all(&store);
        let _ = std::fs::remove_dir_all(&state);
        end_of(&[&Claude], CLAUDE_SESSION, &state, &ends);
        let covered = doctor();
        assert!(warns(&covered).is_empty(), "{covered:?}");
        let silent = "claude 2.1.312: its hook captured no rejected tool call since 2026-10-28, its transcript 1";
        assert!(covered.iter().any(|(l, m)| *l == Level::Ok && m.starts_with(silent)), "{covered:?}");
        let w = warns(&doctor_with(Some(&cfg), &base, now, false, &logs));
        assert!(w.len() == 1 && w[0].starts_with(silent), "{w:?}");
        // An off repo's transcript is nobody's business.
        let off: Config = serde_json::from_str(r#"{ "default": "off", "collectives": [ { "name": "dev", "paths": ["/elsewhere"] } ] }"#).unwrap();
        assert!(doctor_with(Some(&off), &base, now, true, &logs).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_doctor_tells_harness_versions_apart() {
        let root = temp("doctor-versions");
        let (base, state) = (root.join("home"), root.join("state"));
        let cfg: Config = serde_json::from_str(DEV).unwrap();
        let place = |f: Vec<Found>| record_in(f, Some(&cfg), &base);
        let store = base.join("collectives/dev");
        std::fs::create_dir_all(&store).unwrap();
        // Before the update: two sessions under 2.1.311, their rejections captured by the hook.
        const OLD: [&str; 2] = ["0a0a0a0a-5b6d-4e8f-9a01-23456789abcd", "0b0b0b0b-5b6d-4e8f-9a01-23456789abcd"];
        let fixture_text = std::fs::read_to_string(claude_fixture()).unwrap();
        let mut logs = Vec::new();
        for (i, old) in OLD.iter().enumerate() {
            let t = root.join(format!("{old}.jsonl"));
            let day = format!("2026-10-2{}T09", 5 + i);
            std::fs::write(&t, fixture_text.replace("2.1.312", "2.1.311").replace(CLAUDE_SESSION, old).replace("2026-10-28T09", &day)).unwrap();
            let hook = Episode {
                id: format!("{}-{i}", &old[..8]),
                ms: borg_trace::parse_ts(&format!("{day}:01:11.000Z")),
                session: (*old).into(),
                rejected_tool: Some("Edit {}".into()),
                correction: "use exponential backoff with jitter, a fixed sleep hammers the broker".into(),
                ..Episode::default()
            };
            learn::record_episode_in(&store, &hook).unwrap();
            logs.push(t);
        }
        // After it: the hook went quiet and nothing read the transcript.
        let t = root.join(format!("{CLAUDE_SESSION}.jsonl"));
        std::fs::copy(claude_fixture(), &t).unwrap();
        logs.push(t.clone());
        let now = borg_trace::parse_ts("2026-10-29T00:00:00Z");
        let list = |h: &dyn Harness| (h.name() == Claude.name()).then(|| logs.clone());
        let warns = |c: &[(Level, String)]| c.iter().filter(|(l, _)| *l == Level::Warn).map(|(_, m)| m.clone()).collect::<Vec<_>>();
        // Summed over the window, 2 of 3 rejections are held; the new version holds none of its one.
        let w = warns(&doctor_with(Some(&cfg), &base, now, true, &list));
        assert!(
            w.iter().any(|m| m.starts_with("claude 2.1.312: 1 rejections with the engineer's words in its transcripts since 2026-10-28, 0 captured")),
            "{w:?}"
        );
        assert!(!w.iter().any(|m| m.starts_with("claude 2.1.311: 2 rejections")), "the old version's were captured: {w:?}");
        // Transcript capture picks up the new session: the silent hook is told for the new version
        // alone, though the hook captured under the old one in the same window.
        let ends = Ends { locate: &|h| (h.name() == Claude.name()).then(|| t.clone()), export: &|_| None, place: &place };
        end_of(&[&Claude], CLAUDE_SESSION, &state, &ends);
        let off = warns(&doctor_with(Some(&cfg), &base, now, false, &list));
        assert!(off.iter().any(|m| m.starts_with("claude 2.1.312: its hook captured no rejected tool call since 2026-10-28, its transcript 1")), "{off:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn status_counts_captures_per_source() {
        let root = temp("sources");
        let e = |id: &str, harness: &str, kind: &str, ms: i64| Episode { id: id.into(), ms, harness: harness.into(), kind: kind.into(), ..Episode::default() };
        for x in [e("a", "", "", 5), e("b", "claude", "", 7), e("c", "claude", "", 6), e("d", "", crate::veto::KIND, 9), e("r", "", learn::regret::KIND, 11)] {
            learn::record_episode_in(&root, &x).unwrap();
        }
        assert_eq!(
            sources(&root),
            BTreeMap::from([("claude transcript".to_string(), (2, 7)), ("borg no".to_string(), (1, 9)), ("hook or older import".to_string(), (1, 5))]),
            "a regret is no capture"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
