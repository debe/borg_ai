//! Candidate corrections under `BORG_CORRECTION_GATE=model` (learn.rs [`super::Gate`]): every human
//! prompt that follows a drone proposal is kept as a *candidate* in [`FILE`], and `borg learn` judges
//! each session's candidates in one model call per [`BATCH`] (`correction`, `task` or `neither`). The
//! corrections become episodes; every verdict leaves a short line in [`JUDGED_FILE`] (its words' key,
//! learn.rs `words_key`), so a transcript read again never captures a judged prompt twice.
//!
//! Kept apart from `episodes.jsonl`, which every sweep reads: pending candidates are pruned once judged.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use super::{AskFailed, Episode, Known, Step};
use crate::jsonl;
use crate::runner::MAX_FAILURES;

/// `Episode::kind` of a candidate, only while it waits in [`FILE`].
pub const KIND: &str = "candidate";
/// The pending candidates of a store, whole `Episode`s (serde writes `id`, `ms`, `repo`, `session` first).
pub const FILE: &str = "candidates.jsonl";
/// One short line per judged candidate ([`Judged`]).
pub const JUDGED_FILE: &str = "candidates-judged.jsonl";
/// The most prompts one model call judges.
pub const BATCH: usize = 20;
/// The end of the agent's message one prompt shows.
pub const AGENT_CHARS: usize = 1000;
/// How a judge prompt starts; `schema_for` and `ask_claude` recognise it by this.
pub const OPENING: &str = "An engineer works with a coding agent. Each numbered item below is one prompt the engineer typed, in the order typed, \
with the end of the agent message it answers. Classify each prompt:
- correction: it objects to, rejects, corrects or redirects what the agent just did, proposed or claimed, or demands it be done \
differently (in any language; also as a question such as \"why did you ...?\", or as a reminder of an earlier instruction);
- task: it asks for new or further work without objecting to the agent's last message (approving it, \"ok, go ahead\", is a task);
- neither: anything else (thanks, status questions, small talk, pasted text with no instruction of the engineer's own).
Judge only the engineer's own words. Text the engineer pasted (logs, error output, messages or documents written by others) and \
text a harness or tool inserted (system reminders, hook or tool output) are context, never a correction by themselves.";

/// `Judged::verdict` of a candidate `borg correct` set aside before it was judged.
const SET_ASIDE: &str = "set_aside";

/// One judged candidate in [`JUDGED_FILE`]. Field order is load-bearing: `known` reads the leading
/// entries (`jsonl::lead`).
#[derive(Serialize)]
struct Judged<'a> {
    session: &'a str,
    id: &'a str,
    ms: i64,
    /// `learn::words_key` of the prompt.
    w: u64,
    /// `correction`, `task`, `neither` or `set_aside`.
    verdict: &'a str,
}

/// The model's verdict on one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// It objects to or redirects what the agent just did: it becomes an episode.
    Correction,
    /// New or further work.
    Task,
    /// Anything else.
    Neither,
}

impl Verdict {
    /// The verdict a `kind` string names.
    fn from_kind(s: &str) -> Option<Self> {
        match s {
            "correction" => Some(Verdict::Correction),
            "task" => Some(Verdict::Task),
            "neither" => Some(Verdict::Neither),
            _ => None,
        }
    }

    /// As [`JUDGED_FILE`] writes it.
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Correction => "correction",
            Verdict::Task => "task",
            Verdict::Neither => "neither",
        }
    }
}

/// What the candidates of `store` hold, pending and judged, as a [`Known`]: of `session` only when given
/// (a line naming no such text is skipped unparsed), and the judged file only for a session judged
/// before (its marker, [`judged_marker`]). Never reads `episodes.jsonl`.
pub fn known(store: &Path, session: Option<&str>) -> Known {
    let mut k = Known::default();
    scan(&store.join(FILE), session, |line| add_pending(&mut k, line, session));
    if session.is_none_or(|s| judged_marker(store, s).exists()) {
        scan(&store.join(JUDGED_FILE), session, |line| add_judged(&mut k, line, session));
    }
    k
}

/// The empty file saying `session` has a line in `store`'s [`JUDGED_FILE`], so a sweep of a session
/// never judged skips that file. Under the store's own `state/`, never `BORG_STATE`: the hook and
/// `borg learn` must agree on it.
fn judged_marker(store: &Path, session: &str) -> PathBuf {
    store.join("state").join(format!("{}.judged", crate::notes::safe(session)))
}

/// Writes `session`'s [`judged_marker`]; before any judged line of it, so no line goes unmarked.
fn mark_judged(store: &Path, session: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(store.join("state"))?;
    std::fs::write(judged_marker(store, session), b"")
}

/// Each line of `path` that names `session` (every line without one). With a session, through
/// `scan_with`; without, line by line through one reused buffer (the CLI's whole-store read).
fn scan(path: &Path, session: Option<&str>, mut each: impl FnMut(&str)) {
    if let Some(s) = session {
        return scan_with(path, s, each);
    }
    let Ok(f) = std::fs::File::open(path) else { return };
    let mut r = BufReader::new(f);
    let mut line = String::new();
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => each(line.trim_end()),
        }
    }
}

/// Each line of `path` holding `session`: the file mapped (read whole only where it cannot be) and
/// searched for the session once; only the lines a hit falls in are cut out and checked as UTF-8, the
/// rest is never copied, checked or parsed.
fn scan_with(path: &Path, session: &str, mut each: impl FnMut(&str)) {
    let Ok(f) = std::fs::File::open(path) else { return };
    if !f.metadata().is_ok_and(|m| m.len() > 0) {
        return;
    }
    let finder = memchr::memmem::Finder::new(session.as_bytes());
    // SAFETY: the candidate files are only appended to, or replaced by a rename (`prune` writes
    // `FILE.tmp` and renames it over `FILE`), never truncated or rewritten in place, so the mapped bytes
    // stay valid for as long as the map lives. Populated: the whole map is read, so one pre-fault beats
    // a fault per page run in the hook's fresh process. Only a hand truncation of a mapped file (outside
    // borg) could fault the read (SIGBUS); accepted.
    match unsafe { memmap2::MmapOptions::new().populate().map(&f) } {
        Ok(map) => hits(&map, &finder, &mut each),
        Err(_) => hits(&std::fs::read(path).unwrap_or_default(), &finder, &mut each),
    }
}

/// Each line of `bytes` (whole lines) that `finder` finds in, once.
fn hits(bytes: &[u8], finder: &memchr::memmem::Finder, each: &mut impl FnMut(&str)) {
    let mut at = 0;
    while let Some(i) = finder.find(&bytes[at..]) {
        let hit = at + i;
        let start = memchr::memrchr(b'\n', &bytes[..hit]).map_or(0, |j| j + 1);
        let end = memchr::memchr(b'\n', &bytes[hit..]).map_or(bytes.len(), |j| hit + j);
        if let Ok(line) = std::str::from_utf8(&bytes[start..end]) {
            each(line.trim_end());
        }
        if end >= bytes.len() {
            return;
        }
        at = end + 1;
    }
}

/// A pending candidate's line into `k`, when of `session`.
fn add_pending(k: &mut Known, line: &str, session: Option<&str>) {
    let Some([id, ms, s]) = jsonl::lead(line, ["id", "ms", "session"]) else { return };
    let (Some(id), Some(ms), Some(s)) = (id.as_str(), ms.as_i64(), s.as_str()) else { return };
    if session.is_some_and(|want| want != s) {
        return;
    }
    let Some([c]) = jsonl::pick(line, ["correction"]) else { return };
    let Some(c) = c.as_str() else { return };
    k.add_key(id, s, ms, true, super::from_call_id(id, s), super::words_key(c));
}

/// A judged candidate's line into `k`, when of `session`.
fn add_judged(k: &mut Known, line: &str, session: Option<&str>) {
    let Some([s, id, ms, w]) = jsonl::lead(line, ["session", "id", "ms", "w"]) else { return };
    let (Some(s), Some(id), Some(ms), Some(w)) = (s.as_str(), id.as_str(), ms.as_i64(), w.as_u64()) else { return };
    if session.is_some_and(|want| want != s) {
        return;
    }
    k.add_key(id, s, ms, true, super::from_call_id(id, s), w);
}

/// Appends the hook's candidate `e` to `store`'s [`FILE`] under its lock, unread: the sweep dedupes.
///
/// # Errors
/// When it cannot be appended.
pub fn record(store: &Path, e: &Episode) -> std::io::Result<()> {
    std::fs::create_dir_all(store)?;
    let path = store.join(FILE);
    crate::live::locked(&path, || super::append_jsonl(&path, e))
}

/// `learn::record_new_in` for candidates: appends to [`FILE`] each of `eps` the store's candidates do
/// not hold yet ([`known`], of their one session when they share one), uncounted (promotion counts the
/// corrections), and returns those appended.
///
/// # Errors
/// When a candidate cannot be appended; none after it is.
pub fn record_new_in<T: AsMut<Episode>>(store: &Path, mut eps: Vec<T>, ready: impl FnMut(&mut T)) -> std::io::Result<Vec<T>> {
    let one = one_session(&mut eps);
    super::append_new_in(store, FILE, |s| known(s, one.as_deref()), eps, ready, false)
}

/// The session all of `eps` share, if they share one.
fn one_session<T: AsMut<Episode>>(eps: &mut [T]) -> Option<String> {
    let (first, rest) = eps.split_first_mut()?;
    let s = first.as_mut().session.as_str();
    rest.iter_mut().all(|x| x.as_mut().session == s).then(|| s.to_owned())
}

/// The judge prompt for `batch`: [`OPENING`], then each prompt with the end of the agent's message it
/// answers, numbered from 1.
pub fn prompt(batch: &[Episode]) -> String {
    let size = OPENING.len() + batch.iter().map(|e| agent_tail(e).len() + e.correction.len() + 96).sum::<usize>();
    let mut p = String::with_capacity(size);
    p.push_str(OPENING);
    p.push_str("\n\n");
    for (n, e) in batch.iter().enumerate() {
        let _ = write!(p, "## Prompt {}\n### End of the agent's message\n{}\n### Engineer\n{}\n\n", n + 1, agent_tail(e), e.correction);
    }
    p
}

/// The end of `e`'s agent text a prompt shows, `(none)` when blank.
fn agent_tail(e: &Episode) -> &str {
    let t = super::tail_chars(&e.agent, AGENT_CHARS);
    if t.trim().is_empty() { "(none)" } else { t }
}

/// The answer schema of a judge prompt.
pub fn schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "properties": {"verdicts": {"type": "array", "items": {
        "type": "object", "additionalProperties": false, "properties": {"prompt": {"type": "integer"},
        "kind": {"type": "string", "enum": ["correction", "task", "neither"]}}, "required": ["prompt", "kind"]}}},
        "required": ["verdicts"]})
}

/// The verdicts of an answer about `n` prompts, one slot per prompt: an item counts only with a `prompt`
/// in `1..=n` and a valid `kind`; of two for one prompt, the first.
///
/// # Errors
/// `AskFailed` when `verdicts` is no array or fills no slot.
pub fn parse(v: &Value, n: usize) -> Result<Vec<Option<Verdict>>, AskFailed> {
    let items = v["verdicts"].as_array().ok_or(AskFailed)?;
    let mut out = vec![None; n];
    for it in items {
        let Some(kind) = it["kind"].as_str().and_then(Verdict::from_kind) else { continue };
        let slot = it["prompt"].as_u64().and_then(|p| usize::try_from(p).ok()?.checked_sub(1)).and_then(|i| out.get_mut(i));
        if let Some(slot) = slot {
            slot.get_or_insert(kind);
        }
    }
    if out.iter().all(Option::is_none) { Err(AskFailed) } else { Ok(out) }
}

/// One model call judging `batch`.
///
/// # Errors
/// `AskFailed` when the model gave no usable answer.
pub fn judge_batch(batch: &[Episode], ask: &dyn Fn(&str) -> Option<Value>) -> Result<Vec<Option<Verdict>>, AskFailed> {
    parse(&ask(&prompt(batch)).ok_or(AskFailed)?, batch.len())
}

/// The model calls judging `eps` takes: one per [`BATCH`] of each session's.
pub fn calls<'a>(eps: impl IntoIterator<Item = &'a Episode>) -> usize {
    let mut per: HashMap<&str, usize> = HashMap::new();
    for e in eps {
        *per.entry(e.session.as_str()).or_default() += 1;
    }
    per.values().map(|n| n.div_ceil(BATCH)).sum()
}

/// `borg correct`'s set-aside of the pending candidate `id`: judged `set_aside` and pruned, under the
/// lock of [`FILE`]. Whether it was pending.
///
/// # Errors
/// When the judged line or the pruned file cannot be written.
pub fn set_aside(store: &Path, id: &str) -> std::io::Result<bool> {
    let path = store.join(FILE);
    crate::live::locked(&path, || Ok(set_aside_unlocked(store, &path, |l| lead_id(l).is_some_and(|i| i == id), |_| true)?.is_some()))
}

/// `borg correct`'s set-aside of `session`'s latest prompt `prompt` (written at `ms`) that no hook recorded
/// (under the model gate the turn-end sweep captures Claude's, omp's and opencode's prompts, after the
/// drone's `borg correct`): the pending candidate of the same words is set aside ([`set_aside`]), else a
/// judged `set_aside` line holds its words, so the sweep never captures them and no model judges them.
/// Returns the id set aside.
///
/// # Errors
/// When the judged line or the pruned file cannot be written.
pub fn set_aside_prompt(store: &Path, session: &str, prompt: &str, ms: i64) -> std::io::Result<String> {
    // As the transcript replay cuts it (assimilate.rs `Replay::prompt`), so the words key matches.
    let w = super::words_key(super::head_chars(prompt.trim(), super::EXCERPT_CHARS));
    let path = store.join(FILE);
    crate::live::locked(&path, || {
        if let Some(id) = set_aside_unlocked(store, &path, |l| l.contains(session), |e| e.session == session && super::words_key(&e.correction) == w)? {
            return Ok(id);
        }
        let id = format!("{}-set-aside-{ms}", super::head_chars(session, 8));
        mark_judged(store, session)?;
        super::append_jsonl(&store.join(JUDGED_FILE), &Judged { session, id: &id, ms, w, verdict: SET_ASIDE })?;
        Ok(id)
    })
}

/// The first pending candidate in `path` whose line passes `line` and whose episode passes `which`:
/// judged `set_aside` and pruned. Its id, if one was pending. Under [`FILE`]'s lock.
fn set_aside_unlocked(store: &Path, path: &Path, line: impl Fn(&str) -> bool, which: impl Fn(&Episode) -> bool) -> std::io::Result<Option<String>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let Some(e) = text.lines().filter(|l| line(l)).filter_map(|l| serde_json::from_str::<Episode>(l).ok()).find(|e| which(e)) else {
        return Ok(None);
    };
    let j = Judged { session: &e.session, id: &e.id, ms: e.ms, w: super::words_key(&e.correction), verdict: SET_ASIDE };
    mark_judged(store, &e.session)?;
    super::append_jsonl(&store.join(JUDGED_FILE), &j)?;
    prune(path, &HashSet::from([e.id.as_str()]))?;
    Ok(Some(e.id))
}

/// The `id` a line leads with.
fn lead_id(line: &str) -> Option<Cow<'_, str>> {
    match jsonl::lead(line, ["id"]) {
        Some([jsonl::Scalar::Str(id)]) => Some(id),
        _ => None,
    }
}

/// Rewrites `path` without the lines of the ids in `gone` (a temp file renamed over it); lines appended
/// since it was read last stay.
fn prune(path: &Path, gone: &HashSet<&str>) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut kept = String::with_capacity(text.len());
    for line in text.lines().filter(|l| !lead_id(l).is_some_and(|i| gone.contains(i.as_ref()))) {
        kept.push_str(line);
        kept.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, kept)?;
    std::fs::rename(&tmp, path)
}

/// A pending candidate's leading fields and its line's bytes in [`FILE`].
struct Head<'t> {
    id: Cow<'t, str>,
    ms: i64,
    session: Cow<'t, str>,
    line: Range<usize>,
}

/// The pending candidates in `text` (a read of [`FILE`]) not yet in [`JUDGED_FILE`] (only after a crash
/// between its append and the prune), and the ids found judged.
fn pending<'t>(home: &Path, text: &'t str) -> (Vec<Head<'t>>, HashSet<String>) {
    let mut heads = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let range = at..at + line.trim_end().len();
        at += line.len();
        if let Some([jsonl::Scalar::Str(id), ms, jsonl::Scalar::Str(session)]) = jsonl::lead(&text[range.clone()], ["id", "ms", "session"])
            && let Some(ms) = ms.as_i64()
        {
            heads.push(Head { id, ms, session, line: range });
        }
    }
    if heads.is_empty() {
        return (heads, HashSet::new());
    }
    let ids: HashSet<&str> = heads.iter().map(|h| h.id.as_ref()).collect();
    let mut judged = HashSet::new();
    scan(&home.join(JUDGED_FILE), None, |line| {
        if let Some([_, jsonl::Scalar::Str(id)]) = jsonl::lead(line, ["session", "id"])
            && ids.contains(id.as_ref())
        {
            judged.insert(id.into_owned());
        }
    });
    heads.retain(|h| !judged.contains(h.id.as_ref()));
    (heads, judged)
}

/// Indices into `heads` this run judges, per session in ms order: those of `session` (all without one)
/// and of quiet sessions (learn.rs `quiet`).
fn select<'h>(home: &Path, heads: &'h [Head<'_>], session: Option<&str>, now: i64) -> BTreeMap<&'h str, Vec<usize>> {
    // `quiet` reads only a candidate's session and time here: nothing is done or skipped yet.
    let lite: Vec<Episode> = heads.iter().map(|h| Episode { ms: h.ms, session: h.session.as_ref().to_owned(), ..Episode::default() }).collect();
    let quiet = super::quiet(&lite, session, &HashSet::new(), &HashSet::new(), now, |s| super::open(&super::ctx_path(home, s), now));
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, h) in heads.iter().enumerate() {
        let s = h.session.as_ref();
        if session.is_none_or(|want| want == s) || quiet.contains(s) {
            groups.entry(s).or_default().push(i);
        }
    }
    for idx in groups.values_mut() {
        idx.sort_by_key(|&i| heads[i].ms);
    }
    groups
}

/// One judge run's progress: chunks asked, failures in a row, candidates left pending.
#[derive(Default)]
struct Run {
    done: usize,
    total: usize,
    failures_in_row: usize,
    left: usize,
}

/// `borg learn`'s first step: judges the pending candidates of `session` (all without one) and of quiet
/// sessions in [`BATCH`]es, promotes the corrections to episodes, and marks each verdict judged. Never
/// holds [`FILE`]'s lock during a model call (the prompt hook takes it). Returns the candidates left
/// pending after failed calls.
pub(super) fn judge(home: &Path, session: Option<&str>, ask: &dyn Fn(&str) -> Option<Value>, now: i64, report: &mut dyn FnMut(Step)) -> usize {
    let path = home.join(FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return 0 };
    let (heads, stale) = pending(home, &text);
    if !stale.is_empty() {
        let gone: HashSet<&str> = stale.iter().map(String::as_str).collect();
        // Best effort: a failed prune leaves them, and the next run drops them again.
        let _ = crate::live::locked(&path, || prune(&path, &gone));
    }
    let groups = select(home, &heads, session, now);
    let mut run = Run { total: groups.values().map(|v| v.len().div_ceil(BATCH)).sum(), ..Run::default() };
    for (s, idx) in &groups {
        for chunk in idx.chunks(BATCH) {
            let batch: Vec<Episode> = chunk.iter().filter_map(|&i| serde_json::from_str(&text[heads[i].line.clone()]).ok()).collect();
            // A line whose leading fields read but whose episode does not stays pending, counted.
            run.left += chunk.len() - batch.len();
            if !batch.is_empty() {
                judge_chunk(home, s, &batch, ask, &mut run, report);
            }
        }
    }
    run.left
}

/// Judges one chunk of a session's candidates, unless the run already stopped asking (`MAX_FAILURES`).
fn judge_chunk(home: &Path, session: &str, batch: &[Episode], ask: &dyn Fn(&str) -> Option<Value>, run: &mut Run, report: &mut dyn FnMut(Step)) {
    if run.failures_in_row >= MAX_FAILURES {
        run.left += batch.len();
        return;
    }
    let started = std::time::Instant::now();
    let verdicts = judge_batch(batch, ask);
    run.done += 1;
    let what = match &verdicts {
        Err(AskFailed) => "no answer from the model, left pending".to_owned(),
        Ok(v) => {
            let corrections = v.iter().filter(|x| **x == Some(Verdict::Correction)).count();
            format!("judged {} prompts of {}: {corrections} corrections", batch.len(), super::head_chars(session, 8))
        }
    };
    report(Step { done: run.done, total: run.total, verdict: &what, secs: started.elapsed().as_secs_f64() });
    match verdicts {
        Err(AskFailed) => {
            run.left += batch.len();
            run.failures_in_row += 1;
        }
        Ok(v) => {
            run.failures_in_row = 0;
            let open = v.iter().filter(|x| x.is_none()).count();
            run.left += if settle(home, batch, &v).is_ok() { open } else { batch.len() };
        }
    }
}

/// Promotes `batch`'s corrections to episodes (learn.rs `record_new_in`: deduped, counted), then marks
/// each verdict judged and prunes it from [`FILE`]. An unfilled slot stays pending.
///
/// # Errors
/// When the promotion fails (nothing is marked judged then), or the judged lines or the prune.
fn settle(home: &Path, batch: &[Episode], verdicts: &[Option<Verdict>]) -> std::io::Result<()> {
    let promote: Vec<Episode> =
        batch.iter().zip(verdicts).filter(|(_, v)| **v == Some(Verdict::Correction)).map(|(e, _)| Episode { kind: String::new(), ..e.clone() }).collect();
    super::record_new_in(home, promote, |_| {})?;
    let path = home.join(FILE);
    let judged_path = home.join(JUDGED_FILE);
    crate::live::locked(&path, || {
        if let Some(e) = batch.first()
            && verdicts.iter().any(Option::is_some)
        {
            mark_judged(home, &e.session)?;
        }
        let mut gone = HashSet::with_capacity(batch.len());
        for (e, v) in batch.iter().zip(verdicts) {
            let Some(v) = v else { continue };
            let j = Judged { session: &e.session, id: &e.id, ms: e.ms, w: super::words_key(&e.correction), verdict: v.as_str() };
            super::append_jsonl(&judged_path, &j)?;
            gone.insert(e.id.as_str());
        }
        prune(&path, &gone)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;

    use super::*;
    use crate::learn::{head_chars, learn_in, tail_chars, transcript_id, words_key};

    fn temp_home(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-candidates-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cand(session: &str, ms: i64, correction: &str) -> Episode {
        Episode {
            id: format!("{}-{ms}", head_chars(session, 8)),
            ms,
            repo: "r".into(),
            session: session.into(),
            task: "fix the merge lookup".into(),
            agent: "I will retry with a fixed pause".into(),
            correction: correction.into(),
            kind: KIND.into(),
            ..Episode::default()
        }
    }

    fn put(home: &Path, eps: &[Episode]) {
        for e in eps {
            record(home, e).unwrap();
        }
    }

    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// A lesson for any learn prompt; for a judge prompt, `kinds[n]` for prompt n + 1.
    fn answering(kinds: &[&str]) -> impl Fn(&str) -> Option<Value> {
        let kinds: Vec<String> = kinds.iter().map(|k| (*k).to_owned()).collect();
        move |p: &str| {
            if p.starts_with(OPENING) {
                let v: Vec<Value> = kinds.iter().enumerate().map(|(i, k)| json!({"prompt": i + 1, "kind": k})).collect();
                return Some(json!({"verdicts": v}));
            }
            Some(json!({"is_lesson": true, "question": "Q backoff", "situations": [], "consequence": "c", "same_as": "", "supersedes": []}))
        }
    }

    fn three(home: &Path) -> Vec<Episode> {
        let eps = vec![cand("sess-one", 1_000, "nein, nimm Backoff mit Jitter"), cand("sess-one", 2_000, "now add a test"), cand("sess-one", 3_000, "thanks")];
        put(home, &eps);
        eps
    }

    #[test]
    fn judged_corrections_become_episodes_and_every_candidate_leaves_the_pending_file() {
        let home = temp_home("judged");
        three(&home);
        let got = learn_in(&home, Some("sess-one"), &answering(&["correction", "task", "neither"]), false, 10_000);
        assert_eq!(got, (1, 0, 0));
        assert!(lines(&home.join(FILE)).is_empty());
        assert_eq!(lines(&home.join(JUDGED_FILE)).len(), 3);
        let eps: Vec<Episode> = lines(&home.join("episodes.jsonl")).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(eps.len(), 1);
        assert_eq!((eps[0].kind.as_str(), eps[0].correction.as_str()), ("", "nein, nimm Backoff mit Jitter"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_failed_judge_call_keeps_every_candidate_pending_and_counts_it_waiting() {
        let home = temp_home("failed");
        three(&home);
        assert_eq!(learn_in(&home, Some("sess-one"), &|_| None, false, 10_000), (0, 0, 3));
        assert_eq!(lines(&home.join(FILE)).len(), 3);
        assert!(!home.join(JUDGED_FILE).exists());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_prompt_the_answer_left_out_stays_pending() {
        let home = temp_home("partial");
        let eps = [cand("sess-one", 1_000, "go on"), cand("sess-one", 2_000, "and the docs")];
        put(&home, &eps);
        assert_eq!(learn_in(&home, Some("sess-one"), &answering(&["task"]), false, 10_000), (0, 0, 1));
        let left = lines(&home.join(FILE));
        assert_eq!(left.len(), 1);
        assert!(left[0].contains(&eps[1].id));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn twenty_five_candidates_of_one_session_take_two_calls() {
        let home = temp_home("batches");
        let eps: Vec<Episode> = (0..25).map(|i| cand("sess-one", 1_000 + i * 10_000, &format!("prompt {i}"))).collect();
        put(&home, &eps);
        let calls = Cell::new(0);
        let inner = answering(&["neither"; BATCH]);
        let ask = |p: &str| {
            if p.starts_with(OPENING) {
                calls.set(calls.get() + 1);
            }
            inner(p)
        };
        learn_in(&home, Some("sess-one"), &ask, false, 1_000_000);
        assert_eq!(calls.get(), 2);
        assert_eq!(super::calls(&eps), 2);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_judged_prompt_read_again_from_the_transcript_is_held_only_for_its_session() {
        let home = temp_home("known");
        three(&home);
        learn_in(&home, Some("sess-one"), &answering(&["correction", "task", "neither"]), false, 10_000);
        let again = Episode { id: transcript_id("sess-one", "", 900_000), ms: 900_000, ..cand("sess-one", 900_000, "nein,  nimm Backoff\tmit Jitter") };
        assert!(known(&home, Some("sess-one")).holds(&again));
        assert!(known(&home, None).holds(&again));
        assert!(!known(&home, Some("other-session")).holds(&again));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_pending_candidate_recorded_again_is_not_appended_twice() {
        let home = temp_home("record-new");
        let e = cand("sess-one", 1_000, "nein, so nicht");
        assert_eq!(record_new_in(&home, vec![e.clone()], |_| {}).unwrap().len(), 1);
        assert!(record_new_in(&home, vec![e], |_| {}).unwrap().is_empty());
        assert_eq!(lines(&home.join(FILE)).len(), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_set_aside_candidate_is_judged_set_aside_and_pruned() {
        let home = temp_home("set-aside");
        let eps = three(&home);
        assert!(set_aside(&home, &eps[0].id).unwrap());
        assert!(!set_aside(&home, &eps[0].id).unwrap());
        assert_eq!(lines(&home.join(FILE)).len(), 2);
        assert!(lines(&home.join(JUDGED_FILE))[0].contains(SET_ASIDE));
        assert!(known(&home, Some("sess-one")).holds(&eps[0]));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_session_without_a_judged_marker_skips_the_judged_file() {
        let home = temp_home("marker");
        let e = cand("sess-x", 1_000, "nein, so nicht");
        let j = Judged { session: &e.session, id: &e.id, ms: e.ms, w: words_key(&e.correction), verdict: "correction" };
        crate::learn::append_jsonl(&home.join(JUDGED_FILE), &j).unwrap();
        assert!(!known(&home, Some("sess-x")).holds(&e));
        assert!(known(&home, None).holds(&e));
        mark_judged(&home, "sess-x").unwrap();
        assert!(known(&home, Some("sess-x")).holds(&e));
        assert!(known(&home, None).holds(&e));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_restated_prompt_the_sweep_has_not_captured_yet_is_never_captured_after() {
        let home = temp_home("set-aside-prompt");
        let id = set_aside_prompt(&home, "sess-one", "  nein, nimm  Backoff mit Jitter\n", 5_000).unwrap();
        assert!(lines(&home.join(JUDGED_FILE))[0].contains(&id));
        // The turn-end sweep reads the same prompt from the transcript afterwards: held, never appended.
        let swept = Episode { id: transcript_id("sess-one", "", 60_000), ms: 60_000, ..cand("sess-one", 60_000, "nein, nimm Backoff mit Jitter") };
        assert!(record_new_in(&home, vec![swept], |_| {}).unwrap().is_empty());
        assert!(!known(&home, Some("sess-one")).holds(&cand("sess-one", 90_000, "now add a test")), "only those words");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_restated_prompt_the_sweep_already_captured_is_set_aside_and_pruned() {
        let home = temp_home("set-aside-pending");
        let eps = three(&home);
        let id = set_aside_prompt(&home, "sess-one", "now add a test", 9_000).unwrap();
        assert_eq!(id, eps[1].id);
        assert!(!lines(&home.join(FILE)).iter().any(|l| l.contains(&eps[1].id)));
        assert_eq!(lines(&home.join(FILE)).len(), 2);
        assert!(lines(&home.join(JUDGED_FILE))[0].contains(SET_ASIDE));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn words_key_ignores_whitespace_but_not_words() {
        assert_eq!(words_key("no,  use\tthe API"), words_key("no, use the API"));
        assert_ne!(words_key("no, use the APIs"), words_key("no, use the API"));
    }

    #[test]
    fn a_sessions_lines_are_found_once_each_the_last_without_a_newline_too() {
        let home = temp_home("scan");
        let path = home.join("lines.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut got = Vec::new();
        scan_with(&path, "sess-target", |l| got.push(l.to_owned()));
        assert!(got.is_empty(), "an empty file has no lines");
        let text = "sess-target first\na-other-1\n{\"session\":\"sess-target\",\"n\":1}\nx\nother\nend sess-target sess-target";
        std::fs::write(&path, text).unwrap();
        scan_with(&path, "sess-target", |l| got.push(l.to_owned()));
        assert_eq!(got, ["sess-target first", "{\"session\":\"sess-target\",\"n\":1}", "end sess-target sess-target"]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn words_key_of_a_text_past_its_buffer_is_blake3_of_the_joined_words() {
        let long_word = "x".repeat(5_000);
        let text = format!("{}  {long_word}\n{}", "word ".repeat(1_500), "tail ".repeat(10));
        let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let digest = blake3::hash(joined.as_bytes());
        let want = u64::from_le_bytes(digest.as_bytes()[..8].try_into().unwrap());
        assert_eq!(words_key(&text), want);
    }

    #[test]
    fn head_and_tail_chars_cut_on_char_boundaries() {
        let s = "ä€😀x";
        assert_eq!([0, 1, 3, 9].map(|n| head_chars(s, n)), ["", "ä", "ä€😀", s]);
        assert_eq!([0, 1, 3, 9].map(|n| tail_chars(s, n)), ["", "x", "€😀x", s]);
    }

    #[test]
    fn parse_rejects_an_error_and_keeps_the_first_valid_verdict_per_prompt() {
        assert_eq!(parse(&json!({"error": "quota"}), 2), Err(AskFailed));
        assert_eq!(parse(&json!({"verdicts": [{"prompt": 3, "kind": "task"}, {"prompt": 0, "kind": "task"}]}), 2), Err(AskFailed));
        let v = json!({"verdicts": [{"prompt": 2, "kind": "bogus"}, {"prompt": 2, "kind": "task"}, {"prompt": 2, "kind": "correction"}]});
        assert_eq!(parse(&v, 2), Ok(vec![None, Some(Verdict::Task)]));
    }

    #[test]
    fn the_judge_prompt_numbers_each_prompt_with_its_agent_tail() {
        let p = prompt(&[cand("s", 1, "nein"), Episode { agent: " ".into(), ..cand("s", 2, "ok") }]);
        assert!(p.starts_with(OPENING));
        assert!(p.contains("## Prompt 1\n### End of the agent's message\nI will retry with a fixed pause\n### Engineer\nnein\n\n"));
        assert!(p.contains("## Prompt 2\n### End of the agent's message\n(none)\n### Engineer\nok\n\n"));
        assert_eq!(crate::learn::schema_for(&p), schema());
    }
}
