//! What the mesh daemon (borg-mesh) leaves in a store for other readers, typed here so borg web reads it
//! without depending on borg-mesh: its session log, and where a synced line came from.
//!
//! `<store>/mesh/sessions.jsonl` (`sessions_path`) is append-only, one [`Session`] per session of this
//! node with a member that moved lines, or that failed where the last one with that member had completed
//! (borg-mesh daemon.rs decides): an idle round writes nothing. Local, never synced. Bounded by time, not
//! count: `note` drops the lines older than msg.rs `TTL_MS` once the first line is a day older than that,
//! so the file is rewritten at most once a day and never holds more than eight days. Only borg web's Cube
//! view reads it, and only that view watches it.
//!
//! `<store>/mesh/origins.jsonl` (`origins_path`) is borg-mesh origin.rs's: one signed record per line
//! hash. [`OriginRef`] reads the three fields a reader that does not verify them needs, and
//! [`line_hash`] is the hash a record names its line by.
//!
//! The paths, the line hash and the hex a record writes it in are defined here once: borg-mesh's
//! (identity.rs `dir`, origin.rs `path`, lines.rs `hash`, lib.rs `from_lower_hex`) are these, so the
//! two never read the files differently.

use std::borrow::Cow;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clock::DAY_MS;
use crate::msg::TTL_MS;

/// The mesh's directory in a store (borg-mesh identity.rs `dir`).
#[must_use]
pub fn dir(home: &Path) -> PathBuf {
    home.join("mesh")
}

/// The session log of the store `home`.
#[must_use]
pub fn sessions_path(home: &Path) -> PathBuf {
    dir(home).join("sessions.jsonl")
}

/// The origin records of the store `home` (borg-mesh origin.rs `path`).
#[must_use]
pub fn origins_path(home: &Path) -> PathBuf {
    dir(home).join("origins.jsonl")
}

/// One session of this node with a member, as the session log holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session<'a> {
    /// When it ended, on this node's clock.
    pub ms: i64,
    /// The member's node key.
    #[serde(borrow)]
    pub peer: Cow<'a, str>,
    /// True when this node dialled, false when the member did.
    pub dial: bool,
    /// Lines this node sent, and newly stored (borg-mesh sync.rs `Report`).
    #[serde(default)]
    pub sent: u64,
    #[serde(default)]
    pub received: u64,
    /// Why it failed, one line; absent when it completed.
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub error: Option<Cow<'a, str>>,
}

impl Session<'_> {
    /// Lines it moved, both ways.
    #[must_use]
    pub const fn moved(&self) -> u64 {
        self.sent.saturating_add(self.received)
    }
}

/// The sessions of a session log's `text`, in file order; a malformed line is skipped.
pub fn sessions(text: &str) -> impl Iterator<Item = Session<'_>> {
    text.lines().filter_map(|l| serde_json::from_str(l).ok())
}

/// Appends `s` to the session log of `home` at `now`.
///
/// When its first line is older than `TTL_MS` and a day, or does not read as a session (a write torn
/// by a crash, or a later shape), the log is first rewritten with only the sessions within `TTL_MS` (a
/// temp file, then a rename): a reader never sees half of it, and the check reads one line.
///
/// # Errors
///
/// When the log cannot be read, written or renamed.
pub fn note(home: &Path, s: &Session<'_>, now: i64) -> std::io::Result<()> {
    let path = sessions_path(home);
    let line = serde_json::to_string(s)?;
    let due = std::fs::File::open(&path).is_ok_and(|f| {
        let mut l = String::new();
        match std::io::BufReader::new(f).read_line(&mut l) {
            Ok(0) | Err(_) => false,
            Ok(_) => serde_json::from_str::<Session>(l.trim_end()).map_or(true, |s| s.ms < now - TTL_MS - DAY_MS),
        }
    });
    if due {
        let text = std::fs::read_to_string(&path)?;
        let mut kept: String = text.lines().filter(|l| serde_json::from_str::<Session>(l).is_ok_and(|s| s.ms >= now - TTL_MS)).fold(
            String::with_capacity(text.len()),
            |mut out, l| {
                out.push_str(l);
                out.push('\n');
                out
            },
        );
        kept.push_str(&line);
        kept.push('\n');
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, kept)?;
        return std::fs::rename(&tmp, &path);
    }
    crate::learn::append_line(&path, &line)
}

/// A record of origins.jsonl, read for the line it names, who stamped it and when; its signature is not
/// read (only borg-mesh verifies, on receipt).
#[derive(Debug, Clone, Deserialize)]
pub struct OriginRef<'a> {
    /// The line's hash, lower hex (`line_hash`).
    #[serde(borrow)]
    pub h: Cow<'a, str>,
    /// The node that first stamped the line.
    #[serde(borrow)]
    pub key: Cow<'a, str>,
    /// When it first saw the line, on its clock.
    pub ms: i64,
}

impl OriginRef<'_> {
    /// The record of origins.jsonl `line` holds, if it is one.
    #[must_use]
    pub fn parse(line: &str) -> Option<OriginRef<'_>> {
        serde_json::from_str(line).ok()
    }

    /// The hash `h` names, if it is 64 lowercase hex digits (`lower_hex`), as borg-mesh writes it.
    #[must_use]
    pub fn hash(&self) -> Option<[u8; 32]> {
        lower_hex(&self.h)
    }
}

/// The bytes of exactly `2 · N` lowercase hex digits, the form borg-mesh writes hashes and signatures
/// in; `None` for any other text.
#[must_use]
pub fn lower_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != 2 * N {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (o, [hi, lo]) in out.iter_mut().zip(b.as_chunks::<2>().0) {
        *o = (nibble(*hi)? << 4) | nibble(*lo)?;
    }
    Some(out)
}

/// The hash a synced line is named by (borg-mesh lines.rs `hash`): blake3 of its text, without its line
/// end.
#[must_use]
#[inline]
pub fn line_hash(line: &str) -> [u8; 32] {
    *blake3::hash(line.as_bytes()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fmt::Write as _;

    fn session(ms: i64, peer: &str) -> Session<'static> {
        Session { ms, peer: Cow::Owned(peer.into()), dial: true, sent: 1, received: 2, error: None }
    }

    /// Appended in order; once the first line is a day past the messages' lifetime the log keeps only
    /// what is within it, and reads back the same records.
    #[test]
    fn the_session_log_appends_and_drops_what_outlived_the_window() {
        let home = std::env::temp_dir().join(format!("borg-mesh-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let now = 100 * DAY_MS;
        let old = now - TTL_MS - DAY_MS - 1;
        note(&home, &session(old, "a"), now).unwrap();
        note(&home, &Session { error: Some("no answer".into()), ..session(now - TTL_MS + 5, "b") }, old + 1).unwrap();
        let text = std::fs::read_to_string(sessions_path(&home)).unwrap();
        assert_eq!(sessions(&text).count(), 2, "not rewritten while the first line was young: {text}");
        note(&home, &session(now, "c"), now).unwrap();
        let text = std::fs::read_to_string(sessions_path(&home)).unwrap();
        let held: Vec<Session> = sessions(&text).collect();
        assert_eq!(held.iter().map(|s| s.peer.as_ref()).collect::<Vec<_>>(), ["b", "c"], "{text}");
        assert_eq!(held[0].error.as_deref(), Some("no answer"));
        assert_eq!(held[1].moved(), 3);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A first line that reads as no session (a torn write) does not keep the log from being pruned: the
    /// next note rewrites it with only the sessions of the window.
    #[test]
    fn a_torn_first_line_is_dropped_by_the_next_note() {
        let home = std::env::temp_dir().join(format!("borg-mesh-sessions-torn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let now = 100 * DAY_MS;
        std::fs::create_dir_all(dir(&home)).unwrap();
        let old = serde_json::to_string(&session(now - TTL_MS - 1, "a")).unwrap();
        std::fs::write(sessions_path(&home), format!("{{\"ms\":1,\"pe\n{old}\n")).unwrap();
        note(&home, &session(now, "b"), now).unwrap();
        let text = std::fs::read_to_string(sessions_path(&home)).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert_eq!(sessions(&text).map(|s| s.peer.into_owned()).collect::<Vec<_>>(), ["b"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Lowercase hex reads back exactly the bytes, and only lowercase hex of the length asked.
    #[test]
    fn lower_hex_reads_only_lowercase_of_its_length() {
        assert_eq!(lower_hex::<2>("0aff"), Some([0x0a, 0xff]));
        for bad in ["0AFF", "0af", "0aff0", "0agf"] {
            assert_eq!(lower_hex::<2>(bad), None, "{bad}");
        }
    }

    /// The narrow origin read names the line by the hash borg-mesh gives it.
    #[test]
    fn an_origin_names_its_line_by_hash() {
        let line = r#"{"id":"L1","version":1}"#;
        let hex = line_hash(line).iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        let rec = format!(r#"{{"h":"{hex}","key":"k","ms":7,"sig":"00"}}"#);
        let o = OriginRef::parse(&rec).unwrap();
        assert_eq!((o.hash(), o.key.as_ref(), o.ms), (Some(line_hash(line)), "k", 7));
        assert_eq!(OriginRef::parse(r#"{"h":"zz","key":"k","ms":1}"#).unwrap().hash(), None);
    }
}
