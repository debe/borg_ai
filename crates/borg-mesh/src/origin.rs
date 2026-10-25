//! Where each synced line came from: `<store>/mesh/origins.jsonl`, one signed record per line hash.
//!
//! ```json
//! { "h": "<blake3 of the line>", "key": "<hex key of the node that first stamped it>", "ms": 1760000000000,
//!   "sig": "<that key's signature of borg-origin-v1, h and ms>" }
//! ```
//!
//! A node stamps the lines its own hooks wrote (any line without a record) with its key and the time
//! it first saw them; a line received from a peer travels with its record, which the receiver
//! verifies before storing either. The membership then decides whether a line is admitted
//! (`Membership::admits`): a member's always, a revoked key's only if stamped before the revocation.
//! Records are verified on receipt, not on every load: only this node writes this file.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::{self, Identity};
use crate::lines::{self, Hash};
use crate::membership::Membership;
use crate::{from_hex, to_hex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub h: String,
    pub key: String,
    pub ms: i64,
    pub sig: String,
}

fn signed(h: &str, ms: i64) -> Vec<u8> {
    format!("borg-origin-v1\n{h}\n{ms}").into_bytes()
}

impl Origin {
    pub fn new(me: &Identity, h: &Hash, ms: i64) -> Origin {
        let h = to_hex(h);
        let sig = me.sign(&signed(&h, ms));
        Origin { key: me.public(), h, ms, sig }
    }

    /// The record is `key`'s signature for exactly this line.
    pub fn verify(&self, line: &str) -> Result<(), String> {
        if from_hex::<32>(&self.h)? != lines::hash(line) {
            return Err("origin names another line".into());
        }
        identity::verify(&self.key, &signed(&self.h, self.ms), &self.sig).map_err(|e| format!("origin of {}: {e}", &self.h[..8]))
    }
}

pub fn path(home: &Path) -> PathBuf {
    identity::dir(home).join("origins.jsonl")
}

/// The first record per line hash.
pub fn load(home: &Path) -> HashMap<Hash, Origin> {
    let mut out = HashMap::new();
    for l in std::fs::read_to_string(path(home)).unwrap_or_default().lines() {
        let Ok(o) = serde_json::from_str::<Origin>(l) else { continue };
        let Ok(h) = from_hex::<32>(&o.h) else { continue };
        out.entry(h).or_insert(o);
    }
    out
}

pub fn append(home: &Path, records: &[Origin]) -> Result<(), String> {
    if records.is_empty() {
        return Ok(());
    }
    let p = path(home);
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    let text: String = records.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))
}

/// Stamps every line of `logs` that has no record yet as this node's, at `now`; returns how many.
/// A line that stays on this node (`lines::local`) is never stamped, so never offered.
pub fn stamp(home: &Path, me: &Identity, logs: &[PathBuf], now: i64) -> Result<usize, String> {
    let mut known: std::collections::HashSet<Hash> = load(home).into_keys().collect();
    let mut new = Vec::new();
    for log in logs {
        for (h, line) in lines::read(log) {
            if !lines::local(log, &line) && known.insert(h) {
                new.push(Origin::new(me, &h, now));
            }
        }
    }
    append(home, &new)?;
    Ok(new.len())
}

/// The lines of `set` the membership admits (lines without a record are not shared yet).
pub fn admitted(set: BTreeMap<Hash, String>, origins: &HashMap<Hash, Origin>, m: &Membership) -> BTreeMap<Hash, String> {
    set.into_iter().filter(|(h, _)| origins.get(h).is_some_and(|o| m.admits(&o.key, o.ms))).collect()
}

/// Applies revocations to a log already on disk: lines whose origin the membership no longer
/// admits move to `<store>/mesh/quarantine.jsonl`, so the conscience stops asking them. Returns
/// how many moved.
pub fn quarantine(home: &Path, log: &Path, m: &Membership) -> Result<usize, String> {
    let origins = load(home);
    let refused = |l: &str| origins.get(&lines::hash(l)).is_some_and(|o| !m.admits(&o.key, o.ms));
    // Read to rename under the log's lock, which its writers take for each append (borg live.rs
    // `locked`: the hive's `append_bytes`, the lessons' learn): a line appended meanwhile is never lost.
    let moved = borg::live::locked(log, || -> Result<usize, String> {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let (out, keep): (Vec<&str>, Vec<&str>) = text.lines().partition(|l| refused(l));
        if out.is_empty() {
            return Ok(0);
        }
        let q = identity::dir(home).join("quarantine.jsonl");
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&q).map_err(|e| format!("{}: {e}", q.display()))?;
        f.write_all(out.iter().map(|l| format!("{l}\n")).collect::<String>().as_bytes()).map_err(|e| e.to_string())?;
        let tmp = log.with_extension("jsonl.quarantine-tmp");
        std::fs::write(&tmp, keep.iter().map(|l| format!("{l}\n")).collect::<String>()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, log).map_err(|e| format!("{}: {e}", log.display()))?;
        Ok(out.len())
    })?;
    if moved > 0 {
        // A removed line may have been the winning version of a lesson: make the winner among the
        // remaining lines the last one again, or the conscience would ask an older version.
        lines::settle(log)?;
    }
    Ok(moved)
}

/// `<store>/mesh/quarantined-at`: which membership (`fingerprint`) the logs were last checked against.
fn marker(home: &Path) -> PathBuf {
    identity::dir(home).join("quarantined-at")
}

/// The membership as this node applies it: the signed version and the fork revocations on top.
fn fingerprint(m: &Membership) -> String {
    to_hex(blake3::hash(&serde_json::to_vec(m).expect("membership serialises")).as_bytes())
}

/// Quarantines every log of `home` against `m` and records that it did. Returns how many lines moved.
pub fn apply(home: &Path, m: &Membership) -> Result<usize, String> {
    let mut moved = 0;
    for log in lines::logs(home) {
        moved += quarantine(home, &log, m)?;
    }
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    std::fs::write(marker(home), fingerprint(m)).map_err(|e| format!("{}: {e}", marker(home).display()))?;
    Ok(moved)
}

/// `apply`, unless the logs were already checked against exactly this membership.
pub fn apply_if_new(home: &Path, m: &Membership) -> Result<usize, String> {
    if std::fs::read_to_string(marker(home)).is_ok_and(|t| t.trim() == fingerprint(m)) {
        return Ok(0);
    }
    apply(home, m)
}

/// `borg mesh join`: installs the documents an admin handed over (one version, or the history
/// file) and quarantines what the resulting membership no longer admits. Returns the installed
/// membership and how many lines moved.
pub fn join(home: &Path, collective: &str, docs: &[Membership]) -> Result<(Membership, usize), String> {
    for d in docs {
        d.verify()?;
        if d.collective != collective {
            return Err(format!("the membership of {:?}, this directory belongs to {collective:?}", d.collective));
        }
    }
    let installed = match Membership::load(home)? {
        Some(_) => match Membership::adopt(home, docs)? {
            (m, true) => m,
            (_, false) => return Err("nothing newer than the installed membership that an admin of it signed; hand over membership-history.jsonl".into()),
        },
        None => {
            docs.iter().min_by_key(|d| d.version).ok_or("no membership document")?.save(home)?;
            Membership::adopt(home, docs)?.0
        }
    };
    let moved = apply(home, &installed)?;
    Ok((installed, moved))
}

/// `borg mesh revoke`: the admin's new version, and its own logs quarantined at once (a line the
/// revoked node stamped after `now`, a clock running ahead, may already be here).
pub fn revoke(home: &Path, admin: &Identity, key: &str, now: i64) -> Result<(Membership, usize), String> {
    let mut m = Membership::load(home)?.ok_or("no membership")?;
    m.revoke(admin, key, now)?;
    m.save(home)?;
    let moved = apply(home, &m)?;
    Ok((m, moved))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_verify_only_for_their_line_and_key() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let me = Identity::load_or_create(&home).unwrap();
        let o = Origin::new(&me, &lines::hash("{\"a\":1}"), 5);
        assert!(o.verify("{\"a\":1}").is_ok());
        assert!(o.verify("{\"a\":2}").is_err(), "another line");
        let backdated = Origin { ms: 4, ..o.clone() };
        assert!(backdated.verify("{\"a\":1}").is_err(), "the time is signed");
        let log = home.join("lessons.jsonl");
        std::fs::write(&log, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        assert_eq!(stamp(&home, &me, std::slice::from_ref(&log), 7).unwrap(), 2);
        assert_eq!(stamp(&home, &me, &[log], 8).unwrap(), 0, "stamped once");
        // A goal's spend line that names no node is from before spend synced: it stays here.
        let spend = home.join("hive/g/spend.jsonl");
        std::fs::create_dir_all(spend.parent().unwrap()).unwrap();
        std::fs::write(
            &spend,
            "{\"drone\":\"d1\",\"usd\":1.0}\n{\"drone\":\"d1\",\"node\":\"\",\"usd\":2.0}\n{\"drone\":\"d1\",\"node\":\"n1\",\"usd\":3.0}\n",
        )
        .unwrap();
        assert_eq!(stamp(&home, &me, &[spend], 9).unwrap(), 1, "only the line that names its node");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn lesson(id: &str, version: u32, q: &str) -> String {
        serde_json::json!({"id": id, "origin_ms": 1, "question": q, "version": version}).to_string()
    }

    /// B holds L1 v3, then merges [v2 (bob), v4 (carol)]: the file reads v3, v2, v4. Carol is
    /// revoked, v4 is quarantined, and the conscience must ask v3 (as every other node does), not
    /// the v2 that is now last.
    #[test]
    fn quarantine_settles_so_the_version_never_goes_back() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (admin, bob, carol) = (
            Identity::load_or_create(&home).unwrap(),
            Identity::load_or_create(&home.join("bob")).unwrap(),
            Identity::load_or_create(&home.join("carol")).unwrap(),
        );
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &bob.public(), "member").unwrap();
        m.invite(&admin, &carol.public(), "member").unwrap();
        let log = home.join("lessons.jsonl");
        let (v3, v2, v4) = (lesson("L1", 3, "three"), lesson("L1", 2, "two"), lesson("L1", 4, "four"));
        std::fs::write(&log, format!("{v3}\n")).unwrap();
        lines::merge(&log, &[v2.clone(), v4.clone()]).unwrap();
        let o = |who: &Identity, l: &str, ms| Origin::new(who, &lines::hash(l), ms);
        append(&home, &[o(&admin, &v3, 1), o(&bob, &v2, 2), o(&carol, &v4, 50)]).unwrap();
        let asked = || borg::conscience::load(&log, None).into_iter().map(|c| c.question).collect::<Vec<_>>();
        assert_eq!(asked(), vec!["four".to_string()]);

        m.revoke(&admin, &carol.public(), 10).unwrap();
        assert_eq!(apply(&home, &m).unwrap(), 1);
        assert_eq!(asked(), vec!["three".to_string()], "the highest remaining version, not the last line");
        assert_eq!(apply_if_new(&home, &m).unwrap(), 0, "checked against this membership already");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A quarantine rewrites a hive log while drones append to it: both take the log's lock, so the
    /// rewrite waits for a writer and a writer for the rewrite, and the line appended is kept.
    #[test]
    fn quarantine_and_hive_appends_share_the_log_lock() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (admin, carol) = (Identity::load_or_create(&home).unwrap(), Identity::load_or_create(&home.join("carol")).unwrap());
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &carol.public(), "member").unwrap();
        let log = home.join("hive/g1/tasks.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let bad = r#"{"v":1,"ev":"claim","task":"t1","ms":2,"by":"carol"}"#;
        std::fs::write(&log, format!("{bad}\n")).unwrap();
        append(&home, &[Origin::new(&carol, &lines::hash(bad), 50)]).unwrap();
        m.revoke(&admin, &carol.public(), 10).unwrap();
        let wait = std::time::Duration::from_millis(300);
        // A writer holds the lock: the quarantine waits for it.
        let moved = borg::live::locked(&log, || {
            let (h, l, m) = (home.clone(), log.clone(), m.clone());
            let t = std::thread::spawn(move || quarantine(&h, &l, &m).unwrap());
            std::thread::sleep(wait);
            assert!(!t.is_finished(), "the rewrite waits for the writer");
            std::fs::OpenOptions::new()
                .append(true)
                .open(&log)
                .unwrap()
                .write_all(b"{\"v\":1,\"ev\":\"progress\",\"task\":\"t1\",\"ms\":3,\"by\":\"s\"}\n")
                .unwrap();
            t
        })
        .join()
        .unwrap();
        assert_eq!(moved, 1);
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().collect::<Vec<_>>(), [r#"{"v":1,"ev":"progress","task":"t1","ms":3,"by":"s"}"#]);
        // The hive's appends take the same lock.
        borg::live::locked(&log, || {
            let l = log.clone();
            let t = std::thread::spawn(move || borg::hive::board::append(&l, &[borg::hive::Event::new(borg::hive::Ev::Progress, "t1", "s", 4)]).unwrap());
            std::thread::sleep(wait);
            assert!(!t.is_finished(), "an append waits for a rewrite in progress");
            t
        })
        .join()
        .unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Carol's outcome line, stamped after her revocation, moves to quarantine and stops counting.
    #[test]
    fn a_revoked_key_s_outcomes_are_quarantined() {
        let home = std::env::temp_dir().join(format!("borg-mesh-origin-outcomes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let (admin, carol) = (Identity::load_or_create(&home).unwrap(), Identity::load_or_create(&home.join("carol")).unwrap());
        let mut m = Membership::found("work", &admin);
        m.invite(&admin, &carol.public(), "member").unwrap();
        std::fs::write(home.join("lessons.jsonl"), format!("{}\n", lesson("L1", 1, "one"))).unwrap();
        let line = |s: &str| {
            let x = borg::outcome::Shared {
                kind: borg::outcome::CHANGED.into(),
                s: s.into(),
                lessons: vec!["L1".into()],
                ms: 5,
                node: "n".into(),
                ..Default::default()
            };
            serde_json::to_string(&x).unwrap()
        };
        let (good, bad) = (line("t1"), line("t2"));
        let team = home.join(borg::outcome::TEAM_FILE);
        std::fs::write(&team, format!("{good}\n{bad}\n")).unwrap();
        append(&home, &[Origin::new(&admin, &lines::hash(&good), 1), Origin::new(&carol, &lines::hash(&bad), 50)]).unwrap();
        let l1 = [borg::conscience::Challenge { id: "L1".into(), ..Default::default() }];
        let changed = || borg::memory::derive(&borg::outcome::read_team(&home.join("lessons.jsonl")), &l1, None)["L1"].changed;
        assert_eq!(changed(), 2);
        m.revoke(&admin, &carol.public(), 10).unwrap();
        assert_eq!(apply(&home, &m).unwrap(), 1);
        assert_eq!(changed(), 1, "the revoked key's result no longer counts");
        assert!(std::fs::read_to_string(identity::dir(&home).join("quarantine.jsonl")).unwrap().contains("\"t2\""));
        let _ = std::fs::remove_dir_all(&home);
    }
}
