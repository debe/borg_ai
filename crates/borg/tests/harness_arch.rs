//! The harness abstraction's gates that need no tools (docs/harnesses.md, scripts/arch.sh prints the
//! rest): outside `src/harness/` no code branches on a harness's name (M1; the deferred files in
//! [`M1_DEFERRED`] aside), and `harness::ALL` is the one table of harness names (M2). Every harness module
//! is in it, and docs/harnesses.md names every answer key only an adapter gets.

use std::path::{Path, PathBuf};

use regex::Regex;

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(rust_files(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// `path`'s lines before its unit tests, numbered from 1.
fn code_lines(path: &Path) -> Vec<(usize, String)> {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines().take_while(|l| !l.starts_with("#[cfg(test)]")).enumerate().map(|(i, l)| (i + 1, l.to_string())).collect()
}

fn outside_harness() -> Vec<PathBuf> {
    rust_files(&src()).into_iter().filter(|p| !p.starts_with(src().join("harness"))).collect()
}

const NAMES: &str = "claude|omp|opencode|codex|sim";
const VARIANTS: &str = "Claude|Omp|Opencode|Codex|Sim";

/// Files that still branch per harness, each a follow-up of its own (docs/harnesses.md): borg's model
/// runners (runner.rs, `Runner::Claude => …`) and the import of other agents' history (assimilate*.rs,
/// `Source::Claude => …`). M1 does not count them; scripts/arch.sh reports their branches apart.
const M1_DEFERRED: &[&str] = &["runner.rs", "assimilate.rs", "assimilate_dir.rs"];

#[test]
fn m1_no_code_outside_the_harness_modules_branches_on_a_harness_name() {
    // A comparison or match arm on a name, or on an enum variant named after a harness (any enum's).
    let branch = Regex::new(&format!(r#""({NAMES})"\s*(=>|\||==)|(==|!=)\s*"({NAMES})"|\b\w+::({VARIANTS})\s*=>|(==|!=)\s*\w+::({VARIANTS})\b"#)).unwrap();
    let mut found = Vec::new();
    for f in outside_harness().into_iter().filter(|f| !M1_DEFERRED.iter().any(|d| f.ends_with(d))) {
        let text = std::fs::read_to_string(&f).unwrap();
        for (i, l) in text.lines().enumerate() {
            if branch.is_match(l) {
                found.push(format!("{}:{}: {}", f.display(), i + 1, l.trim()));
            }
        }
    }
    assert!(found.is_empty(), "ask the harness's Caps, wiring or methods instead (src/harness/mod.rs):\n{}", found.join("\n"));
    // The regex sees what it is meant to: a branch on another enum's variant, in a deferred file.
    let runner = std::fs::read_to_string(src().join("runner.rs")).unwrap();
    assert!(runner.lines().any(|l| branch.is_match(l)), "runner.rs branches per runner today");
}

/// Model runners (runner.rs, borg's own model calls through a CLI) are not harnesses yet: folding
/// them into the trait is a follow-up (docs/harnesses.md).
const NOT_HARNESS_TABLES: &[&str] = &["runner.rs"];

#[test]
fn m2_all_is_the_one_table_of_harness_names() {
    // A row of a name table (a variant or type mapped to a harness's name), or a list of names.
    let row = Regex::new(&format!(r#"=>\s*"({NAMES})"\s*,?\s*$"#)).unwrap();
    let name = Regex::new(&format!(r#""({NAMES})""#)).unwrap();
    let list = |l: &str| name.captures_iter(l).map(|c| c[1].to_string()).collect::<std::collections::BTreeSet<_>>().len() >= 2;
    let mut tables = Vec::new();
    for f in outside_harness() {
        let rows = code_lines(&f).into_iter().filter(|(_, l)| row.is_match(l) || list(l)).count();
        let file = f.file_name().unwrap().to_string_lossy().into_owned();
        if rows > 0 && !NOT_HARNESS_TABLES.contains(&file.as_str()) {
            tables.push(format!("{} ({rows} rows)", f.display()));
        }
    }
    assert!(tables.is_empty(), "harness names live in harness::ALL only:\n{}", tables.join("\n"));
    let enums: Vec<String> =
        rust_files(&src()).into_iter().filter(|f| code_lines(f).iter().any(|(_, l)| l.contains("enum Harness"))).map(|f| f.display().to_string()).collect();
    assert!(enums.is_empty(), "no harness enum beside the trait: {enums:?}");
}

#[test]
fn every_harness_module_is_registered_in_all() {
    let dir = src().join("harness");
    let all = std::fs::read_to_string(dir.join("mod.rs")).unwrap();
    let all = all.lines().find(|l| l.starts_with("pub static ALL")).expect("pub static ALL in harness/mod.rs").to_string();
    let imp = Regex::new(r"impl Harness for (\w+)").unwrap();
    let mut modules = 0;
    for f in rust_files(&dir) {
        let text = std::fs::read_to_string(&f).unwrap();
        for c in imp.captures_iter(&text) {
            let module = f.file_stem().unwrap().to_string_lossy();
            assert!(all.contains(&format!("&{module}::{}", &c[1])), "{} implements Harness but is not in ALL: {all}", f.display());
            modules += 1;
        }
    }
    assert_eq!(modules, borg::harness::ALL.len(), "one module per harness");
}

/// The answer keys only an adapter gets (hook.rs `for_harness` takes them out for a `CommandHooks`
/// harness, which parses the answer itself) are each named in docs/harnesses.md, so the page that says
/// what an adapter reads from borg's answer keeps up with what borg sends.
#[test]
fn docs_name_every_answer_key_only_an_adapter_gets() {
    let hook = std::fs::read_to_string(src().join("hook.rs")).unwrap();
    let body: Vec<&str> = hook.lines().skip_while(|l| !l.starts_with("fn for_harness(")).take_while(|l| *l != "}").collect();
    let key = Regex::new(r#"shift_remove\("(\w+)"\)"#).unwrap();
    let keys: Vec<String> = body.iter().flat_map(|l| key.captures_iter(l).map(|c| c[1].to_string())).collect();
    assert!(!keys.is_empty(), "hook.rs `for_harness` removes no key this test can read: follow it");
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/harnesses.md")).unwrap();
    let missing: Vec<&String> = keys.iter().filter(|k| !doc.contains(&format!("`\"{k}\"`"))).collect();
    assert!(missing.is_empty(), "docs/harnesses.md does not name {missing:?}, sent to adapters only (hook.rs `for_harness`)");
}
