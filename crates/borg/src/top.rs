//! `borg top [--dir DIR] [--watch] [--json]`: the drones of each collective, one screen.
//!
//! Local drones come from presence records (presence.rs), remote ones from what the mesh daemon
//! last heard from members (only for a collective that may sync). Drones are designated by start
//! order across both, Borg style: "Two of Five". `--dir` shows only the collective DIR routes to;
//! otherwise every collective on this machine that has drones. `--watch` redraws every 2 s,
//! `--json` prints the same data for scripts.

use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::collective::{self, Route, Share};
use crate::presence::{self, Presence, State};
use crate::{install, learn, steer};

#[derive(Debug, Clone, Serialize)]
pub struct Drone {
    /// "Two of Five".
    pub designation: String,
    /// 1-based start order.
    pub number: usize,
    /// The member key of the node it runs on; empty for this machine.
    pub node: String,
    pub state: State,
    /// Paused by a steering directive (local drones only).
    pub paused: bool,
    #[serde(flatten)]
    pub p: Presence,
}

const NUMBERS: [&str; 20] = [
    "One",
    "Two",
    "Three",
    "Four",
    "Five",
    "Six",
    "Seven",
    "Eight",
    "Nine",
    "Ten",
    "Eleven",
    "Twelve",
    "Thirteen",
    "Fourteen",
    "Fifteen",
    "Sixteen",
    "Seventeen",
    "Eighteen",
    "Nineteen",
    "Twenty",
];

pub fn word(n: usize) -> String {
    NUMBERS.get(n.wrapping_sub(1)).map(|s| s.to_string()).unwrap_or_else(|| n.to_string())
}

/// The drones of the collective stored at `home` (pure over its inputs, for tests).
pub fn assemble(local: Vec<Presence>, remote: Vec<presence::Remote>, now: i64, alive: impl Fn(u32) -> bool, paused: impl Fn(&Presence) -> bool) -> Vec<Drone> {
    let mut all: Vec<(String, Presence, State, bool)> = local
        .into_iter()
        .map(|p| {
            let s = presence::state(&p, now, &alive);
            let z = s != State::Ended && paused(&p);
            (String::new(), p, s, z)
        })
        .collect();
    // A remote pid means nothing here: its state comes from its events and age only.
    all.extend(remote.into_iter().map(|r| {
        let s = presence::state(&r.p, now, |_| true);
        (r.key, r.p, s, false)
    }));
    all.sort_by(|a, b| (a.1.started, &a.0, &a.1.session).cmp(&(b.1.started, &b.0, &b.1.session)));
    let n = all.len();
    all.into_iter()
        .enumerate()
        .map(|(i, (node, p, state, paused))| Drone { designation: format!("{} of {}", word(i + 1), word(n)), number: i + 1, node, state, paused, p })
        .collect()
}

/// The drones of the collective stored at `home` as of `now`; remote ones only if it may sync.
pub fn drones(home: &Path, may_sync: bool, now: i64) -> Vec<Drone> {
    let remote = if may_sync { presence::remote(home, now) } else { Vec::new() };
    assemble(presence::local(home, now), remote, now, presence::alive, |p| steer::pending(home, &p.session, p.started, now).paused.is_some())
}

/// Which drone `target` names: "all", a designation ("Two", "Two of Five", "2") or a session id
/// (or a unique prefix of at least 4 characters). `Ok(None)` is every drone.
pub fn resolve<'a>(drones: &'a [Drone], target: &str) -> Result<Option<&'a Drone>, String> {
    let t = target.trim();
    if t.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let first = t.split_whitespace().next().unwrap_or_default();
    let number = first.parse::<usize>().ok().or_else(|| NUMBERS.iter().position(|w| w.eq_ignore_ascii_case(first)).map(|i| i + 1));
    if let Some(n) = number {
        return drones.iter().find(|d| d.number == n).map(Some).ok_or_else(|| format!("no drone {target}: there are {} (`borg top`)", drones.len()));
    }
    let hits: Vec<&Drone> = drones.iter().filter(|d| d.p.session == t || (t.len() >= 4 && d.p.session.starts_with(t))).collect();
    match hits.as_slice() {
        [d] => Ok(Some(d)),
        [] => Err(format!("no drone or session {target:?} (`borg top`)")),
        _ => Err(format!("{target:?} names {} sessions: give more of the id", hits.len())),
    }
}

fn ago(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

/// One collective's screen.
pub fn render(name: &str, share: Share, drones: &[Drone], now: i64) -> String {
    let working = drones.iter().filter(|d| matches!(d.state, State::Working | State::Waiting)).count();
    let remote = drones.iter().filter(|d| !d.node.is_empty()).count();
    let share = match share {
        Share::Team => "team",
        Share::Local => "local, never leaves this machine",
    };
    let mut s = format!("{name} ({share}): {} drone(s), {working} active, {remote} remote\n", drones.len());
    if drones.is_empty() {
        return s;
    }
    s.push_str(&format!("  {:<18} {:<8} {:<24} {:<14} {:<8} {:>6}  {:<40} {}\n", "DRONE", "SESSION", "ENGINEER", "REPO", "STATE", "IDLE", "DOING", "FILES"));
    for d in drones {
        let state = if d.paused { "paused" } else { d.state.name() };
        let engineer = if d.node.is_empty() { d.p.engineer.clone() } else { format!("{} @{}", d.p.engineer, d.node.get(..6).unwrap_or(&d.node)) };
        let line = format!(
            "  {:<18} {:<8} {:<24} {:<14} {:<8} {:>6}  {:<40} {}",
            d.designation,
            cut(&d.p.session, 8),
            cut(if engineer.is_empty() { "?" } else { &engineer }, 24),
            cut(&d.p.repo, 14),
            state,
            ago(now - d.p.ms),
            cut(&d.p.prompt, 40),
            d.p.files.join(", ")
        );
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

pub fn to_json(name: &str, share: Share, drones: &[Drone]) -> Value {
    json!({"collective": name, "share": if share == Share::Team { "team" } else { "local" }, "drones": drones})
}

/// The collectives to show: the one `dir` routes to, or every one on this machine.
fn routes(dir: Option<&str>) -> Result<Vec<Route>, String> {
    let cfg = collective::load()?;
    let base = learn::home();
    Ok(match dir {
        Some(d) => vec![collective::route(cfg.as_ref(), d, &base)].into_iter().filter(|r| *r != Route::Off).collect(),
        None => install::collectives(cfg.as_ref(), &base),
    })
}

fn screen(dir: Option<&str>, json_out: bool, now: i64) -> Result<String, String> {
    let mut text = String::new();
    let mut all = Vec::new();
    for r in routes(dir)? {
        let Route::Collective { name, home, share } = &r else { continue };
        presence::prune(home, now);
        let mut ds = drones(home, collective::may_sync(&r), now);
        // An engineer's several emails show as their name (collective.rs "Engineers"), peers' too.
        for d in &mut ds {
            d.p.engineer = collective::aliases().display(&d.p.engineer);
        }
        if json_out {
            all.push(to_json(name, *share, &ds));
        } else if !ds.is_empty() || dir.is_some() {
            text.push_str(&render(name, *share, &ds, now));
            text.push('\n');
        }
    }
    if json_out {
        return Ok(serde_json::to_string_pretty(&all).unwrap_or_default() + "\n");
    }
    if text.is_empty() {
        text = "no drones (borg top lists sessions whose hooks ran in the last day)\n".into();
    }
    Ok(text)
}

pub fn main(args: &[&str]) -> i32 {
    let flag = |f: &str| args.contains(&f);
    let dir = args.iter().position(|a| *a == "--dir").and_then(|i| args.get(i + 1)).copied();
    let (watch, json_out) = (flag("--watch"), flag("--json"));
    loop {
        match screen(dir, json_out, learn::now_ms()) {
            Ok(s) if watch => print!("\x1b[2J\x1b[H{s}"),
            Ok(s) => print!("{s}"),
            Err(e) => {
                eprintln!("borg top: {e}");
                return 1;
            }
        }
        if !watch {
            return 0;
        }
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(session: &str, engineer: &str, started: i64, ms: i64, event: &str, prompt: &str, files: &[&str]) -> Presence {
        Presence {
            session: session.into(),
            harness: "claude".into(),
            engineer: engineer.into(),
            repo: "borg-ai".into(),
            collective: "corp".into(),
            prompt: prompt.into(),
            files: files.iter().map(|f| f.to_string()).collect(),
            event: event.into(),
            ms,
            started,
            pid: None,
        }
    }

    const NOW: i64 = 10_000_000;

    fn fixture() -> Vec<Drone> {
        let local = vec![
            p("aaaa1111-x", "alice@x", 100, NOW - 5_000, "post_tool", "fix the flaky sync test", &["crates/borg-mesh/src/sync.rs", "src/recon.rs"]),
            p("cccc3333-x", "alice@x", 300, NOW - presence::IDLE_MS - 60_000, "stop", "write the release notes for the mesh daemon and more words", &[]),
        ];
        let remote = vec![presence::Remote {
            p: p("bbbb2222-y", "bob@y", 200, NOW - 120_000, "stop", "review PR 42", &["README.md"]),
            key: "beefcafe".repeat(8),
            sig: String::new(),
        }];
        assemble(local, remote, NOW, |_| true, |p| p.session.starts_with("aaaa"))
    }

    #[test]
    fn renders_one_screen_per_collective() {
        let out = render("corp", Share::Team, &fixture(), NOW);
        let expected = "\
corp (team): 3 drone(s), 2 active, 1 remote
  DRONE              SESSION  ENGINEER                 REPO           STATE      IDLE  DOING                                    FILES
  One of Three       aaaa111… alice@x                  borg-ai        paused       5s  fix the flaky sync test                  crates/borg-mesh/src/sync.rs, src/recon.rs
  Two of Three       bbbb222… bob@y @beefca            borg-ai        waiting      2m  review PR 42                             README.md
  Three of Three     cccc333… alice@x                  borg-ai        idle        31m  write the release notes for the mesh da…
";
        assert_eq!(out, expected, "\n{out}");
        assert_eq!(render("private", Share::Local, &[], NOW), "private (local, never leaves this machine): 0 drone(s), 0 active, 0 remote\n");
    }

    #[test]
    fn json_for_scripts() {
        let v = to_json("corp", Share::Team, &fixture());
        assert_eq!(v["collective"], "corp");
        let d = &v["drones"];
        assert_eq!(d.as_array().unwrap().len(), 3);
        assert_eq!(d[0]["designation"], "One of Three");
        assert_eq!(d[0]["session"], "aaaa1111-x");
        assert_eq!(d[0]["state"], "working");
        assert_eq!(d[0]["paused"], true);
        assert_eq!(d[1]["node"], "beefcafe".repeat(8));
        assert_eq!(d[1]["state"], "waiting");
        assert_eq!(d[2]["state"], "idle");
        assert_eq!(d[2]["files"], json!([]));
    }

    #[test]
    fn resolves_drones_by_designation_or_session() {
        let ds = fixture();
        assert_eq!(resolve(&ds, "Two").unwrap().unwrap().p.session, "bbbb2222-y");
        assert_eq!(resolve(&ds, "two of three").unwrap().unwrap().p.session, "bbbb2222-y");
        assert_eq!(resolve(&ds, "3").unwrap().unwrap().p.session, "cccc3333-x");
        assert_eq!(resolve(&ds, "aaaa").unwrap().unwrap().p.session, "aaaa1111-x");
        assert!(resolve(&ds, "all").unwrap().is_none());
        assert!(resolve(&ds, "Four").is_err());
        assert!(resolve(&ds, "zzzz").is_err());
        assert!(resolve(&ds, "aa").is_err(), "too short a prefix");
        assert_eq!(word(21), "21");
    }
}
