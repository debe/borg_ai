//! `borg setup --home --yes` as a process: the collectives config, the wiring and the skill it writes,
//! and a second run that changes nothing (install/setup.rs).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use borg::install::Targets;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-setup-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// borg with a clean environment and no stdin: nothing of the caller's config leaks in. PATH holds
/// only `bin` (borg itself, which the adapters need to find).
fn borg(args: &[&str], bin: &Path) -> std::process::Output {
    let out =
        Command::new(env!("CARGO_BIN_EXE_borg")).args(args).env_clear().env("BORG_UPDATE_CHECK", "off").env("PATH", bin).stdin(Stdio::null()).output().unwrap();
    eprintln!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    out
}

fn all_files(d: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    for e in std::fs::read_dir(d).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            all_files(&p, out);
        } else {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out.sort();
}

#[test]
fn setup_yes_writes_config_wiring_and_skill_and_a_second_run_changes_nothing() {
    let root = tmp("yes");
    let (home, bin) = (root.join("home"), root.join("bin"));
    std::fs::create_dir_all(home.join("src/app")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_borg"), bin.join("borg")).unwrap();
    let h = home.to_str().unwrap();
    let args = ["setup", "--home", h, "--yes", "--no-vectors", "--harness", "claude,opencode", "--private", &format!("{h}/src")];

    let out = borg(&args, &bin);
    assert_eq!(out.status.code(), Some(0));
    let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join(".config/borg/collectives.json")).unwrap()).unwrap();
    assert_eq!(cfg, serde_json::json!({"default": "off", "collectives": [{"name": "private", "paths": ["~/src"], "share": "local"}], "personal": "private"}));
    let settings = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
    assert!(settings.contains("hook claude stop"), "{settings}");
    let t = Targets::under(&home);
    for name in ["claude", "opencode"] {
        let h = borg::harness::by_name(name).unwrap();
        assert!(t.path(h).is_file(), "{name} wired");
        assert!(t.skill_dir(h).unwrap().join("SKILL.md").is_file(), "{name} skill");
    }
    assert!(!home.join(".codex").exists(), "only the named harnesses");

    let mut before = Vec::new();
    all_files(&home, &mut before);
    let out = borg(&args, &bin);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to change"));
    let mut after = Vec::new();
    all_files(&home, &mut after);
    assert_eq!(before.iter().map(|f| &f.0).collect::<Vec<_>>(), after.iter().map(|f| &f.0).collect::<Vec<_>>());
    assert!(before == after, "a second run changes nothing");
}

#[test]
fn setup_without_a_terminal_needs_yes() {
    let home = tmp("tty");
    let out = borg(&["setup", "--home", home.to_str().unwrap(), "--no-vectors"], Path::new(""));
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert!(std::fs::read_dir(&home).unwrap().next().is_none(), "nothing written");
}
