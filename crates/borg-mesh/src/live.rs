//! Presence over the mesh: which drones run on the other members' machines, for `borg top`.
//!
//! Ephemeral and small: each round a node offers the presence records of its own sessions (signed
//! with its member key at send time, the prompt line redacted) and the remote records it holds
//! (signed by their own node, relayed as they are). A receiver keeps the latest record per member
//! and session in `<store>/mesh/presence.jsonl`, rewritten whole; records older than a day are
//! neither sent nor kept. Presence never enters lessons or any reconciled log, and only travels in
//! a session the gate let through: a local collective's presence never leaves the machine.

use std::collections::BTreeMap;
use std::path::Path;

use borg::presence::{self, PRUNE_MS, Presence, Remote};

use crate::identity::{self, Identity};
use crate::membership::Membership;

/// The most records one round takes from a peer. Guard: a peer sending an endless list cannot make
/// this node verify and hold more than this many records per round.
const MAX_RECEIVED: usize = 10_000;

fn body(p: &Presence) -> Vec<u8> {
    let mut b = b"borg-presence-v1\n".to_vec();
    b.extend(serde_json::to_vec(p).unwrap_or_default());
    b
}

/// This node's sessions, as they may leave the machine: the prompt line redacted, the pid dropped.
pub fn own(home: &Path, me: &Identity, now: i64) -> Vec<Remote> {
    presence::local(home, now)
        .into_iter()
        .map(|mut p| {
            p.prompt = borg::notes::redact(&p.prompt);
            p.pid = None;
            let sig = me.sign(&body(&p));
            Remote { p, key: me.public(), sig }
        })
        .collect()
}

/// What this node offers a peer: its own sessions and the other members' it holds.
pub fn outgoing(home: &Path, me: &Identity, now: i64) -> Vec<Remote> {
    let mut v = own(home, me, now);
    v.extend(presence::remote(home, now).into_iter().filter(|r| r.key != me.public()));
    v
}

/// Keeps the verified, admitted, fresh records of `got` that are newer than what this node holds.
/// Returns how many were new.
pub fn receive(home: &Path, m: &Membership, me: &str, got: Vec<Remote>, now: i64) -> Result<usize, String> {
    let mut held: BTreeMap<(String, String), Remote> = presence::remote(home, now).into_iter().map(|r| ((r.key.clone(), r.p.session.clone()), r)).collect();
    let mut new = 0;
    for r in got.into_iter().take(MAX_RECEIVED) {
        let fresh = now - r.p.ms <= PRUNE_MS && r.p.ms <= now + borg::msg::SKEW_MS;
        let ok = fresh && r.key != me && m.role_of(&r.key).is_some() && identity::verify(&r.key, &body(&r.p), &r.sig).is_ok();
        if !ok {
            continue;
        }
        let k = (r.key.clone(), r.p.session.clone());
        if held.get(&k).is_none_or(|h| h.p.ms < r.p.ms) {
            held.insert(k, r);
            new += 1;
        }
    }
    // Members that left take their drones with them.
    let before = held.len();
    held.retain(|(key, _), _| m.role_of(key).is_some());
    if new > 0 || held.len() != before {
        let p = presence::remote_path(home);
        std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("jsonl.tmp");
        let text: String = held.values().map(|r| format!("{}\n", serde_json::to_string(r).unwrap_or_default())).collect();
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-mesh-live-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn p(session: &str, ms: i64, prompt: &str) -> Presence {
        Presence {
            session: session.into(),
            harness: "claude".into(),
            engineer: "a@x".into(),
            repo: "r".into(),
            collective: "corp".into(),
            prompt: prompt.into(),
            files: vec![],
            event: "prompt".into(),
            ms,
            started: ms,
            pid: Some(42),
        }
    }

    #[test]
    fn signed_redacted_newest_kept_strangers_dropped() {
        let d = tmp("recv");
        let (a, b) = (d.join("a"), d.join("b"));
        let alice = Identity::load_or_create(&a).unwrap();
        let bob = Identity::load_or_create(&b).unwrap();
        let mut m = Membership::found("corp", &alice);
        m.invite(&alice, &bob.public(), "observe").unwrap();
        let now = borg::learn::now_ms();
        presence::write(&a, &p("s1", now - 10, "deploy with token ghp_abcdefghijklmnopqrstuvwxyz0123456789")).unwrap();
        let sent = outgoing(&a, &alice, now);
        assert_eq!(sent.len(), 1);
        assert!(!sent[0].p.prompt.contains("ghp_abc"), "redacted before it leaves: {}", sent[0].p.prompt);
        assert_eq!(sent[0].p.pid, None);

        assert_eq!(receive(&b, &m, &bob.public(), sent.clone(), now), Ok(1));
        assert_eq!(receive(&b, &m, &bob.public(), sent.clone(), now), Ok(0), "nothing newer");
        let held = presence::remote(&b, now);
        assert_eq!((held.len(), held[0].key.clone()), (1, alice.public()));

        let mut forged = sent[0].clone();
        forged.p.prompt = "something else".into();
        let eve = Identity::load_or_create(&d.join("e")).unwrap();
        let strangers = own(&a, &eve, now);
        assert_eq!(receive(&b, &m, &bob.public(), [vec![forged], strangers].concat(), now), Ok(0), "bad signature, unknown key");
        assert_eq!(receive(&b, &m, &bob.public(), outgoing(&a, &alice, now + PRUNE_MS + 100), now + PRUNE_MS + 100), Ok(0), "stale");
        // Bob relays alice's record to carol unchanged; it still verifies.
        let relayed = outgoing(&b, &bob, now);
        assert!(relayed.iter().any(|r| r.key == alice.public()));
        let c = d.join("c");
        assert_eq!(receive(&c, &m, "carol", relayed, now), Ok(1));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A member's clock may run ahead by the one clock skew (msg.rs `SKEW_MS`), not further.
    #[test]
    fn a_record_ahead_by_the_clock_skew_counts_and_no_further() {
        let d = tmp("skew");
        let (a, b, c) = (d.join("a"), d.join("b"), d.join("c"));
        let alice = Identity::load_or_create(&a).unwrap();
        let bob = Identity::load_or_create(&b).unwrap();
        let mut m = Membership::found("team", &alice);
        m.invite(&alice, &bob.public(), "member").unwrap();
        let now = 1_000_000;
        presence::write(&a, &p("s1", now + borg::msg::SKEW_MS, "ahead")).unwrap();
        let sent = outgoing(&a, &alice, now);
        assert_eq!(receive(&b, &m, &bob.public(), sent.clone(), now), Ok(1), "as far ahead as the skew");
        assert_eq!(receive(&c, &m, &bob.public(), sent, now - 1), Ok(0), "one ms further");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `MAX_RECEIVED` alone stops a round from looking past that many records: a valid record behind
    /// a flood of strangers is not taken.
    #[test]
    fn a_round_takes_at_most_max_received_records() {
        let d = tmp("flood");
        let (a, b) = (d.join("a"), d.join("b"));
        let alice = Identity::load_or_create(&a).unwrap();
        let bob = Identity::load_or_create(&b).unwrap();
        let mut m = Membership::found("team", &alice);
        m.invite(&alice, &bob.public(), "member").unwrap();
        let now = borg::learn::now_ms();
        presence::write(&a, &p("s1", now - 10, "work")).unwrap();
        let sent = outgoing(&a, &alice, now);
        let mut stranger = sent[0].clone();
        stranger.key = "not-a-member".into();
        let flood = [vec![stranger; MAX_RECEIVED], sent.clone()].concat();
        assert_eq!(receive(&b, &m, &bob.public(), flood, now), Ok(0), "the valid record comes after the cap");
        assert_eq!(receive(&b, &m, &bob.public(), sent, now), Ok(1), "on its own it is taken");
        let _ = std::fs::remove_dir_all(&d);
    }
}
