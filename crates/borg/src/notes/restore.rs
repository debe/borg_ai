//! `borg notes fetch`, the write side: what the mesh handed over (borg-mesh `fetch`, into the
//! store's `reasoning.jsonl`) becomes notes. An excerpt becomes the note of a commit that has none;
//! a decision → fix link record (links.rs) goes into the notes on both sides that exist.

use std::path::Path;

use crate::notes::links::{self, Link};
use crate::notes::{Excerpt, git, has_note, note_of, write_note};

/// The commits to ask the mesh about: the named ones (any form git resolves), or else the last 200
/// on HEAD, noted ones too: a teammate's later fix of a decision noted here links to it (the mesh
/// answers a link record by either side).
///
/// # Errors
/// A named commit git cannot resolve.
pub fn wanted(repo: &Path, named: &[&str]) -> Result<Vec<String>, String> {
    if !named.is_empty() {
        return named.iter().map(|c| git(repo, &["rev-parse", "--verify", &format!("{c}^{{commit}}")]).ok_or(format!("{c} is not a commit"))).collect();
    }
    Ok(git(repo, &["rev-list", "--max-count=200", "HEAD"]).unwrap_or_default().lines().map(str::to_string).collect())
}

/// Adds the link records naming `e`'s commit to its note's lists (links.rs `adopt`).
fn adopt(e: &mut Excerpt, records: &[Link]) -> bool {
    links::adopt(&e.commit, &e.collective, records, &mut links::Lists { fixed_by: &mut e.fixed_by, fixes: &mut e.fixes })
}

/// Writes the link records naming `commit` into its existing borg note; whether it changed.
fn relink(repo: &Path, commit: &str, records: &[Link]) -> Result<bool, String> {
    let Some(mut e) = note_of(repo, commit) else { return Ok(false) };
    if !adopt(&mut e, records) {
        return Ok(false);
    }
    write_note(repo, &e).map(|()| true)
}

/// The commits whose notes the link `records` touching `commits` concern: `commits` and each
/// link's other side.
fn link_sides(commits: &[String], records: &[Link]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in records.iter().filter(|l| commits.contains(&l.commit) || commits.contains(&l.fix)) {
        for c in [&l.commit, &l.fix] {
            if !out.contains(c) {
                out.push(c.clone());
            }
        }
    }
    out
}

/// The excerpts of `text` (reasoning.jsonl) for `commits` written as borg notes with their links,
/// where the commit exists and has no borg note yet.
fn restore_excerpts(repo: &Path, text: &str, records: &[Link], commits: &[String]) -> Result<Vec<String>, String> {
    let mut done: Vec<String> = Vec::new();
    for mut e in text.lines().filter_map(|l| serde_json::from_str::<Excerpt>(l).ok()) {
        if e.kind != "reasoning" || !commits.contains(&e.commit) || done.contains(&e.commit) {
            continue;
        }
        if git(repo, &["cat-file", "-e", &format!("{}^{{commit}}", e.commit)]).is_none() || has_note(repo, &e.commit) {
            continue;
        }
        adopt(&mut e, records);
        write_note(repo, &e)?;
        done.push(e.commit);
    }
    Ok(done)
}

/// Writes the excerpts `store`'s reasoning.jsonl holds for `commits` of `repo` as borg notes,
/// where the commit exists and has no borg note yet, with the decision → fix links the store holds
/// for them (fetched from the mesh beside the excerpts; links.rs). A link also goes into an existing
/// note on either side. Returns the commits noted or relinked.
///
/// # Errors
/// A note git would not write.
pub fn restore(repo: &Path, store: &Path, commits: &[String]) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(store.join("reasoning.jsonl")).unwrap_or_default();
    let records = links::records(&text);
    let mut done = restore_excerpts(repo, &text, &records, commits)?;
    let sides: Vec<String> = link_sides(commits, &records).into_iter().filter(|c| !done.contains(c)).collect();
    for c in sides {
        if relink(repo, &c, &records)? {
            done.push(c);
        }
    }
    Ok(done)
}
