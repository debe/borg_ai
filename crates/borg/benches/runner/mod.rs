//! How a bench target runs its units: criterion's timed loop, or a fixed amount of work.
//!
//! A loaded machine distorts times, not work. With `BORG_BENCH_ITERS=N` every unit whose id contains the
//! filter (the first argument that is not a flag, as criterion takes it) runs once to warm up, then
//! exactly `N` times: no clock, no statistics, no time-based loop. Callgrind, `perf stat`, heaptrack and
//! the peak RSS then measure that fixed work, and `N=0` runs setup and warm-ups only, so one run costs
//! (cost of `N` − cost of `0`) / `N`. Under `perf stat --delay=-1 --control fifo:<ctl>,<ack>` with
//! `BORG_BENCH_PERF_CTL=<ctl>,<ack>`, the counters run only around the `N` runs, without setup or warm-up;
//! callgrind isolates them the same way (`measured`), and on Linux the run prints its RSS before the `N`
//! runs and the peak during them.
//!
//! Without it, criterion runs each unit, short by default (1 s warm-up, 3 s measurement, 10 flat
//! samples); its flags override that. Shared by both crates' benches (`#[path]`).

use std::hint::black_box;
use std::io::{Read, Write};
use std::time::Duration;

use criterion::{Criterion, SamplingMode};

/// Runs each matching unit this many times after one warm-up run, instead of criterion's timed loop.
pub const ITERS: &str = "BORG_BENCH_ITERS";
/// `perf stat --control`'s fifos, `<ctl>,<ack>`: a fixed run enables the counters for its measured runs only.
pub const PERF_CTL: &str = "BORG_BENCH_PERF_CTL";

/// How this run measures each unit (module doc).
pub enum Runner {
    /// Criterion's timed loop and statistics.
    Timed(Box<Criterion>),
    /// `iters` runs of each unit whose id contains `filter`; `matched` counts the units that ran.
    Fixed { iters: u64, filter: Option<String>, matched: usize },
}

impl Runner {
    /// Fixed when `ITERS` is set, timed otherwise.
    pub fn from_env() -> Runner {
        match std::env::var(ITERS) {
            Ok(n) => Runner::Fixed {
                iters: n.trim().parse().unwrap_or_else(|_| panic!("{ITERS} is a count of runs, not {n:?}")),
                filter: std::env::args().skip(1).find(|a| !a.starts_with('-')),
                matched: 0,
            },
            Err(_) => Runner::Timed(Box::new(
                Criterion::default().warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3)).sample_size(10).configure_from_args(),
            )),
        }
    }

    /// Whether unit `group/name` runs: always under criterion (its own filter decides), in a fixed run when
    /// the filter matches. Lets a target skip setup that only this unit needs.
    pub fn wants(&self, group: &str, name: &str) -> bool {
        match self {
            Runner::Timed(_) => true,
            Runner::Fixed { filter, .. } => filter.as_deref().is_none_or(|w| format!("{group}/{name}").contains(w)),
        }
    }

    /// One unit, `group/name`: `f` is the work it measures.
    pub fn bench<O>(&mut self, group: &str, name: &str, mut f: impl FnMut() -> O) {
        match self {
            Runner::Timed(c) => {
                let mut g = c.benchmark_group(group);
                g.sampling_mode(SamplingMode::Flat);
                g.bench_function(name, |b| b.iter(&mut f));
                g.finish();
            }
            Runner::Fixed { .. } if !self.wants(group, name) => {}
            Runner::Fixed { iters, matched, .. } => {
                let id = format!("{group}/{name}");
                *matched += 1;
                black_box(f());
                let before = resident(Kind::Now);
                reset_peak();
                perf("enable");
                measured(*iters, &mut f);
                perf("disable");
                let kb = |r: Option<u64>| r.map_or("?".into(), |k| k.to_string());
                eprintln!("{id}: 1 warm-up + {iters} runs; RSS {} kB before, peak {} kB during", kb(before), kb(resident(Kind::Peak)));
            }
        }
    }

    /// Criterion's summary, or a warning that a fixed run's filter matched nothing.
    pub fn finish(self) {
        match self {
            Runner::Timed(c) => c.final_summary(),
            Runner::Fixed { filter, matched: 0, .. } => eprintln!("no unit matches {filter:?}: nothing ran"),
            Runner::Fixed { .. } => {}
        }
    }
}

/// A fixed run's `iters` measured runs, in a frame of their own: callgrind's
/// `--dump-before='*runner*measured*' --dump-after='*runner*measured*'` writes them as one part, every thread
/// included, without setup or warm-up.
#[inline(never)]
fn measured<O>(iters: u64, f: &mut impl FnMut() -> O) {
    for _ in 0..iters {
        black_box(f());
    }
}

/// Which resident set size `resident` reads.
enum Kind {
    Now,
    Peak,
}

/// The process's resident set in kB, now (`VmRSS`) or its peak since `reset_peak` (`VmHWM`); Linux only.
fn resident(kind: Kind) -> Option<u64> {
    let field = match kind {
        Kind::Now => "VmRSS:",
        Kind::Peak => "VmHWM:",
    };
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| l.strip_prefix(field))?.trim().trim_end_matches("kB").trim().parse().ok()
}

/// Resets the peak resident set to the current one (Linux `clear_refs` 5), so the next peak is the
/// measured runs' own; elsewhere the write fails and the peak stays the process's.
fn reset_peak() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

/// Sends `cmd` to `perf stat --control` when `PERF_CTL` names its fifos, and waits for perf's ack.
fn perf(cmd: &str) {
    let Ok(fifos) = std::env::var(PERF_CTL) else { return };
    let (ctl, ack) = fifos.split_once(',').unwrap_or_else(|| panic!("{PERF_CTL} is <ctl>,<ack>, not {fifos:?}"));
    let mut ctl = std::fs::OpenOptions::new().write(true).open(ctl).expect("perf's control fifo");
    ctl.write_all(format!("{cmd}\n").as_bytes()).expect("tell perf");
    let mut reply = [0u8; 8];
    std::fs::File::open(ack).and_then(|mut a| a.read(&mut reply)).expect("perf's ack");
}
