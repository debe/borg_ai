//! `borg top [--dir DIR] [--once] [--json]`: the drones of each collective, one screen.
//!
//! Local drones come from presence records (presence.rs), remote ones from what the mesh daemon
//! last heard from members (only for a collective that may sync). Drones are designated by start
//! order across both, Borg style: "Two of Five". `--dir` shows only the collective DIR routes to;
//! otherwise every collective on this machine that has drones. On a terminal it is a live screen until
//! `q` (top_live.rs); piped, or with `--once`, it prints the text once; `--json` prints the same data
//! for scripts.
//!
//! Messages (msg.rs): each local drone's unread count (what its next prompt or tool call shows), and
//! the collective's last few messages, one line each. A remote drone's reads are claimed on its node.
//! A sender is named by what its line claims, with the node the mesh checked it against (`sender`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::collective::{self, Route, Share};
use crate::msg::{self, Address, Message};
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

/// The messages side of a collective's screen.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Inbox {
    /// Per local drone's session, the messages not shown to it yet.
    pub unread: BTreeMap<String, usize>,
    /// The newest `RECENT` messages, newest first.
    pub recent: Vec<Message>,
    /// The messages sent in the last day.
    pub day: usize,
    /// This node as messages name it (msg.rs `node`).
    #[serde(skip)]
    pub here: String,
}

/// How many messages the screen lists.
pub const RECENT: usize = 5;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

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

/// The messages of the store at `home` for its `drones` as of `now` (one read of the log, one
/// directory listing per local drone that has a message).
pub fn inbox(home: &Path, drones: &[Drone], now: i64) -> Inbox {
    if std::fs::metadata(msg::path(home)).is_err() {
        return Inbox::default();
    }
    let all: Vec<Message> = msg::read(home).into_iter().filter(|m| now - m.ms <= msg::TTL_MS).collect();
    let here = msg::node(home);
    let unread = unread_of(home, drones, &here, &all, now);
    let day = all.iter().filter(|m| now - m.ms <= DAY_MS).count();
    let mut recent = all;
    recent.sort_by(|a, b| (b.ms, &b.id).cmp(&(a.ms, &a.id)));
    recent.truncate(RECENT);
    Inbox { unread, recent, day, here }
}

/// Per live local drone's session, how many of the already-read `all` messages were not shown to it
/// (`here`: this node, msg.rs `node`).
pub fn unread_of(home: &Path, drones: &[Drone], here: &str, all: &[Message], now: i64) -> BTreeMap<String, usize> {
    drones
        .iter()
        .filter(|d| d.node.is_empty() && d.state != State::Ended)
        .map(|d| (d.p.session.clone(), msg::unread(home, all, here, &d.p.session, d.p.started, now)))
        .collect()
}

/// The drone of `drones` that is `session` on `node` (`here`: this node).
fn drone_at<'a>(drones: &'a [Drone], here: &str, node: &str, session: &str) -> Option<&'a Drone> {
    drones.iter().find(|d| d.p.session == session && if d.node.is_empty() { node == here } else { d.node == node })
}

/// Who sent `m`, as the screen says it: the drone its (node, `by`) pair is, else the name it was sent
/// under, else the start of `by`; the engineer by email. Every part but the node is the sender's claim,
/// so a line from another node than `here` carries that node's short key (as msg.rs `line`).
pub fn sender(drones: &[Drone], here: &str, m: &Message) -> String {
    let name = match Address::parse(&m.by) {
        Some(Address::Engineer { email }) if email.is_empty() => msg::ENGINEER.into(),
        Some(Address::Engineer { email }) => collective::aliases().display(&email),
        _ => match drone_at(drones, here, &m.node, &m.by) {
            Some(d) => d.designation.clone(),
            None if !m.name.is_empty() => m.name.clone(),
            None => m.by.chars().take(8).collect(),
        },
    };
    if m.node == here { name } else { format!("{name} @{}", m.node.get(..12).unwrap_or(&m.node)) }
}

/// Whom `to` names, as the screen says it: `all`, the engineer, the drone it is, else the start of the
/// session id.
pub fn recipient(drones: &[Drone], here: &str, to: &str) -> String {
    match Address::parse(to) {
        Some(Address::Engineer { email }) if !email.is_empty() => collective::aliases().display(&email),
        Some(Address::Session { id, node }) => {
            let d = match node {
                Some(n) => drone_at(drones, here, &n, &id),
                None => drones.iter().find(|d| d.p.session == id),
            };
            d.map_or_else(|| id.chars().take(8).collect(), |d| d.designation.clone())
        }
        Some(a) => a.to_string(),
        None => to.chars().take(8).collect(),
    }
}

/// One message as one screen line (without its age): `sender -> recipient: text` (msg.rs keeps the
/// text on one line, its whitespace collapsed).
pub fn message_line(drones: &[Drone], here: &str, m: &Message) -> String {
    let ask = if m.asks() { " asks" } else { "" };
    format!("{}{ask} -> {}: {}", sender(drones, here, m), recipient(drones, here, &m.to), m.text)
}

/// A drone's unread count as the screen shows it: `-` where this node cannot tell (a remote or ended one).
pub fn unread_cell(inbox: &Inbox, d: &Drone) -> String {
    inbox.unread.get(&d.p.session).map_or_else(|| "-".into(), usize::to_string)
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
pub fn render(name: &str, share: Share, drones: &[Drone], inbox: &Inbox, now: i64) -> String {
    let working = drones.iter().filter(|d| matches!(d.state, State::Working | State::Waiting)).count();
    let remote = drones.iter().filter(|d| !d.node.is_empty()).count();
    let share = match share {
        Share::Team => "team",
        Share::Local => "local, never leaves this machine",
    };
    let mut s = format!("{name} ({share}): {} drone(s), {working} active, {remote} remote\n", drones.len());
    if !drones.is_empty() {
        s.push_str(&drone_table(drones, inbox, now));
    }
    if !inbox.recent.is_empty() {
        s.push_str(&format!("  messages: {} in the last day, {} unread\n", inbox.day, inbox.unread.values().sum::<usize>()));
        for m in &inbox.recent {
            s.push_str(&format!("  {:>6}  {}\n", ago(now - m.ms), cut(&message_line(drones, &inbox.here, m), 120)));
        }
    }
    s
}

fn drone_table(drones: &[Drone], inbox: &Inbox, now: i64) -> String {
    let mut s = format!(
        "  {:<18} {:<8} {:<24} {:<14} {:<8} {:>6} {:>6}  {:<40} {}\n",
        "DRONE", "SESSION", "ENGINEER", "REPO", "STATE", "IDLE", "UNREAD", "DOING", "FILES"
    );
    for d in drones {
        let state = if d.paused { "paused" } else { d.state.name() };
        let engineer = if d.node.is_empty() { d.p.engineer.clone() } else { format!("{} @{}", d.p.engineer, d.node.get(..6).unwrap_or(&d.node)) };
        let line = format!(
            "  {:<18} {:<8} {:<24} {:<14} {:<8} {:>6} {:>6}  {:<40} {}",
            d.designation,
            cut(&d.p.session, 8),
            cut(if engineer.is_empty() { "?" } else { &engineer }, 24),
            cut(&d.p.repo, 14),
            state,
            ago(now - d.p.ms),
            unread_cell(inbox, d),
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

/// `to_json` with the messages side: each local drone's `unread`, and the collective's `messages`.
pub fn to_json_with(name: &str, share: Share, drones: &[Drone], inbox: &Inbox) -> Value {
    let mut v = to_json(name, share, drones);
    if let Some(ds) = v["drones"].as_array_mut() {
        for (o, d) in ds.iter_mut().zip(drones) {
            if let Some(n) = inbox.unread.get(&d.p.session) {
                o["unread"] = json!(n);
            }
        }
    }
    v["messages"] = json!({"day": inbox.day, "recent": inbox.recent});
    v
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

/// Every collective to show with its drones (top_live.rs).
pub fn gather(dir: Option<&str>, now: i64) -> Result<Vec<crate::top_live::Coll>, String> {
    let mut out = Vec::new();
    for r in routes(dir)? {
        let Route::Collective { name, home, share } = &r else { continue };
        presence::prune(home, now);
        let mut ds = drones(home, collective::may_sync(&r), now);
        for d in &mut ds {
            d.p.engineer = collective::aliases().display(&d.p.engineer);
        }
        let inbox = inbox(home, &ds, now);
        if ds.is_empty() && inbox.recent.is_empty() && dir.is_none() {
            continue;
        }
        out.push(crate::top_live::Coll { name: name.clone(), share: *share, drones: ds, inbox });
    }
    Ok(out)
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
        let inbox = inbox(home, &ds, now);
        if json_out {
            all.push(to_json_with(name, *share, &ds, &inbox));
        } else if !ds.is_empty() || !inbox.recent.is_empty() || dir.is_some() {
            text.push_str(&render(name, *share, &ds, &inbox, now));
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
    let (mut dir, mut once, mut json_out) = (None, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--dir" if dir.is_none() => match it.next() {
                Some(d) => dir = Some(*d),
                None => return usage("--dir needs a value"),
            },
            "--once" => once = true,
            "--json" => json_out = true,
            a => return usage(&format!("unknown flag {a}")),
        }
    }
    // SAFETY: isatty only inspects the descriptor.
    if !json_out && !once && unsafe { libc::isatty(1) } == 1 {
        return crate::top_live::run(dir);
    }
    match screen(dir, json_out, learn::now_ms()) {
        Ok(s) => {
            print!("{s}");
            0
        }
        Err(e) => {
            eprintln!("borg top: {e}");
            1
        }
    }
}

fn usage(why: &str) -> i32 {
    eprintln!("borg top: {why}\nusage: borg top [--once] [--json] [--dir DIR]");
    2
}

#[cfg(test)]
pub(crate) fn tests_fixture() -> Vec<Drone> {
    tests::fixture()
}

#[cfg(test)]
pub(crate) fn tests_inbox() -> Inbox {
    tests::inbox_fixture()
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

    pub(crate) fn fixture() -> Vec<Drone> {
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

    fn message(by: &str, name: &str, to: &str, kind: &str, ms: i64, text: &str) -> Message {
        message_on("n1", by, name, to, kind, ms, text)
    }

    fn message_on(node: &str, by: &str, name: &str, to: &str, kind: &str, ms: i64, text: &str) -> Message {
        let from = msg::Sender { by: by.into(), name: name.into(), harness: "claude".into(), node: node.into() };
        Message::new(kind, &from, to, None, ms, text)
    }

    pub(crate) fn inbox_fixture() -> Inbox {
        Inbox {
            unread: [("aaaa1111-x".to_string(), 2), ("cccc3333-x".to_string(), 0)].into_iter().collect(),
            recent: vec![
                message("cccc3333-x", "Three of Three", "aaaa1111-x", msg::ASK, NOW - 60_000, "which   test\nflakes?"),
                message("engineer:alice@x", "alice@x", msg::ALL, msg::TELL, NOW - 120_000, "release at five"),
                message_on(&"feed".repeat(16), "gone-session", "Six of Six", "zzzz9999-q", msg::TELL, NOW - 180_000, "done"),
            ],
            day: 3,
            here: "n1".into(),
        }
    }

    #[test]
    fn renders_one_screen_per_collective() {
        let out = render("corp", Share::Team, &fixture(), &inbox_fixture(), NOW);
        let expected = "\
corp (team): 3 drone(s), 2 active, 1 remote
  DRONE              SESSION  ENGINEER                 REPO           STATE      IDLE UNREAD  DOING                                    FILES
  One of Three       aaaa111… alice@x                  borg-ai        paused       5s      2  fix the flaky sync test                  crates/borg-mesh/src/sync.rs, src/recon.rs
  Two of Three       bbbb222… bob@y @beefca            borg-ai        waiting      2m      -  review PR 42                             README.md
  Three of Three     cccc333… alice@x                  borg-ai        idle        31m      0  write the release notes for the mesh da…
  messages: 3 in the last day, 2 unread
      1m  Three of Three asks -> One of Three: which test flakes?
      2m  alice@x -> all: release at five
      3m  Six of Six @feedfeedfeed -> zzzz9999: done
";
        assert_eq!(out, expected, "\n{out}");
        let none = Inbox::default();
        assert_eq!(render("private", Share::Local, &[], &none, NOW), "private (local, never leaves this machine): 0 drone(s), 0 active, 0 remote\n");
    }

    /// The unread count is what the next prompt would show: a message shown once no longer counts,
    /// broadcasts only to drones running when sent, never its own; no log costs no read.
    #[test]
    fn unread_counts_follow_the_shown_once_claims() {
        let home = std::env::temp_dir().join(format!("borg-top-inbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let ds = fixture();
        assert!(inbox(&home, &ds, NOW).recent.is_empty());
        let here = msg::node(&home);
        msg::send(&home, message_on(&here, "cccc3333-x", "Three of Three", "aaaa1111-x", msg::TELL, NOW - 3_000, "look at recon"), NOW).unwrap();
        msg::send(&home, message_on(&here, "cccc3333-x", "Three of Three", msg::ALL, msg::TELL, NOW - 2_000, "lunch"), NOW).unwrap();
        let i = inbox(&home, &ds, NOW);
        assert_eq!(i.unread.get("aaaa1111-x"), Some(&2), "{i:?}");
        assert_eq!(i.unread.get("cccc3333-x"), Some(&0), "its own broadcast");
        assert!(!i.unread.contains_key(&ds[1].p.session), "remote: claimed on its node");
        assert_eq!((i.day, i.recent[0].text.as_str()), (2, "lunch"));
        let caps = crate::harness::by_name("claude").expect("a harness").caps();
        let ev = json!({"session_id": "aaaa1111-x"});
        assert!(msg::deliver(&home, caps, "prompt", &ev, Some(100), NOW, None).is_some());
        assert_eq!(inbox(&home, &ds, NOW).unread.get("aaaa1111-x"), Some(&0));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A line's `by` and `name` are its sender's claims: one from another node that names a local drone's
    /// session is not shown as that drone, and carries its node's key.
    #[test]
    fn a_sender_is_tied_to_the_node_its_line_names() {
        let ds = fixture();
        let own = message("aaaa1111-x", "anything", msg::ALL, msg::TELL, NOW, "hi");
        assert_eq!(sender(&ds, "n1", &own), "One of Three", "this node's drone");
        let forged = message_on("evil0123456789", "aaaa1111-x", "One of Three", msg::ALL, msg::TELL, NOW, "hi");
        assert_eq!(sender(&ds, "n1", &forged), "One of Three @evil01234567", "the claim, with the node it came from");
        let remote = message_on(&"beefcafe".repeat(8), &ds[1].p.session, "x", msg::ALL, msg::TELL, NOW, "hi");
        assert_eq!(sender(&ds, "n1", &remote), "Two of Three @beefcafebeef", "the remote drone on its own node");
        assert_eq!(recipient(&ds, "n1", "aaaa1111-x@n1"), "One of Three");
        assert_eq!(recipient(&ds, "n1", "aaaa1111-x@evil"), "aaaa1111", "not the local drone of that id");
        assert_eq!((recipient(&ds, "n1", "engineer"), recipient(&ds, "n1", msg::ALL)), ("engineer".into(), "all".into()));
    }

    #[test]
    fn json_for_scripts() {
        let v = to_json_with("corp", Share::Team, &fixture(), &inbox_fixture());
        assert_eq!((v["drones"][0]["unread"].as_u64(), v["drones"][1].get("unread")), (Some(2), None));
        assert_eq!((v["messages"]["day"].as_u64(), v["messages"]["recent"][0]["kind"].as_str()), (Some(3), Some("ask")));
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
