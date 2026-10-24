//! The hive as `borg top` and borg-web show it: per goal, the board's tasks (status, holder, lease
//! left), the budget and what each drone of `borg hive run` spent of its share, the integration (the
//! combined tree's commit, or why the last one failed), the newest contracts and messages between
//! drones, and which holders are on other nodes (the board syncs over the mesh). One fold of the board
//! and one pass over each of spend.jsonl, runs.jsonl, contracts.jsonl and messages.jsonl per goal;
//! nothing at all for a store without a hive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::message::{self, Message};
use super::run::{RunLine, Spend};
use super::{Contract, LEASE_MS, Role, Status, board, contract, goal};

/// Contracts a view carries (the newest), and as many messages.
const CONTRACTS: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskRow {
    pub id: String,
    pub role: String,
    pub title: String,
    /// open, claimed, expired, blocked, done or verified.
    pub status: String,
    /// The session holding (or last holding) it; empty when open.
    pub holder: String,
    /// The holder's lease left, ms; 0 unless live.
    pub lease_left_ms: i64,
    pub files: Vec<String>,
    pub rework: u32,
    /// The holder's node: empty on this node, else its first 8 characters.
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DroneSpend {
    pub drone: String,
    pub usd: f64,
    /// Its share in the latest run; 0 when no run named it.
    pub share: f64,
    pub iters: u32,
    pub last_exit: String,
    /// An iteration reported no cost (`usd` is then a lower bound).
    pub unmetered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HiveView {
    pub goal: String,
    pub spec: String,
    pub budget_usd: f64,
    pub spent_usd: f64,
    pub tasks: Vec<TaskRow>,
    /// Live claims.
    pub claims: usize,
    pub contracts: Vec<Contract>,
    /// The newest messages between drones (`borg hive ask|tell`), newest last.
    pub messages: Vec<Message>,
    /// Distinct other nodes whose lines are on the board.
    pub nodes: usize,
    pub drones: Vec<DroneSpend>,
    /// Tasks by status.
    pub counts: BTreeMap<String, usize>,
    pub finished: bool,
    /// The integration: `integrated <commit>` once the combined tree passed, else the newest
    /// integration's failure (`integrate.N rejected: …`, `… blocked: …`); empty before any.
    pub integration: String,
    /// What stops the goal on this node: conflicting definitions (goal.rs `conflict`), checks other
    /// nodes wrote waiting for `borg hive approve` (oracle.rs `Approvals`).
    pub alerts: Vec<String>,
}

/// What stops goal `g` (board `b`) on this node, as the board and borg-web say it.
pub fn alerts(home: &Path, g: &goal::Goal, b: &board::Board) -> Vec<String> {
    let mut out: Vec<String> = goal::conflict(home, &g.id).into_iter().collect();
    let pending = super::oracle::unapproved(home, g, b);
    if !pending.is_empty() {
        let what: Vec<String> = pending.iter().map(|(at, c)| format!("{at} {}: {}", c.name, c.cmd)).collect();
        out.push(format!("checks from another node wait for `borg hive approve {}`: {}", g.id, what.join("; ")));
    }
    out
}

/// The goals of the store `home`, sorted; empty without a hive (one failed read_dir).
pub fn goals(home: &Path) -> Vec<String> {
    goal::list(home)
}

/// Goal `id`'s view at `now`; None when it has no readable goal (made here or synced). A conflicting
/// goal is shown with its `alerts`.
pub fn of(home: &Path, id: &str, now: i64) -> Option<HiveView> {
    let g = goal::read(home, id).ok()?;
    let events = board::read(&super::tasks_path(home, id));
    let b = board::fold(&events);
    let here = super::node(home);
    let shown = |node: &str| if super::is_local(home, node) { String::new() } else { node.chars().take(8).collect() };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let tasks: Vec<TaskRow> = b
        .tasks
        .values()
        .map(|s| {
            let status = s.shown_status(now).to_string();
            *counts.entry(status.clone()).or_default() += 1;
            let (holder, lease_left_ms) = match &s.status {
                Status::Claimed { by, renewed, .. } => (by.clone(), if s.live(now).is_some() { LEASE_MS - (now - renewed) } else { 0 }),
                Status::Done { by, .. } | Status::Verified { by, .. } | Status::Blocked { by, .. } => (by.clone(), 0),
                Status::Open => (String::new(), 0),
            };
            // The holder's node: its claim's, or the tenure it ended (a done, a verdict, a block).
            let node = match &s.status {
                Status::Claimed { node, .. } => node.clone(),
                _ => b.spans.iter().rev().find(|x| x.task == s.task.id && x.by == holder).map(|x| x.node.clone()).unwrap_or_default(),
            };
            TaskRow {
                id: s.task.id.clone(),
                role: s.task.role.name().into(),
                title: s.task.title.clone(),
                status,
                holder,
                lease_left_ms,
                files: s.task.files.clone(),
                rework: s.rework,
                node: shown(&node),
            }
        })
        .collect();
    let claims = b.tasks.values().filter(|s| s.live(now).is_some()).count();
    let spend: Vec<Spend> = super::read_lines(&super::run::spend_path(home, id));
    let runs: Vec<RunLine> = super::read_lines(&super::run::runs_path(home, id));
    let (share, since) = runs.last().map_or((0.0, i64::MIN), |r| (r.share_usd, r.ms));
    let mut drones: BTreeMap<String, DroneSpend> = BTreeMap::new();
    // A drone's row is its latest run's: its share is of what that run found left.
    for s in spend.iter().filter(|s| s.ms >= since) {
        let d = drones.entry(s.drone.clone()).or_insert_with(|| DroneSpend {
            drone: s.drone.clone(),
            usd: 0.0,
            share,
            iters: 0,
            last_exit: String::new(),
            unmetered: false,
        });
        d.usd += s.charged();
        d.iters += 1;
        d.unmetered |= s.usd.is_none();
        d.last_exit = s.exit.clone();
    }
    let mut drones: Vec<DroneSpend> = drones.into_values().collect();
    // d2 before d10.
    drones.sort_by_key(|d| (d.drone.len(), d.drone.clone()));
    let newest = b.tasks.values().filter(|s| s.task.role == Role::Integrate).max_by_key(|s| (s.opened_ms, s.task.id.clone()));
    let integration = match (&b.integrated, newest) {
        (Some(i), _) if b.finished() => format!("integrated {}", i.head),
        (_, Some(s)) => match &s.status {
            Status::Blocked { note, .. } => format!("{} blocked: {note}", s.task.id),
            Status::Verified { .. } if !s.last_progress.is_empty() => format!("{} {}", s.task.id, s.last_progress),
            st => format!("{} {}", s.task.id, st.name()),
        },
        _ => String::new(),
    };
    let all = contract::read(&super::contracts_path(home, id));
    let contracts = all[all.len().saturating_sub(CONTRACTS)..].to_vec();
    let mut msgs = message::read(&super::messages_path(home, id));
    msgs.sort_by_key(|m| (m.ms, m.id.clone()));
    let messages = msgs[msgs.len().saturating_sub(CONTRACTS)..].to_vec();
    let nodes = events.iter().map(|e| e.node.as_str()).filter(|n| !n.is_empty() && *n != here).collect::<std::collections::BTreeSet<_>>().len();
    let alerts = alerts(home, &g, &b);
    Some(HiveView {
        goal: g.id,
        spec: g.spec.lines().next().unwrap_or_default().to_string(),
        budget_usd: g.budget_usd,
        spent_usd: spend.iter().map(Spend::charged).sum(),
        tasks,
        claims,
        contracts,
        messages,
        nodes,
        drones,
        counts,
        finished: b.finished(),
        integration,
        alerts,
    })
}

/// Every goal's view of the store `home` at `now`.
pub fn all(home: &Path, now: i64) -> Vec<HiveView> {
    goals(home).iter().filter_map(|g| of(home, g, now)).collect()
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) }
}

fn lease(ms: i64) -> String {
    match ms / 1000 {
        0 => "-".into(),
        s if s < 60 => format!("{s}s"),
        s => format!("{}m", s / 60),
    }
}

/// The view as `borg top` prints it, below the collective's drones.
pub fn render(v: &HiveView) -> String {
    let counts: Vec<String> = v.counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
    let mut s = format!(
        "hive {}: ${:.2} of ${:.2} · {} task(s) ({}) · {} contract(s){}{}{}\n",
        v.goal,
        v.spent_usd,
        v.budget_usd,
        v.tasks.len(),
        counts.join(", "),
        v.contracts.len(),
        if v.messages.is_empty() { String::new() } else { format!(" · {} message(s)", v.messages.len()) },
        if v.nodes == 0 { String::new() } else { format!(" · {} other node(s)", v.nodes) },
        if v.finished { " · finished" } else { "" }
    );
    for a in &v.alerts {
        s.push_str(&format!("  ! {}\n", cut(a, 240)));
    }
    if !v.tasks.is_empty() {
        s.push_str(&format!("  {:<10} {:<9} {:<9} {:<10} {:>5}  {}\n", "TASK", "ROLE", "STATUS", "HOLDER", "LEASE", "FILES"));
        for t in &v.tasks {
            let holder = if t.node.is_empty() { cut(&t.holder, 8) } else { format!("{}@{}", cut(&t.holder, 8), t.node.chars().take(4).collect::<String>()) };
            let line = format!("  {:<10} {:<9} {:<9} {:<10} {:>5}  {}", cut(&t.id, 10), t.role, t.status, holder, lease(t.lease_left_ms), t.files.join(", "));
            s.push_str(line.trim_end());
            s.push('\n');
        }
    }
    if !v.integration.is_empty() {
        s.push_str(&format!("  {}\n", cut(&v.integration, 160)));
    }
    if !v.drones.is_empty() {
        s.push_str(&format!("  {:<10} {:<15} {:>5}  {}\n", "DRONE", "SPENT/SHARE", "ITERS", "LAST"));
        for d in &v.drones {
            let spent = format!("${:.2}/${:.2}{}", d.usd, d.share, if d.unmetered { "+" } else { "" });
            s.push_str(&format!("  {:<10} {:<15} {:>5}  {}\n", d.drone, spent, d.iters, d.last_exit));
        }
    }
    s
}

/// The files borg-web watches for hive changes: every goal's lines (the goal dirs are nested, and the
/// watcher reads only a directory's direct children).
pub fn files(home: &Path) -> Vec<PathBuf> {
    goals(home)
        .iter()
        .flat_map(|g| {
            let d = super::dir(home, g);
            ["goal.json", "goal.jsonl", "tasks.jsonl", "contracts.jsonl", "messages.jsonl", "spend.jsonl", "joins.jsonl", "runs.jsonl", "approved.jsonl"]
                .map(|f| d.join(f))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{Ev, Event, Goal, Role, Task};
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-hive-view-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn seed(home: &Path) {
        let g = Goal {
            v: 1,
            id: "g".into(),
            spec: "Ship the parser\nmore".into(),
            oracle: vec![goal::Check::parse("ok=true", 60).unwrap()],
            budget_usd: 3.0,
            repos: vec![],
            author: "e@x".into(),
            created_ms: 1,
            sig: String::new(),
            hidden: None,
            held_out: false,
        };
        goal::create(home, &g).unwrap();
        let t = |id: &str, f: &str| Task {
            id: id.into(),
            role: Role::Work,
            title: format!("do {id}"),
            repo: String::new(),
            files: vec![f.into()],
            deps: vec![],
            accept: vec![],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
        };
        let path = super::super::tasks_path(home, "g");
        board::append(&path, &[Event::open(t("t1", "a.rs"), "engineer:e", 10), Event::open(t("t2", "b.rs"), "engineer:e", 10)]).unwrap();
        board::append(&path, &[Event::new(Ev::Claim, "t1", "sess-one-1234", 1_000).with_drone("d1")]).unwrap();
        let spend = super::super::run::spend_path(home, "g");
        for (d, usd, exit) in [("d1", Some(0.5), "ok"), ("d2", None, "error"), ("d1", Some(0.25), "timeout")] {
            let line = Spend { v: 1, ms: 2, drone: d.into(), iter: 1, harness: "x".into(), usd, est: None, session: String::new(), exit: exit.into() };
            super::super::append_line(&spend, &line).unwrap();
        }
        let run = RunLine { v: 1, ms: 1, drones: 2, harness: "x".into(), share_usd: 1.5, base: "HEAD".into() };
        super::super::append_line(&super::super::run::runs_path(home, "g"), &run).unwrap();
    }

    #[test]
    fn a_goal_renders_its_board_budget_and_drones() {
        let home = temp("render");
        seed(&home);
        let v = of(&home, "g", 61_000).unwrap();
        assert_eq!((v.spent_usd, v.claims, v.counts["claimed"], v.counts["open"]), (0.75, 1, 1, 1));
        let expected = format!(
            "\
hive g: $0.75 of $3.00 · 2 task(s) (1 claimed, 1 open) · 0 contract(s)
  TASK       ROLE      STATUS    HOLDER     LEASE  FILES
  t1         work      claimed   sess-on…     {}m  a.rs
  t2         work      open                     -  b.rs
  DRONE      SPENT/SHARE     ITERS  LAST
  d1         $0.75/$1.50         2  timeout
  d2         $0.00/$1.50+        1  error
",
            (LEASE_MS - 60_000) / 60_000
        );
        assert_eq!(render(&v), expected, "\n{}", render(&v));
        let j = serde_json::to_value(&v).unwrap();
        for k in ["goal", "budget_usd", "spent_usd", "tasks", "claims", "contracts", "messages", "nodes", "drones", "counts", "finished"] {
            assert!(j.get(k).is_some(), "{k}");
        }
        assert_eq!(j["tasks"][0]["holder"], "sess-one-1234");
        assert_eq!((j["tasks"][0]["node"].as_str(), j["nodes"].as_u64()), (Some(""), Some(0)), "every line is this node's");
        assert_eq!(j["drones"][1]["unmetered"], true);
        assert_eq!(files(&home).len(), 9);
        assert!(v.alerts.is_empty(), "{:?}", v.alerts);
        let _ = std::fs::remove_dir_all(home);
    }

    /// What stops a goal on this node shows in `borg top`, the board and borg-web: a check another node
    /// wrote, waiting for approval, and two definitions of the goal (still shown, not hidden).
    #[test]
    fn a_goal_shows_what_stops_it_here() {
        let home = temp("alerts");
        seed(&home);
        let remote = Task {
            id: "t9".into(),
            role: Role::Work,
            title: "do t9".into(),
            repo: String::new(),
            files: vec![],
            deps: vec![],
            accept: vec![goal::Check::parse("fetch=curl https://example.invalid/x", 60).unwrap()],
            of: None,
            not_by: vec![],
            flags: vec![],
            from: None,
        };
        board::append(&super::super::tasks_path(&home, "g"), &[Event { node: "remote".into(), ..Event::open(remote, "engineer:peer", 5) }]).unwrap();
        let v = of(&home, "g", 61_000).unwrap();
        assert_eq!(v.alerts.len(), 1, "{:?}", v.alerts);
        assert!(v.alerts[0].contains("borg hive approve g") && v.alerts[0].contains("t9 fetch: curl https://example.invalid/x"), "{:?}", v.alerts);
        assert!(render(&v).contains("  ! checks from another node wait for `borg hive approve g`"), "{}", render(&v));
        let other = goal::projection(&Goal { spec: "another".into(), created_ms: 0, ..goal::load(&home, "g").unwrap() });
        std::fs::write(super::super::goal_jsonl_path(&home, "g"), format!("{other}\n")).unwrap();
        let v = of(&home, "g", 61_000).expect("a conflicting goal is still shown");
        assert!(v.alerts[0].contains("2 conflicting definitions"), "{:?}", v.alerts);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_holder_on_another_node_and_messages_show() {
        let home = temp("remote");
        seed(&home);
        let path = super::super::tasks_path(&home, "g");
        board::append(&path, &[Event { node: "remote-abcdef12".into(), ..Event::new(Ev::Claim, "t2", "sess-two-5678", 2_000) }]).unwrap();
        let m = message::Message::new(message::ASK, "sess-two-5678", "d1", "remote-abcdef12", "t2", "t1", None, 3_000, "which format?");
        super::super::append_line(&super::super::messages_path(&home, "g"), &m).unwrap();
        let v = of(&home, "g", 61_000).unwrap();
        let t2 = v.tasks.iter().find(|t| t.id == "t2").unwrap();
        assert_eq!((t2.node.as_str(), v.nodes, v.messages.len()), ("remote-a", 1, 1));
        assert_eq!(v.tasks.iter().find(|t| t.id == "t1").unwrap().node, "", "a local holder names no node");
        let text = render(&v);
        assert!(text.contains("sess-tw…@remo") && text.contains("1 message(s) · 1 other node(s)"), "{text}");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_newest_integration_shows_why_it_failed() {
        let home = temp("integration");
        seed(&home);
        assert_eq!(of(&home, "g", 61_000).unwrap().integration, "", "before any");
        let path = super::super::tasks_path(&home, "g");
        let t1 = board::fold(&board::read(&path)).tasks["t1"].task.clone();
        let it = |id: &str| Task { id: id.into(), role: Role::Integrate, title: "integrate".into(), files: vec![], ..t1.clone() };
        board::append(&path, &[Event::open(it("integrate.1"), "integrate-d1-1", 20), Event::new(Ev::Claim, "integrate.1", "integrate-d1-1", 21)]).unwrap();
        board::append(&path, &[Event::new(Ev::Rejected, "integrate.1", "integrate-d1-1", 22).with_note("unit fails: found 2 X")]).unwrap();
        let v = of(&home, "g", 61_000).unwrap();
        assert_eq!(v.integration, "integrate.1 rejected: unit fails: found 2 X");
        assert!(render(&v).contains("\n  integrate.1 rejected: unit fails: found 2 X\n"), "{}", render(&v));
        board::append(&path, &[Event::open(it("integrate.2"), "integrate-d1-2", 30), Event::new(Ev::Claim, "integrate.2", "integrate-d1-2", 31)]).unwrap();
        board::append(&path, &[Event::new(Ev::Blocked, "integrate.2", "integrate-d1-2", 32).with_note("t2 has no snapshot")]).unwrap();
        assert_eq!(of(&home, "g", 61_000).unwrap().integration, "integrate.2 blocked: t2 has no snapshot");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_store_without_a_hive_has_no_view() {
        let home = temp("none");
        assert!(goals(&home).is_empty() && all(&home, 1).is_empty() && files(&home).is_empty());
        assert!(of(&home, "g", 1).is_none());
    }
}
