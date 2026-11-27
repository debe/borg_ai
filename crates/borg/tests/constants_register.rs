//! docs/design/constants.md, the register of the numeric constants by kind, against the code: each row
//! names what its file defines (a removed constant's row goes with it), and each numeric constant whose
//! doc comment starts with its kind is in the register. A row whose files are not in this checkout (an
//! evaluation kept local) is skipped.

use std::path::{Path, PathBuf};

/// The kinds a doc comment starts with (the register's "Kinds").
const KINDS: [&str; 8] = ["Protocol", "Guard", "Derived", "Learned", "Method", "Baseline", "Display", "Experiment"];

/// The numeric types a registered constant has.
const NUMERIC: [&str; 12] = ["u8", "u16", "u32", "u64", "u128", "usize", "i32", "i64", "f32", "f64", "Duration", "std::time::Duration"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn register() -> String {
    std::fs::read_to_string(root().join("docs/design/constants.md")).unwrap()
}

/// A cell made of backticked items only (`a`, `b`): the items; none for any other cell.
fn ticked(cell: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = cell.split('`').collect();
    let between_ok = parts.iter().step_by(2).all(|p| p.is_empty() || *p == ", ");
    (parts.len() >= 3 && parts.len() % 2 == 1 && between_ok).then(|| parts.iter().skip(1).step_by(2).copied().collect())
}

fn ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `text` holds `word` as a whole identifier.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back().is_none_or(|c| !ident(c));
        let after = text[i + word.len()..].chars().next().is_none_or(|c| !ident(c));
        before && after
    })
}

#[test]
fn every_row_names_what_its_file_defines() {
    let reg = register();
    let mut stale = Vec::new();
    for line in reg.lines().filter(|l| l.starts_with("| `")) {
        let mut cells = line.trim_start_matches("| ").split(" | ");
        let (Some(files), Some(names)) = (cells.next().and_then(ticked), cells.next().and_then(ticked)) else { continue };
        let sources: Vec<String> = files
            .iter()
            .flat_map(|f| [root().join("crates").join(f), root().join("crates/borg/assets/adapters").join(f)])
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect();
        if sources.is_empty() {
            continue;
        }
        for name in names.iter().filter(|n| n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| ident(c) || c == ':')) {
            let last = name.rsplit("::").next().unwrap_or(name);
            if !sources.iter().any(|s| has_word(s, last)) {
                stale.push(format!("{} {name}", files.join(", ")));
            }
        }
    }
    assert!(stale.is_empty(), "rows naming what their files no longer define: {stale:#?}");
}

/// Every `.rs` file under `dir`.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The numeric constants of `text` whose doc comment starts with a kind.
fn labelled(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let decl = line.trim_start().trim_start_matches("pub(crate) ").trim_start_matches("pub ");
        let Some((name, ty)) = decl.strip_prefix("const ").and_then(|d| d.split_once(':')) else { continue };
        if !NUMERIC.iter().any(|t| ty.trim_start().split([' ', '=']).next() == Some(*t)) {
            continue;
        }
        let doc = lines[..i].iter().rev().skip_while(|l| l.trim_start().starts_with("#[")).take_while(|l| l.trim_start().starts_with("///")).last();
        let first = doc.map(|l| l.trim_start().trim_start_matches("///").trim_start()).unwrap_or_default();
        if KINDS.iter().any(|k| first.strip_prefix(k).is_some_and(|rest| rest.starts_with([':', ' ', '(', ',']))) {
            out.push(name.trim());
        }
    }
    out
}

#[test]
fn every_kind_labelled_constant_is_in_the_register() {
    let reg = register();
    let mut files = Vec::new();
    for e in std::fs::read_dir(root().join("crates")).unwrap().flatten() {
        sources(&e.path().join("src"), &mut files);
    }
    // The page is a workspace of its own inside borg-web.
    sources(&root().join("crates/borg-web/ui/src"), &mut files);
    let mut missing = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for name in labelled(&text) {
            if !reg.contains(&format!("`{name}`")) {
                missing.push(format!("{} {name}", f.strip_prefix(root()).unwrap_or(f).display()));
            }
        }
    }
    assert!(missing.is_empty(), "constants labelled with their kind but not in docs/design/constants.md: {missing:#?}");
}

#[test]
fn the_checks_read_what_they_should() {
    assert_eq!(ticked("`borg/src/top.rs`, `borg/src/top_live.rs`"), Some(vec!["borg/src/top.rs", "borg/src/top_live.rs"]));
    assert_eq!(ticked("12, 8 chars"), None);
    assert!(has_word("const LABEL: usize", "LABEL") && !has_word("const LABEL_CHARS: usize", "LABEL"));
    let code = "/// Display: a label.\npub const LABEL: usize = 34;\n/// The port.\nconst PORT: u16 = 1;\n/// Guard: a cap.\n#[allow(x)]\npub(crate) const CAP: Duration = x;\n";
    assert_eq!(labelled(code), vec!["LABEL", "CAP"]);
}
