//! `borg mesh serve`: one node, running until it is told to stop.
//!
//! - **Accepts** members dialling in, each session in its own task (sync.rs gates every one).
//! - **Dials** every member listed in peers.json: every `interval`, and at once when the store
//!   changed locally (a hook wrote a lesson or an excerpt, a session brought new lines, `revoke`
//!   re-signed the membership), checked every `poll`. Each peer is dialled on its own, side by side:
//!   one that does not answer holds up no other for its `timeout`. A change made while a peer's dial
//!   runs, or that a failed dial did not carry, is owed to it and dialled once that dial ends or its
//!   backoff is over. A changed membership is applied to the logs (quarantine) at once, before the
//!   round. New lines therefore travel on: what B learns from A reaches C in B's next round, at the
//!   next `poll` once the session ends.
//! - **Backs off** per peer on failure: `poll` after the first, then `interval · 2^(failures - 1)`,
//!   at most `backoff_max`; a success resets it. A failed dial also drops the peer from
//!   `mesh/synced.json` (borg presence.rs).
//! - **Re-checks the gate every round** on the config as it is then: once the collective turns
//!   local or off (or the repo moves to another store) no round dials anyone, and incoming
//!   sessions are refused by sync.rs. Turning it back on resumes without a restart.
//! - **Stops cleanly** when `shutdown` resolves: no new sessions, running ones get `grace`, then
//!   the endpoint closes (peers see a closed connection, not a timeout).
//!
//! Not a coordinator: every node runs the same loop, and the hooks never depend on it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::addr::Target;
use crate::membership::{self, Membership};
use crate::peers;
use crate::sync::{Node, Report};

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub interval: Duration,
    pub poll: Duration,
    pub backoff_max: Duration,
    /// Longest one dial may take before it counts as a failure.
    pub timeout: Duration,
    pub grace: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            interval: Duration::from_secs(60),
            poll: Duration::from_secs(2),
            backoff_max: Duration::from_secs(30 * 60),
            timeout: Duration::from_secs(30),
            grace: Duration::from_secs(5),
        }
    }
}

/// What the loop tells its caller (the CLI prints it, tests count it).
#[derive(Debug, Clone)]
pub enum Event {
    Served(Result<Report, String>),
    Synced {
        peer: String,
        result: Result<Report, String>,
    },
    /// The gate closed (or opened again): no round runs while closed.
    Gate(Result<(), String>),
}

#[derive(Default)]
struct Backoff {
    failures: u32,
    next: Option<Instant>,
}

pub type State = BTreeMap<PathBuf, (u64, Option<SystemTime>)>;

/// A cheap fingerprint of what a round would carry: size and mtime of the synced files.
fn local_state(home: &Path) -> State {
    let mut files = crate::lines::logs(home);
    files.push(membership::path(home));
    files.push(crate::steer::outbox(home)); // a new directive travels at once (presence waits for the interval)
    state(files)
}

/// One stat per file; a missing one reads (0, None).
fn state(files: Vec<PathBuf>) -> State {
    files
        .into_iter()
        .map(|p| {
            let at = std::fs::metadata(&p).map_or((0, None), |m| (m.len(), m.modified().ok()));
            (p, at)
        })
        .collect()
}

/// How long a peer waits after its `failures`th failure in a row: `poll` after the first (a session
/// that failed once, under load or on a network hiccup, is retried at once), then `interval ·
/// 2^(failures - 1)`, at most `backoff_max`.
fn backoff_wait(opts: &Options, failures: u32) -> Duration {
    match failures {
        0 | 1 => opts.poll,
        n => opts.interval.saturating_mul(1u32 << (n - 1).min(16)).min(opts.backoff_max),
    }
}

pub async fn serve(node: Arc<Node>, opts: Options, shutdown: impl Future<Output = ()>, on: impl Fn(Event)) {
    let mut sessions: JoinSet<Result<Report, String>> = JoinSet::new();
    // One dial per peer, side by side: a peer that does not answer holds up no other.
    let mut dials: JoinSet<Result<Report, String>> = JoinSet::new();
    let mut dialling: HashMap<tokio::task::Id, String> = HashMap::new();
    // The members a change has yet to reach: each dialled once not dialling already and not backing
    // off, so a change during a dial, or before a failure, reaches its peer after it.
    let mut owed: BTreeSet<String> = BTreeSet::new();
    let mut backoff: HashMap<String, Backoff> = HashMap::new();
    let mut seen: Option<State> = None;
    let mut last_round: Option<Instant> = None;
    let mut dirty = true;
    let mut gate_open: Option<bool> = None;
    // The gate and the store are looked at every `poll`.
    let mut tick = tokio::time::interval(opts.poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            incoming = node.incoming() => match incoming {
                Some(i) => {
                    let n = node.clone();
                    sessions.spawn(async move { n.handle(i).await });
                }
                None => break,
            },
            Some(done) = sessions.join_next() => {
                let result = done.unwrap_or_else(|e| Err(format!("session task: {e}")));
                if result.as_ref().is_ok_and(|r| r.received > 0 || r.quarantined > 0 || r.steered > 0 || r.joined > 0) {
                    dirty = true; // pass it on to the others
                }
                on(Event::Served(result));
            }
            Some(done) = dials.join_next_with_id() => {
                let (id, result) = match done {
                    Ok((id, r)) => (id, r),
                    Err(e) => (e.id(), Err(format!("dial task: {e}"))),
                };
                let Some(peer) = dialling.remove(&id) else { continue };
                let b = backoff.entry(peer.clone()).or_default();
                match &result {
                    Ok(r) => {
                        *b = Backoff::default();
                        dirty |= r.received > 0 || r.quarantined > 0 || r.steered > 0;
                    }
                    Err(_) => {
                        b.failures = b.failures.saturating_add(1);
                        b.next = Some(Instant::now() + backoff_wait(&opts, b.failures));
                        owed.insert(peer.clone()); // what it did not get, once the wait is over
                        node.unreachable(&peer);
                    }
                }
                on(Event::Synced { peer, result });
            }
            _ = tick.tick() => {
                let gate = node.gate().map(|_| ());
                if gate_open != Some(gate.is_ok()) {
                    gate_open = Some(gate.is_ok());
                    on(Event::Gate(gate.clone()));
                }
                // A closing gate also refuses every session it would start (sync.rs).
                if gate_open != Some(true) {
                    continue;
                }
                let mut changed = false;
                let mut now_state = local_state(node.home());
                if seen.as_ref() != Some(&now_state) {
                    // A `join` or `revoke` run beside the daemon changed the membership: quarantine
                    // what it no longer admits before anything is offered (no-op when already done).
                    let _ = node.enforce();
                    let _ = node.stamp(); // a hook's new lines are stamped as this node's when first seen
                    // What those two wrote (fresh outcomes, a quarantine) goes in this round, not the next.
                    now_state = local_state(node.home());
                    changed = true;
                }
                seen = Some(now_state);
                let round = dirty || changed || last_round.is_none_or(|t: Instant| t.elapsed() >= opts.interval);
                let now = Instant::now();
                let ready = |k: &String| !dialling.values().any(|d| d == k) && backoff.get(k).and_then(|b| b.next).is_none_or(|at| at <= now);
                // Peers.json and the membership are read only when a dial may start: not every tick a
                // peer that backs off is owed a change.
                if !round && !owed.iter().any(ready) {
                    continue;
                }
                let all = targets(&node);
                if round {
                    (dirty, last_round) = (false, Some(now));
                    owed.extend(all.iter().map(|t| t.key.clone()));
                }
                // A member no longer listed or admitted is owed nothing.
                owed.retain(|k| all.iter().any(|t| t.key == *k));
                let due: Vec<Target> = all.into_iter().filter(|t| owed.contains(&t.key) && ready(&t.key)).collect();
                for t in due {
                    owed.remove(&t.key);
                    let (n, timeout, key) = (node.clone(), opts.timeout, t.key.clone());
                    let handle = dials.spawn(async move {
                        match tokio::time::timeout(timeout, n.sync(&t.to_string())).await {
                            Ok(r) => r,
                            Err(_) => Err(format!("no answer within {timeout:?}")),
                        }
                    });
                    dialling.insert(handle.id(), key);
                }
            }
        }
    }
    // Clean shutdown: no new sessions; running ones get the grace period, then the endpoint closes.
    let _ = tokio::time::timeout(opts.grace, async {
        while sessions.join_next().await.is_some() {}
        while dials.join_next().await.is_some() {}
    })
    .await;
    sessions.abort_all();
    dials.abort_all();
    node.close().await;
}

/// The members a round dials: listed in peers.json, still members, not this node.
fn targets(node: &Node) -> Vec<Target> {
    let Ok(Some(m)) = Membership::load(node.home()) else { return Vec::new() };
    let me = node.public();
    peers::targets(node.home()).into_iter().filter(|t| t.key != me && m.role_of(&t.key).is_some()).collect()
}
