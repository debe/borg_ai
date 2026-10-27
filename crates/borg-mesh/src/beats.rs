//! Goal heartbeats over the mesh (phase g; borg hive/board.rs `beats_path`): each node's newest beat of
//! each hive goal, which says the node is still in the goal (its liveness, phase f F4) and carries its
//! watermarks (F5).
//!
//! A beat is no reconciled line. The board's lines sync as a set union (lines.rs), so a beat every 3 s
//! would cost a line per beat on every node for good, and each line a signed origin record (origin.rs):
//! about 1,200 lines an hour for one idle node of one goal. Instead each goal keeps a beats store with
//! the newest beat of each node (and its newest complete one, borg board.rs `kept`), and every session
//! carries the stores in its ephemeral exchange, as presence is carried (live.rs): this node's own
//! beats signed with its member key at send time, the other members' relayed as they were signed. A
//! receiver keeps a beat only from an admitted member other than itself, with a valid signature over
//! the goal and the exact line, for a goal it holds, and only when it is newer than what it holds
//! (borg board.rs `merge_beats`); a beat's liveness counts from when this node first held it, on its
//! own clock. An idle hour costs one record a node, and the bytes of one beat a session.

use std::path::Path;

use borg::hive::board::{self, BeatRecord};
use serde::{Deserialize, Serialize};

use crate::identity::{self, Identity};
use crate::membership::Membership;

/// One goal's beat as it travels: the goal, the beat's board line, the member key that signed it and
/// the signature (`body`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beat {
    pub goal: String,
    pub line: String,
    pub key: String,
    pub sig: String,
}

/// The most beats one exchange takes in: a node per member per goal.
const MAX_BEATS: usize = 10_000;

fn body(goal: &str, line: &str) -> Vec<u8> {
    format!("borg-beat-v1\n{goal}\n{line}").into_bytes()
}

/// The goals of `home` (valid ids, each a directory under `hive/`) and the beats store of each.
fn stores(home: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut ids: Vec<String> = std::fs::read_dir(home.join("hive"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|id| borg::hive::goal::valid_id(id))
        .collect();
    ids.sort();
    ids.into_iter().map(|id| (id.clone(), borg::hive::tasks_path(home, &id))).collect()
}

/// The beats store of every goal of `home` (what the daemon watches for a new beat to carry).
pub fn files(home: &Path) -> Vec<std::path::PathBuf> {
    stores(home).into_iter().map(|(_, board)| board::beats_path(&board)).collect()
}

/// How long a held beat still travels, on this node's clock (`BeatRecord::seen`): a lease. A node whose
/// newest beat is older is suspected long since (borg hive/mod.rs `SUSPECT_MS`, well below a lease), and
/// a claim waits for no suspected node's mark, so an older beat says nothing a peer's fold acts on. A
/// store of goals finished long ago costs a session no bytes and the receiver no read of their boards.
const FRESH_MS: i64 = borg::hive::LEASE_MS;

/// What this node offers a peer at `now`: its own beats of each goal, signed now, and the other members'
/// it holds, each held within `FRESH_MS`.
pub fn outgoing(home: &Path, me: &Identity, now: i64) -> Vec<Beat> {
    let mine = me.public();
    let mut out = Vec::new();
    for (goal, board) in stores(home) {
        for r in board::beat_records(&board::beats_path(&board)).into_iter().filter(|r| now - r.seen <= FRESH_MS) {
            match r.key.is_empty() {
                true => {
                    let sig = me.sign(&body(&goal, &r.line));
                    out.push(Beat { goal: goal.clone(), line: r.line, key: mine.clone(), sig });
                }
                false => out.push(Beat { goal: goal.clone(), line: r.line, key: r.key, sig: r.sig }),
            }
        }
    }
    out
}

/// Keeps the beats of `got` that a member other than `me` signed, for goals this node holds (their
/// directory exists: the session's reconciliation brought every goal either side holds), each held from
/// `now`; a beat no newer than what the store holds is dropped (borg board.rs `merge_beats`), and so is
/// every held beat of a key the membership no longer admits. How many were new.
pub fn receive(home: &Path, m: &Membership, me: &str, got: Vec<Beat>, now: i64) -> Result<usize, String> {
    let mut by_goal: std::collections::BTreeMap<String, Vec<BeatRecord>> = std::collections::BTreeMap::new();
    for b in got.into_iter().take(MAX_BEATS) {
        let ok = b.key != me
            && borg::hive::goal::valid_id(&b.goal)
            && m.role_of(&b.key).is_some()
            && identity::verify(&b.key, &body(&b.goal, &b.line), &b.sig).is_ok();
        if ok {
            by_goal.entry(b.goal).or_default().push(BeatRecord { line: b.line, seen: now, key: b.key, sig: b.sig });
        }
    }
    let mut new = 0;
    for (goal, records) in by_goal {
        if !borg::hive::dir(home, &goal).is_dir() {
            continue;
        }
        let path = borg::hive::tasks_path(home, &goal);
        new += board::merge_beats(&path, records, |r| r.key.is_empty() || m.role_of(&r.key).is_some()).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use borg::hive::board::{BEAT, Ev, Event};

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-mesh-beats-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("hive/g1")).unwrap();
        d
    }

    fn beat(node: &str, ms: i64) -> String {
        serde_json::to_string(&Event { node: node.into(), seq: Some(0), ..Event::new(Ev::Heartbeat, "", "runner", ms).with_note(BEAT) }).unwrap()
    }

    fn own(home: &Path, line: String, seen: i64) {
        board::merge_beats(&borg::hive::tasks_path(home, "g1"), vec![BeatRecord { line, seen, key: String::new(), sig: String::new() }], |_| true).unwrap();
    }

    #[test]
    fn a_members_newest_beat_is_kept_signed_and_relayed_and_nothing_else() {
        let d = tmp("recv");
        let (a, b, c) = (d.join("a"), d.join("b"), d.join("c"));
        for h in [&a, &b, &c] {
            std::fs::create_dir_all(h.join("hive/g1")).unwrap();
        }
        let alice = Identity::load_or_create(&a).unwrap();
        let bob = Identity::load_or_create(&b).unwrap();
        let mut m = Membership::found("corp", &alice);
        m.invite(&alice, &bob.public(), "member").unwrap();
        // An idle hour of alice's beats: one record in her store, one beat sent.
        for k in 0..1200 {
            own(&a, beat("A", 3000 * k), 3000 * k);
        }
        let now = 3000 * 1199;
        let sent = outgoing(&a, &alice, now);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(outgoing(&a, &alice, now + FRESH_MS + 1).is_empty(), "a beat held longer than a lease travels no more");
        assert_eq!(sent[0].line, beat("A", 3000 * 1199));
        assert_eq!(receive(&b, &m, &bob.public(), sent.clone(), 7), Ok(1));
        assert_eq!(receive(&b, &m, &bob.public(), sent.clone(), 9), Ok(0), "held already");
        let held = board::beats(&board::beats_path(&borg::hive::tasks_path(&b, "g1")));
        assert_eq!((held.len(), held[0].ms, held[0].seen.0), (1, 3000 * 1199, Some(7)), "held from its first arrival, on bob's clock");
        // An older beat after a newer one: dropped.
        own(&a, beat("A", 5), 5);
        let older = Beat { line: beat("A", 5), sig: alice.sign(&body("g1", &beat("A", 5))), ..sent[0].clone() };
        assert_eq!(receive(&b, &m, &bob.public(), vec![older], 10), Ok(0));
        // Forged, a stranger's, another goal's signature, a goal bob does not hold: none kept.
        let forged = Beat { line: beat("A", 9_999_999), ..sent[0].clone() };
        let eve = Identity::load_or_create(&d.join("e")).unwrap();
        let stranger = Beat { goal: "g1".into(), line: beat("E", 9_999_999), key: eve.public(), sig: eve.sign(&body("g1", &beat("E", 9_999_999))) };
        let moved = Beat { goal: "g2".into(), ..sent[0].clone() };
        assert_eq!(receive(&b, &m, &bob.public(), vec![forged, stranger, moved], 11), Ok(0));
        assert!(!borg::hive::dir(&b, "g2").exists(), "no goal made from a beat");
        // Bob relays alice's beat to carol as it was signed; it still verifies there. Bob's own beat
        // goes signed by bob; alice's own beat is not taken back by alice.
        own(&b, beat("B", 50), 50);
        let relayed = outgoing(&b, &bob, 50);
        assert_eq!(relayed.iter().map(|r| r.key.as_str()).collect::<std::collections::BTreeSet<_>>(), [alice.public().as_str(), bob.public().as_str()].into());
        assert_eq!(receive(&c, &m, "carol", relayed.clone(), 12), Ok(2));
        assert_eq!(receive(&a, &m, &alice.public(), relayed, 12), Ok(1), "bob's only");
        // Bob revoked: his beats go from every store that holds them.
        m.revoke(&alice, &bob.public(), 0).unwrap();
        assert_eq!(receive(&c, &m, "carol", Vec::new(), 13), Ok(0));
        own(&c, beat("C", 1), 1);
        board::merge_beats(&borg::hive::tasks_path(&c, "g1"), Vec::new(), |r| r.key.is_empty() || m.role_of(&r.key).is_some()).unwrap();
        let left: Vec<String> = board::beats(&board::beats_path(&borg::hive::tasks_path(&c, "g1"))).into_iter().map(|e| e.node).collect();
        assert_eq!(left, ["A", "C"]);
        let _ = std::fs::remove_dir_all(d);
    }
}
