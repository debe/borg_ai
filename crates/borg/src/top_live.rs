//! `borg top` on a terminal: one live screen, Unix top style. It redraws every second in the terminal's
//! alternate screen, fits the window, and quits on `q`, Esc or Ctrl-C with the terminal as it was.
//!
//! The screen per collective: a header with the drones by state, then the drones as a table with what
//! each is doing. `frame` is pure over its inputs (`Coll`); `run` is the terminal around it. Piped or
//! with `--once` / `--json`, `borg top` prints once instead (top.rs).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::collective::Share;
use crate::presence::State;
use crate::top::Drone;

/// One collective's data for a frame.
pub struct Coll {
    pub name: String,
    pub share: Share,
    pub drones: Vec<Drone>,
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

/// A drone's state word and its colour.
fn state_style(s: &str) -> St {
    match s {
        "working" => St::Green,
        "waiting" => St::Cyan,
        "idle" | "paused" => St::Yellow,
        _ => St::Dim,
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

/// The screen: at most `h` lines of at most `w` columns.
pub fn frame(cs: &[Coll], now: i64, w: usize, h: usize, color: bool) -> Vec<String> {
    let mut lines: Vec<Ln> = Vec::new();
    let all: Vec<&Drone> = cs.iter().flat_map(|c| &c.drones).collect();
    let count = |s: State| all.iter().filter(|d| d.state == s && !d.paused).count();
    let head = Ln::default()
        .add(St::Bold, format!("borg top {}", clock(now)))
        .add(St::Plain, format!("   {} drone(s): ", all.len()))
        .add(St::Green, format!("{} working ", count(State::Working)))
        .add(St::Cyan, format!("{} waiting ", count(State::Waiting)))
        .add(St::Yellow, format!("{} idle ", count(State::Idle)))
        .add(St::Dim, format!("{} ended", count(State::Ended)));
    let hint = "q quit";
    let gap = w.saturating_sub(head.len() + hint.len());
    lines.push(if gap >= 2 { head.add(St::Plain, " ".repeat(gap)).add(St::Dim, hint) } else { head });
    if cs.is_empty() {
        lines.push(Ln::default());
        lines.push(Ln::default().add(St::Dim, "no drones yet: sessions show here once their hooks run (borg setup wires them)"));
    }
    for c in cs {
        lines.push(Ln::default());
        let share = if c.share == Share::Team { "team" } else { "local" };
        lines.push(Ln::default().add(St::Head, format!(" {} ", c.name)).add(St::Head, format!("· {share} · {} drone(s)", c.drones.len())));
        lines.push(
            Ln::default()
                .add(St::Bold, format!("  {}{}{}{}{}", pad("DRONE", 16), pad("HARNESS", 9), pad("NODE", 8), pad("STATE", 13), pad("IDLE", 6)) + "DOING"),
        );
        let mut rows: Vec<&Drone> = c.drones.iter().collect();
        rows.sort_by_key(|d| (d.state == State::Ended, d.number));
        for d in rows {
            let word = if d.paused { "paused" } else { d.state.name() };
            let node = if d.node.is_empty() { "here".to_string() } else { short(&d.node) };
            let ended = d.state == State::Ended;
            lines.push(
                Ln::default()
                    .add(St::Plain, "  ")
                    .add(if ended { St::Dim } else { St::Bold }, pad(&d.designation, 16))
                    .add(St::Plain, pad(&d.p.harness, 9))
                    .add(St::Dim, pad(&node, 8))
                    .add(state_style(word), pad(&format!("● {word}"), 13))
                    .add(St::Dim, pad(&ago(now - d.p.ms), 6))
                    .add(if ended { St::Dim } else { St::Plain }, doing(&d.p.prompt)),
            );
        }
    }
    lines.truncate(h);
    lines.iter().map(|l| l.render(w, color)).collect()
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
    }

    #[test]
    fn the_screen_fits_the_window_and_counts_the_drones() {
        let cs = vec![Coll { name: "corp".into(), share: Share::Team, drones: crate::top::tests_fixture() }];
        let f = frame(&cs, 10_000_000, 100, 6, false);
        assert_eq!(f.len(), 6, "{f:#?}");
        assert!(f.iter().all(|l| l.chars().count() <= 100), "{f:#?}");
        assert!(frame(&cs, 10_000_000, 40, 6, false).iter().all(|l| l.chars().count() <= 40));
        assert!(f[0].contains("3 drone(s): 0 working 1 waiting 1 idle 0 ended") && f[0].ends_with("q quit"), "{}", f[0]);
        assert!(f[2].starts_with(" corp · team · 3 drone(s)"), "{}", f[2]);
        assert!(f[4].contains("One of Three") && f[4].contains("● paused"), "{}", f[4]);
        assert!(f[5].contains("Two of Three") && !f[5].contains('@') && f[5].contains("beefca"), "{}", f[5]);
        let empty = frame(&[], 0, 60, 10, false);
        assert!(empty[2].starts_with("no drones yet"), "{empty:#?}");
    }
}
