//! Who belongs to a collective: `<store>/mesh/membership.json`, signed by the collective's admin.
//!
//! ```json
//! { "collective": "work", "admin": "<hex key>", "version": 3,
//!   "members": [ { "key": "<hex key>", "role": "admin" }, { "key": "<hex key>", "role": "member" } ],
//!   "sig": "<hex ed25519 signature of the document without sig>" }
//! ```
//!
//! Trust is rooted in the document a node holds: `init` makes this node the admin, `join` installs a
//! document an admin handed over out of band. `admin` is the key that signed this version. A newer
//! document is adopted only when it verifies, names the same collective, is *exactly* one version
//! newer, names the held version's hash as its `prev`, and is signed by a key that is an admin *in
//! the version the node holds*: a member can never promote itself, a peer cannot roll back a
//! revocation, an admin cannot jump the version to the maximum, and an admin can hand over to
//! another one (each step is checked against the previous one; peers exchange their whole history,
//! so a node that missed a hand-over still walks the chain).
//!
//! **Forks** (`resolve`): two admins can each sign a valid child of the same version. Every node
//! resolves them the same way, whatever order it saw them in: the child with the lowest `id` (hash)
//! wins and the chain continues from it; the revocations the losing branch added are not lost but
//! held as `also` on top of the winner (unsigned, recomputed from the signed documents, never taken
//! from a peer), until an admin's next version folds them in (`merged` names the folded branches).
//! A revocation therefore always survives a race; an invitation on the losing branch has to be
//! repeated.
//!
//! **Revocation** (`revoke`): the key leaves `members` and is listed in `revoked` with the time of
//! revocation. Data originating from it (sync.rs: every synced line carries a signed origin) is
//! admitted only if stamped before that time. A revoked node could backdate its stamps; it can no
//! longer connect to any node that holds the new version, so backdated data can only arrive through
//! a member that has not learned of the revocation yet, which the history makes short-lived.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::{self, Identity};
use crate::to_hex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub key: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    pub collective: String,
    /// The key that signed this version; an admin under it and under the version it replaces.
    pub admin: String,
    pub version: u64,
    /// The `id` of the version this one replaces; empty for a founding version.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prev: String,
    pub members: Vec<Member>,
    /// Revoked keys. Absent when empty, so documents signed before revocation existed still verify.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked: Vec<Revoked>,
    /// The losing fork branches (their first document's `id`) whose revocations this version folded in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged: Vec<String>,
    #[serde(default)]
    pub sig: String,
    /// Not signed: revocations of losing fork branches applied on top of this version (module doc).
    /// Only `resolve` sets it; documents from peers have it cleared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Also>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoked {
    pub key: String,
    /// Epoch ms of the revocation: data this key stamped from then on is refused.
    pub at_ms: i64,
}

/// A revocation from a losing fork branch (`from`: that branch's first document).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Also {
    pub from: String,
    pub key: String,
    pub at_ms: i64,
}

/// The signed bytes: a domain tag and the document without its signature (and without `also`).
fn body(m: &Membership) -> Vec<u8> {
    let unsigned = Membership { sig: String::new(), also: Vec::new(), ..m.clone() };
    let mut b = b"borg-membership-v1\n".to_vec();
    b.extend(serde_json::to_vec(&unsigned).expect("membership serialises"));
    b
}

pub fn path(home: &Path) -> PathBuf {
    identity::dir(home).join("membership.json")
}

/// Every version this node has held or seen attach to its chain (one JSON document per line): sent
/// to peers so that a node several versions behind can follow admin hand-overs step by step.
pub fn history_path(home: &Path) -> PathBuf {
    identity::dir(home).join("membership-history.jsonl")
}

impl Membership {
    /// A new collective with `admin` as its only member.
    pub fn found(collective: &str, admin: &Identity) -> Membership {
        let mut m = Membership {
            collective: collective.into(),
            admin: admin.public(),
            version: 1,
            prev: String::new(),
            members: vec![Member { key: admin.public(), role: "admin".into() }],
            revoked: Vec::new(),
            merged: Vec::new(),
            sig: String::new(),
            also: Vec::new(),
        };
        m.sig = admin.sign(&body(&m));
        m
    }

    /// The hash that names this signed version (what a child puts in `prev`).
    pub fn id(&self) -> String {
        let mut b = body(self);
        b.extend(self.sig.as_bytes());
        to_hex(blake3::hash(&b).as_bytes())
    }

    /// The document as its admin signed it (`also` dropped).
    pub fn signed(&self) -> Membership {
        Membership { also: Vec::new(), ..self.clone() }
    }

    /// Signed by its admin, and the admin is listed as admin.
    pub fn verify(&self) -> Result<(), String> {
        identity::verify(&self.admin, &body(self), &self.sig).map_err(|e| format!("membership of {}: {e}", self.collective))?;
        if self.listed(&self.admin) != Some("admin") {
            return Err(format!("membership of {}: its admin is not listed as admin", self.collective));
        }
        Ok(())
    }

    /// The role the signed document gives `key`.
    fn listed(&self, key: &str) -> Option<&str> {
        self.members.iter().find(|m| m.key == key).map(|m| m.role.as_str())
    }

    /// The role of `key`, revocations of losing fork branches applied.
    pub fn role_of(&self, key: &str) -> Option<&str> {
        if self.also.iter().any(|a| a.key == key) {
            return None;
        }
        self.listed(key)
    }

    /// Adds (or changes the role of) `key`; only an admin can, and the document is re-signed.
    /// Inviting a revoked key again lifts its revocation (a fork's included).
    pub fn invite(&mut self, admin: &Identity, key: &str, role: &str) -> Result<(), String> {
        self.check_admin(admin, "invite")?;
        identity::check_key(key)?;
        let parent = self.id();
        match self.members.iter_mut().find(|m| m.key == key) {
            Some(m) => m.role = role.into(),
            None => self.members.push(Member { key: key.into(), role: role.into() }),
        }
        self.revoked.retain(|r| r.key != key);
        self.also.retain(|a| a.key != key);
        self.resign(admin, parent)
    }

    /// Removes `key` and records its revocation at `now_ms`; only an admin can, never itself.
    pub fn revoke(&mut self, admin: &Identity, key: &str, now_ms: i64) -> Result<(), String> {
        self.check_admin(admin, "revoke")?;
        identity::check_key(key)?;
        if key == admin.public() {
            return Err("an admin cannot revoke itself; another admin can".into());
        }
        if self.role_of(key).is_none() {
            return Err(format!("{key} is not a member of {}", self.collective));
        }
        let parent = self.id();
        self.members.retain(|m| m.key != key);
        self.revoked.retain(|r| r.key != key);
        self.revoked.push(Revoked { key: key.into(), at_ms: now_ms });
        self.resign(admin, parent)
    }

    fn check_admin(&self, admin: &Identity, what: &str) -> Result<(), String> {
        self.verify()?;
        if self.role_of(&admin.public()) != Some("admin") {
            return Err(format!("only an admin of {} can {what}", self.collective));
        }
        Ok(())
    }

    /// The next version, signed by `admin`, with the fork revocations held in `also` folded in.
    fn resign(&mut self, admin: &Identity, parent: String) -> Result<(), String> {
        self.version = self.version.checked_add(1).ok_or("membership version exhausted")?;
        for a in std::mem::take(&mut self.also) {
            if self.members.iter().any(|m| m.key == a.key) {
                self.members.retain(|m| m.key != a.key);
                self.revoked.retain(|r| r.key != a.key);
                self.revoked.push(Revoked { key: a.key.clone(), at_ms: a.at_ms });
            } else if let Some(r) = self.revoked.iter_mut().find(|r| r.key == a.key) {
                r.at_ms = r.at_ms.min(a.at_ms);
            }
            if !self.merged.contains(&a.from) {
                self.merged.push(a.from);
            }
        }
        self.prev = parent;
        self.admin = admin.public();
        self.sig = admin.sign(&body(self));
        self.verify()
    }

    /// Whether `newer` may directly replace this document (see the module doc).
    pub fn superseded_by(&self, newer: &Membership) -> bool {
        newer.verify().is_ok()
            && newer.collective == self.collective
            && Some(newer.version) == self.version.checked_add(1)
            && newer.prev == self.id()
            && self.role_of(&newer.admin) == Some("admin")
    }

    /// The canonical version reachable from this one through `docs` (any order, any source), and
    /// every document of `docs` that attached to the chain on the way (module doc: forks).
    pub fn resolve(&self, docs: &[Membership]) -> (Membership, Vec<Membership>) {
        let mut pool: Vec<(String, Membership)> =
            docs.iter().map(Membership::signed).filter(|d| d.collective == self.collective && d.verify().is_ok()).map(|d| (d.id(), d)).collect();
        pool.sort_by(|a, b| a.0.cmp(&b.0));
        pool.dedup_by(|a, b| a.0 == b.0);
        let kids = |p: &Membership| pool.iter().filter(|(_, d)| p.superseded_by(d)).cloned().collect::<Vec<_>>();
        let mut cur = self.clone();
        let mut attached = Vec::new();
        loop {
            let children = kids(&cur);
            let Some((_, winner)) = children.first().cloned() else { break };
            let mut also = cur.also.clone();
            for (from, loser) in &children[1..] {
                // The whole losing branch: what each of its documents revoked beyond its parent.
                let mut todo = vec![(cur.signed(), loser.clone())];
                while let Some((parent, d)) = todo.pop() {
                    for r in d.revoked.iter().filter(|r| !parent.revoked.iter().any(|p| p.key == r.key)) {
                        also.push(Also { from: from.clone(), key: r.key.clone(), at_ms: r.at_ms });
                    }
                    todo.extend(kids(&d).into_iter().map(|(_, c)| (d.clone(), c)));
                    attached.push(d);
                }
            }
            also.retain(|a| !winner.merged.contains(&a.from) && winner.listed(&a.key).is_some());
            also.sort_by(|a, b| (&a.key, a.at_ms, &a.from).cmp(&(&b.key, b.at_ms, &b.from)));
            also.dedup();
            attached.push(winner.clone());
            cur = Membership { also, ..winner };
        }
        (cur, attached)
    }

    /// The newest document reachable from this one through `docs`.
    pub fn newest_via(&self, docs: &[Membership]) -> Membership {
        self.resolve(docs).0
    }

    /// Whether data `key` stamped at `ms` is admitted: a member's always, a revoked key's only
    /// from before its revocation, anyone else's never.
    pub fn admits(&self, key: &str, ms: i64) -> bool {
        let cut = self.also.iter().filter(|a| a.key == key).map(|a| a.at_ms).min();
        (self.listed(key).is_some() && cut.is_none_or(|at| ms < at)) || self.revoked.iter().any(|r| r.key == key && ms < r.at_ms)
    }

    /// Every version held so far, the current one included (as signed: `also` dropped).
    pub fn history(home: &Path) -> Vec<Membership> {
        let mut v = recorded(home);
        if let Ok(Some(cur)) = Membership::load(home) {
            let cur = cur.signed();
            if !v.contains(&cur) {
                v.push(cur);
            }
        }
        v.sort_by_key(|m| m.version);
        v.dedup();
        v
    }

    /// Resolves this node's history together with `more` from its oldest held version and installs
    /// the result if it differs from the held one. Returns the held version afterwards and whether
    /// it changed.
    pub fn adopt(home: &Path, more: &[Membership]) -> Result<(Membership, bool), String> {
        let held = Membership::load(home)?.ok_or_else(|| format!("no membership in {}", path(home).display()))?;
        let own = Membership::history(home);
        let root = own.first().cloned().unwrap_or_else(|| held.signed());
        let mut pool = own;
        pool.extend(more.iter().cloned());
        let (tip, attached) = root.resolve(&pool);
        if tip == held {
            return Ok((held, false));
        }
        for d in &attached {
            d.record(home)?;
        }
        tip.save(home)?;
        Ok((tip, true))
    }

    /// The verified document in `home`'s store, if any.
    pub fn load(home: &Path) -> Result<Option<Membership>, String> {
        let p = path(home);
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", p.display())),
        };
        let m: Membership = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        m.verify()?;
        Ok(Some(m))
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        self.verify()?;
        let p = path(home);
        std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
        // A key this version no longer lists never completes another session to clear its protocol record.
        let _ = borg::presence::retain_mismatches(home, |k| self.role_of(k).is_some());
        self.record(home)
    }

    /// Adds this version (as signed) to the history, once.
    pub fn record(&self, home: &Path) -> Result<(), String> {
        let signed = self.signed();
        signed.verify()?;
        if recorded(home).contains(&signed) {
            return Ok(());
        }
        use std::io::Write;
        std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
        let h = history_path(home);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&h).map_err(|e| format!("{}: {e}", h.display()))?;
        writeln!(f, "{}", serde_json::to_string(&signed).unwrap()).map_err(|e| format!("{}: {e}", h.display()))
    }
}

/// The verified documents in the history file.
fn recorded(home: &Path) -> Vec<Membership> {
    std::fs::read_to_string(history_path(home))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Membership>(l).ok())
        .map(|m| m.signed())
        .filter(|m| m.verify().is_ok())
        .collect()
}

/// The documents in a file an admin handed over: one document (membership.json) or one per line
/// (membership-history.jsonl).
pub fn parse_docs(text: &str) -> Result<Vec<Membership>, String> {
    if let Ok(m) = serde_json::from_str::<Membership>(text) {
        return Ok(vec![m.signed()]);
    }
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str::<Membership>(l).map(|m| m.signed()).map_err(|e| e.to_string())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tag: &str) -> (PathBuf, Identity) {
        let home = std::env::temp_dir().join(format!("borg-mesh-mem-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let id = Identity::load_or_create(&home).unwrap();
        (home, id)
    }

    #[test]
    fn a_member_that_leaves_takes_its_protocol_record_along() {
        let ((ha, admin), (hb, bob), (hc, eve)) = (node("pa"), node("pb"), node("pc"));
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &bob.public(), "member").unwrap();
        m.invite(&admin, &eve.public(), "member").unwrap();
        m.save(&ha).unwrap();
        let old = |ms| borg::presence::Mismatch { theirs: "borg/mesh/5".into(), ours: "borg/mesh/10".into(), ms };
        borg::presence::note_mismatch(&ha, &bob.public(), old(1)).unwrap();
        borg::presence::note_mismatch(&ha, &eve.public(), old(2)).unwrap();
        m.revoke(&admin, &bob.public(), 3).unwrap();
        m.save(&ha).unwrap();
        let left: Vec<String> = borg::presence::mismatches(&ha).into_keys().collect();
        assert_eq!(left, vec![eve.public()], "only the member still listed keeps its warning");
        for h in [ha, hb, hc] {
            let _ = std::fs::remove_dir_all(h);
        }
    }

    #[test]
    fn admin_invites_and_nobody_else_can() {
        let ((ha, admin), (hb, bob), (hc, eve)) = (node("a"), node("b"), node("c"));
        let mut m = Membership::found("work", &admin);
        m.verify().unwrap();
        assert_eq!(m.role_of(&bob.public()), None);
        m.invite(&admin, &bob.public(), "member").unwrap();
        assert_eq!((m.version, m.role_of(&bob.public())), (2, Some("member")));
        assert!(m.clone().invite(&bob, &eve.public(), "member").is_err(), "a member cannot invite");
        assert!(m.clone().invite(&admin, "not-a-key", "member").is_err());

        let mut forged = m.clone();
        forged.members.push(Member { key: eve.public(), role: "member".into() });
        assert!(forged.verify().is_err(), "an edited document no longer verifies");
        let mut hijack = Membership::found("work", &eve);
        hijack.version = 99;
        assert!(!m.superseded_by(&hijack), "another admin's document is never adopted");
        let old = Membership::found("work", &admin);
        assert!(!m.superseded_by(&old) && old.superseded_by(&m), "only newer versions are adopted");

        m.save(&ha).unwrap();
        assert_eq!(Membership::load(&ha).unwrap(), Some(m));
        for h in [ha, hb, hc] {
            let _ = std::fs::remove_dir_all(h);
        }
    }

    #[test]
    fn revocation_and_admin_hand_over() {
        let ((ha, admin), (hb, bob), (hc, carol)) = (node("ra"), node("rb"), node("rc"));
        let v1 = Membership::found("work", &admin);
        let mut m = v1.clone();
        m.invite(&admin, &bob.public(), "member").unwrap();
        m.invite(&admin, &carol.public(), "member").unwrap();
        let v3 = m.clone();
        assert!(m.clone().revoke(&bob, &carol.public(), 10).is_err(), "a member cannot revoke");
        assert!(m.clone().revoke(&admin, &admin.public(), 10).is_err(), "an admin cannot revoke itself");
        m.revoke(&admin, &carol.public(), 1000).unwrap();
        assert_eq!((m.version, m.role_of(&carol.public())), (4, None));
        assert!(v3.superseded_by(&m), "the revocation is adopted by holders of the previous version");
        assert!(m.admits(&carol.public(), 999) && !m.admits(&carol.public(), 1000), "before the revocation only");
        assert!(m.admits(&bob.public(), 5) && !m.admits("ff", 5));

        let v4 = m.clone();
        // Hand-over: admin promotes bob (v5); bob, now admin, signs v6. A node at v3 cannot adopt v6
        // directly (bob was no admin in v3) but walks v3 → v4 → v5 → v6 through the history.
        m.invite(&admin, &bob.public(), "admin").unwrap();
        let v5 = m.clone();
        m.invite(&bob, &carol.public(), "member").unwrap();
        assert_eq!((m.version, m.admin.clone()), (6, bob.public()));
        assert!(!v3.superseded_by(&m), "bob was not an admin in v3");
        assert_eq!(v3.newest_via(&[m.clone(), v1.clone(), v5.clone()]), v3, "no step skips a version");
        assert_eq!(v3.newest_via(&[m.clone(), v1.clone(), v5.clone(), v4]), m);
        assert!(m.admits(&carol.public(), 5000), "re-invited: the revocation is lifted");

        m.save(&ha).unwrap();
        v1.save(&hb).unwrap();
        v5.save(&hb).unwrap();
        assert_eq!(Membership::history(&hb), vec![v1, v5]);
        for h in [ha, hb, hc] {
            let _ = std::fs::remove_dir_all(h);
        }
    }

    /// An admin signs `d` as it is (the test's stand-in for a hand-edited document).
    fn sign_as(admin: &Identity, mut d: Membership) -> Membership {
        d.admin = admin.public();
        d.sig = admin.sign(&body(&d));
        d
    }

    #[test]
    fn a_version_can_only_move_by_one_and_never_overflow() {
        let ((ha, a1), (hb, a2)) = (node("max-a"), node("max-b"));
        let mut v4 = Membership::found("work", &a1);
        v4.invite(&a1, &a2.public(), "admin").unwrap();
        // A2, about to be revoked, signs v(2^64-1) revoking A1: valid signature, parent named, but no +1.
        let parent = v4.id();
        let mut maxed = v4.clone();
        maxed.members.retain(|m| m.key != a1.public());
        maxed.revoked.push(Revoked { key: a1.public(), at_ms: 1 });
        maxed.prev = parent;
        maxed.version = u64::MAX;
        let maxed = sign_as(&a2, maxed);
        maxed.verify().unwrap();
        assert!(!v4.superseded_by(&maxed), "a version jump is never adopted");
        assert_eq!(v4.newest_via(std::slice::from_ref(&maxed)), v4);
        let mut wrong_parent = v4.clone();
        wrong_parent.invite(&a2, &a2.public(), "admin").unwrap();
        wrong_parent.prev = "00".repeat(32);
        assert!(!v4.superseded_by(&sign_as(&a2, wrong_parent)), "a child must name its parent");
        // resign never wraps around.
        let mut top = maxed.clone();
        top.members.push(Member { key: a1.public(), role: "admin".into() });
        let mut top = sign_as(&a1, top);
        assert!(top.invite(&a1, &a2.public(), "member").unwrap_err().contains("exhausted"));
        for h in [ha, hb] {
            let _ = std::fs::remove_dir_all(h);
        }
    }

    /// Two admins sign a child of v4 at once: A1 revokes C, A2 revokes A1 and then signs on. Every
    /// node resolves the fork the same way whatever order it saw the documents in, and both
    /// revocations stand. The next version an admin signs folds them in for good.
    #[test]
    fn concurrent_admins_resolve_the_same_way_and_no_revocation_is_lost() {
        let ((ha, a1), (hb, a2), (hc, c), (hd, d)) = (node("fork-a"), node("fork-b"), node("fork-c"), node("fork-d"));
        let mut v4 = Membership::found("work", &a1);
        v4.invite(&a1, &a2.public(), "admin").unwrap();
        v4.invite(&a1, &c.public(), "member").unwrap();
        v4.invite(&a1, &d.public(), "admin").unwrap();
        let mut x = v4.clone();
        x.revoke(&a1, &c.public(), 100).unwrap();
        let mut y = v4.clone();
        y.revoke(&a2, &a1.public(), 200).unwrap();
        let y5 = y.clone();
        y.invite(&a2, &d.public(), "member").unwrap();
        let docs = vec![x.clone(), y5.clone(), y.clone()];
        let mut rev = docs.clone();
        rev.reverse();
        let (tip, attached) = v4.resolve(&docs);
        assert_eq!(tip, v4.resolve(&rev).0, "order does not matter");
        assert_eq!(attached.len(), 3);
        assert_eq!(tip.role_of(&c.public()), None, "A1's revocation of C stands");
        assert_eq!(tip.role_of(&a1.public()), None, "A2's revocation of A1 stands");
        assert!(tip.admits(&c.public(), 99) && !tip.admits(&c.public(), 100));
        // A node that adopted either branch first moves to the same result.
        assert_eq!(x.newest_via(&docs).version, x.version, "a loser alone has nowhere to go");
        assert_eq!(v4.resolve(&[x.clone()]).0.resolve(&docs).0.role_of(&c.public()), None);

        // The surviving admin signs the next version: the fork's revocations are folded in.
        let survivor = [&a1, &a2, &d].into_iter().find(|k| tip.role_of(&k.public()) == Some("admin")).expect("an admin survives");
        let mut next = tip.clone();
        next.invite(survivor, &hd_key(&hd), "member").unwrap();
        assert!(next.also.is_empty() && !next.merged.is_empty());
        assert_eq!(next.role_of(&c.public()), None);
        let mut all = docs.clone();
        all.push(next.clone());
        assert_eq!(v4.resolve(&all).0, next, "folded revocations are not applied twice");
        for h in [ha, hb, hc, hd] {
            let _ = std::fs::remove_dir_all(h);
        }
    }

    fn hd_key(h: &Path) -> String {
        Identity::load_or_create(&h.join("e")).unwrap().public()
    }
}
