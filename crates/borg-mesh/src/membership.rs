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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use borg::follow::{self, Mark};
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

/// A document as its admin signs it, borrowed: serialises byte for byte as the `Membership` with `sig`
/// empty and `also` dropped.
#[derive(Serialize)]
struct Unsigned<'a> {
    collective: &'a str,
    admin: &'a str,
    version: u64,
    #[serde(skip_serializing_if = "str::is_empty")]
    prev: &'a str,
    members: &'a [Member],
    #[serde(skip_serializing_if = "<[Revoked]>::is_empty")]
    revoked: &'a [Revoked],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    merged: &'a [String],
    sig: &'a str,
}

/// The signed bytes: a domain tag and the document without its signature (and without `also`).
fn body(m: &Membership) -> Vec<u8> {
    let unsigned = Unsigned {
        collective: &m.collective,
        admin: &m.admin,
        version: m.version,
        prev: &m.prev,
        members: &m.members,
        revoked: &m.revoked,
        merged: &m.merged,
        sig: "",
    };
    let mut b = b"borg-membership-v1\n".to_vec();
    serde_json::to_writer(&mut b, &unsigned).expect("membership serialises");
    b
}

/// The `id`s of the documents this process verified: each is signature-checked once, however many
/// sessions bring it again. In a sync session, `resolve` checks a peer's document only where it would
/// attach; a join (origin.rs) checks every document it was handed.
static VERIFIED: LazyLock<Mutex<HashSet<[u8; 32]>>> = LazyLock::new(Mutex::default);

#[cfg(test)]
thread_local! {
    /// Signature checks this thread ran (`verify`), for the tests that count them.
    static CHECKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Resolutions this thread ran (`resolve`), for the tests that count them.
    static RESOLVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
        to_hex(self.named(&body(self)).as_bytes())
    }

    /// The hash of this document's signed bytes `body` and its signature: what `id` names.
    fn named(&self, body: &[u8]) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(body);
        h.update(self.sig.as_bytes());
        h.finalize()
    }

    /// The document as its admin signed it (`also` dropped).
    pub fn signed(&self) -> Membership {
        Membership { also: Vec::new(), ..self.clone() }
    }

    /// Whether `other` is this document as signed, `also` aside.
    fn same_signed(&self, other: &Membership) -> bool {
        let Membership { collective, admin, version, prev, members, revoked, merged, sig, also: _ } = self;
        (version, sig, collective, admin, prev, members, revoked, merged)
            == (&other.version, &other.sig, &other.collective, &other.admin, &other.prev, &other.members, &other.revoked, &other.merged)
    }

    /// Signed by its admin, and the admin is listed as admin. Both follow from what `id` hashes, so a
    /// document this process verified before is not checked again (`VERIFIED`).
    pub fn verify(&self) -> Result<(), String> {
        let body = body(self);
        let id = *self.named(&body).as_bytes();
        if VERIFIED.lock().unwrap_or_else(|e| e.into_inner()).contains(&id) {
            return Ok(());
        }
        #[cfg(test)]
        CHECKED.with(|c| c.set(c.get() + 1));
        identity::verify(&self.admin, &body, &self.sig).map_err(|e| format!("membership of {}: {e}", self.collective))?;
        if self.listed(&self.admin) != Some("admin") {
            return Err(format!("membership of {}: its admin is not listed as admin", self.collective));
        }
        VERIFIED.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
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

    /// Whether `newer` may directly replace this version, named `id` (see the module doc), short of
    /// checking its signature: the cheap checks, run before `verify`.
    fn extended_by(&self, id: &str, newer: &Membership) -> bool {
        newer.collective == self.collective
            && Some(newer.version) == self.version.checked_add(1)
            && self.role_of(&newer.admin) == Some("admin")
            && newer.prev == id
    }

    /// The canonical version reachable from this one through `docs` (any order, any source), and
    /// every document of `docs` that attached to the chain on the way (module doc: forks).
    pub fn resolve(&self, docs: &[Membership]) -> (Membership, Vec<Membership>) {
        #[cfg(test)]
        RESOLVED.with(|c| c.set(c.get() + 1));
        // A document is signature-checked only where it would attach (`kids`), so what never attaches
        // costs no check.
        let mut pool: Vec<(String, Membership)> = docs.iter().filter(|d| d.collective == self.collective).map(|d| (d.id(), d.signed())).collect();
        pool.sort_by(|a, b| a.0.cmp(&b.0));
        pool.dedup_by(|a, b| a.0 == b.0);
        let kids = |p: &Membership, id: &str| pool.iter().filter(|(_, d)| p.extended_by(id, d) && d.verify().is_ok()).cloned().collect::<Vec<_>>();
        let mut cur = self.clone();
        let mut id = self.id();
        let mut attached = Vec::new();
        loop {
            let children = kids(&cur, &id);
            let Some((winner_id, winner)) = children.first().cloned() else { break };
            let mut also = cur.also.clone();
            for (from, loser) in &children[1..] {
                // The whole losing branch: what each of its documents revoked beyond its parent.
                let mut todo = vec![(cur.signed(), from.clone(), loser.clone())];
                while let Some((parent, did, d)) = todo.pop() {
                    for r in d.revoked.iter().filter(|r| !parent.revoked.iter().any(|p| p.key == r.key)) {
                        also.push(Also { from: from.clone(), key: r.key.clone(), at_ms: r.at_ms });
                    }
                    todo.extend(kids(&d, &did).into_iter().map(|(cid, c)| (d.clone(), cid, c)));
                    attached.push(d);
                }
            }
            also.retain(|a| !winner.merged.contains(&a.from) && winner.listed(&a.key).is_some());
            also.sort_by(|a, b| (&a.key, a.at_ms, &a.from).cmp(&(&b.key, b.at_ms, &b.from)));
            also.dedup();
            attached.push(winner.clone());
            cur = Membership { also, ..winner };
            id = winner_id;
        }
        (cur, attached)
    }

    /// Whether data `key` stamped at `ms` is admitted: a member's always, a revoked key's only
    /// from before its revocation, anyone else's never.
    pub fn admits(&self, key: &str, ms: i64) -> bool {
        let cut = self.also.iter().filter(|a| a.key == key).map(|a| a.at_ms).min();
        (self.listed(key).is_some() && cut.is_none_or(|at| ms < at)) || self.revoked.iter().any(|r| r.key == key && ms < r.at_ms)
    }

    /// Every version held so far, the current one included (as signed: `also` dropped).
    pub fn history(home: &Path) -> Vec<Membership> {
        files(home, |f| f.history(home))
    }

    /// Resolves this node's history together with `more` from its oldest held version and installs
    /// the result if it differs from the held one. Returns the held version afterwards and whether
    /// it changed. With the files unchanged since a call found that their history, with the same
    /// documents of `more` beyond it, resolves to the held version, the answer is known without
    /// resolving: `resolve` sees the same documents.
    pub fn adopt(home: &Path, more: &[Membership]) -> Result<(Membership, bool), String> {
        let (held, own, read, settled) = files(home, |f| {
            let held = f.held(home)?.cloned().ok_or_else(|| format!("no membership in {}", path(home).display()))?;
            let own = f.history(home);
            Ok::<_, String>((held, own, f.read(), f.settled.clone()))
        })?;
        let mut extra: Vec<[u8; 32]> = more.iter().filter(|d| !own.iter().any(|o| o.same_signed(d))).map(|d| *d.named(&body(d)).as_bytes()).collect();
        extra.sort_unstable();
        extra.dedup();
        let key = read.map(|r| (r, extra));
        if key.is_some() && settled == key {
            return Ok((held, false));
        }
        let root = own.first().cloned().unwrap_or_else(|| held.signed());
        let mut pool = own;
        pool.extend(more.iter().cloned());
        let (tip, attached) = root.resolve(&pool);
        if tip == held {
            files(home, |f| f.settled = key);
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
        files(home, |f| Ok(f.held(home)?.cloned()))
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        self.verify()?;
        let p = path(home);
        std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
        // A key this version no longer lists never completes another session to clear its records.
        let _ = borg::presence::retain_mismatches(home, |k| self.role_of(k).is_some());
        let _ = borg::presence::retain_reads(home, |k| self.role_of(k).is_some());
        let _ = borg::protocol::retain_versions(home, |k| self.role_of(k).is_some());
        self.record(home)
    }

    /// Adds this version (as signed) to the history, once.
    pub fn record(&self, home: &Path) -> Result<(), String> {
        let signed = self.signed();
        signed.verify()?;
        if files(home, |f| f.recorded(home).contains(&signed)) {
            return Ok(());
        }
        use std::io::Write;
        std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
        let h = history_path(home);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&h).map_err(|e| format!("{}: {e}", h.display()))?;
        writeln!(f, "{}", serde_json::to_string(&signed).unwrap()).map_err(|e| format!("{}: {e}", h.display()))
    }
}

/// What this process last read of each home's membership files, by home.
static FILES: LazyLock<Mutex<HashMap<PathBuf, Files>>> = LazyLock::new(Mutex::default);

/// A home's membership files as last read, each with the stat it was read under (borg follow.rs): a
/// file one stat finds unchanged is not read, parsed or verified again.
#[derive(Default)]
struct Files {
    /// `membership.json`: the verified document, none when the file is missing.
    held: Option<(Mark, Option<Membership>)>,
    /// `membership-history.jsonl`: the verified documents in it.
    recorded: Option<(Mark, Vec<Membership>)>,
    /// What `adopt` last found to resolve to the held version.
    settled: Option<Pool>,
}

/// What `adopt` resolves, short of reading it: the stats of both files (`Files::read`) and the sorted
/// `id`s of the peer's documents beyond their history.
type Pool = ((Mark, Mark), Vec<[u8; 32]>);

impl Files {
    /// The held document, `membership.json` read again if it changed. An error is not kept: the next
    /// call reads the file again.
    fn held(&mut self, home: &Path) -> Result<Option<&Membership>, String> {
        let p = path(home);
        if let Some((mark, text)) = changed(&p, self.held.as_ref().map(|h| h.0)) {
            self.held = None;
            self.held = Some((mark, held_in(&p, &text)?));
        }
        Ok(self.held.as_ref().and_then(|h| h.1.as_ref()))
    }

    /// The documents of the history file, read again if it changed.
    fn recorded(&mut self, home: &Path) -> &[Membership] {
        if let Some((mark, text)) = changed(&history_path(home), self.recorded.as_ref().map(|r| r.0)) {
            self.recorded = Some((mark, recorded_in(&text)));
        }
        self.recorded.as_ref().map_or(&[], |r| &r.1)
    }

    /// `Membership::history`.
    fn history(&mut self, home: &Path) -> Vec<Membership> {
        let mut v = self.recorded(home).to_vec();
        if let Ok(Some(cur)) = self.held(home) {
            let cur = cur.signed();
            if !v.contains(&cur) {
                v.push(cur);
            }
        }
        v.sort_by_key(|m| m.version);
        v.dedup();
        v
    }

    /// The stats both files were last read under.
    fn read(&self) -> Option<(Mark, Mark)> {
        Some((self.held.as_ref()?.0, self.recorded.as_ref()?.0))
    }
}

/// Runs `f` on what this process holds of `home`'s membership files.
fn files<R>(home: &Path, f: impl FnOnce(&mut Files) -> R) -> R {
    let mut all = FILES.lock().unwrap_or_else(|e| e.into_inner());
    f(all.entry(home.to_path_buf()).or_default())
}

/// The text of `p` and the stat it was read under, unless one stat says it is still the file read
/// under `seen`.
fn changed(p: &Path, seen: Option<Mark>) -> Option<(Mark, String)> {
    (seen != Some(Mark::at(p))).then(|| follow::whole(p))
}

/// The verified document in `text`, what `p` holds. Text that is no document (a missing, unreadable or
/// non-UTF-8 file reads as empty) is read again the plain way, for its exact answer.
fn held_in(p: &Path, text: &str) -> Result<Option<Membership>, String> {
    let Ok(m) = serde_json::from_str::<Membership>(text) else { return read_held(p) };
    m.verify()?;
    Ok(Some(m))
}

/// The verified document at `p`, read the plain way: none when the file is missing.
fn read_held(p: &Path) -> Result<Option<Membership>, String> {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let m: Membership = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
    m.verify()?;
    Ok(Some(m))
}

/// The verified documents in the history file's `text`, as signed.
fn recorded_in(text: &str) -> Vec<Membership> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Membership>(l).ok())
        .map(|mut m| {
            m.also.clear();
            m
        })
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
    fn a_member_that_leaves_takes_its_protocol_records_along() {
        let ((ha, admin), (hb, bob), (hc, eve)) = (node("pa"), node("pb"), node("pc"));
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &bob.public(), "member").unwrap();
        m.invite(&admin, &eve.public(), "member").unwrap();
        m.save(&ha).unwrap();
        let old = |ms| borg::presence::Mismatch { theirs: "borg/mesh/5".into(), ours: "borg/mesh/10".into(), ms };
        borg::presence::note_mismatch(&ha, &bob.public(), old(1)).unwrap();
        borg::presence::note_mismatch(&ha, &eve.public(), old(2)).unwrap();
        borg::presence::note_reads(&ha, &bob.public(), 1).unwrap();
        borg::presence::note_reads(&ha, &eve.public(), 2).unwrap();
        for (k, ms) in [(bob.public(), 1), (eve.public(), 2)] {
            let v = borg::protocol::PeerVersion { borg: Some("0.7.0".into()), protocol: 15, ms, engineer: None };
            borg::protocol::note_version(&ha, &k, &v).unwrap();
        }
        m.revoke(&admin, &bob.public(), 3).unwrap();
        m.save(&ha).unwrap();
        let left: Vec<String> = borg::presence::mismatches(&ha).into_keys().collect();
        assert_eq!(left, vec![eve.public()], "only the member still listed keeps its warning");
        let reads: Vec<String> = borg::presence::reads(&ha).into_keys().collect();
        assert_eq!(reads, vec![eve.public()], "and the line format its node reads");
        let versions: Vec<String> = borg::protocol::versions(&ha).into_keys().collect();
        assert_eq!(versions, vec![eve.public()], "and the version it last told");
        for h in [ha, hb, hc] {
            let _ = std::fs::remove_dir_all(h);
        }
    }

    /// Whether `newer` directly replaces `m` (module doc): offered alone, it attaches.
    fn adopts(m: &Membership, newer: &Membership) -> bool {
        m.resolve(std::slice::from_ref(newer)).1 == [newer.signed()]
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
        assert!(!adopts(&m, &hijack), "another admin's document is never adopted");
        let old = Membership::found("work", &admin);
        assert!(!adopts(&m, &old) && adopts(&old, &m), "only newer versions are adopted");

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
        assert!(adopts(&v3, &m), "the revocation is adopted by holders of the previous version");
        assert!(m.admits(&carol.public(), 999) && !m.admits(&carol.public(), 1000), "before the revocation only");
        assert!(m.admits(&bob.public(), 5) && !m.admits("ff", 5));

        let v4 = m.clone();
        // Hand-over: admin promotes bob (v5); bob, now admin, signs v6. A node at v3 cannot adopt v6
        // directly (bob was no admin in v3) but walks v3 → v4 → v5 → v6 through the history.
        m.invite(&admin, &bob.public(), "admin").unwrap();
        let v5 = m.clone();
        m.invite(&bob, &carol.public(), "member").unwrap();
        assert_eq!((m.version, m.admin.clone()), (6, bob.public()));
        assert!(!adopts(&v3, &m), "bob was not an admin in v3");
        assert_eq!(v3.resolve(&[m.clone(), v1.clone(), v5.clone()]).0, v3, "no step skips a version");
        assert_eq!(v3.resolve(&[m.clone(), v1.clone(), v5.clone(), v4]).0, m);
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
        assert!(!adopts(&v4, &maxed), "a version jump is never adopted");
        assert_eq!(v4.resolve(std::slice::from_ref(&maxed)).0, v4);
        let mut wrong_parent = v4.clone();
        wrong_parent.invite(&a2, &a2.public(), "admin").unwrap();
        wrong_parent.prev = "00".repeat(32);
        assert!(!adopts(&v4, &sign_as(&a2, wrong_parent)), "a child must name its parent");
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
        assert_eq!(x.resolve(&docs).0.version, x.version, "a loser alone has nowhere to go");
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

    /// The implementation before documents were verified once per process and the files memoised,
    /// kept as the oracle for `resolve`, `adopt`, `load` and `history`.
    mod old {
        use std::path::Path;

        use super::super::{Also, Membership, history_path, path};
        use crate::identity;

        pub fn body(m: &Membership) -> Vec<u8> {
            let unsigned = Membership { sig: String::new(), also: Vec::new(), ..m.clone() };
            let mut b = b"borg-membership-v1\n".to_vec();
            b.extend(serde_json::to_vec(&unsigned).expect("membership serialises"));
            b
        }

        pub fn id(m: &Membership) -> String {
            let mut b = body(m);
            b.extend(m.sig.as_bytes());
            crate::to_hex(blake3::hash(&b).as_bytes())
        }

        pub fn verify(m: &Membership) -> Result<(), String> {
            identity::verify(&m.admin, &body(m), &m.sig).map_err(|e| format!("membership of {}: {e}", m.collective))?;
            if m.members.iter().find(|x| x.key == m.admin).map(|x| x.role.as_str()) != Some("admin") {
                return Err(format!("membership of {}: its admin is not listed as admin", m.collective));
            }
            Ok(())
        }

        pub fn superseded_by(m: &Membership, newer: &Membership) -> bool {
            verify(newer).is_ok()
                && newer.collective == m.collective
                && Some(newer.version) == m.version.checked_add(1)
                && newer.prev == id(m)
                && m.role_of(&newer.admin) == Some("admin")
        }

        pub fn resolve(m: &Membership, docs: &[Membership]) -> (Membership, Vec<Membership>) {
            let mut pool: Vec<(String, Membership)> =
                docs.iter().map(Membership::signed).filter(|d| d.collective == m.collective && verify(d).is_ok()).map(|d| (id(&d), d)).collect();
            pool.sort_by(|a, b| a.0.cmp(&b.0));
            pool.dedup_by(|a, b| a.0 == b.0);
            let kids = |p: &Membership| pool.iter().filter(|(_, d)| superseded_by(p, d)).cloned().collect::<Vec<_>>();
            let mut cur = m.clone();
            let mut attached = Vec::new();
            loop {
                let children = kids(&cur);
                let Some((_, winner)) = children.first().cloned() else { break };
                let mut also = cur.also.clone();
                for (from, loser) in &children[1..] {
                    let mut todo = vec![(cur.signed(), loser.clone())];
                    while let Some((parent, d)) = todo.pop() {
                        for r in d.revoked.iter().filter(|r| !parent.revoked.iter().any(|p| p.key == r.key)) {
                            also.push(Also { from: from.clone(), key: r.key.clone(), at_ms: r.at_ms });
                        }
                        todo.extend(kids(&d).into_iter().map(|(_, c)| (d.clone(), c)));
                        attached.push(d);
                    }
                }
                also.retain(|a| !winner.merged.contains(&a.from) && winner.members.iter().any(|x| x.key == a.key));
                also.sort_by(|a, b| (&a.key, a.at_ms, &a.from).cmp(&(&b.key, b.at_ms, &b.from)));
                also.dedup();
                attached.push(winner.clone());
                cur = Membership { also, ..winner };
            }
            (cur, attached)
        }

        pub fn history(home: &Path) -> Vec<Membership> {
            let mut v = recorded(home);
            if let Ok(Some(cur)) = load(home) {
                let cur = cur.signed();
                if !v.contains(&cur) {
                    v.push(cur);
                }
            }
            v.sort_by_key(|m| m.version);
            v.dedup();
            v
        }

        pub fn adopt(home: &Path, more: &[Membership]) -> Result<(Membership, bool), String> {
            let held = load(home)?.ok_or_else(|| format!("no membership in {}", path(home).display()))?;
            let own = history(home);
            let root = own.first().cloned().unwrap_or_else(|| held.signed());
            let mut pool = own;
            pool.extend(more.iter().cloned());
            let (tip, attached) = resolve(&root, &pool);
            if tip == held {
                return Ok((held, false));
            }
            for d in &attached {
                record(d, home)?;
            }
            save(&tip, home)?;
            Ok((tip, true))
        }

        pub fn load(home: &Path) -> Result<Option<Membership>, String> {
            let p = path(home);
            let text = match std::fs::read_to_string(&p) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("{}: {e}", p.display())),
            };
            let m: Membership = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
            verify(&m)?;
            Ok(Some(m))
        }

        pub fn save(m: &Membership, home: &Path) -> Result<(), String> {
            verify(m)?;
            let p = path(home);
            std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
            let tmp = p.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_string_pretty(m).unwrap()).map_err(|e| format!("{}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
            let _ = borg::presence::retain_mismatches(home, |k| m.role_of(k).is_some());
            let _ = borg::presence::retain_reads(home, |k| m.role_of(k).is_some());
            let _ = borg::protocol::retain_versions(home, |k| m.role_of(k).is_some());
            record(m, home)
        }

        pub fn record(m: &Membership, home: &Path) -> Result<(), String> {
            use std::io::Write;
            let signed = m.signed();
            verify(&signed)?;
            if recorded(home).contains(&signed) {
                return Ok(());
            }
            std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
            let h = history_path(home);
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&h).map_err(|e| format!("{}: {e}", h.display()))?;
            writeln!(f, "{}", serde_json::to_string(&signed).unwrap()).map_err(|e| format!("{}: {e}", h.display()))
        }

        fn recorded(home: &Path) -> Vec<Membership> {
            std::fs::read_to_string(history_path(home))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<Membership>(l).ok())
                .map(|m| m.signed())
                .filter(|m| verify(m).is_ok())
                .collect()
        }
    }

    /// Signature checks and resolutions this thread ran so far.
    fn ran() -> (usize, usize) {
        (CHECKED.with(|c| c.get()), RESOLVED.with(|c| c.get()))
    }

    /// The membership files of `home`, as bytes (missing: empty).
    fn file_bytes(home: &Path) -> (Vec<u8>, Vec<u8>) {
        (std::fs::read(path(home)).unwrap_or_default(), std::fs::read(history_path(home)).unwrap_or_default())
    }

    /// A deterministic stream of choices (xorshift64).
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }

        fn shuffle<T>(&mut self, v: &mut [T]) {
            for i in (1..v.len()).rev() {
                v.swap(i, self.below(i + 1));
            }
        }
    }

    /// Documents of one collective as its admins sign them over time: invitations, promotions (a second
    /// admin, hand-overs), revocations, and forks (a child of a version that already has one). Some are
    /// signed on top of a resolved tip, folding a fork's revocations in (`merged`).
    fn chain(rng: &mut Rng, keys: &[Identity], steps: usize) -> Vec<Membership> {
        let mut docs = vec![Membership::found("work", &keys[0])];
        for _ in 0..steps {
            let parent = if rng.below(3) == 0 { old::resolve(&docs[0], &docs).0 } else { docs[docs.len() - 1 - rng.below(docs.len().min(3))].clone() };
            let admins: Vec<&Identity> = keys.iter().filter(|k| parent.role_of(&k.public()) == Some("admin")).collect();
            if admins.is_empty() {
                continue;
            }
            let by = admins[rng.below(admins.len())];
            let key = keys[rng.below(keys.len())].public();
            let at = 1000 + rng.below(1000) as i64;
            let mut d = parent.clone();
            let signed = match rng.below(4) {
                0 => d.invite(by, &key, "admin"),
                1 => d.invite(by, &key, "member"),
                2 => d.revoke(by, &key, at),
                // A fork: another admin revokes someone in a child of the same version.
                _ => d.revoke(by, &key, at).map(|()| {
                    let mut e = parent;
                    let (other, gone) = (admins[rng.below(admins.len())], keys[rng.below(keys.len())].public());
                    if e.revoke(other, &gone, at + 1).is_ok() {
                        docs.push(e.signed());
                    }
                }),
            };
            if signed.is_ok() {
                docs.push(d.signed());
            }
        }
        docs
    }

    #[test]
    fn the_signed_bytes_are_the_old_ones() {
        let (h, a) = node("body");
        let b = Identity::load_or_create(&h.join("b")).unwrap();
        let mut m = Membership::found("work", &a);
        assert_eq!(body(&m), old::body(&m), "a founding version: no prev, revoked or merged");
        m.invite(&a, &b.public(), "member").unwrap();
        m.revoke(&a, &b.public(), 7).unwrap();
        m.merged.push("ab".into());
        m.also.push(Also { from: "cd".into(), key: b.public(), at_ms: 3 });
        assert_eq!(body(&m), old::body(&m), "prev, revoked, merged; also dropped");
        assert_eq!(m.id(), old::id(&m));
        let _ = std::fs::remove_dir_all(h);
    }

    /// `adopt` and `resolve` against the old implementation over generated chains: from the same files and
    /// the same documents a peer sends (some of the chain, any order, now and then a forged one, one of
    /// another collective or one carrying `also`), the same answer and the same files, the same session
    /// repeated (the fast path) included.
    #[test]
    fn adopt_reaches_what_the_old_code_reaches() {
        let (h, _) = node("oracle");
        // Fixed keys, so the chains and their fork order are the same on every run.
        let keys: Vec<Identity> = (0..5u8)
            .map(|i| {
                let home = h.join(format!("k{i}"));
                std::fs::create_dir_all(identity::dir(&home)).unwrap();
                std::fs::write(identity::path(&home), crate::to_hex(&[i + 1; 32])).unwrap();
                Identity::load_or_create(&home).unwrap()
            })
            .collect();
        let stranger = Membership::found("other", &keys[1]);
        let (mut calls, resolved) = (0, ran().1);
        // What the generated chains held, and what the adopts did: [forks, two admins, revocations,
        // folded forks, a tip holding `also`, an adopt that moved].
        let mut seen = [0usize; 6];
        const SEEDS: usize = 20;
        for seed in 1..=SEEDS as u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let docs = chain(&mut rng, &keys, 24);
            seen[0] += docs.windows(2).filter(|w| w[0].version >= w[1].version).count();
            seen[1] += docs.iter().filter(|d| d.members.iter().filter(|m| m.role == "admin").count() > 1).count();
            seen[2] += docs.iter().filter(|d| !d.revoked.is_empty()).count();
            seen[3] += docs.iter().filter(|d| !d.merged.is_empty()).count();
            let (ho, hn) = (h.join(format!("old{seed}")), h.join(format!("new{seed}")));
            let start = &docs[rng.below(docs.len().min(3))];
            old::save(start, &ho).unwrap();
            old::save(start, &hn).unwrap();
            assert_eq!(Membership::load(&hn).unwrap(), old::load(&ho).unwrap());
            assert_eq!(Membership::history(&hn), old::history(&ho));
            for round in 0..5 {
                let mut more: Vec<Membership> = docs.iter().filter(|_| rng.below(3) != 0).cloned().collect();
                match rng.below(6) {
                    0 => more.push(stranger.clone()),
                    1 => {
                        let mut forged = docs[rng.below(docs.len())].clone();
                        forged.version += 1;
                        more.push(forged);
                    }
                    2 => {
                        let mut carrying = docs[rng.below(docs.len())].clone();
                        carrying.also.push(Also { from: "ff".into(), key: keys[0].public(), at_ms: 1 });
                        more.push(carrying);
                    }
                    _ => {}
                }
                rng.shuffle(&mut more);
                assert_eq!(docs[0].resolve(&more), old::resolve(&docs[0], &more), "seed {seed} round {round}: resolve");
                for _ in 0..2 {
                    let want = old::adopt(&ho, &more);
                    assert_eq!(Membership::adopt(&hn, &more), want, "seed {seed} round {round}: adopt");
                    if let Ok((tip, moved)) = &want {
                        seen[4] += usize::from(!tip.also.is_empty());
                        seen[5] += usize::from(*moved);
                    }
                    assert_eq!(file_bytes(&hn), file_bytes(&ho), "seed {seed} round {round}: the files");
                    calls += 1;
                }
                assert_eq!(Membership::load(&hn).unwrap(), old::load(&ho).unwrap());
                assert_eq!(Membership::history(&hn), old::history(&ho));
            }
        }
        // Each round resolved once for `resolve` itself; an `adopt` with nothing new did not.
        assert!(seen.iter().all(|n| *n > 0), "every case occurred: {seen:?}");
        let adopts = ran().1 - resolved - SEEDS * 5;
        assert!(adopts < calls, "the fast path ran: {adopts} of {calls} adopts resolved");
        let _ = std::fs::remove_dir_all(h);
    }

    /// The real store's membership files (`BORG_BENCH_STORE`), copied to two scratch homes: the old and the
    /// new code read them alike and adopt alike from the whole history, each prefix of it and its reverse.
    /// Then from each version of it held alone, as by a node that joined with it: both walk the real
    /// chain from there, record what attached and save the tip alike.
    #[test]
    fn the_real_store_adopts_as_before() {
        let Some(store) = std::env::var_os("BORG_BENCH_STORE").map(PathBuf::from) else {
            eprintln!("BORG_BENCH_STORE unset: the real-store oracle is skipped");
            return;
        };
        let (h, _) = node("real");
        let (ho, hn) = (h.join("old"), h.join("new"));
        for home in [&ho, &hn] {
            std::fs::create_dir_all(identity::dir(home)).unwrap();
            for p in [path, history_path] {
                std::fs::copy(p(&store), p(home)).unwrap();
            }
        }
        assert_eq!(Membership::load(&hn).unwrap(), old::load(&ho).unwrap());
        let docs = old::history(&ho);
        assert_eq!(Membership::history(&hn), docs);
        let mut reversed = docs.clone();
        reversed.reverse();
        let sends = (0..=docs.len()).map(|n| docs[..n].to_vec()).chain([reversed.clone()]);
        for more in sends {
            for _ in 0..2 {
                assert_eq!(Membership::adopt(&hn, &more), old::adopt(&ho, &more));
                assert_eq!(file_bytes(&hn), file_bytes(&ho));
            }
        }
        let mut moved = 0;
        for (k, start) in docs.iter().enumerate() {
            let (ho, hn) = (h.join(format!("old{k}")), h.join(format!("new{k}")));
            old::save(start, &ho).unwrap();
            old::save(start, &hn).unwrap();
            for more in [&docs, &reversed] {
                for _ in 0..2 {
                    let want = old::adopt(&ho, more);
                    assert_eq!(Membership::adopt(&hn, more), want, "from version {}", start.version);
                    assert_eq!(file_bytes(&hn), file_bytes(&ho), "from version {}", start.version);
                    moved += usize::from(want.is_ok_and(|w| w.1));
                }
            }
        }
        assert!(moved > 0 || docs.len() < 2, "an adopt from an older version moved");
        let _ = std::fs::remove_dir_all(h);
    }

    /// A session's membership calls on both sides, as sync.rs `exchange` makes them: the held version,
    /// each side's history sent, each side adopting what the other sent.
    fn session(a: &Path, b: &Path) {
        Membership::load(a).unwrap().unwrap();
        Membership::load(b).unwrap().unwrap();
        let (ours, theirs) = (Membership::history(a), Membership::history(b));
        assert!(!Membership::adopt(a, &theirs).unwrap().1);
        assert!(!Membership::adopt(b, &ours).unwrap().1);
    }

    /// With the membership unchanged, a session runs no signature check and resolves nothing; a file
    /// rewritten with the same document is read again but not checked again; a valid document is
    /// checked once, a forged one every time.
    #[test]
    fn an_unchanged_membership_costs_no_signature_check() {
        let ((ha, admin), (hb, bob)) = (node("once-a"), node("once-b"));
        let mut m = Membership::found("work", &admin);
        m.save(&ha).unwrap();
        m.save(&hb).unwrap();
        for (key, role) in [(bob.public(), "member"), (Identity::load_or_create(&ha.join("c")).unwrap().public(), "admin")] {
            m.invite(&admin, &key, role).unwrap();
            m.save(&ha).unwrap();
            m.save(&hb).unwrap();
        }
        session(&ha, &hb);
        let before = ran();
        session(&ha, &hb);
        assert_eq!(ran(), before, "no signature check, no resolution");

        m.save(&ha).unwrap(); // the same document renamed in: read again, verified before
        let before = ran();
        session(&ha, &hb);
        let after = ran();
        assert_eq!((after.0 - before.0, after.1 - before.1), (0, 1), "read again and resolved once, no signature check");
        session(&ha, &hb);
        assert_eq!(ran(), after, "and then settled again");

        let mut forged = m.clone();
        forged.members.push(Member { key: "ab".repeat(32), role: "member".into() });
        let before = ran().0;
        assert!(forged.verify().is_err() && forged.verify().is_err());
        assert_eq!(ran().0 - before, 2, "a document that does not verify is checked every time");
        let fresh = Membership::found("elsewhere", &bob); // signed, never verified yet
        let before = ran().0;
        fresh.verify().unwrap();
        fresh.clone().verify().unwrap();
        assert_eq!(ran().0 - before, 1, "a document that verifies is checked once");
        for h in [ha, hb] {
            let _ = std::fs::remove_dir_all(h);
        }
    }

    /// A node that joined with the newest version alone holds a shorter history than the admin, who
    /// sends the older versions and a losing fork child every session: once that settled, a session
    /// resolves nothing; a different document sent resolves again.
    #[test]
    fn a_peer_with_a_longer_history_settles_too() {
        let ((ha, admin), (hb, bob)) = (node("long-a"), node("long-b"));
        let mut m = Membership::found("work", &admin);
        m.save(&ha).unwrap();
        m.invite(&admin, &bob.public(), "admin").unwrap();
        m.save(&ha).unwrap();
        let mut lost = m.clone();
        lost.invite(&bob, &Identity::load_or_create(&ha.join("c")).unwrap().public(), "member").unwrap();
        m.invite(&admin, &Identity::load_or_create(&ha.join("d")).unwrap().public(), "member").unwrap();
        // Two children of v2: the one with the lower id wins, and both nodes hold it.
        let (winner, lost) = if m.id() < lost.id() { (m, lost) } else { (lost, m) };
        winner.save(&ha).unwrap();
        lost.record(&ha).unwrap();
        winner.save(&hb).unwrap();
        assert_eq!(Membership::adopt(&ha, &[]).unwrap(), (winner, false), "the admin's history resolves to its tip");
        let sent = Membership::history(&ha);
        assert_eq!((sent.len(), Membership::history(&hb).len()), (4, 1), "v1, v2, both v3s against the winner alone");
        assert!(!Membership::adopt(&hb, &sent).unwrap().1);
        let before = ran();
        assert!(!Membership::adopt(&hb, &sent).unwrap().1);
        let mut shuffled = sent.clone();
        shuffled.reverse();
        shuffled.push(sent[0].clone());
        assert!(!Membership::adopt(&hb, &shuffled).unwrap().1);
        assert_eq!(ran(), before, "the same documents in any order: no signature check, no resolution");
        assert!(!Membership::adopt(&hb, &sent[1..]).unwrap().1);
        assert_eq!(ran().1 - before.1, 1, "fewer documents: resolved again");
        for h in [ha, hb] {
            let _ = std::fs::remove_dir_all(h);
        }
    }
}
