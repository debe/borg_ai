//! Formatting, dates and the vocabulary of event kinds (labels, colours, glyphs).

/// A day in milliseconds. Derived: 24 h of 60 min of 60 s of 1000 ms, a unit (borg's clock.rs
/// `DAY_MS`; this page is a wasm workspace of its own and cannot share it).
pub const DAY_MS: f64 = 86_400_000.0;
/// A day in seconds, for the formats below (derived from `DAY_MS`).
const DAY_S: f64 = DAY_MS / 1000.0;

pub fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's days_from_civil): the page's one calendar
/// routine, with `civil`, the same as borg-trace's. The numbers are the calendar's (method): 146 097
/// days in a 400-year era, 719 468 days from 0000-03-01 to 1970-01-01.
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

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn local_date(ms: f64) -> js_sys::Date {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms))
}

/// Local "HH:MM" of a time in ms.
pub fn clock(ms: f64) -> String {
    let d = local_date(ms);
    format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
}

/// (year, month, date) of the local calendar day of a time in ms, for comparing days.
pub fn local_day_key(ms: f64) -> (u32, u32, u32) {
    let d = local_date(ms);
    (d.get_full_year(), d.get_month(), d.get_date())
}

/// The viewer's local midnight that starts the day of a time in ms (DST included: the calendar's).
#[allow(clippy::cast_possible_wrap, reason = "a month (0 to 11) and a day of the month (1 to 31)")]
pub fn local_midnight(ms: f64) -> f64 {
    let d = local_date(ms);
    js_sys::Date::new_with_year_month_day(d.get_full_year(), d.get_month() as i32, d.get_date() as i32).get_time()
}

/// "Mon": the local weekday of a time in ms.
pub fn weekday(ms: f64) -> &'static str {
    WEEKDAYS[local_date(ms).get_day() as usize % 7]
}

/// A value this browser keeps for the page (`localStorage`); None when it keeps none, or may not
/// (a private window, blocked site data: the accessor throws, which `Reflect` catches).
pub fn kept(key: &str) -> Option<String> {
    use wasm_bindgen::JsCast;
    let s = js_sys::Reflect::get(&web_sys::window()?.into(), &"localStorage".into()).ok()?;
    let get = js_sys::Reflect::get(&s, &"getItem".into()).ok()?.dyn_into::<js_sys::Function>().ok()?;
    get.call1(&s, &key.into()).ok()?.as_string()
}

/// Keeps a value for `kept`, where the browser lets it; a page without it works the same.
pub fn keep(key: &str, value: &str) {
    use wasm_bindgen::JsCast;
    let Some(w) = web_sys::window() else { return };
    let Ok(s) = js_sys::Reflect::get(&w, &"localStorage".into()) else { return };
    if let Some(set) = js_sys::Reflect::get(&s, &"setItem".into()).ok().and_then(|f| f.dyn_into::<js_sys::Function>().ok()) {
        let _ = set.call2(&s, &key.into(), &value.into());
    }
}

/// "Today", "Yesterday" or "Mon, Oct 12": the local calendar day of a time in ms.
pub fn day_label(ms: f64) -> String {
    let (d, today) = (local_date(ms), local_date(now_ms()));
    let ymd = |d: &js_sys::Date| (d.get_full_year(), d.get_month(), d.get_date());
    let day = ymd(&d);
    if day == ymd(&today) {
        return "Today".into();
    }
    // Local midnight today, minus 1 ms, is in yesterday whatever the month, year or DST.
    let midnight = js_sys::Date::new_with_year_month_day(today.get_full_year(), today.get_month() as i32, today.get_date() as i32);
    if day == local_day_key(midnight.get_time() - 1.0) {
        return "Yesterday".into();
    }
    format!("{}, {} {}", WEEKDAYS[d.get_day() as usize % 7], MONTHS[d.get_month() as usize % 12], d.get_date())
}

/// A stable avatar colour for a key: `palette` of its FNV-1a 64-bit hash.
pub fn hue(key: &str) -> &'static str {
    let h = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    palette((h % 8) as usize)
}

/// Two uppercase initials of a name, a drone's label ("Ann Example · borg-ai · claude": its name part)
/// or an email, without the machine a sender from another node comes with ("Bob on bob-laptop": the
/// server adds " on <host>" last): the first letters of its first two words, else the first two of its
/// one word (an email's local part); "?" when empty.
pub fn initials(name: &str) -> String {
    let name = name.split(" · ").next().unwrap_or_default();
    let name = name.rsplit_once(" on ").map_or(name, |(who, _)| who);
    let first = |w: &str| w.chars().find(|c| c.is_alphanumeric());
    let words: Vec<&str> = name.split_whitespace().filter(|w| first(w).is_some()).collect();
    let s: String = match words.as_slice() {
        [a, b, ..] => [first(a), first(b)].into_iter().flatten().flat_map(char::to_uppercase).collect(),
        [one] => one.split('@').next().unwrap_or_default().chars().filter(|c| c.is_alphanumeric()).take(2).flat_map(char::to_uppercase).collect(),
        [] => String::new(),
    };
    if s.is_empty() { "?".into() } else { s }
}

/// "3m ago", "5h ago", "Oct 12": the page's one "ago". Display: a date from 30 days on.
pub fn ago(ms: f64) -> String {
    let s = ((now_ms() - ms) / 1000.0).max(0.0);
    if s < 60.0 {
        "just now".into()
    } else if s < 3600.0 {
        format!("{:.0}m ago", (s / 60.0).floor())
    } else if s < DAY_S {
        format!("{:.0}h ago", (s / 3600.0).floor())
    } else if s < 30.0 * DAY_S {
        format!("{:.0}d ago", (s / DAY_S).floor())
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
    } else if s < DAY_S {
        format!("{:.1} h", s / 3600.0)
    } else {
        format!("{:.1} d", s / DAY_S)
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

/// A person as the server names them (borg names.rs: an alias, a git user.name, else the whole email),
/// shown as it is; "someone" when empty.
pub fn who(e: &str) -> String {
    if e.is_empty() { "someone".into() } else { e.to_string() }
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
        "live_check" => ("live checks (before 0.7.6)", "var(--c-teal)", "◆"),
        "live_ask" => ("questions at the next prompt", "var(--c-teal)", "?"),
        "check_dropped" => ("checks not run", "var(--c-slate)", "◇"),
        "early" => ("early deliveries", "var(--c-teal)", "↯"),
        "queued_free" => ("free hand-overs", "var(--c-teal)", "↯"),
        "changed" => ("used", "var(--c-good)", "✓"),
        "ignored" => ("ignored", "var(--c-slate)", "–"),
        "corrected" => ("corrected anyway", "var(--c-heat)", "!"),
        "not_corrected" => ("not corrected", "var(--c-sky)", "✓"),
        "regretted" => ("regretted later", "var(--c-pink)", "↶"),
        "tested" => ("tests green after", "var(--c-good)", "✓"),
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
        "msg_sent" => ("messages sent", "var(--c-sky)", "✉"),
        "msg_delivered" => ("messages shown", "var(--c-teal)", "✉"),
        "msg_dropped" => ("messages dropped", "var(--c-heat)", "⨯"),
        "handoff" => ("hand-offs", "var(--c-violet)", "⇆"),
        "handoff_refused" => ("edits refused after a hand-off", "var(--c-pink)", "⇆"),
        _ => ("other", "var(--c-muted)", "•"),
    }
}

/// A colour for the i-th category in a list (systems, purposes).
pub fn palette(i: usize) -> &'static str {
    const P: [&str; 8] =
        ["var(--c-accent)", "var(--c-good)", "var(--c-violet)", "var(--c-amber)", "var(--c-teal)", "var(--c-heat)", "var(--c-sky)", "var(--c-pink)"];
    P[i % P.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sender from another node comes with its machine: the initials are the name's, never "on".
    #[test]
    fn initials_leave_the_machine_out() {
        assert_eq!(initials("Bob Example on bob-laptop"), "BE");
        assert_eq!(initials("Ann on ann-desk"), "AN", "one word: its first two letters");
        assert_eq!(initials("git@carol.example on carol-mbp"), "GI", "an email's local part");
        assert_eq!(initials("Ann Example · app · claude on ann-desk"), "AE");
        assert_eq!(initials("Ann Example"), "AE");
    }
}
