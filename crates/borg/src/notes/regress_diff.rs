//! What git says about a fix, parsed (regress.rs runs git): `--numstat -z` sizes, `-U0` hunks, and
//! `blame --porcelain` per line. No git here, so every rule is a unit test.
//!
//! Only lines a fix removed or changed count: a pure addition blames nobody, and lock, snapshot and
//! generated files record no decision. Reflows, renames and moves: `regress_cosmetic.rs`.

use std::collections::HashSet;

/// One `-U0` hunk: lines removed at the parent (with their line numbers there) and lines added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hunk {
    /// The path at the parent (the old path of a rename).
    pub path: String,
    pub removed: Vec<(u32, String)>,
    pub added: Vec<String>,
    /// The line the added lines start at in the fix.
    pub added_at: u32,
}

/// Lock files, vendored builds, snapshots and generated code: their lines record no decision.
#[must_use]
pub fn generated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = std::path::Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or_default();
    let spec = name.starts_with("openapi") || name.starts_with("swagger");
    ["lock", "snap"].contains(&ext)
        || ["package-lock.json", "pnpm-lock.yaml", "yarn.lock", "go.sum"].contains(&name)
        || [".min.", ".pb.", "_pb2.", "_generated.", ".generated."].iter().any(|m| name.contains(m))
        || (spec && ["json", "yaml", "yml"].contains(&ext))
        || path.starts_with("dist/")
        || path.contains("/dist/")
}

/// The paths `git check-attr -z linguist-generated diff` marks generated (`linguist-generated`
/// set, or `-diff`).
#[must_use]
pub fn generated_by_attr(out: &str) -> HashSet<String> {
    let f: Vec<&str> = out.split('\0').collect();
    let (triples, _) = f.as_chunks::<3>();
    triples
        .iter()
        .filter(|[_, attr, value]| matches!((*attr, *value), ("linguist-generated", "set" | "true") | ("diff", "unset")))
        .map(|[p, ..]| (*p).to_string())
        .collect()
}

/// Files and changed lines of `git diff --numstat -z -M`, binary and generated files left out.
#[must_use]
pub fn size(numstat: &str) -> (usize, usize) {
    let (mut files, mut lines) = (0, 0);
    let mut it = numstat.split('\0');
    while let Some(entry) = it.next() {
        let mut f = entry.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (f.next(), f.next(), f.next()) else { continue };
        // A rename: the old and the new path follow as fields of their own.
        let path = if path.is_empty() { it.nth(1).unwrap_or_default() } else { path };
        let (Ok(a), Ok(d)) = (add.trim().parse::<usize>(), del.parse::<usize>()) else { continue };
        if !generated(path) {
            files += 1;
            lines += a + d;
        }
    }
    (files, lines)
}

/// The old and the new start line of a hunk header (`@@ -a,b +c,d @@`).
fn starts(header: &str) -> Option<(u32, u32)> {
    let mut f = header.strip_prefix("@@ -")?.split(' ');
    let start = |s: &str| s.split(',').next()?.parse().ok();
    Some((start(f.next()?)?, start(f.next()?.strip_prefix('+')?)?))
}

const BACKSLASH: u8 = 0x5c;
const QUOTE: char = '\u{22}';

/// A C escape of a quoted git path (`\n`, `\t`, ...): the byte it stands for.
fn unescape(e: u8) -> u8 {
    match e {
        b'n' => 10,
        b't' => 9,
        b'r' => 13,
        b'a' => 7,
        b'b' => 8,
        b'f' => 12,
        b'v' => 11,
        other => other,
    }
}

/// A path git quoted C style (`"a/l\303\257b.rs"`), unquoted.
fn unquote(q: &str) -> Option<String> {
    let inner = q.strip_prefix(QUOTE)?.strip_suffix(QUOTE)?.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while let Some(&b) = inner.get(i) {
        let octal = inner.get(i + 1..i + 4).filter(|o| b == BACKSLASH && o.iter().all(|d| (b'0'..=b'7').contains(d)));
        let (byte, used) = match (b, octal) {
            (_, Some(o)) => (u8::from_str_radix(std::str::from_utf8(o).ok()?, 8).ok()?, 4),
            (BACKSLASH, None) => (unescape(*inner.get(i + 1)?), 2),
            _ => (b, 1),
        };
        out.push(byte);
        i += used;
    }
    String::from_utf8(out).ok()
}

/// The old path a `--- ` line names: `a/<path>` (git ends a path with a space in a tab), or quoted;
/// `None` for `/dev/null` (a new file, nothing to blame).
fn old_path(l: &str) -> Option<String> {
    let p = l.strip_prefix("--- ")?.trim_end_matches(char::is_control);
    let p = if p.starts_with(QUOTE) { unquote(p)? } else { p.to_string() };
    p.strip_prefix("a/").map(str::to_string)
}

/// The hunks of `git diff -U0 --src-prefix=a/ --dst-prefix=b/` output. A hunk whose path could not
/// be read has an empty one.
#[must_use]
pub fn hunks(diff: &str) -> Vec<Hunk> {
    let (mut out, mut path, mut header, mut at) = (Vec::<Hunk>::new(), String::new(), false, 0u32);
    for l in diff.lines() {
        if l.starts_with("diff --git ") {
            (header, path) = (true, String::new());
        } else if let Some((a, b)) = l.starts_with("@@ ").then(|| starts(l)).flatten() {
            (header, at) = (false, a);
            out.push(Hunk { path: path.clone(), added_at: b, ..Hunk::default() });
        } else if header {
            path = old_path(l).unwrap_or(path);
        } else if let Some(h) = out.last_mut() {
            body_line(h, l, &mut at);
        }
    }
    out
}

fn body_line(h: &mut Hunk, l: &str, at: &mut u32) {
    if let Some(t) = l.strip_prefix('-') {
        h.removed.push((*at, t.to_string()));
        *at += 1;
    } else if let Some(t) = l.strip_prefix('+') {
        h.added.push(t.to_string());
    }
}

/// The commit behind each line of `git blame --porcelain` output, with the line's number at the
/// blamed revision; lines of a `boundary` commit (a shallow graft, a range start) and uncommitted
/// lines are left out.
#[must_use]
pub fn blame_lines(porcelain: &str) -> Vec<(String, u32)> {
    let (mut out, mut boundary, mut current) = (Vec::new(), HashSet::<String>::new(), None::<String>);
    for l in porcelain.lines().filter(|l| !l.starts_with('\t')) {
        if l == "boundary" {
            boundary.extend(current.clone());
            continue;
        }
        let mut f = l.split(' ');
        let (Some(sha), Some(_), Some(line)) = (f.next(), f.next(), f.next()) else { continue };
        let Ok(line) = line.parse::<u32>() else { continue };
        if sha.len() >= 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            current = Some(sha.to_string());
            out.push((sha.to_string(), line));
        }
    }
    out.retain(|(sha, _)| !boundary.contains(sha) && !sha.bytes().all(|b| b == b'0'));
    out
}

/// Sorted line numbers as inclusive ranges.
#[must_use]
pub fn ranges(lines: &[u32]) -> Vec<(u32, u32)> {
    let mut v = lines.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for n in v {
        match out.last_mut() {
            Some((_, b)) if *b + 1 == n => *b = n,
            _ => out.push((n, n)),
        }
    }
    out
}

/// The removed lines of `hunks` per path, paths in hunk order.
#[must_use]
pub fn removed_by_path(hunks: &[Hunk]) -> Vec<(String, Vec<u32>)> {
    let mut out: Vec<(String, Vec<u32>)> = Vec::new();
    for h in hunks {
        let lines = h.removed.iter().map(|(n, _)| *n);
        match out.iter_mut().find(|(p, _)| *p == h.path) {
            Some((_, v)) => v.extend(lines),
            None => out.push((h.path.clone(), lines.collect())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/old.rs b/src/new.rs
similarity index 90%
rename from src/old.rs
rename to src/new.rs
--- a/src/old.rs
+++ b/src/new.rs
@@ -2 +2 @@ fn a() {}
-fn b() { flag(); }
+fn b() { lock(); flag(); }
@@ -10,0 +11,2 @@
+fn d() {}
+fn e() {}
@@ -20,2 +22 @@
-let x =
-    1;
+let x = 1;
\\ No newline at end of file
diff --git a/Cargo.lock b/Cargo.lock
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -3 +3 @@
-version = 1
+version = 2
diff --git a/src/added.rs b/src/added.rs
--- /dev/null
+++ b/src/added.rs
@@ -0,0 +1 @@
+fn z() {}
diff --git a/q.sql b/q.sql
--- a/q.sql
+++ b/q.sql
@@ -4 +4 @@
--- the old comment
+-- the new comment
";

    #[test]
    fn hunks_keep_old_paths_and_line_numbers() {
        let h = hunks(DIFF);
        assert_eq!(h.len(), 6, "{h:?}");
        assert_eq!(h[3].path, "Cargo.lock");
        let h: Vec<Hunk> = h.into_iter().filter(|h| !generated(&h.path)).collect();
        assert_eq!(h[4].removed, [(4, "-- the old comment".to_string())], "a removed line starting with -- is not a header");
        assert_eq!((h[0].path.as_str(), h[0].removed.clone()), ("src/old.rs", vec![(2, "fn b() { flag(); }".to_string())]));
        assert!(h[1].removed.is_empty() && h[1].added.len() == 2, "a pure addition");
        assert_eq!(h[2].removed.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [20, 21]);
        assert_eq!(h[3].path, "", "a new file has no old path");
        let reflow = crate::notes::regress_cosmetic::formatting_only;
        assert!(reflow(&h[2]) && !reflow(&h[0]) && !reflow(&h[1]));
        assert_eq!(removed_by_path(&h[..4]), [("src/old.rs".to_string(), vec![2, 20, 21]), (String::new(), vec![])]);
    }

    /// git ends a path with a space in a tab and quotes one with a quote or a control character.
    #[test]
    fn paths_with_spaces_and_quotes() {
        let d = "diff --git a/src/my dir/lib.rs b/src/my dir/lib.rs\n--- a/src/my dir/lib.rs\t\n+++ b/src/my dir/lib.rs\t\n@@ -2 +2 @@\n-x\n+y\n\
diff --git \"a/src/l\\303\\257b \\\"q\\\".rs\" \"b/src/l\\303\\257b \\\"q\\\".rs\"\n--- \"a/src/l\\303\\257b \\\"q\\\".rs\"\n+++ \"b/x\"\n@@ -5 +5 @@\n-x\n+y\n";
        let h = hunks(d);
        assert_eq!(h.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), ["src/my dir/lib.rs", "src/l\u{ef}b \"q\".rs"], "{d}");
        assert_eq!(unquote("\"a\\tb\\\\c\""), Some("a\tb\\c".to_string()));
    }

    #[test]
    fn numstat_sizes_skip_binary_and_generated() {
        let z = "3\t1\tsrc/a.rs\0-\t-\tlogo.png\x002\t2\t\0src/old.rs\0src/new.rs\x00100\t50\tCargo.lock\0";
        assert_eq!(size(z), (2, 8));
        assert!(generated("web/dist/app.js") && generated("a/b.min.css") && generated("yarn.lock") && !generated("src/lock.rs"));
        assert!(generated("api/openapi.json") && generated("pb/user.pb.go") && generated("gen/user_pb2.py") && generated("src/schema_generated.rs"));
        assert!(!generated("src/openapi.rs") && !generated("docs/api.json"));
        let attrs = "api/spec.json\0linguist-generated\0set\0api/spec.json\0diff\0unspecified\0src/a.rs\0linguist-generated\0unspecified\0src/a.rs\0diff\0unset\0b.rs\0diff\0set\0";
        assert_eq!(generated_by_attr(attrs), HashSet::from(["api/spec.json".to_string(), "src/a.rs".to_string()]));
    }

    #[test]
    fn blame_lines_per_line_without_boundary_and_uncommitted() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let z = "0".repeat(40);
        let p = format!(
            "{a} 1 5 2\nauthor A\nsummary x\n\tline5\n{a} 2 6\n\tline6\n{b} 7 7 1\nauthor B\nboundary\nsummary y\n\tline7\n{z} 8 8 1\n\tline8\n{a} 3 9 1\n\tline9\n"
        );
        assert_eq!(blame_lines(&p), [(a.clone(), 5), (a.clone(), 6), (a, 9)]);
        assert_eq!(ranges(&[9, 5, 6, 6, 12]), [(5, 6), (9, 9), (12, 12)]);
    }
}
