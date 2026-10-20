//! The reasoning recorded for commits, as `borg why` and regress.rs look it up: a commit's borg
//! note (notes.rs) or else the excerpt the collective's `reasoning.jsonl` holds, each only where it
//! may be shown (`visible`). One `notes list` plus one `cat-file --batch`, and one read of the store.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::notes::{self, Excerpt};

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_in(dir, args, None).map(|o| String::from_utf8_lossy(&o).into_owned())
}

/// git in `dir`, with `input` on stdin (written from a thread so a large output cannot block it).
///
/// # Errors
/// When git cannot run or fails (its stderr).
pub fn git_in(dir: &Path, args: &[&str], input: Option<String>) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let writer = match (input, child.stdin.take()) {
        (Some(text), Some(mut stdin)) => Some(std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        })),
        _ => None,
    };
    let o = child.wait_with_output().map_err(|e| format!("cannot run git: {e}"))?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(o.stdout)
}

/// Whether an excerpt tagged `tag` may be shown in collective `here` (module doc).
#[must_use]
pub fn visible(tag: &str, here: &str) -> bool {
    tag.is_empty() || tag == here
}

/// The borg notes of `shas` that are visible in `here`, by commit: one `notes list` for the note
/// objects and one `cat-file --batch` for the notes of `shas` that have one.
pub fn notes_of(repo: &Path, here: &str, shas: &[String]) -> HashMap<String, Excerpt> {
    let list = git(repo, &["notes", &format!("--ref={}", notes::NOTES_REF), "list"]).unwrap_or_default();
    let wanted: HashSet<&str> = shas.iter().map(String::as_str).collect();
    let blobs: Vec<(&str, &str)> = list.lines().filter_map(|l| l.split_once(' ')).filter(|(_, commit)| wanted.contains(commit)).collect();
    if blobs.is_empty() {
        return HashMap::new();
    }
    let input: String = blobs.iter().flat_map(|(blob, _)| [*blob, "\n"]).collect();
    let Ok(out) = git_in(repo, &["cat-file", "--batch"], Some(input)) else { return HashMap::new() };
    let mut found = HashMap::new();
    let mut rest = &out[..];
    for (_, commit) in blobs {
        // "<oid> blob <size>\n<content>\n", or "<oid> missing\n".
        let Some(nl) = rest.iter().position(|&b| b == b'\n') else { break };
        let head = String::from_utf8_lossy(&rest[..nl]).into_owned();
        rest = &rest[nl + 1..];
        let Some(size) = head.split(' ').nth(2).and_then(|n| n.parse::<usize>().ok()) else { continue };
        if rest.len() < size {
            break;
        }
        let text = String::from_utf8_lossy(&rest[..size]).into_owned();
        rest = rest.get(size + 1..).unwrap_or_default();
        if let Some(e) = Excerpt::from_note(commit, &text).filter(|e| visible(&e.collective, here)) {
            found.insert(commit.to_string(), e);
        }
    }
    found
}

/// The newest excerpt visible in `here` for each of `shas`, from the store's reasoning.jsonl,
/// read once.
pub fn stored_of<S: std::hash::BuildHasher>(store: &Path, here: &str, shas: &HashSet<&str, S>) -> HashMap<String, Excerpt> {
    let mut found: HashMap<String, Excerpt> = HashMap::new();
    if shas.is_empty() {
        return found;
    }
    let text = std::fs::read_to_string(store.join("reasoning.jsonl")).unwrap_or_default();
    for e in text.lines().filter_map(|l| serde_json::from_str::<Excerpt>(l).ok()) {
        if e.kind != "reasoning" || !shas.contains(e.commit.as_str()) || !visible(&e.collective, here) {
            continue;
        }
        if found.get(&e.commit).is_none_or(|old| e.ms > old.ms) {
            found.insert(e.commit.clone(), e);
        }
    }
    found
}

/// The reasoning visible in `here` for each of `shas`: its note, else the newest store excerpt.
pub fn reasoning_for(repo: &Path, store: &Path, here: &str, shas: &[String]) -> HashMap<String, Excerpt> {
    let mut found = notes_of(repo, here, shas);
    let missing: HashSet<&str> = shas.iter().map(String::as_str).filter(|s| !found.contains_key(*s)).collect();
    let stored = stored_of(store, here, &missing);
    found.extend(stored);
    found
}
