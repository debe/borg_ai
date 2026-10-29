//! Decision → fix links (regress.rs finds them): the record a link leaves in the collective's
//! `reasoning.jsonl` and the `borg fixed in:` / `borg fixes:` sections of a borg note.
//!
//! A record's `commit` is the **decision**, its `fix` the fix: the mesh answers `Ask` for a link by
//! either side, so `borg notes fetch` with the decision or the fix brings it along. Its free text (tasks,
//! subject) is redacted before it gets here.

use serde::{Deserialize, Serialize};

/// `kind` of a link record in reasoning.jsonl.
pub const KIND: &str = "regress";
pub const FIX: &str = "fix";
pub const REVISITED: &str = "revisited";

/// A note lists at most this many commits per section. Display: a git note stays readable.
const MAX_LISTED: usize = 10;

const FIXED_IN: &str = "\n\nborg fixed in:\n";
const FIXES: &str = "\n\nborg fixes:\n";

/// One side of a link as a note lists it: the other commit, its session and task (first line).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRef {
    pub commit: String,
    pub session: String,
    pub task: String,
    /// YYYY-MM-DD (committer date).
    pub date: String,
    /// `FIX` or `REVISITED`.
    pub strength: String,
}

/// A commit as a link record describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    pub session: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    pub date: String,
    /// Committer time, epoch seconds; 0 in records from before it was kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub secs: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Lines of one file at the fix's parent, as inclusive ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touched {
    pub path: String,
    pub lines: Vec<(u32, u32)>,
}

/// A link record: `fix` changed lines `commit` (the decision) wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub kind: String,
    pub commit: String,
    pub fix: String,
    pub repo: String,
    pub collective: String,
    pub strength: String,
    pub signals: Vec<String>,
    pub decision: Side,
    pub fixed: Side,
    pub files: Vec<Touched>,
    pub lines: usize,
    pub age_days: u32,
    pub ms: i64,
}

impl Link {
    #[must_use]
    pub fn strong(&self) -> bool {
        self.strength == FIX
    }
}

/// The link records among reasoning.jsonl's lines (`text`).
#[must_use]
pub fn records(text: &str) -> Vec<Link> {
    text.lines().filter(|l| l.contains("\"regress\"")).filter_map(|l| serde_json::from_str::<Link>(l).ok()).filter(|l| l.kind == KIND).collect()
}

/// The two sides of a link as notes list them: (on the decision, on the fix).
#[must_use]
pub fn refs(l: &Link) -> (LinkRef, LinkRef) {
    let fixed_by = LinkRef {
        commit: l.fix.clone(),
        session: l.fixed.session.clone(),
        task: l.fixed.task.clone(),
        date: l.fixed.date.clone(),
        strength: l.strength.clone(),
    };
    let fixes = LinkRef {
        commit: l.commit.clone(),
        session: l.decision.session.clone(),
        task: l.decision.task.clone(),
        date: l.decision.date.clone(),
        strength: l.strength.clone(),
    };
    (fixed_by, fixes)
}

/// `list` with `r` added (unless its commit is listed), at most `MAX_LISTED`: the oldest weak
/// entries go first. Whether it was added.
pub fn push_ref(list: &mut Vec<LinkRef>, r: LinkRef) -> bool {
    if list.iter().any(|x| x.commit == r.commit) {
        return false;
    }
    list.push(r);
    while list.len() > MAX_LISTED {
        let i = list.iter().position(|x| x.strength == REVISITED).unwrap_or(0);
        list.remove(i);
    }
    true
}

/// A note's two lists, to add links to.
pub struct Lists<'a> {
    pub fixed_by: &'a mut Vec<LinkRef>,
    pub fixes: &'a mut Vec<LinkRef>,
}

/// Adds the `records` naming `commit` (a note of `collective`) to its lists: as the decision under
/// `fixed_by`, as the fix under `fixes`; a record of another collective never. Whether any was new.
#[must_use]
pub fn adopt(commit: &str, collective: &str, records: &[Link], lists: &mut Lists) -> bool {
    let mut changed = false;
    for l in records.iter().filter(|l| l.collective.is_empty() || l.collective == collective) {
        let (by, fixes) = refs(l);
        if l.commit == commit {
            changed |= push_ref(lists.fixed_by, by);
        }
        if l.fix == commit {
            changed |= push_ref(lists.fixes, fixes);
        }
    }
    changed
}

fn line(l: &LinkRef) -> String {
    format!("- {} {} {} session {}: {}\n", l.commit, l.date, l.strength, l.session, l.task)
}

/// The note text after the corrections line: both sections, each only when it lists something.
pub fn sections(fixed_by: &[LinkRef], fixes: &[LinkRef]) -> String {
    let mut s = String::new();
    for (head, list) in [(FIXED_IN, fixed_by), (FIXES, fixes)] {
        if !list.is_empty() {
            s.push_str(&head[1..]);
            s.extend(list.iter().map(line));
        }
    }
    s
}

fn parse_line(l: &str) -> Option<LinkRef> {
    let (commit, rest) = l.strip_prefix("- ")?.split_once(' ')?;
    let (date, rest) = rest.split_once(' ')?;
    let (strength, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix("session ")?;
    // git strips the trailing space a line with an empty task ends in.
    let (session, task) = rest.split_once(": ").or_else(|| Some((rest.strip_suffix(':')?, "")))?;
    Some(LinkRef { commit: commit.into(), session: session.into(), task: task.trim().into(), date: date.into(), strength: strength.into() })
}

fn parse(list: &str) -> Vec<LinkRef> {
    list.lines().filter_map(parse_line).collect()
}

/// `text` split at the last `head`: what comes before it, ending in a newline as the section's
/// predecessor did before git collapsed the blank lines (an empty proposal), and the list after it.
fn cut_at<'a>(text: &'a str, head: &str) -> (&'a str, Vec<LinkRef>) {
    text.rfind(head).map_or((text, Vec::new()), |i| (&text[..=i], parse(&text[i + head.len()..])))
}

/// A note's text without its link sections, and the `fixed in` and `fixes` lists (the inverse of
/// `sections`).
#[must_use]
pub fn split(text: &str) -> (&str, Vec<LinkRef>, Vec<LinkRef>) {
    let (rest, fixes) = cut_at(text, FIXES);
    let (rest, fixed_by) = cut_at(rest, FIXED_IN);
    (rest, fixed_by, fixes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(commit: &str, strength: &str) -> LinkRef {
        LinkRef { commit: commit.into(), session: "s-7".into(), task: "fix the flaky sync: test".into(), date: "2026-10-12".into(), strength: strength.into() }
    }

    #[test]
    fn listed_once_at_most_ten_dropping_old_weak_first() {
        let mut list: Vec<LinkRef> = (0..10).map(|i| r(&format!("c{i}"), if i == 3 { REVISITED } else { FIX })).collect();
        assert!(!push_ref(&mut list, r("c0", FIX)), "listed already");
        assert!(push_ref(&mut list, r("new", FIX)));
        assert_eq!(list.len(), 10);
        assert!(!list.iter().any(|x| x.commit == "c3") && list.iter().any(|x| x.commit == "c0"));
        assert!(push_ref(&mut list, r("newer", FIX)));
        assert!(!list.iter().any(|x| x.commit == "c0"), "then the oldest");
    }

    #[test]
    fn records_are_adopted_by_both_sides_of_their_collective_only() {
        let rec = |commit: &str, fix: &str, collective: &str| Link {
            kind: KIND.into(),
            commit: commit.into(),
            fix: fix.into(),
            collective: collective.into(),
            strength: FIX.into(),
            ..Link::default()
        };
        let text = [rec("d", "f", "work"), rec("d", "g", "private"), rec("e", "d", "work")]
            .iter()
            .map(|l| serde_json::to_string(l).unwrap())
            .chain(["{\"kind\":\"reasoning\",\"commit\":\"d\"}".to_string()])
            .collect::<Vec<_>>()
            .join("\n");
        let records = records(&text);
        assert_eq!(records.len(), 3);
        let (mut by, mut fixes) = (Vec::new(), Vec::new());
        assert!(adopt("d", "work", &records, &mut Lists { fixed_by: &mut by, fixes: &mut fixes }));
        assert_eq!((by.iter().map(|x| x.commit.as_str()).collect::<Vec<_>>(), fixes[0].commit.as_str()), (vec!["f"], "e"));
        assert!(!adopt("d", "work", &records, &mut Lists { fixed_by: &mut by, fixes: &mut fixes }), "once");
    }

    #[test]
    fn sections_round_trip() {
        let (by, fixes) = (vec![r("aa", FIX), r("bb", REVISITED)], vec![r("cc", FIX)]);
        let text = format!("Last proposal:\nkeep it\n{}", sections(&by, &fixes));
        assert!(text.contains("\n\nborg fixed in:\n- aa 2026-10-12 fix session s-7: fix the flaky sync: test\n"), "{text}");
        assert_eq!(split(&text), ("Last proposal:\nkeep it\n", by.clone(), fixes.clone()));
        assert_eq!(split("plain\ntext"), ("plain\ntext", vec![], vec![]));
        let only_fixes = format!("x\n{}", sections(&[], &fixes));
        assert_eq!(split(&only_fixes), ("x\n", vec![], fixes));
        // As git stores it: an empty task loses its trailing space, an empty proposal a blank line.
        let stored = "Last proposal:\n\nborg fixed in:\n- aa 2026-10-12 fix session s-7:\n";
        let empty = LinkRef { task: String::new(), ..r("aa", FIX) };
        assert_eq!(split(stored), ("Last proposal:\n", vec![empty], vec![]));
    }
}
