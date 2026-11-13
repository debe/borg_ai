//! A file read whole once and then followed: the mesh's log snapshots and origin index (borg-mesh
//! cache.rs) and the usage rollups borg-web shows (stats.rs `Rollups`).
//!
//! After the whole read, one stat per use says whether the file changed: unchanged, nothing is read;
//! grown in place (the logs are appended to), only what was appended is read; anything else (a rename
//! puts a new file in, a same-size write moves the ctime) reads it whole again. So a reader's cost grows
//! with what changed, not with the file.

use std::fs::{File, Metadata};
use std::io::{Read as _, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// How many bytes before the point a file was parsed to must read the same for an extension to be
/// trusted (`follow`). Method: one page, read with the tail anyway; a rewrite that grows the file in
/// place and keeps the page before the old end byte for byte is not one this store's writers make
/// (they append, or rename a new file in, which the inode tells).
pub const GUARD: usize = 4096;

/// A file as a stat saw it: which file (device and inode), its size, its mtime and its ctime. A missing
/// file is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mark {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: Option<SystemTime>,
    /// The inode's change time (seconds, nanoseconds): the kernel moves it at every write, truncation and
    /// mtime change, and no caller can set it, so a same-size write that puts the mtime back still shows.
    ctime: (i64, i64),
}

impl Mark {
    /// `path` as one stat sees it now (the default when it is missing): whether a file changed since an
    /// earlier mark, without reading it (vectors.rs `Embedder::shared` keys the loaded model by it).
    pub fn at(path: &Path) -> Mark {
        std::fs::metadata(path).map(|m| Mark::of(&m)).unwrap_or_default()
    }

    fn of(m: &Metadata) -> Mark {
        use std::os::unix::fs::MetadataExt;
        Mark { dev: m.dev(), ino: m.ino(), len: m.len(), mtime: m.modified().ok(), ctime: (m.ctime(), m.ctime_nsec()) }
    }

    fn same_file(&self, other: &Mark) -> bool {
        self.ino != 0 && (self.dev, self.ino) == (other.dev, other.ino)
    }
}

/// What a reader of a followed file holds of it (`Follow::held`).
pub struct Held<'a> {
    /// The stat it was read under.
    pub mark: Mark,
    /// How many bytes it holds.
    pub end: usize,
    /// Where its unterminated last line starts (`end` when it ends with a newline): an extension reads
    /// from here, as that line may have been half written.
    pub parsed: usize,
    /// The bytes before `parsed`, at most `GUARD` of them.
    pub guard: &'a [u8],
}

/// What a followed file holds now (`follow`).
pub enum Read {
    /// What was read: nothing changed.
    Same,
    /// The text from the held `parsed` on, under the new stat.
    Tail(Mark, String),
    /// The whole text (empty when the file is missing or not UTF-8, as `read_to_string` gives nothing).
    Whole(Mark, String),
}

/// A file read whole once and then followed (`follow`).
pub trait Follow: Clone {
    /// What a whole read of the file gives.
    fn whole(mark: Mark, text: String) -> Self;
    /// What it holds, for `follow` to read on from.
    fn held(&self) -> Held<'_>;
    /// `tail`, the text from `held().parsed` on, read under `mark`.
    fn extend(&mut self, mark: Mark, tail: String);
}

/// Reads of `path` past `held`: nothing when one stat says it is unchanged, the appended tail when it is
/// the same file grown past what was read and the `GUARD` bytes before `held.parsed` still read the same,
/// else all of it (a same-size write with a new ctime among them).
/// The stat recorded is the open file's, taken before it is read, and the read stops at the size it saw:
/// what is appended during the read is the next call's tail, so what is held always ends where its stat
/// says.
pub fn follow(path: &Path, held: Held<'_>) -> Read {
    let now = Mark::at(path);
    if held.mark == now {
        return Read::Same;
    }
    let Ok(mut f) = File::open(path) else { return Read::Whole(Mark::default(), String::new()) };
    let (mark, len) = stat(&f);
    if held.mark.same_file(&mark)
        && mark.len > held.end as u64
        && let Some(tail) = tail(&mut f, &held, len)
    {
        return Read::Tail(mark, tail);
    }
    let text = if f.seek(SeekFrom::Start(0)).is_ok() { read_text(&mut f, len) } else { String::new() };
    Read::Whole(mark, text)
}

/// The whole text of `path` and the stat it was read under (empty when it is missing or not UTF-8), up to
/// the size that stat saw.
pub fn whole(path: &Path) -> (Mark, String) {
    let Ok(mut f) = File::open(path) else { return (Mark::default(), String::new()) };
    let (mark, len) = stat(&f);
    (mark, read_text(&mut f, len))
}

/// The open file's stat, and how far to read it: the size the stat saw (to the end, when it failed).
fn stat(f: &File) -> (Mark, u64) {
    f.metadata().map_or((Mark::default(), u64::MAX), |m| (Mark::of(&m), m.len()))
}

/// The first `len` bytes of `f`, read from its start, as text, or nothing, as `read_to_string(..)
/// .unwrap_or_default()` reads it. Sized for `len` up front: a `Take` gives no size hint, and grown by
/// doubling the text would allocate twice its size.
fn read_text(f: &mut File, len: u64) -> String {
    let mut text = String::new();
    // Unsized when the stat failed (`len` is `u64::MAX`): the reservation fails and the read grows it.
    let _ = text.try_reserve_exact(usize::try_from(len).unwrap_or(0));
    if f.take(len).read_to_string(&mut text).is_err() {
        text.clear();
    }
    text
}

/// The text of `f` from `h.parsed` up to offset `end`, if the guard bytes before it are unchanged and it
/// is UTF-8.
fn tail(f: &mut File, h: &Held<'_>, end: u64) -> Option<String> {
    let from = h.parsed.checked_sub(h.guard.len())?;
    f.seek(SeekFrom::Start(from as u64)).ok()?;
    let mut bytes = Vec::new();
    f.take(end.saturating_sub(from as u64)).read_to_end(&mut bytes).ok()?;
    if !bytes.starts_with(h.guard) {
        return None;
    }
    String::from_utf8(bytes.split_off(h.guard.len())).ok()
}

/// The last `GUARD` bytes of `before` (the text up to a parse point), or of `kept` followed by it when
/// `before` is shorter: the guard of a reader that keeps no text (borg-mesh origin.rs `Index`, stats.rs
/// `Rollups`).
pub fn guard_after(kept: &[u8], before: &[u8]) -> Vec<u8> {
    let take = |b: &[u8]| b[b.len().saturating_sub(GUARD)..].to_vec();
    if before.len() >= GUARD {
        return take(before);
    }
    take(&[kept, before].concat())
}

/// A new generation for a whole read: a reader whose generation is unchanged holds the same prefix.
pub fn generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// The simplest follower: the text as read, parsed up to its last newline.
    #[derive(Clone)]
    struct Text {
        mark: Mark,
        text: String,
    }

    impl Text {
        fn parsed(&self) -> usize {
            self.text.rfind('\n').map_or(0, |i| i + 1)
        }
    }

    impl Follow for Text {
        fn whole(mark: Mark, text: String) -> Text {
            Text { mark, text }
        }

        fn held(&self) -> Held<'_> {
            let parsed = self.parsed();
            Held { mark: self.mark, end: self.text.len(), parsed, guard: &self.text.as_bytes()[parsed.saturating_sub(GUARD)..parsed] }
        }

        fn extend(&mut self, mark: Mark, tail: String) {
            self.mark = mark;
            self.text.truncate(self.parsed());
            self.text.push_str(&tail);
        }
    }

    fn add(p: &Path, text: &str) {
        std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap().write_all(text.as_bytes()).unwrap();
    }

    /// What is appended while a file is read is left to the next read, which takes it as a tail: a whole
    /// read and a tail read both stop at the size their stat saw.
    #[test]
    fn a_write_during_a_read_is_read_next_as_a_tail() {
        let d = std::env::temp_dir().join(format!("borg-follow-during-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("lessons.jsonl");
        add(&p, "{\"a\":1}\n");
        let mut f = File::open(&p).unwrap();
        let (mark, len) = stat(&f);
        add(&p, "{\"b\":2}\n");
        let s = Text::whole(mark, read_text(&mut f, len));
        assert_eq!(s.text, "{\"a\":1}\n", "the whole read stops where its stat ended");
        let mut f = File::open(&p).unwrap();
        let (mark, len) = stat(&f);
        add(&p, "{\"c\":3}\n");
        assert_eq!(tail(&mut f, &s.held(), len).as_deref(), Some("{\"b\":2}\n"), "and so does a tail read");
        let mut s = s;
        s.extend(mark, "{\"b\":2}\n".into());
        assert!(matches!(follow(&p, s.held()), Read::Tail(_, t) if t == "{\"c\":3}\n"), "the rest is the next tail");
        let _ = std::fs::remove_dir_all(&d);
    }
}
