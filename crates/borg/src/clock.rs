//! Wall-clock units and formats, each defined once: a day, the civil calendar, how long ago.
//!
//! The calendar is borg-trace's (it parses transcript stamps with it), re-exported here so that
//! every crate on top of borg reaches the one routine. The web page (borg-web ui, its own wasm
//! workspace) has its own copy in ui/src/util.rs.

pub use borg_trace::{civil_from_days, days_from_civil};

/// A day in milliseconds. Derived: 24 h of 60 min of 60 s of 1000 ms, a unit, not a choice. Every
/// "a day" or "a week" in borg is a multiple of it.
pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How long ago something was, from its age in ms, in at most 6 characters for an age under 100
/// days: `45s`, `12m`, `3h05m` under a day, `2d` from a day on. A negative age (a sender's clock
/// ahead of this one) is `0s`.
pub fn ago(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    let day = DAY_MS / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ if s < day => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d", s / day),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_a_day() {
        assert_eq!(DAY_MS, 86_400_000);
    }

    #[test]
    fn an_age_reads_in_its_largest_unit() {
        assert_eq!(ago(-5_000), "0s");
        assert_eq!(ago(59_999), "59s");
        assert_eq!(ago(60_000), "1m");
        assert_eq!(ago(3_599_999), "59m");
        assert_eq!(ago(3_600_000), "1h00m");
        assert_eq!(ago(3 * 3_600_000 + 5 * 60_000 + 59_000), "3h05m");
        assert_eq!(ago(DAY_MS - 1), "23h59m");
        assert_eq!(ago(DAY_MS), "1d");
        assert_eq!(ago(7 * DAY_MS - 1), "6d");
        assert!(ago(99 * DAY_MS).len() <= 6 && ago(DAY_MS - 1).len() <= 6, "fits a 6-wide column");
    }

    #[test]
    fn the_calendar_is_reachable_here() {
        assert_eq!(civil_from_days(days_from_civil(2026, 10, 28)), (2026, 10, 28));
    }
}
