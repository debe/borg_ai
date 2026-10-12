//! `borg mesh serve`: one node, running until it is told to stop.
//!
//! - **Accepts** members dialling in, each session in its own task (sync.rs gates every one).
//! - **Dials** every member listed in peers.json: every `interval`, and at once when the store
//!   changed locally (a hook wrote a lesson or an excerpt, a session brought new lines, `revoke`
//!   re-signed the membership), checked every `poll`. A changed membership is applied to the logs
//!   (quarantine) at once, before the round. New lines therefore travel on: what B learns
//!   from A reaches C in B's next round.
//! - **Backs off** per peer on failure: `interval · 2^failures`, at most `backoff_max`; a success
//!   resets it.
//! - **Re-checks the gate every round** on the config as it is then: once the collective turns
//!   local or off (or the repo moves to another store) no round dials anyone, and incoming
//!   sessions are refused by sync.rs. Turning it back on resumes without a restart.
//! - **Stops cleanly** when `shutdown` resolves: no new sessions, running ones get `grace`, then
//!   the endpoint closes (peers see a closed connection, not a timeout).
//!
//! Not a coordinator: every node runs the same loop, and the hooks never depend on it.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
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

/// A cheap fingerprint of what a round would carry: size and mtime of the synced files.
fn local_state(home: &Path) -> Vec<(u64, Option<SystemTime>)> {
    let mut files = crate::lines::logs(home);
    files.push(membership::path(home));
    files.push(crate::steer::outbox(home)); // a new directive travels at once (presence waits for the interval)
    files.iter().map(|p| std::fs::metadata(p).map(|m| (m.len(), m.modified().ok())).unwrap_or((0, None))).collect()
}

pub async fn serve(node: Arc<Node>, opts: Options, shutdown: impl Future<Output = ()>, on: impl Fn(Event)) {
    let mut sessions: JoinSet<Result<Report, String>> = JoinSet::new();
    let mut rounds: JoinSet<Vec<(String, Result<Report, String>)>> = JoinSet::new();
    let mut backoff: HashMap<String, Backoff> = HashMap::new();
    let mut seen = None;
    let mut last_round: Option<Instant> = None;
    let mut dirty = true;
    let mut gate_open: Option<bool> = None;
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
            Some(done) = rounds.join_next() => {
                for (peer, result) in done.unwrap_or_default() {
                    let b = backoff.entry(peer.clone()).or_default();
                    match &result {
                        Ok(r) => {
                            *b = Backoff::default();
                            dirty |= r.received > 0 || r.quarantined > 0 || r.steered > 0;
                        }
                        Err(_) => {
                            b.failures = b.failures.saturating_add(1);
                            let wait = opts.interval.saturating_mul(1u32 << b.failures.min(16)).min(opts.backoff_max);
                            b.next = Some(Instant::now() + wait);
                        }
                    }
                    on(Event::Synced { peer, result });
                }
            }
            _ = tick.tick() => {
                let gate = node.gate().map(|_| ());
                if gate_open != Some(gate.is_ok()) {
                    gate_open = Some(gate.is_ok());
                    on(Event::Gate(gate.clone()));
                }
                if gate.is_err() || !rounds.is_empty() {
                    continue;
                }
                let now_state = local_state(node.home());
                let changed = seen.as_ref() != Some(&now_state);
                if changed {
                    // A `join` or `revoke` run beside the daemon changed the membership: quarantine
                    // what it no longer admits before anything is offered (no-op when already done).
                    let _ = node.enforce();
                    let _ = node.stamp(); // a hook's new lines are stamped as this node's when first seen
                }
                let due = dirty || changed || last_round.is_none_or(|t: Instant| t.elapsed() >= opts.interval);
                if !due {
                    continue;
                }
                let targets = targets(&node, &backoff);
                (dirty, seen, last_round) = (false, Some(now_state), Some(Instant::now()));
                if targets.is_empty() {
                    continue;
                }
                let (n, timeout) = (node.clone(), opts.timeout);
                rounds.spawn(async move {
                    let mut out = Vec::new();
                    for t in targets {
                        let result = match tokio::time::timeout(timeout, n.sync(&t.to_string())).await {
                            Ok(r) => r,
                            Err(_) => Err(format!("no answer within {timeout:?}")),
                        };
                        out.push((t.key, result));
                    }
                    out
                });
            }
        }
    }
    // Clean shutdown: no new sessions; running ones get the grace period, then the endpoint closes.
    let _ = tokio::time::timeout(opts.grace, async {
        while sessions.join_next().await.is_some() {}
        while rounds.join_next().await.is_some() {}
    })
    .await;
    sessions.abort_all();
    rounds.abort_all();
    node.close().await;
}

/// The members to dial this round: listed in peers.json, still members, not this node, not backing off.
fn targets(node: &Node, backoff: &HashMap<String, Backoff>) -> Vec<Target> {
    let Ok(Some(m)) = Membership::load(node.home()) else { return Vec::new() };
    let me = node.public();
    let now = Instant::now();
    peers::targets(node.home())
        .into_iter()
        .filter(|t| t.key != me && m.role_of(&t.key).is_some())
        .filter(|t| backoff.get(&t.key).and_then(|b| b.next).is_none_or(|at| at <= now))
        .collect()
}
