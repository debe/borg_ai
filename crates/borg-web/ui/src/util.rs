//! Formatting, dates and the vocabulary of event kinds (labels, colours, glyphs).

pub const DAY_MS: f64 = 86_400_000.0;

pub fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's days_from_civil).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day number.
pub fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYY-MM-DD` to a day number.
pub fn day_num(s: &str) -> Option<i64> {
    let mut it = s.split('-').map(|p| p.parse::<i64>().ok());
    Some(days_from_civil(it.next()??, it.next()??, it.next()??))
}

pub fn day_of_ms(ms: f64) -> i64 {
    (ms / DAY_MS).floor() as i64
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "Oct 12".
pub fn short_day(n: i64) -> String {
    let (_, m, d) = civil(n);
    format!("{} {d}", MONTHS[(m - 1) as usize])
}

/// "Oct 12, 2026".
pub fn long_day(n: i64) -> String {
    let (y, m, d) = civil(n);
    format!("{} {d}, {y}", MONTHS[(m - 1) as usize])
}

/// "3m ago", "5h ago", "Oct 12".
pub fn ago(ms: f64) -> String {
    let s = ((now_ms() - ms) / 1000.0).max(0.0);
    if s < 60.0 {
        "just now".into()
    } else if s < 3600.0 {
        format!("{:.0}m ago", (s / 60.0).floor())
    } else if s < 86_400.0 {
        format!("{:.0}h ago", (s / 3600.0).floor())
    } else if s < 30.0 * 86_400.0 {
        format!("{:.0}d ago", (s / 86_400.0).floor())
    } else {
        short_day(day_of_ms(ms))
    }
}

/// A duration in ms as "1.2 s", "4 min", "3 h", "2 d".
pub fn duration(ms: f64) -> String {
    let s = ms / 1000.0;
    if s < 60.0 {
        format!("{s:.1} s")
    } else if s < 3600.0 {
        format!("{:.0} min", s / 60.0)
    } else if s < 86_400.0 {
        format!("{:.1} h", s / 3600.0)
    } else {
        format!("{:.1} d", s / 86_400.0)
    }
}

pub fn usd(v: f64) -> String {
    if v > 0.0 && v < 0.01 {
        "<$0.01".into()
    } else if v < 100.0 {
        format!("${v:.2}")
    } else {
        format!("${v:.0}")
    }
}

pub fn num(v: f64) -> String {
    if v.abs() >= 10_000.0 {
        format!("{:.1}k", v / 1000.0)
    } else if v.fract().abs() > 1e-9 && v.abs() < 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    }
}

pub fn pct(part: f64, whole: f64) -> String {
    if whole <= 0.0 { "–".into() } else { format!("{:.0}%", 100.0 * part / whole) }
}

pub fn trunc(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{}…", t.trim_end()) } else { t }
}

/// "ann@example.com" → "ann"; names stay as they are.
pub fn who(e: &str) -> String {
    if e.is_empty() { "someone".into() } else { e.split('@').next().unwrap_or(e).to_string() }
}

/// A rounded-up axis maximum: 1, 2, 5 × 10^k.
pub fn nice_max(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let p = 10f64.powf(v.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if v <= m * p {
            return m * p;
        }
    }
    10.0 * p
}

/// Label, CSS colour and glyph of an event kind.
pub fn kind(k: &str) -> (&'static str, &'static str, &'static str) {
    match k {
        "turn_end" => ("turn ends", "var(--c-muted)", "·"),
        "gate_skip" => ("skipped free", "var(--c-slate)", "○"),
        "check" => ("model checks", "var(--c-accent)", "◆"),
        "block" => ("extra rounds", "var(--c-violet)", "?"),
        "live_check" => ("live checks", "var(--c-teal)", "◆"),
        "live_ask" => ("live questions", "var(--c-teal)", "?"),
        "early" => ("early deliveries", "var(--c-teal)", "↯"),
        "queued_free" => ("free hand-overs", "var(--c-teal)", "↯"),
        "changed" => ("used", "var(--c-good)", "✓"),
        "ignored" => ("ignored", "var(--c-slate)", "–"),
        "corrected" => ("corrected anyway", "var(--c-heat)", "!"),
        "not_corrected" => ("not corrected", "var(--c-sky)", "✓"),
        "regretted" => ("regretted later", "var(--c-pink)", "↶"),
        "correction" => ("corrections", "var(--c-heat)", "✎"),
        "veto" => ("vetoes", "var(--c-heat)", "⊘"),
        "veto_repeat" => ("repeated vetoes", "var(--c-pink)", "⊘"),
        "review_finding" => ("review fixes", "var(--c-amber)", "⚑"),
        "lesson_new" => ("new lessons", "var(--c-good)", "✦"),
        "lesson_merged" => ("merged lessons", "var(--c-sky)", "⊕"),
        "not_a_lesson" => ("not a lesson", "var(--c-slate)", "∅"),
        "model_call" => ("model calls", "var(--c-amber)", "◇"),
        "steer_issued" => ("steers sent", "var(--c-amber)", "➜"),
        "steer_delivered" => ("steers delivered", "var(--c-amber)", "➜"),
        "steer_dropped" => ("steers refused", "var(--c-heat)", "⨯"),
        "pause_refused" => ("paused tool calls", "var(--c-pink)", "⏸"),
        "queen_queued" => ("queen questions", "var(--c-violet)", "♛"),
        "queen_delivered" => ("queen asked", "var(--c-violet)", "♛"),
        "queen_answered" => ("queen answered", "var(--c-violet)", "♛"),
        "queen_skipped" => ("queen skipped", "var(--c-slate)", "♛"),
        "queen_dismissed" => ("queen dismissed", "var(--c-slate)", "♛"),
        "assimilated" => ("imports", "var(--c-sky)", "⇣"),
        "vectors" => ("vector lookups", "var(--c-slate)", "≈"),
        "sync" => ("mesh syncs", "var(--c-sky)", "⇄"),
        _ => ("other", "var(--c-muted)", "•"),
    }
}

/// A colour for the i-th category in a list (systems, purposes).
pub fn palette(i: usize) -> &'static str {
    const P: [&str; 8] =
        ["var(--c-accent)", "var(--c-good)", "var(--c-violet)", "var(--c-amber)", "var(--c-teal)", "var(--c-heat)", "var(--c-sky)", "var(--c-pink)"];
    P[i % P.len()]
}
