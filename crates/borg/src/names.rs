//! Who people and machines are, as every view names them: the name book.
//!
//! An engineer is named by, in order: the `engineers` alias config (collective.rs `Aliases`), else the
//! git `user.name` learned for their email, else the full email (never cut at '@'). This node learns
//! its engineers' names in `<store>/names.json` (presence.rs `touch`, on a session's first hook); a
//! member's come with the newest machine record its drones carry (presence.rs `Machine::names`), each
//! only for an engineer that member's own drones name and only while no other member names them
//! otherwise. A machine is named by its hostname (`Machine::host`), with the start of its key beside it
//! where the host cannot stand alone (another node's, a word a view names this machine by, or another
//! node's short key), else, for a node of a version without it, by the start of its key (msg.rs
//! `short_node`).
//!
//! Names and hosts from other nodes are peer data: `clean` keeps them printable and short, and they are
//! only ever displayed, never put into a command to run.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::collective::{self, Aliases};
use crate::presence::{Machine, Remote};

/// Display and guard: the most characters a name or a host keeps (`clean`).
pub const CHARS: usize = 64;

/// `s` as a label may show it: no bidi characters (a peer's value cannot reorder the text around it),
/// control characters as spaces, whitespace collapsed, at most `CHARS` characters.
pub fn clean(s: &str) -> String {
    let kept: String = s.chars().filter(|c| !bidi(*c)).map(|c| if c.is_control() { ' ' } else { c }).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(CHARS).collect::<String>().trim_end().to_string()
}

/// The Unicode bidi controls (marks, embeddings, overrides, isolates).
fn bidi(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `<store>/names.json`, `{"<email>": "<git user.name>"}`: the names of the engineers whose sessions
/// ran here. Local; a member learns them from this node's machine record.
pub fn path(home: &Path) -> PathBuf {
    home.join("names.json")
}

/// The names this node learned (`path`); empty when none.
pub fn local(home: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Records `name` for `email`, written only when it changes (one small write to a temp file, then a
/// rename: a reader never sees half of it). The read, change and write run under the file's lock
/// (live.rs `locked`), so two sessions' first hooks recording different emails at once both stay. An
/// empty email or name records nothing.
///
/// # Errors
/// A write failure.
pub fn note(home: &Path, email: &str, name: &str) -> std::io::Result<()> {
    let (email, name) = (email.trim().to_lowercase(), clean(name));
    if email.is_empty() || name.is_empty() || local(home).get(&email) == Some(&name) {
        return Ok(());
    }
    std::fs::create_dir_all(home)?;
    let p = path(home);
    crate::live::locked(&p, || {
        // Anew under the lock: another writer may have added an engineer since the look above.
        let mut all = local(home);
        if all.get(&email) == Some(&name) {
            return Ok(());
        }
        all.insert(email, name);
        let tmp = p.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string(&all).unwrap_or_default())?;
        std::fs::rename(&tmp, &p).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    })
}

/// This machine's hostname, cleaned; empty when the system gives none.
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname writes at most that many bytes.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    clean(&String::from_utf8_lossy(&buf[..end]))
}

/// The words a view names this machine by (top.rs `here`, borg web's hand-offs `this one`): a peer's
/// host that reads as one of them is shown with its key (`Book::host`).
const RESERVED: [&str; 2] = ["here", "this one"];

/// Display: the characters of a node key a table cell keeps beside a host that cannot stand alone
/// (`Book::host_cell`), where the whole `msg::short_node` would leave no room for the host.
pub const CELL_KEY: usize = 6;

/// Who is who, as one view names them: engineers by email, machines by node key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    /// Lowercased email → git user.name.
    names: HashMap<String, String>,
    /// Node key → its machine's hostname, from that node's newest machine record.
    hosts: HashMap<String, String>,
    /// The keys whose host cannot stand alone, shown with the key's start (`host`): another node gives
    /// the same host (in any case), or it is a word views name this machine by (`RESERVED`) or another
    /// node's short key.
    keyed: HashSet<String>,
}

impl Book {
    /// The book of the store at `home` as of `now`: its learned names and the remote machine records.
    pub fn load(home: &Path, now: i64) -> Book {
        Book::of(home, &crate::presence::remote(home, now))
    }

    /// `load` with the remote records already read.
    pub fn of(home: &Path, remote: &[Remote]) -> Book {
        Book::from_parts(local(home), remote)
    }

    /// The book of `local` names and the machine records of `remote` (pure). Each node counts with its
    /// newest machine record: `remote` comes by key and session, and a session's record stays as it was
    /// while its presence does not change. A local name wins. A member's name counts only for an
    /// engineer one of that member's own drones names, and only while no other member names that
    /// engineer otherwise: which engineer a member's drones name is that member's own claim, so where two
    /// members disagree the email shows.
    pub fn from_parts(local: BTreeMap<String, String>, remote: &[Remote]) -> Book {
        let mut names: HashMap<String, String> = local.into_iter().map(|(e, n)| (e.trim().to_lowercase(), clean(&n))).filter(|(_, n)| !n.is_empty()).collect();
        let mut newest: Vec<(&Remote, Machine)> = remote.iter().filter_map(|r| r.machine().map(|m| (r, m))).collect();
        newest.sort_by(|(a, _), (b, _)| (&a.key, b.p.ms).cmp(&(&b.key, a.p.ms)));
        newest.dedup_by(|(a, _), (b, _)| a.key == b.key);
        let mut hosts: HashMap<String, String> = HashMap::new();
        // Lowercased email → the name the members give it, None once two give different ones.
        let mut given: HashMap<String, Option<String>> = HashMap::new();
        for (r, m) in &newest {
            let host = clean(&m.host);
            if !host.is_empty() {
                hosts.insert(r.key.clone(), host);
            }
            for n in &m.names {
                let (email, name) = (n.email.trim().to_lowercase(), clean(&n.name));
                let theirs = remote.iter().any(|o| o.key == r.key && o.p.engineer.trim().eq_ignore_ascii_case(&email));
                if theirs && !name.is_empty() {
                    given.entry(email).and_modify(|held| *held = held.take().filter(|h| *h == name)).or_insert(Some(name));
                }
            }
        }
        for (email, name) in given {
            if let Some(name) = name {
                names.entry(email).or_insert(name);
            }
        }
        // A host two nodes give, a reserved word or another node's short key keeps the key beside it.
        let mut count: HashMap<String, usize> = HashMap::new();
        for h in hosts.values() {
            *count.entry(h.to_lowercase()).or_default() += 1;
        }
        let shorts: HashSet<String> = remote.iter().map(|r| crate::msg::short_node(&r.key).to_lowercase()).collect();
        let keyed = hosts
            .iter()
            .filter(|(_, h)| {
                let h = h.to_lowercase();
                count[&h] > 1 || RESERVED.contains(&h.as_str()) || shorts.contains(&h)
            })
            .map(|(k, _)| k.clone())
            .collect();
        Book { names, hosts, keyed }
    }

    /// An engineer for display (`display_with` this machine's aliases).
    pub fn display(&self, email: &str) -> String {
        self.display_with(collective::aliases(), email)
    }

    /// An engineer for display: the alias's name, else the git user.name learned, else the email as
    /// written; empty stays empty (unknown engineer).
    pub fn display_with(&self, aliases: &Aliases, email: &str) -> String {
        if let Some(n) = aliases.name(email) {
            return n.to_string();
        }
        self.names.get(&email.trim().to_lowercase()).cloned().unwrap_or_else(|| email.to_string())
    }

    /// A machine for display: its hostname, followed by the start of its key where the host cannot
    /// stand alone (`keyed`), else the start of its key (an older node's); `here` for this node (the
    /// empty key, as top.rs names a local drone's).
    pub fn host(&self, node: &str) -> String {
        if node.is_empty() {
            return "here".into();
        }
        match self.hosts.get(node) {
            Some(h) if self.keyed.contains(node) => format!("{h} {}", crate::msg::short_node(node)),
            Some(h) => h.clone(),
            None => crate::msg::short_node(node),
        }
    }

    /// `host` in a table column of `width` characters (top.rs and top_live.rs `ON`), cut with `…`. A
    /// host that cannot stand alone keeps the first `CELL_KEY` characters of its key whole and gives up
    /// the end of the host instead, so two machines of one long host stay apart in the column.
    pub fn host_cell(&self, node: &str, width: usize) -> String {
        let cut = |s: &str, n: usize| {
            if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) }
        };
        match self.hosts.get(node) {
            Some(h) if self.keyed.contains(node) => {
                let key: String = node.chars().take(CELL_KEY).collect();
                format!("{} {key}", cut(h, width.saturating_sub(CELL_KEY + 1)))
            }
            _ => cut(&self.host(node), width),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::{Machine, Named, Presence};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-names-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn remote(key: &str, engineer: &str, machine: Option<Machine>) -> Remote {
        let p = Presence {
            session: format!("s-{key}"),
            harness: "claude".into(),
            engineer: engineer.into(),
            repo: "app".into(),
            collective: "team".into(),
            prompt: String::new(),
            files: vec![],
            event: "prompt".into(),
            ms: 1,
            started: 1,
            pid: None,
        };
        Remote { p, key: key.into(), sig: String::new(), machine: machine.map(|m| serde_json::to_string(&m).unwrap()), msig: String::new() }
    }

    fn machine<'a>(host: &'a str, names: &[(&'a str, &'a str)]) -> Machine<'a> {
        Machine { host: host.into(), names: names.iter().map(|(e, n)| Named { email: (*e).into(), name: (*n).into() }).collect(), ..Machine::default() }
    }

    /// The alias config first, then the git user.name learned, then the whole email.
    #[test]
    fn an_engineer_is_named_by_alias_then_git_name_then_email() {
        let aliases = Aliases::new(&[collective::Engineer { name: "Ann".into(), emails: vec!["ann@example.com".into()] }]);
        let local = BTreeMap::from([("ann@example.com".to_string(), "Ann Example".to_string()), ("bob@example.com".to_string(), "Bob Example".to_string())]);
        let book = Book::from_parts(local, &[]);
        assert_eq!(book.display_with(&aliases, "Ann@Example.com"), "Ann", "the alias wins");
        assert_eq!(book.display_with(&aliases, "bob@example.com"), "Bob Example", "the git name");
        assert_eq!(book.display_with(&aliases, "git@carol.example"), "git@carol.example", "the whole email, never cut at '@'");
        assert_eq!(book.display_with(&aliases, ""), "");
    }

    /// A member's names and host come from its machine record; a name only for its own drones'
    /// engineers, a local one wins, and an older node's record (no host) falls back to its key.
    #[test]
    fn members_name_their_engineers_and_machines() {
        let local = BTreeMap::from([("ann@example.com".to_string(), "Ann Example".to_string())]);
        let rs = [
            remote("k1aaaaaaaaaaaaaa", "bob@example.com", Some(machine("bob-laptop", &[("bob@example.com", "Bob Example"), ("eve@example.com", "Not Eve")]))),
            remote("k2bbbbbbbbbbbbbb", "ann@example.com", Some(machine("ann-desk", &[("ann@example.com", "Someone Else")]))),
            remote("k3cccccccccccccc", "old@example.com", None),
        ];
        let book = Book::from_parts(local, &rs);
        let none = Aliases::default();
        assert_eq!(book.display_with(&none, "bob@example.com"), "Bob Example");
        assert_eq!(book.display_with(&none, "eve@example.com"), "eve@example.com", "no drone of that member names her");
        assert_eq!(book.display_with(&none, "ann@example.com"), "Ann Example", "the local name wins");
        assert_eq!((book.host("k1aaaaaaaaaaaaaa"), book.host("k2bbbbbbbbbbbbbb")), ("bob-laptop".into(), "ann-desk".into()));
        assert_eq!(book.host("k3cccccccccccccc"), "k3cccccccccc", "an older node: its key's start");
        assert_eq!(book.host(""), "here");
        // Two nodes giving one host keep their keys apart.
        let twins = [remote("k1aaaaaaaaaaaaaa", "a@x", Some(machine("box", &[]))), remote("k2bbbbbbbbbbbbbb", "b@x", Some(machine("box", &[])))];
        let book = Book::from_parts(BTreeMap::new(), &twins);
        assert_eq!((book.host("k1aaaaaaaaaaaaaa"), book.host("k2bbbbbbbbbbbbbb")), ("box k1aaaaaaaaaa".into(), "box k2bbbbbbbbbb".into()));
    }

    /// A drone of `key`'s session `session`, last heard at `ms`, with that session's machine record.
    fn record(key: &str, session: &str, ms: i64, engineer: &str, m: Machine) -> Remote {
        let mut r = remote(key, engineer, Some(m));
        (r.p.session, r.p.ms) = (session.into(), ms);
        r
    }

    /// Which engineer a member's drones name is that member's claim: where two members name one email
    /// otherwise, neither name shows, whichever key comes first; where they agree, the name does.
    #[test]
    fn two_members_naming_one_engineer_otherwise_show_the_email() {
        let none = Aliases::default();
        let ann = |key: &str, name: &'static str| remote(key, "ann@example.com", Some(machine("box", &[("ann@example.com", name)])));
        for rs in [
            [ann("k1aaaaaaaaaaaaaa", "Bob Example"), ann("k2bbbbbbbbbbbbbb", "Ann Example")],
            [ann("k1aaaaaaaaaaaaaa", "Ann Example"), ann("k2bbbbbbbbbbbbbb", "Bob Example")],
        ] {
            let book = Book::from_parts(BTreeMap::new(), &rs);
            assert_eq!(book.display_with(&none, "ann@example.com"), "ann@example.com");
        }
        let agree = [ann("k1aaaaaaaaaaaaaa", "Ann Example"), ann("k2bbbbbbbbbbbbbb", "Ann Example")];
        assert_eq!(Book::from_parts(BTreeMap::new(), &agree).display_with(&none, "ann@example.com"), "Ann Example");
        let local = BTreeMap::from([("ann@example.com".to_string(), "Ann Here".to_string())]);
        let differ = [ann("k1aaaaaaaaaaaaaa", "Bob Example"), ann("k2bbbbbbbbbbbbbb", "Ann Example")];
        assert_eq!(Book::from_parts(local, &differ).display_with(&none, "ann@example.com"), "Ann Here", "a local name still wins");
    }

    /// A node's host and names come from its newest record, though an older session's record of it
    /// comes first in `remote`'s order (key, then session).
    #[test]
    fn each_node_counts_with_its_newest_machine_record() {
        let rs = [
            record("k1aaaaaaaaaaaaaa", "0aaa", 1, "ann@example.com", machine("old-name", &[("ann@example.com", "Ann Typo")])),
            record("k1aaaaaaaaaaaaaa", "bbbb", 2, "ann@example.com", machine("new-name", &[("ann@example.com", "Ann Example")])),
        ];
        let book = Book::from_parts(BTreeMap::new(), &rs);
        assert_eq!((book.host("k1aaaaaaaaaaaaaa"), book.display_with(&Aliases::default(), "ann@example.com")), ("new-name".into(), "Ann Example".into()));
    }

    /// A peer's host that reads as this machine in a view (`here`, `this one`) or as another node's short
    /// key never stands alone: its key's start goes beside it.
    #[test]
    fn a_host_that_reads_as_this_machine_or_another_key_keeps_its_key() {
        for host in ["here", "This One", "k2bbbbbbbbbb"] {
            let rs = [remote("k1aaaaaaaaaaaaaa", "a@x", Some(machine(host, &[]))), remote("k2bbbbbbbbbbbbbb", "b@x", None)];
            let book = Book::from_parts(BTreeMap::new(), &rs);
            assert_eq!(book.host("k1aaaaaaaaaaaaaa"), format!("{host} k1aaaaaaaaaa"), "{host}");
        }
        let rs = [remote("k1aaaaaaaaaaaaaa", "a@x", Some(machine("Box", &[]))), remote("k2bbbbbbbbbbbbbb", "b@x", Some(machine("box", &[])))];
        assert_eq!(Book::from_parts(BTreeMap::new(), &rs).host("k1aaaaaaaaaaaaaa"), "Box k1aaaaaaaaaa", "twins in any case");
        let rs = [remote("k1aaaaaaaaaaaaaa", "a@x", Some(machine("bob-laptop", &[])))];
        assert_eq!(Book::from_parts(BTreeMap::new(), &rs).host("k1aaaaaaaaaaaaaa"), "bob-laptop", "a host of its own stands alone");
    }

    /// A table cell keeps the key of a host that needs it, however long the host: the host gives way.
    #[test]
    fn a_cell_keeps_the_key_of_twin_hosts() {
        let twins = [
            remote("k1aaaaaaaaaaaaaa", "a@x", Some(machine("MacBook-Pro.local", &[]))),
            remote("k2bbbbbbbbbbbbbb", "b@x", Some(machine("MacBook-Pro.local", &[]))),
            remote("k3cccccccccccccc", "c@x", Some(machine("a-long-hostname.example", &[]))),
        ];
        let book = Book::from_parts(BTreeMap::new(), &twins);
        let cells = ["k1aaaaaaaaaaaaaa", "k2bbbbbbbbbbbbbb", "k3cccccccccccccc", ""].map(|k| book.host_cell(k, 14));
        assert_eq!(cells, ["MacBoo… k1aaaa", "MacBoo… k2bbbb", "a-long-hostna…", "here"]);
        assert!(cells.iter().all(|c| c.chars().count() <= 14));
    }

    /// A peer's name or host cannot carry control or bidi characters, nor run long.
    #[test]
    fn peer_values_are_cleaned() {
        assert_eq!(clean("Ann\u{202E}moc.evil\n\tExample\u{7}"), "Annmoc.evil Example");
        assert_eq!(clean(&"x".repeat(200)).chars().count(), CHARS);
        let rs = [remote("k1aaaaaaaaaaaaaa", "bob@example.com", Some(machine("bad\u{2066}host\r\n", &[("bob@example.com", "\u{202D}Bob")])))];
        let book = Book::from_parts(BTreeMap::new(), &rs);
        assert_eq!((book.display_with(&Aliases::default(), "bob@example.com"), book.host("k1aaaaaaaaaaaaaa")), ("Bob".into(), "badhost".into()));
    }

    /// Learned names are written once and only when they change.
    #[test]
    fn a_learned_name_is_written_when_it_changes() {
        let home = tmp("note");
        note(&home, "Ann@Example.com", "Ann Example").unwrap();
        let first = std::fs::metadata(path(&home)).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        note(&home, "ann@example.com", "Ann Example").unwrap();
        assert_eq!(std::fs::metadata(path(&home)).unwrap().modified().unwrap(), first, "unchanged: no write");
        note(&home, "ann@example.com", "Ann B. Example").unwrap();
        note(&home, "", "Nobody").unwrap();
        note(&home, "x@example.com", "  ").unwrap();
        assert_eq!(local(&home), BTreeMap::from([("ann@example.com".to_string(), "Ann B. Example".to_string())]));
        let left: Vec<String> = std::fs::read_dir(&home).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(left.iter().all(|f| !f.ends_with(".tmp")), "no temp file left behind: {left:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Sessions recording different engineers at once all stay: each change is made under the lock.
    #[test]
    fn engineers_noted_at_once_all_stay() {
        let home = tmp("note-race");
        let (threads, each) = (8, 25);
        let start = std::sync::Barrier::new(threads);
        std::thread::scope(|s| {
            for t in 0..threads {
                let (home, start) = (&home, &start);
                s.spawn(move || {
                    start.wait();
                    for i in 0..each {
                        note(home, &format!("e{t}-{i}@example.com"), &format!("Engineer {t} {i}")).unwrap();
                    }
                });
            }
        });
        assert_eq!(local(&home).len(), threads * each);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn this_machine_has_a_hostname() {
        let h = hostname();
        assert!(!h.is_empty() && h.chars().count() <= CHARS, "{h:?}");
    }
}
