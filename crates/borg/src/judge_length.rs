//! The judge's effective length (0.7.6): the largest prompt the consequence model still judges about as
//! well as a short one, measured per model on its own task from the judge's cost lines, and the
//! session-end judge's prompt budget from it (hook.rs `end_budget`).
//!
//! **Definition** (NoLiMa, ICML 2025, arXiv 2502.05167: a model's effective length is the longest
//! context at which it keeps at least 85% of its short-context score). The score here is the judge's
//! *answered share*: of the lessons a session-end judge call was sent (`pending=`), those it gave a
//! verdict other than unclear (changed, ignored, not arisen, consequence remains); a failed call
//! (`judge-failed`) sent its lessons and answered none. Calls are grouped by their prompt's characters
//! into ×`STEP` size steps whose upper edges are `END_START_BUDGET` × `STEP`^j (j may be negative:
//! 32,000, 48,000, 72,000, 108,000, …). The reference is the smallest steps pooled until they hold
//! `REFERENCE_LESSONS` sent lessons in `MIN_CALLS` calls; a step above it **holds** when the lower bound
//! of its answered share's 95% interval is at least `HOLD` × the reference's share, **fails** when the
//! upper bound is below that line, and is **open** while its interval still straddles the line or it has
//! fewer than `MIN_CALLS` calls (it counts for neither). The interval is Wilson's over the step's
//! effective number of lessons: the lessons of one call share one prompt, one cut of the work and one
//! answer, so they are not independent trials; the spread of the calls' shares around the step's share
//! sets how many independent lessons they are worth (a design effect, Kish), never more than were sent.
//!
//! **Budget.** It starts at `END_START_BUDGET`'s step. While the step above the budget holds, the budget
//! moves up to it; while its own step fails, it moves down (never below the reference). A share of
//! session-end passes, drawn per pass and logged as `psi=`, uses the step above: `EXPLORE_SHARE` while
//! it is open, so it gathers the evidence that decides it, and `FAILED_EXPLORE_SHARE` once it failed, so
//! a judge that reads longer prompts better later (or a step failed by early bad luck) can still move the
//! budget back up. No ceiling but the model's window: the 85% line is the rot detector.
//!
//! **Lines read** (`<state>/*.cost`, hook.rs `judge_line`): the session-end judge's lines (` at=end`,
//! 0.7.5 on), whose `chars=` is its whole prompt (0.7.5 counted bytes, 0.7.6 characters), and its failed
//! calls' `judge-failed` lines (0.7.6 on). A check's judge line is left out: a check judges an unfinished
//! session beside its own grading, a different task whose share says nothing of the session-end judge's
//! (its `prompt=`, 0.7.6 on, is kept for a check's own budget, docs/roadmap.md). The answered lessons
//! are the `verdicts=` counts other than `unclear`; a line before 0.7.5 has none and gives `resolved=`
//! (changed, ignored, not arisen): a lower bound, since it leaves out a remaining consequence. A line
//! names its judge (`model=`, 0.7.6 on); one before names none and counts as the judge this machine runs
//! by default (`hook::judge_model("")`), the one those versions ran unless `BORG_PREDICT_MODEL` pinned
//! another.
//!
//! **Cache.** The counts per model and step are kept in `<state>/judge-length.json` (`CACHE`, format
//! `VERSION`), with the size and newest change of the `.cost` files they were read from: read as long as
//! those are unchanged, rebuilt otherwise. The decision itself is recomputed from the counts on every
//! read (`Length::of`), so a change of the rule never needs a new cache.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hook::END_START_BUDGET;

/// The ratio between consecutive size steps. Method: NoLiMa's context lengths grow by about ×2; ×1.5 is
/// finer, so the budget moves by at most half again per decision (0.7.6 pre-registration, 2026-11-01).
pub const STEP: f64 = 1.5;

/// A step holds while its answered share's lower bound stays at least this share of the reference's.
/// Method: NoLiMa's effective-length threshold (85% of the short-context score), ICML 2025.
pub const HOLD: f64 = 0.85;

/// The fewest sent lessons the reference holds (the smallest steps pooled until they do), so one call's
/// few lessons never set the line every other step is judged by. Guard: about one session's open
/// deliveries at the 90th percentile (32–78 on copies of three stores, 2026-10-31) is more than one
/// call; 30 makes the reference's share known to about ±18 points at a 50% share (Wilson, 95%).
pub const REFERENCE_LESSONS: u64 = 30;

/// The fewest calls a size step (and the reference) needs before it is decided. Guard: the lessons of
/// one call are not independent (one prompt, one answer), so a step's interval rests on the spread
/// between its calls (the design effect, module doc), which two or three calls do not show; one bad
/// batch never decides a step.
pub const MIN_CALLS: u64 = 5;

/// The chance a session-end pass uses the step above the budget while that step is open, drawn per pass
/// and logged (`psi=`). Baseline: one pass in five; most passes run at the measured budget while the
/// step above gathers evidence. Once the step holds, the budget moves up and the next step is explored;
/// once it fails, `FAILED_EXPLORE_SHARE`.
pub const EXPLORE_SHARE: f64 = 0.2;

/// The chance a session-end pass uses the step above the budget once that step failed, drawn and
/// logged like `EXPLORE_SHARE`. Baseline: a quarter of it (one pass in twenty), so a failed step still
/// gathers evidence (only the session-end judge's own calls reach it) and can hold again, at little
/// cost to the passes that read at the measured budget.
pub const FAILED_EXPLORE_SHARE: f64 = EXPLORE_SHARE / 4.0;

/// Method: the normal quantile of a 95% interval (Wilson's score interval).
const Z95: f64 = 1.96;

/// The cache file in the state directory.
pub const CACHE: &str = "judge-length.json";

/// The cache's format: `{"version", "stamp", "models": {label: [[step, tally], …]}}` (`Tally`), the
/// label "" for lines that name no model. Protocol: a cache of another version is rebuilt (1: sent and
/// answered only, with check lines; 2: the session-end judge's lines, with calls and their spread).
pub const VERSION: u32 = 2;

/// The upper edge of size step `j`, in prompt characters: `END_START_BUDGET` × `STEP`^j, rounded.
#[must_use]
pub fn edge(j: i32) -> usize {
    (END_START_BUDGET as f64 * STEP.powi(j)).round() as usize
}

/// The size step a prompt of `chars` characters falls in: the smallest j whose edge it does not exceed.
#[must_use]
pub fn step_of(chars: usize) -> i32 {
    let mut j = 0;
    while chars > edge(j) {
        j += 1;
    }
    while chars <= edge(j - 1) && edge(j - 1) > 0 {
        j -= 1;
    }
    j
}

/// One session-end judge call as its cost line gives it (`parse`).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// The judge's label (`model=`), empty when the line names none.
    pub model: String,
    /// The prompt's characters.
    pub chars: usize,
    /// Lessons sent, and those that came back with a verdict other than unclear.
    pub sent: u64,
    pub answered: u64,
}

/// A session-end judge cost line's call (see the module doc for each format); None for another line (a
/// check's judge line among them), or one that sent no lesson.
#[must_use]
pub fn parse(line: &str) -> Option<Call> {
    let (rest, failed) = match line.strip_prefix("judge ") {
        Some(rest) => (rest, false),
        None => (line.strip_prefix("judge-failed ")?, true),
    };
    let field = |k: &str| rest.split_whitespace().find_map(|w| w.strip_prefix(k));
    let num = |k: &str| field(k).and_then(|v| v.parse::<u64>().ok());
    let sent = num("pending=").filter(|n| *n > 0)?;
    if field("at=") != Some(crate::outcome::AT_END) {
        return None;
    }
    let chars = num("chars=")? as usize;
    let answered = match field("verdicts=") {
        _ if failed => 0,
        Some(v) => v
            .split(',')
            .filter_map(|p| p.split_once(':'))
            .filter(|(word, _)| *word != crate::predict::Verdict::Unclear.word())
            .filter_map(|(_, n)| n.parse::<u64>().ok())
            .sum(),
        None => num("resolved=").unwrap_or(0),
    };
    Some(Call { model: field("model=").unwrap_or_default().to_string(), chars, sent, answered: answered.min(sent) })
}

/// A size step's calls: lessons sent and answered, the calls, and the sums the spread of the calls'
/// shares is computed from (`interval`): Σ sent², Σ answered², Σ sent × answered over the calls.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub sent: u64,
    pub answered: u64,
    pub calls: u64,
    pub sent_sq: u64,
    pub answered_sq: u64,
    pub cross: u64,
}

impl Tally {
    /// One call's lessons.
    pub fn add(&mut self, sent: u64, answered: u64) {
        self.sent += sent;
        self.answered += answered;
        self.calls += 1;
        self.sent_sq += sent * sent;
        self.answered_sq += answered * answered;
        self.cross += sent * answered;
    }

    /// Two tallies as one (calls pooled).
    pub fn merge(&mut self, o: &Tally) {
        self.sent += o.sent;
        self.answered += o.answered;
        self.calls += o.calls;
        self.sent_sq += o.sent_sq;
        self.answered_sq += o.answered_sq;
        self.cross += o.cross;
    }

    /// The answered share (0 without lessons).
    #[must_use]
    pub fn share(&self) -> f64 {
        if self.sent == 0 { 0.0 } else { self.answered as f64 / self.sent as f64 }
    }

    /// How many independent lessons the calls are worth: the sent lessons divided by the design effect,
    /// which the spread of the calls' shares around the step's share gives (the ratio estimator's
    /// variance between calls, Kish), never more than were sent. Fewer than two calls show no spread:
    /// the lessons sent.
    #[must_use]
    pub fn effective(&self) -> f64 {
        let n = self.sent as f64;
        let p = self.share();
        if self.calls < 2 || p <= 0.0 || p >= 1.0 {
            return n;
        }
        // Σ (answered_i − p × sent_i)² over the calls.
        let spread = (self.answered_sq as f64 - 2.0 * p * self.cross as f64 + p * p * self.sent_sq as f64).max(0.0);
        let c = self.calls as f64;
        let var = c / (c - 1.0) * spread / (n * n);
        if var <= 0.0 { n } else { (p * (1.0 - p) / var).min(n) }
    }

    /// The answered share's 95% Wilson interval over the effective lessons (`effective`).
    #[must_use]
    pub fn interval(&self) -> (f64, f64) {
        wilson_of(self.share(), self.effective())
    }
}

/// Size step → its calls.
pub type Steps = BTreeMap<i32, Tally>;

/// The cache's content: the counts per judge label ("" for lines naming none) and the stamp of the
/// `.cost` files they were read from.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Cache {
    version: u32,
    stamp: String,
    models: BTreeMap<String, Vec<(i32, Tally)>>,
}

/// The `.cost` files of `state`: their count, total size and newest change. Changes when a line is
/// added, a file appears or goes.
fn stamp(state: &Path) -> String {
    let (mut files, mut bytes, mut newest) = (0u64, 0u64, 0u128);
    for e in std::fs::read_dir(state).into_iter().flatten().flatten() {
        if e.path().extension().is_some_and(|x| x == "cost") {
            let Ok(m) = e.metadata() else { continue };
            files += 1;
            bytes += m.len();
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
            newest = newest.max(t);
        }
    }
    format!("{files}:{bytes}:{newest}")
}

/// The counts of every judge line in `state`'s `.cost` files, per judge label.
fn count(state: &Path) -> BTreeMap<String, Steps> {
    let mut out: BTreeMap<String, Steps> = BTreeMap::new();
    for e in std::fs::read_dir(state).into_iter().flatten().flatten() {
        if e.path().extension().is_none_or(|x| x != "cost") {
            continue;
        }
        for c in std::fs::read_to_string(e.path()).unwrap_or_default().lines().filter_map(parse) {
            out.entry(c.model).or_default().entry(step_of(c.chars)).or_default().add(c.sent, c.answered);
        }
    }
    out
}

/// The counts per judge label of `state` (`count`), from the cache while the `.cost` files are unchanged,
/// else counted again and cached (written whole, then renamed in place; a failed write only costs the
/// next reader a recount).
#[must_use]
pub fn counts(state: &Path) -> BTreeMap<String, Steps> {
    let path = state.join(CACHE);
    let now = stamp(state);
    if let Some(c) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Cache>(&t).ok())
        && c.version == VERSION
        && c.stamp == now
    {
        return c.models.into_iter().map(|(m, v)| (m, v.into_iter().collect())).collect();
    }
    let counted = count(state);
    let cache = Cache { version: VERSION, stamp: now, models: counted.iter().map(|(m, s)| (m.clone(), s.iter().map(|(j, t)| (*j, *t)).collect())).collect() };
    if let Ok(text) = serde_json::to_string(&cache) {
        // A temp file of this writer's own: borg web's threads may recount at once.
        static WRITER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = state.join(format!("{CACHE}.{}.{}", std::process::id(), WRITER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    counted
}

/// The 95% Wilson score interval of `k` successes in `n` trials; (0, 1) without any.
#[must_use]
pub fn wilson(k: u64, n: u64) -> (f64, f64) {
    wilson_of(if n == 0 { 0.0 } else { k as f64 / n as f64 }, n as f64)
}

/// The 95% Wilson score interval of share `p` over `n` (possibly effective, `Tally::effective`) trials;
/// (0, 1) without any.
#[must_use]
pub fn wilson_of(p: f64, n: f64) -> (f64, f64) {
    if n <= 0.0 {
        return (0.0, 1.0);
    }
    let z2 = Z95 * Z95;
    let centre = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / (1.0 + z2 / n);
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// What the evidence says of one size step (`Step::decision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The reference: the step where the smallest steps pooled reach `REFERENCE_LESSONS` sent in
    /// `MIN_CALLS` calls; the line is `HOLD` × their share.
    Reference,
    /// Below the reference, pooled into it (no line to compare a smaller step with).
    Below,
    /// Its lower bound is at or above the line.
    Holds,
    /// Its upper bound is below the line.
    Fails,
    /// Its interval straddles the line, it has fewer than `MIN_CALLS` calls, or no reference is known
    /// yet: counts for neither.
    Open,
}

/// One size step of a judge's evidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    pub step: i32,
    /// Its upper edge: prompts up to this many characters (above the step below's edge).
    pub upto: usize,
    pub sent: u64,
    pub answered: u64,
    /// The calls they were sent in.
    pub calls: u64,
    /// The answered share's 95% Wilson interval over the effective lessons (`Tally::interval`).
    pub lo: f64,
    pub hi: f64,
    pub decision: Decision,
}

/// One judge's effective length and the session-end budget it sets (module doc).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Length {
    /// The judge's label (runner.rs `Choice::label`).
    pub model: String,
    /// The budget: a session-end batch's prompt characters at most.
    pub chars: usize,
    /// Whether it is still the start (`END_START_BUDGET`): no decided step moved it.
    pub start: bool,
    /// The line a step's lower bound must reach (`HOLD` × the reference's share); None without a reference.
    pub line: Option<f64>,
    /// The steps with evidence, smallest first, and the explored step when it has none yet.
    pub steps: Vec<Step>,
    /// The step above the budget a share of passes explores (its edge).
    pub explore: Option<usize>,
    /// The chance a session-end pass explores it: `EXPLORE_SHARE` while it is open,
    /// `FAILED_EXPLORE_SHARE` once it failed (0 when none is explored).
    pub psi: f64,
}

impl Length {
    /// The budget from `steps`, a judge's counts (module doc).
    #[must_use]
    pub fn of(model: &str, steps: &Steps) -> Length {
        // The reference: the smallest steps pooled until they hold enough lessons in enough calls.
        let mut pooled = Tally::default();
        let reference = steps.iter().find_map(|(j, t)| {
            pooled.merge(t);
            (pooled.sent >= REFERENCE_LESSONS && pooled.calls >= MIN_CALLS).then_some((*j, pooled.share()))
        });
        let line = reference.map(|(_, share)| HOLD * share);
        let decide = |j: i32| -> Decision {
            let Some((r, _)) = reference else { return Decision::Open };
            match j.cmp(&r) {
                std::cmp::Ordering::Less => return Decision::Below,
                std::cmp::Ordering::Equal => return Decision::Reference,
                std::cmp::Ordering::Greater => {}
            }
            let t = steps.get(&j).copied().unwrap_or_default();
            if t.calls < MIN_CALLS {
                return Decision::Open;
            }
            let (lo, hi) = t.interval();
            let l = line.unwrap_or(0.0);
            if lo >= l {
                Decision::Holds
            } else if hi < l {
                Decision::Fails
            } else {
                Decision::Open
            }
        };
        // Down while its own step fails (never below the reference), else up while the next one holds; a
        // step at or below the reference holds by definition (the reference is the short-context score).
        let holds = |j: i32| matches!(decide(j), Decision::Holds | Decision::Reference | Decision::Below);
        let mut j = 0;
        while decide(j) == Decision::Fails {
            j -= 1;
        }
        if j == 0 {
            while holds(j + 1) {
                j += 1;
            }
        }
        let (explore, psi) = match decide(j + 1) {
            Decision::Open => (Some(edge(j + 1)), EXPLORE_SHARE),
            Decision::Fails => (Some(edge(j + 1)), FAILED_EXPLORE_SHARE),
            _ => (None, 0.0),
        };
        let mut shown: Vec<Step> = steps
            .iter()
            .map(|(&k, t)| {
                let (lo, hi) = t.interval();
                Step { step: k, upto: edge(k), sent: t.sent, answered: t.answered, calls: t.calls, lo, hi, decision: decide(k) }
            })
            .collect();
        if explore.is_some() && !steps.contains_key(&(j + 1)) {
            shown.push(Step { step: j + 1, upto: edge(j + 1), sent: 0, answered: 0, calls: 0, lo: 0.0, hi: 1.0, decision: Decision::Open });
        }
        Length { model: model.to_string(), chars: edge(j), start: j == 0, line, steps: shown, explore, psi }
    }
}

/// The counts of judge `model` among `all` (`counts`): its own lines, and those naming none when it is
/// `unnamed`'s (the default judge, module doc).
fn of_model(all: &BTreeMap<String, Steps>, model: &str, unnamed: &str) -> Steps {
    let mut out = all.get(model).cloned().unwrap_or_default();
    if !unnamed.is_empty() && model == unnamed {
        for (j, t) in all.get("").into_iter().flatten() {
            out.entry(*j).or_default().merge(t);
        }
    }
    out
}

/// Judge `model`'s effective length from `state`'s cost lines (the session-end judge's budget).
#[must_use]
pub fn length(state: &Path, model: &str) -> Length {
    let all = counts(state);
    // Lines naming no judge count for the default one; finding it asks the PATH, so only when there are.
    let unnamed = if all.contains_key("") { crate::hook::judge_model("") } else { String::new() };
    Length::of(model, &of_model(&all, model, &unnamed))
}

/// Every judge's effective length in `state` (`borg status`, borg web, `borg top --json`): each judge
/// whose lines are there, and `current`, the one this environment runs (`hook::judge_model`), even
/// without evidence; by label.
#[must_use]
pub fn lengths(state: &Path, current: &str) -> Vec<Length> {
    let all = counts(state);
    let unnamed = crate::hook::judge_model("");
    let mut models: std::collections::BTreeSet<String> = all.keys().filter(|m| !m.is_empty()).cloned().collect();
    if all.contains_key("") && !unnamed.is_empty() {
        models.insert(unnamed.clone());
    }
    if !current.is_empty() {
        models.insert(current.to_string());
    }
    models.into_iter().map(|m| Length::of(&m, &of_model(&all, &m, &unnamed))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steps from (upper edge, sent, answered), each in ten calls (fewer when fewer lessons) of shares as
    /// even as the counts allow.
    fn steps(rows: &[(usize, u64, u64)]) -> Steps {
        rows.iter()
            .map(|&(upto, n, a)| {
                let c = n.min(10);
                let mut t = Tally::default();
                for i in 0..c {
                    t.add(n / c + u64::from(i < n % c), a / c + u64::from(i < a % c));
                }
                (step_of(upto), t)
            })
            .collect()
    }

    /// A step from its calls' (sent, answered).
    fn calls(rows: &[(u64, u64)]) -> Tally {
        let mut t = Tally::default();
        for &(n, a) in rows {
            t.add(n, a);
        }
        t
    }

    #[test]
    fn steps_are_half_again_apart_around_the_start_budget() {
        assert_eq!((edge(-2), edge(-1), edge(0), edge(1), edge(2)), (32_000, 48_000, 72_000, 108_000, 162_000));
        assert_eq!((step_of(72_000), step_of(72_001), step_of(48_001), step_of(48_000), step_of(1)), (0, 1, 0, -1, step_of(1)));
        assert!(step_of(1) < -10 && edge(step_of(1)) >= 1);
        for c in [1, 999, 31_999, 32_000, 100_000, 2_000_000] {
            let j = step_of(c);
            assert!(c <= edge(j) && c > edge(j - 1), "{c}: {j}");
        }
    }

    /// The interval matches the closed form at known points.
    #[test]
    fn the_wilson_interval_matches_known_values() {
        let (lo, hi) = wilson(50, 100);
        assert!((lo - 0.4038).abs() < 1e-3 && (hi - 0.5962).abs() < 1e-3, "{lo} {hi}");
        let (lo, hi) = wilson(0, 10);
        assert!(lo == 0.0 && (hi - 0.2775).abs() < 1e-3, "{hi}");
        assert_eq!(wilson(0, 0), (0.0, 1.0));
    }

    /// Every format of the session-end judge's cost line: 0.7.6's with `model=` (its `chars=` the prompt);
    /// 0.7.5's without `model=`; before 0.7.5, `resolved=` as the answered count; a failed call sent its
    /// lessons and answered none. A check's judge line (another task) is left out.
    #[test]
    fn every_cost_line_format_gives_what_it_holds() {
        let check = "judge call=3 pending=4 resolved=2 chars=900 returned=a,b,c unmatched= verdicts=changed:1,ignored:1,not_arisen:0,consequence_remains:1,unclear:1 reason_chars=40 model=claude:sonnet prompt=30000";
        assert_eq!(parse(check), None, "a check's line: another task");
        let end = "judge call=5 pending=10 resolved=6 chars=70000 returned= unmatched= verdicts=changed:6,ignored:0,not_arisen:0,consequence_remains:0,unclear:4 reason_chars=9 model=codex:gpt-5 at=end batch=1/3 budget=72000 cut=after";
        assert_eq!(parse(end), Some(Call { model: "codex:gpt-5".into(), chars: 70_000, sent: 10, answered: 6 }));
        let end_075 = "judge call=2 pending=7 resolved=1 chars=40000 returned= unmatched= verdicts=changed:1,ignored:0,not_arisen:0,consequence_remains:2,unclear:4 reason_chars=9 at=end";
        assert_eq!(parse(end_075), Some(Call { model: String::new(), chars: 40_000, sent: 7, answered: 3 }));
        let failed = "judge-failed pending=12 chars=71000 model=claude:sonnet at=end batch=2/3 budget=72000 cut=after";
        assert_eq!(parse(failed), Some(Call { model: "claude:sonnet".into(), chars: 71_000, sent: 12, answered: 0 }));
        assert_eq!(parse("judge call=2 pending=3 resolved=2 chars=4100"), None, "a check's line before 0.7.6");
        assert_eq!(parse("judge call=2 pending=3 resolved=2 chars=4100 at=end").map(|c| c.answered), Some(2), "before 0.7.5: resolved=");
        assert_eq!(parse("judge call=2 pending=0 resolved=0 chars=4100 at=end"), None);
        assert_eq!(parse("block questions=2 chars=600"), None);
    }

    /// One call's lessons are not independent: a single call never decides a step, however it went,
    /// and calls that disagree widely are worth few lessons (the design effect), so they stay open where
    /// the lessons counted as independent would decide.
    #[test]
    fn one_call_never_decides_a_step_and_disagreeing_calls_count_for_less() {
        let mut base = steps(&[(4_000, 59, 15)]);
        // One end call at 100k, both its lessons answered: open, the budget stays.
        base.insert(step_of(100_000), calls(&[(2, 2)]));
        let up = Length::of("m", &base);
        assert_eq!((up.chars, up.start, up.explore), (72_000, true, Some(108_000)), "{up:?}");
        // One call at 70k, none of its 30 answered: open, the budget stays.
        let mut base = steps(&[(4_000, 59, 15)]);
        base.insert(step_of(70_000), calls(&[(30, 0)]));
        let down = Length::of("m", &base);
        assert_eq!((down.chars, down.start, down.psi), (72_000, true, EXPLORE_SHARE), "{down:?}");
        assert_eq!(down.steps.iter().find(|s| s.upto == 72_000).map(|s| s.decision), Some(Decision::Open));
        // Six calls at 108k sharing 30% overall, but all or nothing each: 180 independent lessons would
        // fail it (upper bound 0.37 below the line 0.425), the calls' spread keeps it open.
        let reference = (32_000, 400, 200);
        let clustered = calls(&[(30, 30), (30, 0), (30, 0), (30, 24), (30, 0), (30, 0)]);
        assert!(wilson(clustered.answered, clustered.sent).1 < HOLD * 0.5, "independent: would fail");
        assert!(clustered.effective() < 20.0, "{}", clustered.effective());
        let mut st = steps(&[reference, (72_000, 300, 150)]);
        st.insert(step_of(100_000), clustered);
        let l = Length::of("m", &st);
        assert_eq!(l.steps.iter().find(|s| s.upto == 108_000).map(|s| s.decision), Some(Decision::Open), "{l:?}");
        // The same share in calls that agree: decided.
        let mut st = steps(&[reference, (72_000, 300, 150)]);
        st.insert(step_of(100_000), calls(&[(30, 9); 6]));
        assert_eq!(Length::of("m", &st).steps.iter().find(|s| s.upto == 108_000).map(|s| s.decision), Some(Decision::Fails));
    }

    /// The reference pools the smallest steps until they hold enough lessons in enough calls.
    #[test]
    fn the_reference_pools_the_smallest_steps() {
        let l = Length::of("m", &steps(&[(10_000, 20, 10), (21_333, 20, 10), (72_000, 300, 150)]));
        assert_eq!(l.steps.iter().filter(|s| s.sent > 0).map(|s| s.decision).collect::<Vec<_>>(), [Decision::Below, Decision::Reference, Decision::Holds]);
        assert!((l.line.unwrap() - HOLD * 0.5).abs() < 1e-9);
        // Many lessons in too few calls: no reference yet.
        let mut few = Steps::new();
        few.insert(step_of(10_000), calls(&[(40, 20), (40, 20)]));
        assert_eq!(Length::of("m", &few).line, None);
    }

    /// No evidence, or too little to decide: the budget stays at the start and the step above is explored.
    #[test]
    fn thin_evidence_keeps_the_start_and_explores_the_step_above() {
        let none = Length::of("m", &Steps::new());
        assert_eq!((none.chars, none.start, none.explore, none.psi, none.line), (72_000, true, Some(108_000), EXPLORE_SHARE, None));
        assert_eq!(none.steps.len(), 1, "the explored step shows: {:?}", none.steps);
        // A reference of 40 at 50%, and 108k with 5 lessons: the interval straddles the line.
        let thin = Length::of("m", &steps(&[(32_000, 40, 20), (108_000, 5, 2)]));
        assert_eq!((thin.chars, thin.start, thin.explore), (72_000, true, Some(108_000)));
        assert_eq!(thin.steps.iter().map(|s| s.decision).collect::<Vec<_>>(), [Decision::Reference, Decision::Open]);
        // The steps pooled do not reach the reference's lessons: no line yet.
        let small = Length::of("m", &steps(&[(32_000, 12, 12), (108_000, 10, 1)]));
        assert_eq!((small.chars, small.line, small.explore), (72_000, None, Some(108_000)));
    }

    /// Flat logs: every step above keeps the reference's share, so the budget climbs to the largest
    /// decided step and explores the one above it.
    #[test]
    fn flat_logs_step_the_budget_up() {
        let flat = Length::of("m", &steps(&[(32_000, 400, 200), (72_000, 300, 150), (108_000, 300, 151), (162_000, 300, 149), (243_000, 3, 1)]));
        assert_eq!((flat.chars, flat.start, flat.explore, flat.psi), (162_000, false, Some(243_000), EXPLORE_SHARE));
        let d: Vec<Decision> = flat.steps.iter().map(|s| s.decision).collect();
        assert_eq!(d, [Decision::Reference, Decision::Holds, Decision::Holds, Decision::Holds, Decision::Open]);
        // The step above decided as failing: the budget stays, the failed step explored at the floor.
        let capped = Length::of("m", &steps(&[(32_000, 400, 200), (108_000, 300, 150), (162_000, 400, 100)]));
        assert_eq!((capped.chars, capped.explore, capped.psi), (108_000, Some(162_000), FAILED_EXPLORE_SHARE));
    }

    /// Degrading logs: the share falls with size, below the line by 72k: the budget steps down to the
    /// largest step still holding, never below the reference.
    #[test]
    fn degrading_logs_step_the_budget_down() {
        let worse = Length::of("m", &steps(&[(21_333, 400, 300), (32_000, 300, 250), (48_000, 300, 230), (72_000, 300, 120)]));
        assert_eq!((worse.chars, worse.start), (48_000, false), "{worse:?}");
        assert_eq!((worse.explore, worse.psi), (Some(72_000), FAILED_EXPLORE_SHARE), "72k failed: explored at the floor, so it can hold again");
        let d: Vec<Decision> = worse.steps.iter().map(|s| s.decision).collect();
        assert_eq!(d, [Decision::Reference, Decision::Holds, Decision::Holds, Decision::Fails]);
        // Further down while each step fails; an open one stops the descent (the one above failed: the floor).
        let open = Length::of("m", &steps(&[(21_333, 400, 300), (32_000, 300, 250), (48_000, 300, 180), (72_000, 300, 120)]));
        assert_eq!((open.chars, open.explore, open.psi), (48_000, Some(72_000), FAILED_EXPLORE_SHARE), "{open:?}");
        assert_eq!(open.steps[2].decision, Decision::Open);
        let deep = Length::of("m", &steps(&[(21_333, 400, 300), (32_000, 300, 250), (48_000, 300, 120), (72_000, 300, 120)]));
        assert_eq!(deep.chars, 32_000);
        // Failing down to the reference stops there.
        let bottom = Length::of("m", &steps(&[(48_000, 300, 270), (72_000, 300, 30)]));
        assert_eq!(bottom.chars, 48_000);
    }

    /// The counts are cached beside the cost files and rebuilt when they change; lines naming no model
    /// count for the default judge only; another version's cache is rebuilt.
    #[test]
    fn the_counts_are_cached_until_the_cost_lines_change() {
        let d = std::env::temp_dir().join(format!("borg-judge-length-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let end = |n: u64, a: u64, model: &str| {
            format!(
                "judge call=1 pending={n} resolved={a} chars=60000 returned= unmatched= verdicts=changed:{a},ignored:0,not_arisen:0,consequence_remains:0,unclear:{} reason_chars=1{model} at=end\n",
                n - a
            )
        };
        std::fs::write(d.join("a.cost"), end(10, 4, " model=x:y") + &end(6, 6, "") + "block questions=1 chars=3\n").unwrap();
        let c = counts(&d);
        assert_eq!((c["x:y"][&0].sent, c["x:y"][&0].answered, c["x:y"][&0].calls), (10, 4, 1));
        assert_eq!((c[""][&0].sent, c[""][&0].answered), (6, 6));
        let cached: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join(CACHE)).unwrap()).unwrap();
        assert_eq!(cached["version"], VERSION);
        // Unnamed lines join the default judge, not another.
        let all = counts(&d);
        let pair = |t: &Tally| (t.sent, t.answered, t.calls);
        assert_eq!(pair(&of_model(&all, "x:y", "x:y")[&0]), (16, 10, 2));
        assert_eq!(pair(&of_model(&all, "x:y", "z:w")[&0]), (10, 4, 1));
        // A new line changes the stamp: recounted.
        std::fs::write(d.join("b.cost"), end(5, 5, " model=x:y")).unwrap();
        assert_eq!(pair(&counts(&d)["x:y"][&0]), (15, 9, 2));
        // Another version: rebuilt, not read.
        let mut stale: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join(CACHE)).unwrap()).unwrap();
        stale["version"] = serde_json::json!(VERSION + 1);
        stale["models"]["x:y"][0][1]["sent"] = serde_json::json!(1);
        std::fs::write(d.join(CACHE), stale.to_string()).unwrap();
        assert_eq!(pair(&counts(&d)["x:y"][&0]), (15, 9, 2));
        // Two writers at once (borg web's threads): each its own temp file, the cache whole after.
        std::fs::write(d.join("c.cost"), end(5, 1, " model=x:y")).unwrap();
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| assert_eq!(pair(&counts(&d)["x:y"][&0]), (20, 10, 3)));
            }
        });
        let cache: Cache = serde_json::from_str(&std::fs::read_to_string(d.join(CACHE)).unwrap()).unwrap();
        assert_eq!(cache.version, VERSION);
        assert!(std::fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(&format!("{CACHE}."))), "no temp file left");
        // The listing names the current judge even without evidence.
        assert!(lengths(&d, "q:r").iter().any(|l| l.model == "q:r" && l.start));
        let _ = std::fs::remove_dir_all(&d);
    }
}
