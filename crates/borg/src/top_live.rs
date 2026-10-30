//! `borg top` on a terminal: one live screen, Unix top style. It redraws every second in the terminal's
//! alternate screen, fits the window, and quits on `q`, Esc or Ctrl-C with the terminal as it was.
//!
//! Under the header, a line on borg itself when it has news (top.rs `version_line`: a newer release, a
//! member on another mesh protocol). The screen per collective: a header with the drones by state, then the drones as a table with what
//! each is doing and its unread messages, its lesson selection line (top.rs `Selection`) when it has
//! one, then the collective's last few messages. `frame` is pure over its inputs (`Coll`); `run` is
//! the terminal around it, and acknowledges the messages to the engineer a frame showed when no
//! session runs it (`acknowledge_shown`, top.rs `engineer_looks`). Piped or with `--once` / `--json`,
//! `borg top` prints once instead (top.rs).

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::clock::ago;
use crate::collective::Share;
use crate::msg::Message;
use crate::presence::State;
use crate::top::{self, Drone, Inbox};

/// One collective's data for a frame.
pub struct Coll {
    pub name: String,
    /// Its store, where the messages the frame shows the engineer are acknowledged.
    pub home: PathBuf,
    pub share: Share,
    pub drones: Vec<Drone>,
    pub inbox: Inbox,
    /// Its lesson selection line (top.rs `selection_line`), when `borg top` shows one.
    pub select: Option<String>,
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

    /// Whether `render(w)` shows any of the line's last `n` columns: all of them when the line fits,
    /// else those before the `…` it is cut at (top.rs `shows_last`: `render` cuts as top.rs `cut`).
    fn shows_last(&self, n: usize, w: usize) -> bool {
        top::shows_last(self.len(), n, w)
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

/// The screen: at most `h` lines of at most `w` columns, and per collective (its index in `cs`) the
/// messages its lines show: those within the height whose line, cut to the width, still shows some of
/// its text: a line cut before the text names only who wrote to whom, and `run` acknowledges what the
/// engineer read, as msg.rs `deliver` shows a drone at least the start of a message's text. `version`
/// is the line on borg itself (top.rs `version_line`), right under the header when there is one.
pub fn frame<'a>(cs: &'a [Coll], version: Option<&str>, now: i64, w: usize, h: usize, color: bool) -> (Vec<String>, Vec<(usize, &'a Message)>) {
    let mut lines: Vec<Ln> = Vec::new();
    // The line each message is on.
    let mut shown: Vec<(usize, usize, &Message)> = Vec::new();
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
    if let Some(v) = version {
        lines.push(Ln::default().add(St::Yellow, v));
    }
    if cs.is_empty() {
        lines.push(Ln::default());
        lines.push(Ln::default().add(St::Dim, "no drones yet: sessions show here once their hooks run (borg setup wires them)"));
    }
    for (i, c) in cs.iter().enumerate() {
        lines.push(Ln::default());
        let share = if c.share == Share::Team { "team" } else { "local" };
        lines.push(Ln::default().add(St::Head, format!(" {} ", c.name)).add(St::Head, format!("· {share} · {} drone(s)", c.drones.len())));
        lines.push(Ln::default().add(
            St::Bold,
            format!("  {}{}{}{}{}{}{}", pad("DRONE", 16), pad("HARNESS", 9), pad("NODE", 8), pad("STATE", 13), pad("WAKE", 7), pad("IDLE", 6), pad("MSG", 5))
                + "DOING",
        ));
        let mut rows: Vec<&Drone> = c.drones.iter().collect();
        rows.sort_by_key(|d| (d.state == State::Ended, d.number));
        for d in rows {
            let word = if d.paused { "paused" } else { d.state.name() };
            let node = if d.node.is_empty() { "here".to_string() } else { top::column_node(&d.node) };
            let ended = d.state == State::Ended;
            lines.push(
                Ln::default()
                    .add(St::Plain, "  ")
                    .add(if ended { St::Dim } else { St::Bold }, pad(&d.designation, 16))
                    .add(St::Plain, pad(&d.p.harness, 9))
                    .add(St::Dim, pad(&node, 8))
                    .add(state_style(word), pad(&format!("● {word}"), 13))
                    .add(if c.inbox.listen.contains_key(&d.p.session) { St::Cyan } else { St::Dim }, pad(top::wake_cell(&c.inbox, d), 7))
                    .add(St::Dim, pad(&ago(now - d.p.ms), 6))
                    .add(if c.inbox.unread.get(&d.p.session).is_some_and(|n| *n > 0) { St::Cyan } else { St::Dim }, pad(&top::unread_cell(&c.inbox, d), 5))
                    .add(if ended { St::Dim } else { St::Plain }, doing(&d.p.prompt)),
            );
        }
        if let Some(l) = &c.select {
            lines.push(Ln::default().add(St::Bold, "  SELECTION ").add(St::Dim, l.as_str()));
        }
        if !c.inbox.recent.is_empty() {
            lines.push(Ln::default().add(St::Bold, "  MESSAGES").add(St::Dim, format!("  {} in the last day", c.inbox.day)));
            for m in &c.inbox.recent {
                let st = if m.asks() { St::Cyan } else { St::Plain };
                let whole = top::message_line(&c.drones, &c.inbox.here, m);
                // The columns of its text, which `message_line` ends with (none when it does not, so
                // that only a line shown whole counts).
                let text = if whole.ends_with(m.text.as_str()) { m.text.chars().count() } else { 0 };
                let line = Ln::default().add(St::Plain, "  ").add(St::Dim, pad(&ago(now - m.ms), 6)).add(st, whole);
                if line.shows_last(text, w) {
                    shown.push((lines.len(), i, m));
                }
                lines.push(line);
            }
        }
    }
    lines.truncate(h);
    let shown = shown.into_iter().filter(|(at, ..)| *at < lines.len()).map(|(_, i, m)| (i, m)).collect();
    (lines.iter().map(|l| l.render(w, color)).collect(), shown)
}

/// Acknowledges the messages to the engineer a frame of `cs` showed (`shown`, `frame`'s) as the
/// engineer of the repo at `cwd` (top.rs `acknowledge`; `acked`: the ids this screen already passed
/// on), only when the engineer is the one looking: `caller` (msg.rs `caller`) finds no session over the
/// drones of `cs` (top.rs `engineer_looks`), as the printed screen and `borg inbox` decide. A harness
/// may run a drone's commands on a pseudo-terminal, where a plain `borg top` is this screen.
fn acknowledge_shown(
    cs: &[Coll],
    shown: &[(usize, &Message)],
    cwd: &str,
    caller: impl Fn(&[Drone]) -> Result<Option<(String, &'static str)>, String>,
    acked: &mut HashSet<String>,
    now: i64,
) {
    if shown.is_empty() || !top::engineer_looks(caller, cs.iter().map(|c| c.drones.as_slice())) {
        return;
    }
    for (i, c) in cs.iter().enumerate() {
        let mine: Vec<&Message> = shown.iter().filter(|(j, _)| *j == i).map(|(_, m)| *m).collect();
        top::acknowledge(&c.home, cwd, &mine, acked, now);
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
    // The messages to the engineer this screen already acknowledged (top.rs `acknowledge`), as whom.
    let (mut acked, cwd) = (HashSet::new(), top::engineer_dir(dir));
    while !STOP.load(Ordering::SeqCst) {
        let now = crate::learn::now_ms();
        let (w, h) = size();
        let lines = match crate::top::gather(dir, now) {
            Ok((cs, version)) => {
                let (lines, shown) = frame(&cs, version.as_deref(), now, w, h, color);
                acknowledge_shown(&cs, &shown, &cwd, crate::msg::caller, &mut acked, now);
                lines
            }
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
        // The last 4 columns ("ghij"): whole at 10, "g" at 8, none at 7 (the cut's `…` is not one).
        assert!(l.shows_last(4, 10) && l.shows_last(4, 8) && !l.shows_last(4, 7) && !l.shows_last(4, 0));
        assert!(l.shows_last(0, 10) && !l.shows_last(0, 9), "no columns show only on a line that fits");
    }

    #[test]
    fn the_screen_fits_the_window_and_counts_the_drones() {
        let cs = vec![Coll {
            name: "corp".into(),
            home: PathBuf::new(),
            share: Share::Team,
            drones: crate::top::tests_fixture(),
            inbox: crate::top::tests_inbox(),
            select: None,
        }];
        let f = frame(&cs, None, 10_000_000, 100, 6, false).0;
        assert_eq!(f.len(), 6, "{f:#?}");
        assert!(f.iter().all(|l| l.chars().count() <= 100), "{f:#?}");
        assert!(frame(&cs, None, 10_000_000, 40, 6, false).0.iter().all(|l| l.chars().count() <= 40));
        assert!(f[0].contains("3 drone(s): 0 working 1 waiting 1 idle 0 ended") && f[0].ends_with("q quit"), "{}", f[0]);
        assert!(f[2].starts_with(" corp · team · 3 drone(s)"), "{}", f[2]);
        assert!(f[4].contains("One of Three") && f[4].contains("● paused"), "{}", f[4]);
        assert!(f[5].contains("Two of Three") && !f[5].contains('@') && f[5].contains("beefca"), "{}", f[5]);
        assert!(f[4].contains(" 2    fix"), "the unread count: {}", f[4]);
        let full = frame(&cs, None, 10_000_000, 100, 40, false).0;
        let at = full.iter().position(|l| l.starts_with("  MESSAGES  3 in the last day")).expect("the messages section");
        assert!(full[at + 1].contains("1m") && full[at + 1].ends_with("Three of Three asks -> One of Three: which test flakes?"), "{}", full[at + 1]);
        assert_eq!(full.len(), at + 4, "{full:#?}");
        let empty = frame(&[], None, 0, 60, 10, false).0;
        assert!(empty[2].starts_with("no drones yet"), "{empty:#?}");
    }

    /// What a frame shows the engineer is what its lines hold: a window too short for the messages
    /// shows (and so acknowledges, `run`) only those it kept; a window too narrow shows only the
    /// messages whose line, cut to the width, still shows some of the text.
    #[test]
    fn a_frame_names_the_messages_it_shows() {
        let cs = vec![Coll {
            name: "corp".into(),
            home: PathBuf::new(),
            share: Share::Team,
            drones: crate::top::tests_fixture(),
            inbox: crate::top::tests_inbox(),
            select: None,
        }];
        let ids = |h: usize, w: usize| frame(&cs, None, 10_000_000, w, h, false).1.into_iter().map(|(i, m)| (i, m.id.clone())).collect::<Vec<_>>();
        let all: Vec<(usize, String)> = cs[0].inbox.recent.iter().map(|m| (0, m.id.clone())).collect();
        let (lines, _) = frame(&cs, None, 10_000_000, 200, 40, false);
        assert_eq!(ids(40, 200), all, "{lines:#?}");
        let at = lines.iter().position(|l| l.starts_with("  MESSAGES")).expect("the messages section");
        assert_eq!(ids(at + 2, 200), all[..1], "the first message line only");
        assert!(ids(at + 1, 200).is_empty(), "the section's head, no message");
        // The lines: "  1m    Three of Three asks -> One of Three: " (45 columns) then 18 of text,
        // "  2m    alice@x -> collective: " (31) then 15, "  3m    Six of Six @feedfeedfeed -> zzzz9999: "
        // (46) then 4.
        let line = |w: usize, k: usize| frame(&cs, None, 10_000_000, w, 40, false).0[at + 1 + k].clone();
        assert!(ids(40, 30).is_empty(), "each cut before its text: {:?}", line(30, 0));
        assert_eq!(ids(40, 40), all[1..2], "only the second shows any text: {:?}", line(40, 1));
        assert!(line(46, 0).ends_with("One of Three: …") && line(46, 1).ends_with("release at five"), "{:?}", line(46, 0));
        assert_eq!(ids(40, 46), all[1..2], "the first's cut leaves only its `…` after the recipient");
        assert!(line(47, 0).ends_with("One of Three: w…"), "{:?}", line(47, 0));
        assert_eq!(ids(40, 47), all[..2], "the first shows its text's first character");
        assert!(line(48, 2).ends_with("zzzz9999: d…"), "{:?}", line(48, 2));
        assert_eq!(ids(40, 48), all);
    }

    /// The live screen acknowledges the messages to the engineer a frame shows (`run`, top.rs
    /// `acknowledge`): one whose line a narrow pane cuts before its text is not among them.
    #[test]
    fn a_narrow_pane_does_not_show_a_message_to_the_engineer_cut_before_its_text() {
        let from = crate::msg::Sender { by: "aaaa1111-x".into(), name: "One of Three".into(), harness: "claude".into(), node: "n1".into() };
        let m = Message::new(crate::msg::TELL, &from, "engineer", None, 10_000_000 - 60_000, "please review the sync fix");
        let cs = vec![Coll {
            name: "corp".into(),
            home: PathBuf::new(),
            share: Share::Team,
            drones: crate::top::tests_fixture(),
            inbox: Inbox { recent: vec![m.clone()], ..crate::top::tests_inbox() },
            select: None,
        }];
        let (lines, shown) = frame(&cs, None, 10_000_000, 30, 40, false);
        let at = lines.iter().position(|l| l.starts_with("  MESSAGES")).expect("the messages section");
        assert_eq!(lines[at + 1], "  1m    One of Three -> engin…");
        assert!(shown.is_empty(), "{shown:?}", shown = shown.iter().map(|(_, m)| &m.id).collect::<Vec<_>>());
        let (lines, shown) = frame(&cs, None, 10_000_000, 36, 40, false);
        assert_eq!(lines[at + 1], "  1m    One of Three -> engineer: p…");
        assert_eq!(shown.iter().map(|(i, m)| (*i, m.id.clone())).collect::<Vec<_>>(), [(0, m.id.clone())]);
    }

    /// The calling session msg.rs `caller` tells from a shell whose session variables are `sessions` and
    /// harness pids `pids` (its own `pick`), whatever the shell running the tests sets.
    fn shell(sessions: Vec<(String, &'static str)>, pids: Vec<(u32, &'static str)>) -> impl Fn(&[Drone]) -> Result<Option<(String, &'static str)>, String> {
        move |ds| crate::msg::pick(sessions.clone(), &pids, ds)
    }

    /// The live screen acknowledges the messages to the engineer a frame showed only when no session
    /// runs it (`acknowledge_shown`, top.rs `engineer_looks`): a harness that runs a drone's commands on
    /// a pseudo-terminal gets this screen for a plain `borg top`. A session variable, or opencode's pid
    /// naming a drone of another collective on the screen, is a session's; a terminal is the engineer's.
    #[test]
    fn a_session_running_the_live_screen_acknowledges_nothing_as_the_engineer() {
        let base = std::env::temp_dir().join(format!("borg-top-live-ack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (home, work) = (base.join("store"), base.join("work"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let git = |a: &[&str]| std::process::Command::new("git").arg("-C").arg(&work).args(a).output().is_ok_and(|o| o.status.success());
        assert!(git(&["init", "-q"]) && git(&["config", "user.email", "ann@x"]));
        let here = crate::msg::node(&home);
        let from = crate::msg::Sender { by: "aaaa1111-x".into(), name: "One of Three".into(), harness: "claude".into(), node: here.clone() };
        let m = Message::new(crate::msg::TELL, &from, crate::msg::ENGINEER, None, 10_000_000 - 3_000, "the build is green");
        crate::msg::send(&home, m.clone(), 10_000_000).unwrap();
        let pid = std::process::id();
        let mut side = crate::top::tests_fixture();
        let oc = side.iter_mut().find(|d| d.node.is_empty()).expect("a local drone");
        oc.p.harness = "opencode".into();
        oc.p.pid = Some(pid);
        let cs = vec![
            Coll {
                name: "corp".into(),
                home: home.clone(),
                share: Share::Local,
                drones: Vec::new(),
                inbox: Inbox { recent: vec![m.clone()], here, ..Inbox::default() },
                select: None,
            },
            Coll { name: "side".into(), home: base.join("side"), share: Share::Local, drones: side, inbox: Inbox::default(), select: None },
        ];
        let (_, shown) = frame(&cs, None, 10_000_000, 100, 40, false);
        assert_eq!(shown.iter().map(|(i, m)| (*i, m.id.as_str())).collect::<Vec<_>>(), [(0, m.id.as_str())], "the frame shows it");
        let seen = || crate::msg::lines(&home).into_iter().filter(|l| matches!(l, crate::msg::Line::Seen(_))).count();
        let cwd = work.display().to_string();
        let shells = [
            ("Claude Code", shell(vec![("eeee5555-x".into(), "claude")], Vec::new())),
            ("opencode, its drone in the other collective", shell(Vec::new(), vec![(pid, "opencode")])),
        ];
        for (what, caller) in shells {
            acknowledge_shown(&cs, &shown, &cwd, caller, &mut HashSet::new(), 10_000_000);
            assert_eq!(seen(), 0, "{what}: acknowledged nothing as the engineer");
        }
        acknowledge_shown(&cs, &shown, &cwd, shell(Vec::new(), vec![(pid + 1, "opencode")]), &mut HashSet::new(), 10_000_000);
        assert_eq!(seen(), 1, "a terminal (no drone of its pid): the engineer's");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A collective's selection line sits under its drones and above its messages, cut to the width.
    #[test]
    fn the_selection_line_sits_between_the_drones_and_the_messages() {
        let line = "learned, gate 0.120 (quality it may still risk below the fixed rule) · 480 checks";
        let cs = vec![Coll {
            name: "corp".into(),
            home: PathBuf::new(),
            share: Share::Team,
            drones: crate::top::tests_fixture(),
            inbox: crate::top::tests_inbox(),
            select: Some(line.into()),
        }];
        let full = frame(&cs, None, 10_000_000, 200, 40, false).0;
        let at = full.iter().position(|l| l.starts_with("  SELECTION ")).expect("the selection line");
        assert_eq!(full[at], format!("  SELECTION {line}"));
        assert!(full[at - 1].contains("Three of Three") && full[at + 1].starts_with("  MESSAGES"), "{full:#?}");
        assert!(frame(&cs, None, 10_000_000, 40, 40, false).0.iter().all(|l| l.chars().count() <= 40));
    }

    /// The line on borg itself sits right under the header, cut to the width; without one the screen is
    /// as before.
    #[test]
    fn the_line_on_borg_itself_sits_under_the_header() {
        let cs = vec![Coll {
            name: "corp".into(),
            home: PathBuf::new(),
            share: Share::Team,
            drones: crate::top::tests_fixture(),
            inbox: crate::top::tests_inbox(),
            select: None,
        }];
        let v = "borg 9.0.0 is released, this is 0.7.0: borg update";
        let with = frame(&cs, Some(v), 10_000_000, 200, 40, false).0;
        let without = frame(&cs, None, 10_000_000, 200, 40, false).0;
        assert_eq!(with[1], v);
        assert_eq!(with[0], without[0]);
        assert_eq!(&with[2..], &without[1..], "the rest moves down one line");
        assert!(frame(&cs, Some(v), 10_000_000, 30, 40, false).0[1].chars().count() <= 30);
    }
}
