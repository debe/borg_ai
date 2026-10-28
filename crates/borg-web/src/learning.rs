//! `/api/learning`: whether learning works in this collective, week by week, from the logs Borg keeps
//! anyway (no state of its own, so the history is there from the first view).
//!
//! Each section says where its numbers come from: `team` when they fold synced logs (stats.jsonl
//! rollups, lessons.jsonl) in a team collective, `this machine`
//! when they fold the local event log (stats/events.jsonl, which keeps the lesson sources) or the
//! collective is local.
//! - lessons written per source (session, review, regret, veto, queen, import, ...), and
//!   the team's lessons written (from lessons.jsonl, each lesson once) and merged;
//! - asked lessons' verdicts: changed the work, ignored, corrected anyway (the asked→changed rate);
//! - vetoes and repeated vetoes (the same "no" said twice: a lesson that did not hold).
//!
//! The weeks run from the first counted one to the current one (`partial_week`). The history before
//! live counting began (stats.rs `backfill`) derives no merges, repeated vetoes or lesson sources:
//! those series are null before `live_from`, unknown rather than zero.

use std::collections::BTreeMap;
use std::path::Path;

use borg::collective::Share;
use borg::{learn, stats};
use serde_json::{Value, json};

const DAY_MS: i64 = 86_400_000;

/// The Monday (UTC) of the week `ms` falls in, as a day number since 1970-01-01 (a Thursday).
fn week(ms: i64) -> i64 {
    let d = ms.div_euclid(DAY_MS);
    d - (d + 3).rem_euclid(7)
}

/// The Monday of the week `day` (`YYYY-MM-DD`) falls in; None for an unreadable day.
fn week_of_day(day: &str) -> Option<i64> {
    let mut it = day.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(week((era * 146_097 + doe - 719_468) * DAY_MS))
}

/// Counts per (series, week).
#[derive(Default)]
struct Weekly(BTreeMap<String, BTreeMap<i64, u64>>);

impl Weekly {
    fn add(&mut self, series: &str, week: i64, n: u64) {
        if n > 0 {
            *self.0.entry(series.to_string()).or_default().entry(week).or_default() += n;
        }
    }

    fn weeks(&self) -> impl Iterator<Item = i64> + '_ {
        self.0.values().flat_map(|w| w.keys().copied())
    }

    /// `series`' counts over `weeks`, zero where nothing was counted.
    fn of(&self, series: &str, weeks: &[i64]) -> Vec<u64> {
        let w = self.0.get(series);
        weeks.iter().map(|k| w.and_then(|w| w.get(k)).copied().unwrap_or(0)).collect()
    }
}

/// Monday day numbers from the first week any series counted to the week of `now` (the newest is
/// the current, partial week), contiguous: idle weeks show as zeros, never as the last active ones.
/// Empty when nothing was counted.
fn span(all: &[&Weekly], now: i64) -> Vec<i64> {
    let ks: Vec<i64> = all.iter().flat_map(|w| w.weeks()).collect();
    match (ks.iter().min(), ks.iter().max()) {
        (Some(&a), Some(&b)) => (a..=b.max(week(now))).step_by(7).collect(),
        _ => Vec::new(),
    }
}

/// `counts` with the weeks before `from` (the week live counting began) unknown: null, never a 0.
fn known(counts: Vec<u64>, weeks: &[i64], from: Option<i64>) -> Vec<Option<u64>> {
    counts.into_iter().zip(weeks).map(|(n, w)| from.is_some_and(|f| *w >= f).then_some(n)).collect()
}

pub fn view(name: &str, home: &Path, share: Share) -> Value {
    view_at(name, home, share, learn::now_ms())
}

fn view_at(name: &str, home: &Path, share: Share, now: i64) -> Value {
    let synced = if share == Share::Team { "team" } else { "this machine" };
    let local = "this machine";
    // The derived history (stats.rs `backfill`) has no merges, no repeated vetoes and no lesson
    // sources (and its lessons include peers'): those series start when live counting began here.
    let buckets = stats::current(home);
    let live_week = stats::live_since(home).map(week);
    // Lessons written by source: the live events keep the source (`why`), the rollups do not.
    let mut sources = Weekly::default();
    for e in stats::live(home) {
        if e.kind == stats::LESSON_NEW {
            sources.add(if e.why.is_empty() { "session" } else { &e.why }, week(e.ms), 1);
        }
    }
    // Team-wide counts from every node's daily rollups.
    let mut team = Weekly::default();
    const KINDS: [&str; 7] = [stats::LESSON_MERGED, stats::CHANGED, stats::IGNORED, stats::CORRECTED, stats::NOT_CORRECTED, stats::VETO, stats::VETO_REPEAT];
    for b in &buckets {
        let Some(w) = week_of_day(&b.day) else { continue };
        for k in KINDS {
            team.add(k, w, b.counts.get(k).copied().unwrap_or(0));
        }
    }
    // New lessons from the lessons themselves (synced in a team): each once, in the week it was
    // learned, whoever learned it; a node's rollup would count a peer's lesson again in its history.
    let lessons = learn::lessons(&home.join("lessons.jsonl"));
    let mut first: BTreeMap<&str, i64> = BTreeMap::new();
    for l in &lessons {
        let ms = first.entry(l.id.as_str()).or_insert(l.origin_ms);
        *ms = (*ms).min(l.origin_ms);
    }
    for ms in first.values().filter(|ms| **ms > 0) {
        team.add(stats::LESSON_NEW, week(*ms), 1);
    }
    let weeks = span(&[&sources, &team], now);
    let mut names: Vec<(&String, u64)> = sources.0.iter().map(|(k, w)| (k, w.values().sum())).collect();
    names.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let by_source: Vec<Value> = names.iter().map(|(s, n)| json!({"source": s, "total": n, "weeks": known(sources.of(s, &weeks), &weeks, live_week)})).collect();
    let t = |k: &str| team.of(k, &weeks);
    let since = |k: &str| known(t(k), &weeks, live_week);
    json!({
        "collective": name,
        "share": if share == Share::Team { "team" } else { "local" },
        "weeks": weeks.iter().map(|w| stats::day(w * DAY_MS)).collect::<Vec<_>>(),
        // The newest week is the current one, still running.
        "partial_week": weeks.last().is_some_and(|w| *w == week(now)),
        // The Monday live counting began on this machine: before it, the series that are null.
        "live_from": live_week.map(|w| stats::day(w * DAY_MS)),
        "lessons": {"scope": local, "by_source": by_source,
            "team": {"scope": synced, "new": t(stats::LESSON_NEW), "merged": since(stats::LESSON_MERGED)}},
        "asked": {"scope": synced, "changed": t(stats::CHANGED), "ignored": t(stats::IGNORED), "corrected": t(stats::CORRECTED),
            "not_corrected": t(stats::NOT_CORRECTED)},
        "vetoes": {"scope": synced, "veto": t(stats::VETO), "repeat": since(stats::VETO_REPEAT)},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_start_on_monday() {
        // 2026-10-25 is a Sunday, 2026-10-19 its Monday, 2026-10-26 the next.
        let monday = week_of_day("2026-10-19").unwrap();
        assert_eq!(stats::day(monday * DAY_MS), "2026-10-19");
        assert_eq!(week_of_day("2026-10-25"), Some(monday));
        assert_eq!(week_of_day("2026-10-26"), Some(monday + 7));
        assert_eq!(week(0), week_of_day("1969-12-29").unwrap(), "1970-01-01 was a Thursday");
    }

    #[test]
    fn the_weeks_run_to_the_current_one_and_unknown_weeks_are_null() {
        // Counts in the weeks of 10-12 and 10-19, viewed on 2026-12-01 (a Tuesday): the idle weeks
        // up to that week's Monday 11-30 are there, so the last four are idle, not the last active.
        let (a, b) = (week_of_day("2026-10-12").unwrap(), week_of_day("2026-10-19").unwrap());
        let mut w = Weekly::default();
        w.add("x", a, 2);
        w.add("x", b, 1);
        let now = (week_of_day("2026-11-30").unwrap() + 1) * DAY_MS + 3_600_000;
        let weeks = span(&[&w], now);
        assert_eq!(weeks.len(), 8);
        assert_eq!(stats::day(weeks[7] * DAY_MS), "2026-11-30");
        assert_eq!(w.of("x", &weeks), [2, 1, 0, 0, 0, 0, 0, 0]);
        assert!(span(&[&Weekly::default()], now).is_empty(), "nothing counted: no weeks");
        assert_eq!(known(vec![4, 5, 6], &weeks[..3], Some(b)), [None, Some(5), Some(6)]);
        assert_eq!(known(vec![4], &weeks[..1], None), [None], "no live counting yet: unknown");
    }
}
