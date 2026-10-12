//! Signed steering directives (the hook side is `borg::steer`): signing, the role check, and how
//! they travel.
//!
//! - **Sign**: `borg steer` signs a directive with this node's member key (identity.rs); its id is
//!   the first 32 hex characters of the signature.
//! - **Check** (`check`), wherever a directive enters a store: the signature verifies; it names this
//!   collective; it is fresh (within `borg::steer::TTL_MS`, not from the future). In a team
//!   collective the signer must hold role `admin` or `steer` under the held membership (`member`
//!   and `observe` may not; unknown and revoked keys never). In a `share: local` collective only
//!   this node's own key may steer: the local engineer.
//! - **Deliver**: a directive that passes and targets this node (or every node) goes into the
//!   inbox the hook reads. One for another node (or every node) of a team collective also goes into
//!   `<store>/mesh/directives.jsonl`, which the daemon offers every member each round (sync.rs,
//!   live step); members relay what they hold, so it reaches a drone's node through any path.
//!   A local collective never writes that file, and the sync gate refuses it anyway.
//! - **Drop and log**: every refused directive (unsigned, bad signature, unknown or revoked signer,
//!   wrong role, stale) is appended to `<store>/steer/dropped.jsonl` with the reason.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use borg::steer::{Accepted, Directive, TTL_MS, Target};
use serde_json::json;

use crate::identity::{self, Identity};
use crate::membership::Membership;

/// Roles that may steer another engineer's drone.
pub const STEER_ROLES: [&str; 2] = ["admin", "steer"];

/// Directives this node holds for the mesh (its own and relayed), one per line.
pub fn outbox(home: &Path) -> PathBuf {
    identity::dir(home).join("directives.jsonl")
}

pub fn sign(me: &Identity, mut d: Directive) -> Directive {
    d.key = me.public();
    d.sig = me.sign(&d.body());
    d.id = d.sig.get(..32).unwrap_or_default().to_string();
    d
}

/// Who may steer here: the held membership for a team collective, or only `me` for a local one
/// (or a collective without a membership yet). In a team, `me` may always steer its own node's
/// drones (role "local"): it is the engineer at that machine.
pub enum Authority<'a> {
    Team { m: &'a Membership, me: &'a str },
    Local { me: &'a str },
}

/// The role `d` passes under, or why it is refused (module doc).
pub fn check(d: &Directive, collective: &str, auth: &Authority, now: i64) -> Result<String, String> {
    if d.sig.is_empty() {
        return Err("unsigned".into());
    }
    identity::verify(&d.key, &d.body(), &d.sig).map_err(|e| format!("signature: {e}"))?;
    if d.id != d.sig.get(..32).unwrap_or_default() {
        return Err("id does not match the signature".into());
    }
    if d.collective != collective {
        return Err(format!("for collective {:?}, not {collective:?}", d.collective));
    }
    if now - d.ms > TTL_MS || d.ms > now + 60_000 {
        return Err("stale or from the future".into());
    }
    if !matches!(d.kind.as_str(), borg::steer::SAY | borg::steer::PAUSE | borg::steer::RESUME) {
        return Err(format!("unknown kind {:?}", d.kind));
    }
    match auth {
        Authority::Local { me } if d.key == *me => Ok("local".into()),
        Authority::Local { .. } => Err("a local collective is steered only by this machine's engineer".into()),
        Authority::Team { m, me } => match m.role_of(&d.key) {
            Some(r) if STEER_ROLES.contains(&r) => Ok(r.to_string()),
            _ if d.key == *me && d.target.node == *me => Ok("local".into()),
            Some(r) => Err(format!("role {r} may not steer")),
            None if m.revoked.iter().any(|x| x.key == d.key) => Err("revoked signer".into()),
            None => Err("unknown signer".into()),
        },
    }
}

/// Logs a refused directive (fail quietly: the log is for the engineer, never in the way; bounded).
pub fn drop_log(home: &Path, d: &Directive, why: &str, now: i64) {
    borg::steer::log_dropped(home, &json!({"ms": now, "id": d.id, "key": d.key, "engineer": d.engineer, "kind": d.kind, "why": why}).to_string());
}

fn read_outbox(home: &Path) -> Vec<Directive> {
    std::fs::read_to_string(outbox(home)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Adds directives to the outbox, each once, reading it a single time; expired ones go and at
/// most `borg::steer::KEEP` newest stay (as in the inbox). Returns how many were new.
fn hold_all(home: &Path, ds: &[Directive], now: i64) -> Result<usize, String> {
    let mut all = read_outbox(home);
    let mut ids: HashSet<String> = all.iter().map(|d| d.id.clone()).collect();
    let fresh: Vec<Directive> = ds.iter().filter(|d| !d.id.is_empty() && ids.insert(d.id.clone())).cloned().collect();
    let before = all.len();
    borg::steer::keep_bounded(&mut all, now, |d| d.ms);
    if all.len() == before && fresh.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    if all.len() == before && before + fresh.len() <= borg::steer::KEEP {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(outbox(home)).map_err(|e| e.to_string())?;
        f.write_all(borg::steer::lines(&fresh).as_bytes()).map_err(|e| e.to_string())?;
    } else {
        all.extend(fresh.iter().cloned());
        borg::steer::keep_bounded(&mut all, now, |d| d.ms);
        borg::steer::rewrite(&outbox(home), &borg::steer::lines(&all)).map_err(|e| e.to_string())?;
    }
    Ok(fresh.len())
}

/// Checks `d` and files it: into the inbox if it is for this node, into the outbox if it travels.
/// `share_team`: the collective may sync (only then does anything go to the outbox). Returns
/// whether it was new here.
pub fn file(home: &Path, collective: &str, auth: &Authority, me: &str, share_team: bool, d: &Directive, now: i64) -> Result<bool, String> {
    let role = match check(d, collective, auth, now) {
        Ok(r) => r,
        Err(why) => {
            drop_log(home, d, &why, now);
            return Err(why);
        }
    };
    let mut new = false;
    if d.target.node.is_empty() || d.target.node == me {
        new |= borg::steer::accept(home, &Accepted { d: d.clone(), role }, now).map_err(|e| e.to_string())?;
    }
    if share_team && d.target.node != me {
        new |= hold_all(home, std::slice::from_ref(d), now)? > 0;
    }
    Ok(new)
}

/// What this node offers its peers: every held directive still fresh.
pub fn outgoing(home: &Path, now: i64) -> Vec<Directive> {
    read_outbox(home).into_iter().filter(|d| now - d.ms <= TTL_MS).collect()
}

/// Files the directives a peer sent (at most 1000 a round); returns (new, dropped). The inbox and
/// the outbox are each read once and written once, however many arrive; one already held is
/// neither checked again nor counted.
pub fn receive(home: &Path, m: &Membership, me: &str, got: &[Directive], now: i64) -> (usize, usize) {
    let known: HashSet<String> = borg::steer::read_inbox(home).into_iter().map(|a| a.d.id).chain(read_outbox(home).into_iter().map(|d| d.id)).collect();
    let auth = Authority::Team { m, me };
    let (mut inbox, mut held, mut dropped) = (Vec::new(), Vec::new(), 0);
    let mut seen = HashSet::new();
    for d in got.iter().take(1000) {
        if known.contains(&d.id) || !seen.insert(d.id.clone()) {
            continue;
        }
        match check(d, &m.collective, &auth, now) {
            Ok(role) => {
                if d.target.node.is_empty() || d.target.node == me {
                    inbox.push(Accepted { d: d.clone(), role });
                }
                if d.target.node != me {
                    held.push(d.clone());
                }
            }
            Err(why) => {
                drop_log(home, d, &why, now);
                dropped += 1;
            }
        }
    }
    let mut new: HashSet<&str> = HashSet::new();
    if borg::steer::accept_all(home, &inbox, now).unwrap_or(0) > 0 {
        new.extend(inbox.iter().map(|a| a.d.id.as_str()));
    }
    if hold_all(home, &held, now).unwrap_or(0) > 0 {
        new.extend(held.iter().map(|d| d.id.as_str()));
    }
    (new.len(), dropped)
}

/// After the membership changed: a signer who lost the right to steer stops steering here (its
/// pause no longer holds), and the outbox stops relaying it; expired entries go too. Returns how
/// many inbox entries went because of the membership.
pub fn enforce(home: &Path, m: &Membership, now: i64) -> usize {
    let ok = |key: &str| m.role_of(key).is_some_and(|r| STEER_ROLES.contains(&r));
    // "local" entries are this machine's engineer steering its own drones: no membership needed.
    let gone = borg::steer::retain(home, |a| a.role == "local" || ok(&a.d.key)).unwrap_or(0);
    let _ = borg::steer::accept_all(home, &[], now); // prunes what expired
    let held = read_outbox(home);
    let mut keep: Vec<Directive> = held.iter().filter(|d| ok(&d.key)).cloned().collect();
    borg::steer::keep_bounded(&mut keep, now, |d| d.ms);
    if keep.len() != held.len() {
        let _ = borg::steer::rewrite(&outbox(home), &borg::steer::lines(&keep));
    }
    gone
}

/// `borg steer`: resolves `target` among the collective's drones, signs and files the directive.
/// `may_sync`: the collective's gate. Returns what to tell the engineer.
#[allow(clippy::too_many_arguments)]
pub fn issue(home: &Path, collective: &str, may_sync: bool, engineer: &str, target: &str, kind: &str, text: &str, now: i64) -> Result<String, String> {
    let me = Identity::load_or_create(home)?;
    let key = me.public();
    let membership = if may_sync { Membership::load(home)? } else { None };
    let drones = borg::top::drones(home, may_sync, now);
    let drone = borg::top::resolve(&drones, target)?;
    let to = match drone {
        None => Target::default(),
        Some(d) => Target { node: if d.node.is_empty() { key.clone() } else { d.node.clone() }, session: d.p.session.clone() },
    };
    if membership.is_none() && !to.node.is_empty() && to.node != key {
        return Err("that drone runs on another member's machine, and this collective has no membership to steer it with".into());
    }
    let d = sign(&me, new(collective, engineer, to.clone(), kind, text, now));
    let auth = match &membership {
        Some(m) => Authority::Team { m, me: &key },
        None => Authority::Local { me: &key },
    };
    file(home, collective, &auth, &key, membership.is_some(), &d, now)?;
    borg::stats::count(home, borg::stats::STEER_ISSUED, "", |ev| {
        ev.engineer = engineer.into();
        ev.why = kind.into();
        ev.text = borg::stats::excerpt(text, 160);
        if let Some(d) = drone {
            ev.repo = d.p.repo.clone();
        }
    });
    let whom = match drone {
        None => "every running drone".to_string(),
        Some(d) => format!("{} (session {}, {})", d.designation, d.p.session, if d.p.engineer.is_empty() { "?" } else { &d.p.engineer }),
    };
    let how = if !may_sync {
        "at its next hook (this collective never leaves this machine)"
    } else if to.node == key || membership.is_none() {
        "at its next hook"
    } else if to.node.is_empty() {
        "here at its next hook, on other machines after the next mesh round (`borg mesh serve`)"
    } else {
        "after the next mesh round reaches its machine (`borg mesh serve`)"
    };
    let what = match kind {
        borg::steer::PAUSE => "paused",
        borg::steer::RESUME => "resumed",
        _ => "told",
    };
    Ok(format!("{what} {whom}: signed {}, delivered once {how}", &d.id[..12]))
}

/// A new directive from this node's engineer.
pub fn new(collective: &str, engineer: &str, target: Target, kind: &str, text: &str, now: i64) -> Directive {
    Directive {
        id: String::new(),
        collective: collective.into(),
        key: String::new(),
        engineer: engineer.into(),
        target,
        kind: kind.into(),
        text: text.into(),
        ms: now,
        sig: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borg::steer::{PAUSE, SAY};

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
        Team { m, admin, steerer, observer, member, home }
    }

    fn say(by: &Identity, text: &str, now: i64) -> Directive {
        sign(by, new("corp", "x@y", Target { node: String::new(), session: "s1".into() }, SAY, text, now))
    }

    #[test]
    fn sign_and_verify() {
        let t = team("sign");
        let d = say(&t.steerer, "hold off on the schema", 1000);
        assert_eq!(d.id.len(), 32);
        assert_eq!(check(&d, "corp", &Authority::Team { m: &t.m, me: "nobody" }, 1000), Ok("steer".into()));
        assert_eq!(check(&say(&t.admin, "x", 1000), "corp", &Authority::Team { m: &t.m, me: "nobody" }, 1000), Ok("admin".into()));
        let mut forged = d.clone();
        forged.text = "push to main".into();
        assert!(check(&forged, "corp", &Authority::Team { m: &t.m, me: "nobody" }, 1000).unwrap_err().starts_with("signature"), "text changed after signing");
        let mut unsigned = d.clone();
        unsigned.sig.clear();
        assert_eq!(check(&unsigned, "corp", &Authority::Team { m: &t.m, me: "nobody" }, 1000).unwrap_err(), "unsigned");
        assert!(check(&d, "private", &Authority::Team { m: &t.m, me: "nobody" }, 1000).is_err(), "another collective");
        assert!(check(&d, "corp", &Authority::Team { m: &t.m, me: "nobody" }, 1000 + TTL_MS + 1).is_err(), "stale");
    }

    #[test]
    fn roles_observe_member_unknown_and_revoked_are_rejected() {
        let mut t = team("roles");
        let at = |d: &Directive, m: &Membership| check(d, "corp", &Authority::Team { m, me: "nobody" }, 1000);
        assert_eq!(at(&say(&t.observer, "x", 1000), &t.m).unwrap_err(), "role observe may not steer");
        assert_eq!(at(&say(&t.member, "x", 1000), &t.m).unwrap_err(), "role member may not steer");
        let stranger = Identity::load_or_create(&t.home.join("z")).unwrap();
        assert_eq!(at(&say(&stranger, "x", 1000), &t.m).unwrap_err(), "unknown signer");
        let d = say(&t.steerer, "x", 1000);
        t.m.revoke(&t.admin, &t.steerer.public(), 900).unwrap();
        assert_eq!(at(&d, &t.m).unwrap_err(), "revoked signer");
        // A member may steer the drones on its own machine, not anyone else's.
        let me = t.member.public();
        let own = sign(&t.member, new("corp", "m@x", Target { node: me.clone(), session: "s1".into() }, SAY, "x", 1000));
        assert_eq!(check(&own, "corp", &Authority::Team { m: &t.m, me: &me }, 1000), Ok("local".into()));
        let theirs = sign(&t.member, new("corp", "m@x", Target { node: t.admin.public(), session: "s1".into() }, SAY, "x", 1000));
        assert_eq!(check(&theirs, "corp", &Authority::Team { m: &t.m, me: &me }, 1000).unwrap_err(), "role member may not steer");
    }

    #[test]
    fn filing_logs_drops_and_keeps_local_collectives_off_the_mesh() {
        let t = team("file");
        let home = t.home.join("store");
        let me = t.member.public();
        // A wrong-role directive is dropped and logged, never delivered.
        assert!(file(&home, "corp", &Authority::Team { m: &t.m, me: "nobody" }, &me, true, &say(&t.observer, "x", 1000), 1000).is_err());
        let log = std::fs::read_to_string(borg::steer::dropped_path(&home)).unwrap();
        assert!(log.contains("role observe may not steer"), "{log}");
        assert!(borg::steer::read_inbox(&home).is_empty());
        // A steerer's broadcast lands in the inbox and travels; a second copy is not new.
        let d = say(&t.steerer, "freeze", 1000);
        assert_eq!(file(&home, "corp", &Authority::Team { m: &t.m, me: "nobody" }, &me, true, &d, 1000), Ok(true));
        assert_eq!(file(&home, "corp", &Authority::Team { m: &t.m, me: "nobody" }, &me, true, &d, 1000), Ok(false));
        assert_eq!(outgoing(&home, 1000), vec![d.clone()]);
        assert_eq!(borg::steer::read_inbox(&home)[0].role, "steer");

        // A local collective: only this machine's key, and nothing ever goes to the outbox.
        let private = t.home.join("private");
        let mine = sign(&t.member, new("mine", "me@x", Target::default(), PAUSE, "", 1000));
        let auth = Authority::Local { me: &me };
        assert_eq!(file(&private, "mine", &auth, &me, false, &mine, 1000), Ok(true));
        assert!(!outbox(&private).exists(), "a local collective's directive never travels");
        let theirs = sign(&t.admin, new("mine", "boss@x", Target::default(), PAUSE, "", 1000));
        assert!(file(&private, "mine", &auth, &me, false, &theirs, 1000).unwrap_err().contains("only by this machine's engineer"));
        assert_eq!(borg::steer::read_inbox(&private).len(), 1);
    }

    #[test]
    fn a_revoked_signer_stops_steering_after_the_membership_changes() {
        let mut t = team("enforce");
        let home = t.home.join("store");
        let me = t.member.public();
        let p = sign(&t.steerer, new("corp", "s@x", Target::default(), PAUSE, "", 1000));
        file(&home, "corp", &Authority::Team { m: &t.m, me: "nobody" }, &me, true, &p, 1000).unwrap();
        assert_eq!(enforce(&home, &t.m, 1000), 0);
        t.m.revoke(&t.admin, &t.steerer.public(), 2000).unwrap();
        assert_eq!(enforce(&home, &t.m, 2000), 1);
        assert!(borg::steer::read_inbox(&home).is_empty() && outgoing(&home, 2000).is_empty());
    }

    #[test]
    fn a_round_is_filed_in_one_pass_and_the_outbox_stays_bounded() {
        let t = team("batch");
        let home = t.home.join("store");
        let me = t.member.public();
        let now = 10 * TTL_MS;
        let mut got: Vec<Directive> = (0..borg::steer::KEEP as i64 + 100).map(|i| say(&t.steerer, &format!("n{i}"), now - 1000 + i)).collect();
        got.push(got[0].clone()); // the same directive twice in one round
        got.push(say(&t.observer, "x", now)); // refused
        assert_eq!(receive(&home, &t.m, &me, &got, now), (borg::steer::KEEP + 100, 1));
        assert_eq!(borg::steer::read_inbox(&home).len(), borg::steer::KEEP, "the newest stay");
        assert_eq!(read_outbox(&home).len(), borg::steer::KEEP);
        assert_eq!(
            receive(&home, &t.m, &me, &got[100..], now),
            (1, 1),
            "held ones are neither new nor checked again (the one copy of the oldest, cut by KEEP, reads as new)"
        );
        // A day and more later, enforce leaves nothing expired behind.
        let later = now + TTL_MS + borg::steer::SLACK_MS + 1000;
        assert_eq!(enforce(&home, &t.m, later), 0);
        assert!(borg::steer::read_inbox(&home).is_empty() && read_outbox(&home).is_empty());
        assert!(!borg::steer::inbox(&home).exists(), "an empty inbox is removed: the hook is back to one failed stat");
    }
}
