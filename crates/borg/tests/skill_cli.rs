//! The installed `borg` skill (install/skill.rs `FILES`) names only commands and flags that exist: every
//! `borg <command>` in it is in `borg help`, every `borg mesh <command>` in borg-mesh's usage, and every
//! flag it gives `setup`, `install`, `uninstall`, `vectors` or `mesh` is one that command accepts. So the
//! skill cannot drift from the CLI it teaches.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use borg::install::skill::FILES;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn borg(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_borg")).args(args).env_clear().env("BORG_UPDATE_CHECK", "off").stdin(Stdio::null()).output().unwrap()
}

/// The command-like text of a Markdown file: its inline code spans and its fenced code lines, without
/// shell comments. A `borg doctor` line (`WARN ...`) is output, not a command.
fn code(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if fenced {
            out.push(line.split(" #").next().unwrap_or_default().to_string());
        } else {
            let spans = line.split('`').skip(1).step_by(2);
            out.extend(spans.filter(|s| !["ok ", "WARN ", "FAIL ", "..."].iter().any(|p| s.starts_with(p))).map(str::to_string));
        }
    }
    out
}

/// One mention: the command after `borg`, then the words after it.
fn mentions(snippet: &str) -> Vec<(String, Vec<String>)> {
    let words: Vec<&str> = snippet.split_whitespace().collect();
    let at = words.iter().enumerate().filter(|(_, w)| **w == "borg" || w.ends_with("/borg")).map(|(i, _)| i);
    at.filter_map(|i| {
        let cmd = words.get(i + 1).filter(|w| w.chars().all(|c| c.is_ascii_lowercase()))?;
        let rest = words[i + 2..].iter().take_while(|w| **w != "borg" && !matches!(**w, "&&" | ";" | "|"));
        Some(((*cmd).to_string(), rest.map(|w| (*w).to_string()).collect()))
    })
    .collect()
}

/// The flags among `words`: `[--relay`, `--fetch\|--rebuild` and `--paths,` count.
fn flags(words: &[String]) -> Vec<String> {
    let parts = words.iter().flat_map(|w| w.split('|').map(|p| p.trim_matches(|c: char| "[]().,\\".contains(c)).to_string()).collect::<Vec<_>>());
    parts.filter(|p| p.starts_with("--") && p.len() > 2).collect()
}

fn all_mentions() -> Vec<(String, Vec<String>)> {
    FILES.iter().flat_map(|(_, md)| code(md)).flat_map(|s| mentions(&s)).collect()
}

#[test]
fn every_markdown_file_of_the_skill_is_installed() {
    let dir = root().join("crates/borg/assets/skills/borg");
    let mut on_disk = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for p in std::fs::read_dir(&d).unwrap().flatten().map(|e| e.path()) {
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "md") {
                on_disk.push(p.strip_prefix(&dir).unwrap().display().to_string());
            }
        }
    }
    let shipped: BTreeSet<String> = FILES.iter().map(|(f, _)| (*f).to_string()).collect();
    assert_eq!(on_disk.into_iter().collect::<BTreeSet<_>>(), shipped, "install/skill.rs FILES lists every file under assets/skills/borg");
}

#[test]
fn every_borg_command_the_skill_names_is_in_borg_help() {
    let out = borg(&["help"]);
    assert_eq!(out.status.code(), Some(0));
    let help = String::from_utf8_lossy(&out.stdout);
    let mut known: BTreeSet<String> = help.lines().filter(|l| l.starts_with("  ")).filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect();
    known.insert("help".into());
    let named: BTreeSet<String> = all_mentions().into_iter().map(|(c, _)| c).collect();
    assert!(named.len() > 10, "the skill names commands: {named:?}");
    let unknown: Vec<&String> = named.difference(&known).collect();
    assert!(unknown.is_empty(), "the skill names commands borg help does not list: {unknown:?}\n{help}");
}

/// borg-mesh's usage header: its commands, and the flags its source reads.
fn mesh() -> (BTreeSet<String>, String) {
    let read = |f: &str| std::fs::read_to_string(root().join("crates/borg-mesh/src").join(f)).unwrap();
    // join's own flags (`--paths`) are read beside its config change.
    let src = read("main.rs") + &read("invite/join_config.rs");
    let cmds = src.lines().take_while(|l| l.starts_with("//!")).filter_map(|l| l.split("borg-mesh ").nth(1)?.split_whitespace().next().map(str::to_string));
    (cmds.collect(), src)
}

#[test]
fn every_mesh_command_and_flag_the_skill_names_exists_in_borg_mesh() {
    let (cmds, src) = mesh();
    let used: Vec<Vec<String>> = all_mentions().into_iter().filter(|(c, _)| c == "mesh").map(|(_, rest)| rest).collect();
    assert!(used.len() > 5, "the skill teaches the mesh");
    for rest in used {
        if let Some(sub) = rest.first().filter(|w| !w.starts_with('-')) {
            assert!(cmds.contains(sub), "borg mesh {sub}: not in borg-mesh's usage {cmds:?}");
        }
        for f in flags(&rest).into_iter().filter(|f| f != "--help") {
            assert!(src.contains(&format!("\"{f}\"")), "borg mesh {f}: borg-mesh reads no such flag");
        }
    }
}

/// `vectors` ignores flags it does not know, so a flag it accepts is one its source reads.
#[test]
fn every_vectors_flag_the_skill_names_is_read_by_borg_vectors() {
    let src = std::fs::read_to_string(root().join("crates/borg/src/vectors.rs")).unwrap();
    let used: Vec<String> = all_mentions().into_iter().filter(|(c, _)| c == "vectors").flat_map(|(_, rest)| flags(&rest)).collect();
    assert!(used.iter().any(|f| f == "--fetch"), "{used:?}");
    for f in used {
        assert!(src.contains(&format!("\"{f}\"")), "borg vectors {f}: vectors.rs reads no such flag");
    }
}

/// A flag `setup` does not know fails its parse with "unknown flag"; a trailing unknown flag makes every
/// probe fail there, before setup asks or writes anything.
#[test]
fn every_setup_flag_the_skill_names_is_accepted() {
    let used: BTreeSet<String> = all_mentions().into_iter().filter(|(c, _)| c == "setup").flat_map(|(_, rest)| flags(&rest)).collect();
    assert!(used.contains("--join"), "{used:?}");
    for f in used {
        let out = borg(&["setup", &f, "--zz-probe"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains(&format!("unknown flag {f}")), "borg setup {f}: {err}");
        assert!(matches!(out.status.code(), Some(0 | 2)), "borg setup {f}: {err}");
    }
}

/// `install` and `uninstall` stop at their usage on any flag they do not know; under `--home` a known
/// one runs.
#[test]
fn every_install_flag_the_skill_names_is_accepted() {
    let home = std::env::temp_dir().join(format!("borg-skill-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_str().unwrap();
    let used: BTreeSet<(String, String)> = all_mentions()
        .into_iter()
        .filter(|(c, _)| matches!(c.as_str(), "install" | "uninstall"))
        .flat_map(|(c, rest)| flags(&rest).into_iter().map(move |f| (c.clone(), f)))
        .collect();
    assert!(used.contains(&("install".into(), "--apply".into())), "{used:?}");
    for (cmd, f) in used {
        let mut args = vec![cmd.as_str(), "all", "--home", h, &f];
        if f == "--home" {
            args.push(h);
        }
        let out = borg(&args);
        assert_eq!(out.status.code(), Some(0), "borg {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
    }
    let _ = std::fs::remove_dir_all(&home);
}
