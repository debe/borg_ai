//! `borg why`, both directions of a decision → fix link (notes/regress.rs): on a decision, the later
//! commits that fixed (or revisited) its lines; on a fix, the decisions it fixed. From the commit's
//! note (`borg fixed in:` / `borg fixes:`) and the collective's link records (those `borg notes
//! fetch` brought from the mesh too), one line per other commit, the note's first. Both are visible
//! in the collective already; the text is redacted again here.

use serde::Serialize;

use crate::notes::links::{self, FIX, Link, LinkRef};
use crate::notes::{Excerpt, redact};

/// At most this many links per direction in the compact form. Display, as why.rs `MAX_LESSONS`.
const MAX_LINKS: usize = 3;
use super::TASK_CAP;

/// The other commit of a link.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LinkLine {
    pub sha: String,
    pub short: String,
    pub date: String,
    /// "fix" or "revisited".
    pub strength: String,
    /// Its session's task (first line).
    pub task: String,
}

fn line(r: &LinkRef) -> LinkLine {
    LinkLine {
        sha: r.commit.clone(),
        short: r.commit.chars().take(7).collect(),
        date: r.date.clone(),
        strength: r.strength.clone(),
        task: redact(r.task.lines().next().unwrap_or_default().trim()),
    }
}

fn merged<'a>(noted: &'a [LinkRef], stored: impl Iterator<Item = LinkRef> + 'a) -> Vec<LinkLine> {
    let mut out: Vec<LinkLine> = noted.iter().map(line).collect();
    for r in stored {
        if !out.iter().any(|x| x.sha == r.commit) {
            out.push(line(&r));
        }
    }
    out
}

/// The links of `sha`: (fixed in, fixes), from its note (`noted`) and the store's records.
#[must_use]
pub fn of(sha: &str, noted: Option<&Excerpt>, stored: &[Link]) -> (Vec<LinkLine>, Vec<LinkLine>) {
    let (by, fixes) = noted.map_or((&[][..], &[][..]), |e| (e.fixed_by.as_slice(), e.fixes.as_slice()));
    let fixed_in = merged(by, stored.iter().filter(|l| l.commit == sha).map(|l| links::refs(l).0));
    let fixes = merged(fixes, stored.iter().filter(|l| l.fix == sha).map(|l| links::refs(l).1));
    (fixed_in, fixes)
}

/// The compact form: at most `MAX_LINKS`, tasks cut.
pub fn compact(lines: &mut Vec<LinkLine>) {
    lines.truncate(MAX_LINKS);
    for l in lines {
        if l.task.chars().count() > TASK_CAP {
            l.task = format!("{}…", l.task.chars().take(TASK_CAP).collect::<String>());
        }
    }
}

fn verb(l: &LinkLine) -> &'static str {
    if l.strength == FIX { "fixed" } else { "revisited" }
}

fn task(l: &LinkLine) -> String {
    if l.task.is_empty() { String::new() } else { format!(" (task: {})", l.task) }
}

/// `→ fixed in Y` under a decision, `← decided in X … → fixed here` under a fix.
#[must_use]
pub fn render(fixed_in: &[LinkLine], fixes: &[LinkLine]) -> String {
    let later = fixed_in.iter().map(|l| format!("  → {} in {} {}{}\n", verb(l), l.short, l.date, task(l)));
    let earlier = fixes.iter().map(|l| format!("  ← decided in {} {}{} → {} here\n", l.short, l.date, task(l), verb(l)));
    later.chain(earlier).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::links::{REVISITED, Side};

    fn r(commit: &str, strength: &str, task: &str) -> LinkRef {
        LinkRef { commit: commit.into(), session: "s".into(), task: task.into(), date: "2026-10-12".into(), strength: strength.into() }
    }

    #[test]
    fn note_and_store_merge_one_line_per_commit() {
        let note = Excerpt { fixed_by: vec![r("aaaaaaaa1", FIX, "fix the race")], ..Excerpt::default() };
        let stored = |commit: &str, fix: &str| Link {
            commit: commit.into(),
            fix: fix.into(),
            strength: REVISITED.into(),
            decision: Side { task: "decide".into(), date: "2026-09-30".into(), ..Side::default() },
            fixed: Side { task: "rename token=sk-abcdefghijklmnopqrstuvwx".into(), date: "2026-10-14".into(), ..Side::default() },
            ..Link::default()
        };
        let links = [stored("d", "aaaaaaaa1"), stored("d", "bbbbbbbb2"), stored("cccccccc3", "d")];
        let (fixed_in, fixes) = of("d", Some(&note), &links);
        assert_eq!(fixed_in.iter().map(|l| (l.short.as_str(), l.strength.as_str())).collect::<Vec<_>>(), [("aaaaaaa", FIX), ("bbbbbbb", REVISITED)]);
        assert!(!fixed_in[1].task.contains("sk-abc"), "redacted: {}", fixed_in[1].task);
        assert_eq!((fixes.len(), fixes[0].task.as_str()), (1, "decide"));
        let text = render(&fixed_in[..1], &fixes);
        assert_eq!(text, "  → fixed in aaaaaaa 2026-10-12 (task: fix the race)\n  ← decided in ccccccc 2026-09-30 (task: decide) → revisited here\n");
        let (later, earlier) = of("x", None, &links);
        assert!(later.is_empty() && earlier.is_empty());
        let mut many: Vec<LinkLine> = (0..5).map(|i| line(&r(&format!("c{i}"), FIX, &"t".repeat(300)))).collect();
        compact(&mut many);
        assert_eq!((many.len(), many[0].task.chars().count()), (3, TASK_CAP + 1));
    }
}
