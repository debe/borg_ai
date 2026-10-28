//! Steering over the mesh: which pause, resume and veto messages of another node count here (the
//! hook side, the admissions file and delivery are borg steer.rs).
//!
//! - **Authenticated by origin.** A privileged message (borg msg.rs `Kind::privileged`) is a line of
//!   `messages.jsonl` like any other, and travels only under a signed origin of the key its `node`
//!   names (lines.rs `travels`). That origin is the sender's node, verified; no second signature.
//! - **Admit** (`admit`), when the mesh stores such a line from another node: in force here only
//!   under the role the held membership gives that node, `admin` or `steer` (borg steer.rs
//!   `Role::of_member`). Any other role, an unknown or a revoked key is refused (`Refusal`) and
//!   logged to `<store>/messages/dropped.jsonl`. The line stays in the log, as the union merge
//!   keeps it, but without an admission it is neither shown nor in force anywhere.
//! - **Own lines.** This node's lines are admitted by borg when sent (`Role::Local`: the engineer at
//!   this machine steers its own sessions, whatever its member role). A `share: local` collective
//!   never syncs, so only this machine's key ever steers it.
//! - **Enforce** (`enforce`), every round and after the membership changed: an admission whose node
//!   lost the right to steer goes, so its pause no longer holds here, and so does an expired one
//!   (borg steer.rs `keep_bounded`), so the file goes and the hook is back to one failed stat.
//! - **Veto**: when a round admitted any, and at every `enforce` (a failed record, or one made while
//!   learning was off, is retried), borg veto.rs `apply_messages` records each veto for a session of
//!   this node once.
//!
//! 0.3.0 sent signed directives in its `Live` frame; they are ignored (sync.rs `Live`).

use std::fmt;
use std::path::Path;

use borg::msg::Message;
use borg::steer::{Admission, Dropped, Role};

use crate::membership::Membership;

/// Why another node's privileged message does not count here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A member whose role may not steer (`member`, `observe`).
    Role(String),
    Revoked,
    Unknown,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Role(r) => write!(f, "role {r} may not steer"),
            Refusal::Revoked => write!(f, "revoked node"),
            Refusal::Unknown => write!(f, "unknown node"),
        }
    }
}

/// The role a privileged message from `node` counts under in `m`.
///
/// # Errors
/// The `Refusal` when it does not count.
pub fn check(m: &Membership, node: &str) -> Result<Role, Refusal> {
    match m.role_of(node) {
        Some(r) => Role::of_member(r).ok_or_else(|| Refusal::Role(r.to_string())),
        None if m.revoked.iter().any(|x| x.key == node) => Err(Refusal::Revoked),
        None => Err(Refusal::Unknown),
    }
}

/// What a round admitted and refused.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Admitted {
    pub new: usize,
    pub dropped: usize,
}

/// Admits the privileged messages among `got`, lines new to this node `me` whose origin verified as
/// their node, under `m` (module doc). Refused ones are logged. The caller records the vetoes among
/// them (`veto::apply_messages`) once it holds no lock, as that reads the whole log and the outcomes.
///
/// # Errors
/// When the admissions cannot be written.
pub fn admit(home: &Path, m: &Membership, me: &str, got: &[Message], now: i64) -> std::io::Result<Admitted> {
    let mut ok = Vec::new();
    let mut dropped = 0;
    for msg in got.iter().filter(|x| x.kind.privileged() && x.node != me) {
        match check(m, &msg.node) {
            Ok(role) => ok.push(Admission::of(msg, role)),
            Err(why) => {
                dropped += 1;
                let d = Dropped { ms: now, id: msg.id.clone(), node: msg.node.clone(), engineer: msg.name.clone(), kind: msg.kind, why: why.to_string() };
                borg::steer::log_dropped(home, &d);
            }
        }
    }
    let new = if ok.is_empty() { 0 } else { borg::steer::admit(home, &ok, now)? };
    Ok(Admitted { new, dropped })
}

/// Every round and after the membership changed: drops the admissions of nodes that may no longer
/// steer under `m` (this node's own, `Role::Local`, stay) and the expired ones at `now`, then retries
/// the admitted vetoes not yet recorded. Returns how many admissions went.
pub fn enforce(home: &Path, m: &Membership, now: i64) -> usize {
    if !borg::steer::active(home) {
        return 0;
    }
    let gone = borg::steer::retain(home, now, |a| a.role == Role::Local || check(m, &a.node).is_ok()).unwrap_or(0);
    borg::veto::apply_messages(home, now);
    gone
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use borg::msg::{Kind, Sender};
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-mesh-steer-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    struct Team {
        m: Membership,
        admin: Identity,
        steerer: Identity,
        observer: Identity,
        member: Identity,
        home: PathBuf,
    }

    fn team(tag: &str) -> Team {
        let home = tmp(tag);
        let id = |n: &str| Identity::load_or_create(&home.join(n)).unwrap();
        let (admin, steerer, observer, member) = (id("a"), id("s"), id("o"), id("m"));
        let mut m = Membership::found("corp", &admin);
        m.invite(&admin, &steerer.public(), "steer").unwrap();
        m.invite(&admin, &observer.public(), "observe").unwrap();
        m.invite(&admin, &member.public(), "member").unwrap();
        Team { m, admin, steerer, observer, member, home: home.join("store") }
    }

    fn steer(by: &Identity, kind: Kind, to: &str, now: i64) -> Message {
        let from = Sender { by: "engineer:x@y".into(), name: "x@y".into(), harness: "terminal".into(), node: by.public() };
        let text = if kind == Kind::Veto { "no" } else { "" };
        Message::new(kind, &from, to, None, now, text)
    }

    #[test]
    fn only_admin_and_steer_nodes_are_admitted() {
        let mut t = team("roles");
        assert_eq!(check(&t.m, &t.admin.public()), Ok(Role::Admin));
        assert_eq!(check(&t.m, &t.steerer.public()), Ok(Role::Steer));
        assert_eq!(check(&t.m, &t.observer.public()), Err(Refusal::Role("observe".into())));
        assert_eq!(check(&t.m, &t.member.public()), Err(Refusal::Role("member".into())));
        assert_eq!(check(&t.m, &"0".repeat(64)), Err(Refusal::Unknown));
        t.m.revoke(&t.admin, &t.steerer.public(), 900).unwrap();
        assert_eq!(check(&t.m, &t.steerer.public()), Err(Refusal::Revoked));
    }

    #[test]
    fn a_round_admits_steerers_logs_the_rest_and_skips_tells_and_own_lines() {
        let t = team("admit");
        let now = borg::learn::now_ms();
        let me = t.member.public();
        let tell = Message::new(
            Kind::Tell,
            &Sender { by: "s1".into(), name: "One".into(), harness: "claude".into(), node: t.observer.public() },
            "all",
            None,
            now,
            "hi",
        );
        let got = [steer(&t.steerer, Kind::Pause, "all", now), steer(&t.observer, Kind::Pause, "all", now), steer(&t.member, Kind::Pause, "all", now), tell];
        assert_eq!(admit(&t.home, &t.m, &me, &got, now).unwrap(), Admitted { new: 1, dropped: 1 });
        let held = borg::steer::read_admitted(&t.home);
        assert_eq!((held.len(), held[0].role, held[0].node.as_str()), (1, Role::Steer, t.steerer.public().as_str()));
        let log = std::fs::read_to_string(borg::steer::dropped_path(&t.home)).unwrap();
        assert!(log.contains("role observe may not steer") && log.contains(&got[1].id), "{log}");
        assert_eq!(admit(&t.home, &t.m, &me, &got[..1], now).unwrap(), Admitted { new: 0, dropped: 0 }, "admitted once");
    }

    #[test]
    fn a_demoted_or_revoked_node_stops_steering_and_own_admissions_stay() {
        let mut t = team("enforce");
        let now = borg::learn::now_ms();
        let me = t.member.public();
        admit(&t.home, &t.m, &me, &[steer(&t.steerer, Kind::Pause, "all", now), steer(&t.admin, Kind::Pause, "all", now + 1)], now).unwrap();
        let own = steer(&t.member, Kind::Pause, "all", now);
        borg::steer::admit(&t.home, &[Admission::of(&own, Role::Local)], now).unwrap();
        assert_eq!(enforce(&t.home, &t.m, now), 0);
        t.m.revoke(&t.admin, &t.steerer.public(), now).unwrap();
        assert_eq!(enforce(&t.home, &t.m, now), 1);
        let left: Vec<Role> = borg::steer::read_admitted(&t.home).into_iter().map(|a| a.role).collect();
        assert_eq!(left, [Role::Admin, Role::Local]);
        let late = now + 1 + borg::steer::TTL_MS + borg::steer::SLACK_MS + 1;
        assert_eq!(enforce(&t.home, &t.m, late), 2, "expired admissions go without a new steer");
        assert!(!borg::steer::active(&t.home), "back to the hook's failed stat");
    }

    #[test]
    fn a_veto_not_recorded_when_admitted_is_retried_at_the_next_enforce() {
        let t = team("veto-retry");
        let now = borg::learn::now_ms();
        let me = borg::msg::node(&t.home);
        let veto = steer(&t.steerer, Kind::Veto, &format!("s1@{me}"), now);
        std::fs::create_dir_all(&t.home).unwrap();
        std::fs::write(borg::msg::path(&t.home), format!("{}\n", serde_json::to_string(&veto).unwrap())).unwrap();
        assert_eq!(admit(&t.home, &t.m, &me, std::slice::from_ref(&veto), now).unwrap().new, 1);
        let episodes = t.home.join("episodes.jsonl");
        assert!(!episodes.exists(), "s1 is not known here yet: nothing recorded");
        let asked = serde_json::json!({"kind": "asked", "session": "s1", "lessons": ["L1"], "ms": now - 10, "repo": "r"});
        std::fs::write(t.home.join("outcomes.jsonl"), format!("{asked}\n")).unwrap();
        assert_eq!(enforce(&t.home, &t.m, now), 0);
        let recorded = std::fs::read_to_string(&episodes).unwrap();
        assert_eq!(recorded.lines().count(), 1, "{recorded}");
        enforce(&t.home, &t.m, now);
        assert_eq!(std::fs::read_to_string(&episodes).unwrap().lines().count(), 1, "once");
    }
}
