//! Which hunks of a fix change no decision (regress.rs drops them before blame): a formatter's
//! reflow (equal once all whitespace and trailing commas are gone, also when git split it across
//! hunks), a rename (one identifier swapped throughout), and a move: a removed line the fix adds
//! again verbatim in another file, further away, or as part of a block. A line swapped with its
//! neighbour is a change: an ordering fix.

use std::collections::{HashMap, HashSet};

use crate::notes::regress_diff::Hunk;

/// Removed lines shorter than this (non-space chars) are never taken for moved: `}` recurs. Method:
/// a heuristic of the cosmetic-change classifier, design-chosen.
const MOVE_MIN: usize = 8;
/// A single line re-added at most this many lines from where it was removed was reordered, not moved.
/// Method: as `MOVE_MIN`.
const MOVE_NEAR: u32 = 3;

/// The text without whitespace, and without the trailing commas a formatter adds or drops when it
/// reflows a list (before a closing bracket, or at the end).
fn squeezed<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let all: Vec<char> = lines.flat_map(str::chars).filter(|c| !c.is_whitespace()).collect();
    let trailing = |i: usize| all[i] == ',' && all.get(i + 1).is_none_or(|n| matches!(n, ')' | ']' | '}'));
    (0..all.len()).filter(|&i| !trailing(i)).map(|i| all[i]).collect()
}

/// A formatter's hunk: what it removed and what it added are equal once all whitespace (and
/// trailing commas) are gone.
pub fn formatting_only(h: &Hunk) -> bool {
    !h.removed.is_empty() && squeezed(h.removed.iter().map(|(_, t)| t.as_str())) == squeezed(h.added.iter().map(String::as_str))
}

/// The lines trimmed, in order.
fn sorted<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut v: Vec<&str> = lines.map(str::trim).collect();
    v.sort_unstable();
    v
}

/// The paths whose hunks, taken together, only reflow text: git may split one reflow across
/// hunks (a closing brace matched to the next line's).
#[must_use]
pub fn reflowed_files(hunks: &[Hunk]) -> HashSet<String> {
    let mut whole: Vec<Hunk> = Vec::new();
    for h in hunks {
        match whole.iter_mut().find(|w| w.path == h.path) {
            Some(w) => {
                w.removed.extend(h.removed.iter().cloned());
                w.added.extend(h.added.iter().cloned());
            }
            None => whole.push(h.clone()),
        }
    }
    // The same lines in another order are a reorder or a move (`drop_moves`), not a reflow.
    let reordered = |w: &Hunk| sorted(w.removed.iter().map(|(_, t)| t.as_str())) == sorted(w.added.iter().map(String::as_str));
    whole.into_iter().filter(|w| formatting_only(w) && !reordered(w)).map(|w| w.path).collect()
}

/// Identifier runs and single other characters, whitespace left out.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s.trim_start();
    while let Some(c) = rest.chars().next() {
        let len = if c.is_alphanumeric() || c == '_' { rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len()) } else { c.len_utf8() };
        out.push(&rest[..len]);
        rest = rest[len..].trim_start();
    }
    out
}

/// Whether `new` is `old` with identifiers swapped, each swap the one `subst` holds (or sets).
fn renamed_line<'a>(old: &'a str, new: &'a str, subst: &mut Option<(&'a str, &'a str)>) -> bool {
    let (a, b) = (tokens(old), tokens(new));
    let ident = |t: &str| t.starts_with(|c: char| c.is_alphabetic() || c == '_');
    a.len() == b.len() && a.iter().zip(&b).filter(|(x, y)| x != y).all(|(x, y)| ident(x) && ident(y) && subst.get_or_insert((x, y)) == &(*x, *y))
}

/// A rename's hunk: line for line the same but for one identifier, renamed throughout.
#[must_use]
pub fn renamed_only(h: &Hunk) -> bool {
    let mut subst = None;
    !h.removed.is_empty()
        && h.removed.len() == h.added.len()
        && h.removed.iter().zip(&h.added).all(|((_, old), new)| renamed_line(old, new, &mut subst))
        && subst.is_some()
}

fn movable(t: &str) -> bool {
    t.chars().filter(|c| !c.is_whitespace()).count() >= MOVE_MIN
}

/// Where each movable added line (trimmed) landed: its hunk's path and its line in the fix.
fn landings(hunks: &[Hunk]) -> HashMap<&str, Vec<(&str, u32)>> {
    let mut out: HashMap<&str, Vec<(&str, u32)>> = HashMap::new();
    for h in hunks {
        for (k, t) in (0u32..).zip(&h.added).filter(|(_, t)| movable(t)) {
            out.entry(t.trim()).or_default().push((h.path.as_str(), h.added_at + k));
        }
    }
    out
}

/// Whether removed line `i` of `h` was moved: added again in another file, more than `MOVE_NEAR`
/// lines away, or together with a removed neighbour (a block).
fn moved(h: &Hunk, i: usize, landed: &HashMap<&str, Vec<(&str, u32)>>) -> bool {
    let (n, t) = &h.removed[i];
    let Some(spots) = landed.get(t.trim()) else { return false };
    let again = |j: usize| h.removed.get(j).is_some_and(|(_, u)| landed.contains_key(u.trim()));
    spots.iter().any(|&(p, at)| p != h.path || at.abs_diff(*n) > MOVE_NEAR) || (i > 0 && again(i - 1)) || again(i + 1)
}

/// `hunks` without the removed lines the fix moved (`moved`), and the number of lines dropped so;
/// a hunk left with nothing removed is dropped.
#[must_use]
pub fn drop_moves(hunks: Vec<Hunk>) -> (Vec<Hunk>, usize) {
    let keep: Vec<Vec<bool>> = {
        let landed = landings(&hunks);
        hunks.iter().map(|h| (0..h.removed.len()).map(|i| !moved(h, i, &landed)).collect()).collect()
    };
    let mut dropped = 0;
    let mut out = Vec::new();
    for (mut h, keep) in hunks.into_iter().zip(keep) {
        dropped += keep.iter().filter(|k| !**k).count();
        let mut k = keep.into_iter();
        h.removed.retain(|_| k.next().unwrap_or(true));
        out.extend((!h.removed.is_empty()).then_some(h));
    }
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_lines_are_dropped_short_ones_kept() {
        let h = vec![
            Hunk { path: "a.rs".into(), removed: vec![(1, "fn helper(x: u32) -> u32 {".into()), (2, "}".into())], ..Hunk::default() },
            Hunk { path: "b.rs".into(), added: vec!["    fn helper(x: u32) -> u32 {".into(), "}".into()], added_at: 1, ..Hunk::default() },
        ];
        let (kept, dropped) = drop_moves(h);
        assert_eq!(dropped, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].removed, [(2, "}".to_string())], "a lone brace is not a move");
    }

    /// Taking the lock before reading the flag swaps two lines: the line git shows removed and added
    /// again one line further down was reordered, and is blamed; a block moved down the file is not.
    #[test]
    fn a_reorder_is_a_change_a_block_far_away_a_move() {
        let reorder = "\
diff --git a/src/sync.rs b/src/sync.rs
--- a/src/sync.rs
+++ b/src/sync.rs
@@ -2 +1,0 @@
-    let ready = state.flag.load();
@@ -3,0 +3 @@
+    let ready = state.flag.load();
";
        let (kept, dropped) = drop_moves(crate::notes::regress_diff::hunks(reorder));
        assert_eq!((kept.len(), dropped), (1, 0), "{kept:?}");
        assert_eq!(kept[0].removed, [(2, "    let ready = state.flag.load();".to_string())]);
        let far = vec![
            Hunk { path: "a.rs".into(), removed: vec![(2, "let ready = flag.load();".into())], ..Hunk::default() },
            Hunk { path: "a.rs".into(), added: vec!["let ready = flag.load();".into()], added_at: 40, ..Hunk::default() },
        ];
        assert_eq!(drop_moves(far).1, 1, "far away");
        let block = vec![
            Hunk { path: "a.rs".into(), removed: vec![(2, "let guard = lock();".into()), (3, "let ready = flag.load();".into())], ..Hunk::default() },
            Hunk { path: "a.rs".into(), added: vec!["let guard = lock();".into(), "let ready = flag.load();".into()], added_at: 3, ..Hunk::default() },
        ];
        assert_eq!(drop_moves(block).1, 2, "a block");
    }

    #[test]
    fn reflows_with_trailing_commas_and_renames() {
        let reflow = Hunk {
            removed: vec![(4, "    Config { name: \"x\".into(), retries: 3 }".into())],
            added: ["    Config {", "        name: \"x\".into(),", "        retries: 3,", "    }"].map(String::from).to_vec(),
            ..Hunk::default()
        };
        assert!(formatting_only(&reflow));
        let split = [
            Hunk {
                path: "b.rs".into(),
                removed: vec![(2, "Config { name: x, retries: 3 }".into())],
                added: vec!["Config {".into(), "name: x,".into()],
                ..Hunk::default()
            },
            Hunk { path: "b.rs".into(), removed: vec![(4, "}".into())], added: vec!["retries: 3,".into(), "}".into(), "}".into()], ..Hunk::default() },
            Hunk { path: "c.rs".into(), removed: vec![(1, "a".into())], added: vec!["b".into()], ..Hunk::default() },
        ];
        assert!(!formatting_only(&split[0]) && !formatting_only(&split[1]));
        assert_eq!(reflowed_files(&split), HashSet::from(["b.rs".to_string()]), "a reflow git split across hunks");
        let swap = [
            Hunk { path: "s.rs".into(), removed: vec![(2, "let ready = flag();".into())], ..Hunk::default() },
            Hunk { path: "s.rs".into(), added: vec!["    let ready = flag();".into()], added_at: 3, ..Hunk::default() },
        ];
        assert!(reflowed_files(&swap).is_empty(), "a swap is no reflow");
        let real = Hunk { removed: vec![(1, "f(a, b)".into())], added: vec!["f(b, a)".into()], ..Hunk::default() };
        assert!(!formatting_only(&real) && !renamed_only(&real));
        let rename = Hunk {
            removed: vec![(1, "let n = total(xs);".into()), (2, "n / total(ys)".into())],
            added: vec!["let n = sum(xs);".into(), "n / sum(ys)".into()],
            ..Hunk::default()
        };
        assert!(renamed_only(&rename));
        let two = Hunk { removed: vec![(1, "a(b)".into())], added: vec!["c(d)".into()], ..Hunk::default() };
        assert!(!renamed_only(&two), "two names changed");
        let fix = Hunk { removed: vec![(1, "xs.iter().sum::<u32>() / 2".into())], added: vec!["xs.iter().sum::<u32>()".into()], ..Hunk::default() };
        assert!(!renamed_only(&fix));
    }
}
