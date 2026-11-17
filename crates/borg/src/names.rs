//! Who people and machines are, as every view names them: the name book.
//!
//! An engineer is named by, in order: the `engineers` alias config (collective.rs `Aliases`), else the
//! git `user.name` learned for their email, else the full email (never cut at '@'). This node learns
//! its engineers' names in `<store>/names.json` (presence.rs `touch`, on a session's first hook); a
//! member's come with the machine record its drones carry (presence.rs `Machine::names`), each only for
//! an engineer that member's own drones name. A machine is named by its hostname (`Machine::host`), else,
//! for a node of a version without it, by the start of its key (msg.rs `short_node`).
//!
//! Names and hosts from other nodes are peer data: `clean` keeps them printable and short, and they are
//! only ever displayed, never put into a command to run.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::collective::{self, Aliases};
use crate::presence::Remote;

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
/// rename: a reader never sees half of it). An empty email or name records nothing.
///
/// # Errors
/// A write failure.
pub fn note(home: &Path, email: &str, name: &str) -> std::io::Result<()> {
    let (email, name) = (email.trim().to_lowercase(), clean(name));
    if email.is_empty() || name.is_empty() {
        return Ok(());
    }
    let mut all = local(home);
    if all.get(&email) == Some(&name) {
        return Ok(());
    }
    all.insert(email, name);
    std::fs::create_dir_all(home)?;
    let p = path(home);
    let tmp = p.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string(&all).unwrap_or_default())?;
    std::fs::rename(&tmp, &p).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
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

/// Who is who, as one view names them: engineers by email, machines by node key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    /// Lowercased email → git user.name.
    names: HashMap<String, String>,
    /// Node key → its machine's hostname, with the key's start where two nodes give one host.
    hosts: HashMap<String, String>,
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

    /// The book of `local` names and the machine records of `remote` (pure). A local name wins; a
    /// member's name counts only for an engineer one of that member's own drones names.
    pub fn from_parts(local: BTreeMap<String, String>, remote: &[Remote]) -> Book {
        let mut names: HashMap<String, String> = local.into_iter().map(|(e, n)| (e.trim().to_lowercase(), clean(&n))).filter(|(_, n)| !n.is_empty()).collect();
        let mut hosts: HashMap<String, String> = HashMap::new();
        for r in remote {
            let Some(m) = r.machine() else { continue };
            let host = clean(&m.host);
            if !host.is_empty() {
                hosts.entry(r.key.clone()).or_insert(host);
            }
            for n in &m.names {
                let email = n.email.trim().to_lowercase();
                let theirs = remote.iter().any(|o| o.key == r.key && o.p.engineer.trim().eq_ignore_ascii_case(&email));
                let name = clean(&n.name);
                if theirs && !name.is_empty() {
                    names.entry(email).or_insert(name);
                }
            }
        }
        // Two nodes giving one host stay apart by their keys.
        let mut count: HashMap<String, usize> = HashMap::new();
        for h in hosts.values() {
            *count.entry(h.clone()).or_default() += 1;
        }
        for (key, h) in &mut hosts {
            if count[h.as_str()] > 1 {
                *h = format!("{h} {}", crate::msg::short_node(key));
            }
        }
        Book { names, hosts }
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

    /// A machine for display: its hostname, else the start of its key (an older node's); `here` for
    /// this node (the empty key, as top.rs names a local drone's).
    pub fn host(&self, node: &str) -> String {
        if node.is_empty() {
            return "here".into();
        }
        self.hosts.get(node).cloned().unwrap_or_else(|| crate::msg::short_node(node))
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
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1, "no temp file left behind");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn this_machine_has_a_hostname() {
        let h = hostname();
        assert!(!h.is_empty() && h.chars().count() <= CHARS, "{h:?}");
    }
}
