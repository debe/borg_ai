//! `borg top` on a terminal: one live screen, Unix top style. It redraws every second in the terminal's
//! alternate screen, fits the window, and quits on `q`, Esc or Ctrl-C with the terminal as it was.
//!
//! The screen per collective: a header with the drones by state, each hive goal as progress bars (tasks
//! verified, budget spent), its work tasks as chips, its plan and nodes, the drones as a table with what
//! each is doing, and the newest board events. `frame` is pure over its inputs (`Coll`); `run` is the
//! terminal around it. Piped or with `--once` / `--json`, `borg top` prints once instead (top.rs).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::collective::Share;
use crate::hive::board::{self, Ev, Event};
use crate::hive::view::HiveView;
use crate::presence::State;
use crate::top::Drone;

/// One collective's data for a frame.
pub struct Coll {
    pub name: String,
    pub share: Share,
    pub drones: Vec<Drone>,
    pub hives: Vec<HiveView>,
    /// Board events of its goals (beats left out), each with its goal.
    pub events: Vec<(String, Event)>,
}

#[derive(Clone, Copy, PartialEq)]
enum St {
    Plain,
    Bold,
    Dim,
    Head,
    Green,
    Cyan,
    Yellow,
    Red,
    Magenta,
    Blue,
}

impl St {
    fn code(self) -> &'static str {
        match self {
            St::Plain => "",
            St::Bold => "\x1b[1m",
            St::Dim => "\x1b[2m",
            St::Head => "\x1b[7m",
            St::Green => "\x1b[32m",
            St::Cyan => "\x1b[36m",
            St::Yellow => "\x1b[33m",
            St::Red => "\x1b[31m",
            St::Magenta => "\x1b[35m",
            St::Blue => "\x1b[34m",
        }
    }
}

/// One screen line of styled pieces.
#[derive(Default)]
struct Ln(Vec<(St, String)>);

impl Ln {
    fn add(mut self, st: St, s: impl Into<String>) -> Self {
        self.0.push((st, s.into()));
        self
    }

    fn len(&self) -> usize {
        self.0.iter().map(|(_, s)| s.chars().count()).sum()
    }

    /// At most `w` columns, a cut piece ending in `…`; a `Head` line is padded to `w` (a full bar).
    fn render(&self, w: usize, color: bool) -> String {
        let mut out = String::new();
        let mut left = w;
        let total = self.len();
        for (st, s) in &self.0 {
            if left == 0 {
                break;
            }
            let n = s.chars().count();
            let text: String =
                if n <= left && !(total > w && n == left) { s.clone() } else { s.chars().take(left.saturating_sub(1)).collect::<String>() + "…" };
            left -= text.chars().count().min(left);
            if color && *st != St::Plain {
                out.push_str(st.code());
                out.push_str(&text);
                out.push_str("\x1b[0m");
            } else {
                out.push_str(&text);
            }
        }
        if self.0.first().is_some_and(|(st, _)| *st == St::Head) && left > 0 {
            let pad = " ".repeat(left);
            out.push_str(&if color { format!("{}{pad}\x1b[0m", St::Head.code()) } else { pad });
        }
        out
    }
}

fn bar(frac: f64, n: usize) -> String {
    let full = ((frac.clamp(0.0, 1.0) * n as f64).round() as usize).min(n);
    format!("{}{}", "█".repeat(full), "░".repeat(n - full))
}

fn ago(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h{:02}", s / 3600, (s % 3600) / 60),
    }
}

/// Local wall-clock time of `ms` as HH:MM:SS.
fn clock(ms: i64) -> String {
    let t = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "--:--:--".into();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn pad(s: &str, n: usize) -> String {
    let c = s.chars().count();
    if c > n { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" } else { format!("{s}{}", " ".repeat(n - c)) }
}

fn short(node: &str) -> String {
    node.chars().take(6).collect()
}

/// A drone's state word and its colour, from the hive (an interactive drone's) or its presence.
fn state_style(s: &str) -> St {
    match s {
        "working" => St::Green,
        "busy" => St::Magenta,
        "waiting" => St::Cyan,
        crate::hive::monitor::STALE => St::Yellow,
        "idle" | "paused" => St::Yellow,
        "not waiting" => St::Red,
        _ => St::Dim,
    }
}

fn task_glyph(status: &str) -> (&'static str, St) {
    match status {
        "verified" => ("✔", St::Green),
        "done" => ("●", St::Blue),
        "claimed" => ("◐", St::Yellow),
        "blocked" => ("✖", St::Red),
        _ => ("○", St::Dim),
    }
}

/// The first line of `s` worth reading: no blank line, no bare `<tag>` line (a woken turn's notification).
fn first_line(s: &str) -> String {
    let tag = |l: &str| l.starts_with('<') && l.ends_with('>') && !l.contains(' ');
    let words = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    s.lines().map(str::trim).find(|l| !l.is_empty() && !tag(l)).map(words).unwrap_or_default()
}

/// What a drone's last prompt asked, a woken turn's notification as such.
fn doing(prompt: &str) -> String {
    match first_line(prompt) {
        s if s.is_empty() && !prompt.trim().is_empty() => "(woken for work)".into(),
        s => s,
    }
}

/// `s` with each full commit hash cut to 7 characters.
fn short_hashes(s: &str) -> String {
    s.split(' ').map(|w| if w.len() == 40 && w.chars().all(|c| c.is_ascii_hexdigit()) { &w[..7] } else { w }).collect::<Vec<_>>().join(" ")
}

/// Per session, from the board's events in order: the task its newest claim still holds and how many
/// tasks it finished (done or verified as a verifier).
fn work_of(events: &[(String, Event)]) -> std::collections::HashMap<&str, (String, usize)> {
    let mut sorted: Vec<&Event> = events.iter().map(|(_, e)| e).collect();
    sorted.sort_by_key(|e| e.ms);
    let mut out: std::collections::HashMap<&str, (String, usize)> = std::collections::HashMap::new();
    for e in sorted {
        let w = out.entry(e.by.as_str()).or_default();
        match e.ev {
            Ev::Claim => w.0 = e.task.clone(),
            Ev::Done | Ev::Verified | Ev::Rejected | Ev::Released | Ev::Blocked if w.0 == e.task => {
                if matches!(e.ev, Ev::Done | Ev::Verified | Ev::Rejected) {
                    w.1 += 1;
                }
                w.0.clear();
            }
            _ => {}
        }
    }
    out
}

/// The screen: at most `h` lines of at most `w` columns.
pub fn frame(cs: &[Coll], now: i64, w: usize, h: usize, color: bool) -> Vec<String> {
    let mut lines: Vec<Ln> = Vec::new();
    let all: Vec<&Drone> = cs.iter().flat_map(|c| &c.drones).collect();
    let count = |s: State| all.iter().filter(|d| d.state == s && !d.paused).count();
    let goals: usize = cs.iter().map(|c| c.hives.len()).sum();
    let head = Ln::default()
        .add(St::Bold, format!("borg top {}", clock(now)))
        .add(St::Plain, format!("   {} drone(s): ", all.len()))
        .add(St::Green, format!("{} working ", count(State::Working)))
        .add(St::Cyan, format!("{} waiting ", count(State::Waiting)))
        .add(St::Yellow, format!("{} idle ", count(State::Idle)))
        .add(St::Dim, format!("{} ended", count(State::Ended)))
        .add(St::Plain, format!("   {goals} goal(s)"));
    let hint = "q quit";
    let gap = w.saturating_sub(head.len() + hint.len());
    lines.push(if gap >= 2 { head.add(St::Plain, " ".repeat(gap)).add(St::Dim, hint) } else { head });
    if cs.is_empty() {
        lines.push(Ln::default());
        lines.push(Ln::default().add(St::Dim, "no drones yet: sessions show here once their hooks run (borg setup wires them)"));
    }
    let mut feed: Vec<(i64, Ln)> = Vec::new();
    for c in cs {
        lines.push(Ln::default());
        let share = if c.share == Share::Team { "team" } else { "local" };
        lines.push(Ln::default().add(St::Head, format!(" {} ", c.name)).add(St::Head, format!("· {share} · {} drone(s)", c.drones.len())));
        // Session -> (drone name, harness) of this node's interactive drones, for the drone table and the feed.
        let mut who: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
        for v in &c.hives {
            for d in &v.monitor.interactive {
                who.insert(d.session.as_str(), format!("{} {}", d.harness, d.drone));
            }
        }
        for d in &c.drones {
            who.insert(d.p.session.as_str(), format!("{} ({})", d.designation, d.p.harness));
        }
        let here: Option<&str> = c.hives.iter().flat_map(|v| &v.monitor.members).find(|n| n.here).map(|n| n.node.as_str());
        let work = work_of(&c.events);
        for v in &c.hives {
            goal_lines(&mut lines, v, now);
        }
        // The drone table.
        lines.push(Ln::default().add(
            St::Bold,
            format!(
                "  {}{}{}{}{}{}{}{}{}",
                pad("DRONE", 16),
                pad("HARNESS", 9),
                pad("NODE", 8),
                pad("STATE", 13),
                pad("TASK", 9),
                pad("DONE", 5),
                pad("WAKE", 7),
                pad("SPENT", 8),
                pad("IDLE", 6)
            ) + "DOING",
        ));
        let mut rows: Vec<&Drone> = c.drones.iter().collect();
        rows.sort_by_key(|d| (d.state == State::Ended, d.number));
        for d in rows {
            // The hive's rows name a session by its first 8 characters.
            let hive = c.hives.iter().flat_map(|v| &v.monitor.interactive).find(|r| !r.session.is_empty() && d.p.session.starts_with(&r.session));
            let held = work.get(d.p.session.as_str()).map(|w| w.0.clone()).unwrap_or_default();
            let (word, task) = match hive {
                Some(r) => match r.state.split_once(' ') {
                    Some(("working", t)) => ("working".to_string(), t.to_string()),
                    _ => (r.state.clone(), r.task.clone()),
                },
                None if d.paused => ("paused".into(), held),
                None if d.state == State::Ended => (d.state.name().to_string(), String::new()),
                None => (d.state.name().to_string(), held),
            };
            let st = state_style(&word);
            let wake = hive.and_then(|r| r.wakes.iter().rev().find(|w| w.taken)).map(|w| format!("{:.1}s", w.latency_ms as f64 / 1000.0)).unwrap_or_default();
            let spent = hive.map(|r| if r.unmetered && r.usd == 0.0 { "-".to_string() } else { format!("${:.2}", r.usd.max(0.0)) }).unwrap_or_default();
            let done = work.get(d.p.session.as_str()).map(|w| w.1).filter(|n| *n > 0).map(|n| n.to_string()).unwrap_or_default();
            let harness = if d.p.harness.is_empty() { hive.map(|r| r.harness.clone()).unwrap_or_default() } else { d.p.harness.clone() };
            let node = if d.node.is_empty() { "here".to_string() } else { short(&d.node) };
            let name_st = if d.state == State::Ended { St::Dim } else { St::Bold };
            lines.push(
                Ln::default()
                    .add(St::Plain, "  ")
                    .add(name_st, pad(&d.designation, 16))
                    .add(St::Plain, pad(&harness, 9))
                    .add(St::Dim, pad(&node, 8))
                    .add(st, pad(&format!("● {word}"), 13))
                    .add(St::Bold, pad(&task, 9))
                    .add(St::Plain, pad(&done, 5))
                    .add(St::Plain, pad(&wake, 7))
                    .add(St::Plain, pad(&spent, 8))
                    .add(St::Dim, pad(&ago(now - d.p.ms), 6))
                    .add(if d.state == State::Ended { St::Dim } else { St::Plain }, doing(&d.p.prompt)),
            );
        }
        for (goal, e) in &c.events {
            let Some((verb, st)) = verb(e) else { continue };
            let by = match who.get(e.by.as_str()) {
                Some(name) => name.clone(),
                None if e.by == "runner" => "node".into(),
                None if e.by.starts_with("integrate") => "integrator".into(),
                None => e.by.strip_prefix("engineer:").map(|m| format!("engineer {m}")).unwrap_or_else(|| e.by.chars().take(12).collect()),
            };
            let node = if e.node.is_empty() || here.is_some_and(|h| short(h) == short(&e.node)) { String::new() } else { format!("  @{}", short(&e.node)) };
            let task = if e.task.is_empty() { goal.clone() } else { e.task.clone() };
            feed.push((
                e.ms,
                Ln::default()
                    .add(St::Dim, format!("  {}  ", clock(e.ms)))
                    .add(St::Bold, pad(&task, 14))
                    .add(st, pad(verb, 12))
                    .add(St::Plain, format!("{by}{node}")),
            ));
        }
    }
    // The newest events fill what is left of the screen.
    let room = h.saturating_sub(lines.len() + 2);
    if room > 0 && !feed.is_empty() {
        feed.sort_by_key(|(ms, _)| std::cmp::Reverse(*ms));
        lines.push(Ln::default());
        lines.push(Ln::default().add(St::Bold, "  RECENT"));
        lines.extend(feed.into_iter().take(room).map(|(_, l)| l));
    }
    lines.truncate(h);
    lines.iter().map(|l| l.render(w, color)).collect()
}

fn verb(e: &Event) -> Option<(&'static str, St)> {
    Some(match e.ev {
        Ev::Open => ("opened", St::Plain),
        Ev::Claim => ("claimed", St::Yellow),
        Ev::Done => ("done", St::Blue),
        Ev::Verified => ("verified ✔", St::Green),
        Ev::Rejected => ("rejected", St::Red),
        Ev::Released => ("released", St::Dim),
        Ev::Blocked => ("blocked", St::Red),
        Ev::Heartbeat if e.task.is_empty() && e.note == board::JOIN => ("node in", St::Cyan),
        Ev::Heartbeat if e.task.is_empty() && e.note == board::LEAVE => ("node out", St::Dim),
        _ => return None,
    })
}

fn goal_lines(lines: &mut Vec<Ln>, v: &HiveView, now: i64) {
    // Work tasks only: verify and integrate tasks open as work finishes, and would move the total.
    let work: Vec<_> = v.tasks.iter().filter(|t| t.role == "work").collect();
    let total = work.len().max(1);
    let verified = work.iter().filter(|t| t.status == "verified").count();
    let claimed = v.counts.get("claimed").copied().unwrap_or(0);
    let (state, st) = if v.finished {
        ("finished", St::Green)
    } else if claimed > 0 {
        ("working", St::Yellow)
    } else {
        ("waiting for drones", St::Cyan)
    };
    let spend = if v.budget_usd > 0.0 { v.spent_usd / v.budget_usd } else { 0.0 };
    lines.push(
        Ln::default()
            .add(St::Plain, "  ")
            .add(St::Bold, format!("GOAL {}  ", v.goal))
            .add(St::Green, bar(verified as f64 / total as f64, 20))
            .add(St::Plain, format!(" {verified}/{} verified   ", work.len()))
            .add(if spend >= 0.9 { St::Red } else { St::Magenta }, bar(spend, 10))
            .add(St::Plain, format!(" ${:.2} of ${:.2}   ", v.spent_usd, v.budget_usd))
            .add(st, format!("● {state}")),
    );
    if !v.spec.is_empty() {
        lines.push(Ln::default().add(St::Dim, format!("    {}", first_line(&v.spec))));
    }
    let mut chips = Ln::default().add(St::Dim, "    tasks ");
    let mut rest = 0;
    for t in &v.tasks {
        if t.role != "work" {
            rest += 1;
            continue;
        }
        let (g, st) = task_glyph(&t.status);
        chips = chips.add(st, format!("{g} ")).add(St::Plain, format!("{}  ", t.id));
    }
    if rest > 0 {
        chips = chips.add(St::Dim, format!("+{rest} verify/integrate"));
    }
    lines.push(chips);
    let mut info = Ln::default().add(St::Dim, "    plan ");
    info = match &v.plan {
        Some(p) if p.state == "approved" => info.add(St::Green, format!("✔ approved{}", if p.by.is_empty() { String::new() } else { format!(" by {}", p.by) })),
        Some(p) if p.state == "waiting" => info.add(St::Yellow, "? waiting for your answer"),
        Some(p) if p.state == "refused" => info.add(St::Red, "✖ refused"),
        Some(p) => info.add(St::Dim, p.state.clone()),
        None => info.add(St::Dim, "-"),
    };
    info = info.add(St::Dim, "   nodes ");
    for n in &v.monitor.members {
        let st = match n.state.as_str() {
            "in" => St::Green,
            "left" => St::Dim,
            _ => St::Red,
        };
        let beat = n.beat_age_ms.map(|a| format!(" beat {}", ago(a))).unwrap_or_default();
        let worker = if n.here && !n.worker.is_empty() && n.worker != "none" { format!(" worker {}", n.worker) } else { String::new() };
        info = info.add(st, "● ").add(St::Plain, format!("{}{}{beat}{worker}   ", short(&n.node), if n.here { " (here)" } else { "" }));
    }
    lines.push(info);
    if let Some(i) = v.monitor.integrations.last() {
        let st = if i.status == "verified" { St::Green } else { St::Yellow };
        lines.push(
            Ln::default()
                .add(St::Dim, "    integration ")
                .add(st, format!("{} {}", i.status, short_hashes(&first_line(&i.result))))
                .add(St::Dim, format!("  {} ago", ago(now - i.opened_ms))),
        );
    }
    for a in v.alerts.iter().take(2) {
        lines.push(Ln::default().add(St::Red, format!("    ! {}", first_line(a))));
    }
    for (kind, r) in v.monitor.stuck.iter().map(|r| ("stuck", r)).chain(v.monitor.declines.iter().map(|r| ("declined", r))).take(3) {
        lines.push(Ln::default().add(St::Yellow, format!("    {kind} {}: {}", r.task, first_line(&r.why))));
    }
}

// ---------------------------------------------------------------- the terminal

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(sig: libc::c_int) {
    if sig != libc::SIGWINCH {
        STOP.store(true, Ordering::SeqCst);
    }
}

/// The terminal while the screen runs: alternate screen, no cursor, keys unbuffered and unechoed.
/// Dropping it puts everything back.
struct Term {
    saved: Option<libc::termios>,
}

impl Term {
    fn enter() -> Term {
        let mut saved = None;
        if unsafe { libc::isatty(0) } == 1 {
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(0, &mut t) } == 0 {
                saved = Some(t);
                t.c_lflag &= !(libc::ICANON | libc::ECHO);
                t.c_cc[libc::VMIN] = 0;
                t.c_cc[libc::VTIME] = 0;
                unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
            }
        }
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGWINCH] {
            let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
            sa.sa_sigaction = on_signal as *const () as usize;
            unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
        }
        print!("\x1b[?1049h\x1b[?25l");
        let _ = std::io::stdout().flush();
        Term { saved }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        print!("\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
        if let Some(t) = self.saved {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
        }
    }
}

fn size() -> (usize, usize) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col as usize, ws.ws_row as usize)
    } else {
        (100, 30)
    }
}

/// Waits up to `d` for a key; true when it asks to quit (`q`, a lone Esc, Ctrl-C).
fn key(d: Duration, keys: bool) -> bool {
    if !keys {
        std::thread::sleep(d);
        return false;
    }
    let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    if unsafe { libc::poll(&mut fd, 1, d.as_millis() as libc::c_int) } <= 0 {
        return false;
    }
    let mut buf = [0u8; 16];
    let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
    let got = &buf[..n.max(0) as usize];
    got == [27] || got.iter().any(|&b| b == b'q' || b == b'Q' || b == 3)
}

/// The live screen until the engineer quits.
pub fn run(dir: Option<&str>) -> i32 {
    let color = std::env::var_os("NO_COLOR").is_none();
    let term = Term::enter();
    let keys = term.saved.is_some();
    while !STOP.load(Ordering::SeqCst) {
        let now = crate::learn::now_ms();
        let (w, h) = size();
        let lines = match crate::top::gather(dir, now) {
            Ok(cs) => frame(&cs, now, w, h, color),
            Err(e) => vec![format!("borg top: {e}")],
        };
        let mut out = String::from("\x1b[H");
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                out.push_str("\r\n");
            }
            out.push_str(l);
            out.push_str("\x1b[K");
        }
        out.push_str("\x1b[J");
        print!("{out}");
        let _ = std::io::stdout().flush();
        if key(Duration::from_secs(1), keys) {
            break;
        }
    }
    drop(term);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_fits_its_width_and_a_head_bar_fills_it() {
        let l = Ln::default().add(St::Bold, "abcdef").add(St::Plain, "ghij");
        assert_eq!(l.render(20, false), "abcdefghij");
        assert_eq!(l.render(10, false), "abcdefghij");
        assert_eq!(l.render(7, false), "abcdef…");
        assert_eq!(l.render(4, false), "abc…");
        assert_eq!(Ln::default().add(St::Head, " g7 ").render(8, false), " g7     ");
        assert_eq!(l.render(7, true), "\x1b[1mabcdef\x1b[0m…");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(2.0, 3), "███");
    }

    #[test]
    fn the_screen_fits_the_window_and_counts_the_drones() {
        let cs = vec![Coll { name: "corp".into(), share: Share::Team, drones: crate::top::tests_fixture(), hives: Vec::new(), events: Vec::new() }];
        let f = frame(&cs, 10_000_000, 100, 6, false);
        assert_eq!(f.len(), 6, "{f:#?}");
        assert!(f.iter().all(|l| l.chars().count() <= 100), "{f:#?}");
        assert!(frame(&cs, 10_000_000, 40, 6, false).iter().all(|l| l.chars().count() <= 40));
        assert!(f[0].contains("3 drone(s): 0 working 1 waiting 1 idle 0 ended   0 goal(s)") && f[0].ends_with("q quit"), "{}", f[0]);
        assert!(f[2].starts_with(" corp · team · 3 drone(s)"), "{}", f[2]);
        assert!(f[4].contains("One of Three") && f[4].contains("● paused"), "{}", f[4]);
        assert!(f[5].contains("Two of Three") && !f[5].contains('@') && f[5].contains("beefca"), "{}", f[5]);
        let empty = frame(&[], 0, 60, 10, false);
        assert!(empty[2].starts_with("no drones yet"), "{empty:#?}");
    }
}
