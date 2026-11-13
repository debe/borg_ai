//! What the readers behind borg-web's pages cost on a store of a real team's size: the baseline the
//! readers' CPU and memory work is measured against.
//!
//! Each reads the whole log it folds, as a page does on every request: the feed (`stats::newest`, and
//! `feed_whole`: the events parsed whole, as the feed read them before `newest`), the outcome logs
//! (`outcome::read`, `outcome::team_parts`), the usage view's off-policy support (`ope::decisions_in`, and
//! `decisions_read`: the log read whole first, as the view read it before), the lessons
//! (`conscience::load_unweighted_with`, which every hook event runs too, `learn::lessons`, and
//! `learn::stored`, the versions and repos the Lessons page shows), and the memory export
//! (`memory_export::document`, the `/memory` view). The usage rollups (`stats::rollups`, and
//! `stats::current` with this node's own events) instead follow `stats.jsonl`, read whole only on first
//! use: `append_rollups` and `append_current` measure a call after one line was appended, as a rollup or
//! a merge appends it, and `renamed_rollups` one after a file was renamed over it, as the quarantine
//! does.
//!
//! The data is a copy of the store `BORG_BENCH_STORE` names, else a generated one of the same shape
//! (shape/mod.rs); a reader that writes a derived file (`stats::current` backfills once) writes it in the
//! copy. `memory_export::document` reads the engineer aliases from the collectives config as borg-web
//! does (`BORG_CONFIG`). Criterion times them, short by default; `BORG_BENCH_ITERS=N` runs a fixed number
//! of each instead, for callgrind, `perf stat`, heaptrack and peak RSS (runner/mod.rs).

use std::hint::black_box;
use std::io::Write;
use std::path::Path;

use borg::collective::Aliases;
use borg::memory_export::Filter;
use borg::stats::{self, Event};

mod runner;
mod shape;

use runner::Runner;

/// The kinds and the page borg-web's feed shows (borg-web lib.rs `NOTABLE`, `PAGE`).
const FEED: [&str; 28] = [
    stats::CORRECTION,
    stats::VETO,
    stats::VETO_REPEAT,
    stats::REVIEW_FINDING,
    stats::BLOCK,
    stats::CHANGED,
    stats::IGNORED,
    stats::CORRECTED,
    stats::REGRETTED,
    stats::TESTED,
    stats::LESSON_NEW,
    stats::LESSON_MERGED,
    stats::NOT_A_LESSON,
    stats::STEER_ISSUED,
    stats::STEER_DELIVERED,
    stats::STEER_DROPPED,
    stats::PAUSE_REFUSED,
    stats::QUEEN_QUEUED,
    stats::QUEEN_DELIVERED,
    stats::QUEEN_ANSWERED,
    stats::QUEEN_SKIPPED,
    stats::QUEEN_DISMISSED,
    stats::ASSIMILATED,
    stats::EARLY,
    stats::QUEUED_FREE,
    stats::MODEL_CALL,
    stats::MSG_SENT,
    stats::MSG_DROPPED,
];
const PAGE: usize = 200;

fn main() {
    let scratch = shape::Scratch::new("stores");
    let home = scratch.0.join("store");
    eprintln!("bench store: {}", shape::fill(&home));
    let (lessons, outcomes) = (home.join("lessons.jsonl"), home.join("outcomes.jsonl"));
    let aliases = Aliases::default();

    let mut r = Runner::from_env();
    r.bench("stats", "rollups", || borg::stats::rollups(black_box(&home)));
    r.bench("stats", "current", || borg::stats::current(black_box(&home)));
    if r.wants("stats", "append_rollups") || r.wants("stats", "append_current") {
        let mut append = appender(&home);
        r.bench("stats", "append_rollups", || {
            append();
            borg::stats::rollups(black_box(&home))
        });
        r.bench("stats", "append_current", || {
            append();
            borg::stats::current(black_box(&home))
        });
    }
    if r.wants("stats", "renamed_rollups") {
        let mut rename = renamer(&home);
        r.bench("stats", "renamed_rollups", || {
            rename();
            borg::stats::rollups(black_box(&home))
        });
    }
    r.bench("stats", "feed_whole", || {
        let mut evs: Vec<Event> = borg::stats::events(black_box(&home)).into_iter().filter(|e| FEED.contains(&e.kind.as_str())).collect();
        evs.sort_by_key(|e| std::cmp::Reverse(e.ms));
        evs.truncate(PAGE);
        evs
    });
    r.bench("stats", "feed_newest", || borg::stats::newest(black_box(&home), &FEED, PAGE));
    r.bench("outcome", "read", || borg::outcome::read(black_box(&outcomes)));
    r.bench("outcome", "team_parts", || borg::outcome::team_parts(black_box(&lessons), true));
    r.bench("ope", "decisions_read", || borg::ope::decisions(&borg::outcome::read(black_box(&outcomes))));
    r.bench("ope", "decisions_in", || borg::ope::decisions_in(black_box(&outcomes)));
    r.bench("lessons", "conscience_load_unweighted_with", || borg::conscience::load_unweighted_with(black_box(&lessons), None, &aliases));
    r.bench("lessons", "learn_lessons", || borg::learn::lessons(black_box(&lessons)));
    r.bench("lessons", "learn_stored", || borg::learn::stored(black_box(&lessons)));
    r.bench("memory_export", "document", || borg::memory_export::document("bench", black_box(&home), &Filter::default()));
    r.finish();
}

/// Appends one line to the store's `stats.jsonl` per call, as a rollup or a peer's merge does: the next
/// version of the bucket with the highest version (the last of them in id order, when several have it).
fn appender(home: &Path) -> impl FnMut() {
    let path = stats::rollups_path(home);
    let mut b = stats::rollups(home).into_values().max_by_key(|b| b.version).expect("a store with rollups");
    move || {
        b.version += 1;
        let line = serde_json::to_string(&b).expect("a bucket as JSON") + "\n";
        std::fs::OpenOptions::new().append(true).open(&path).and_then(|mut f| f.write_all(line.as_bytes())).expect("a line appended");
    }
}

/// Renames a file of the same text over the store's `stats.jsonl` per call, as the quarantine does: its
/// inode changes every time (two hard-linked copies, taken in turn), so the next read is a whole one.
fn renamer(home: &Path) -> impl FnMut() {
    let path = stats::rollups_path(home);
    let copies = [home.join("stats.jsonl.a"), home.join("stats.jsonl.b")];
    for c in &copies {
        std::fs::copy(&path, c).expect("a copy of stats.jsonl");
    }
    let tmp = home.join("stats.jsonl.tmp");
    let mut turn = 0;
    move || {
        turn ^= 1;
        std::fs::hard_link(&copies[turn], &tmp).and_then(|()| std::fs::rename(&tmp, &path)).expect("a copy renamed over stats.jsonl");
    }
}
