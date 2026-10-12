//! `borg install|status|doctor --home` as a process: the environment it inherits must not reach
//! outside the given HOME (install.rs `home_overrides`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-install-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// borg with a clean, fully redirected environment: nothing of the caller's borg/harness config leaks in.
fn borg(args: &[&str], env: &[(&str, &Path)]) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args).env_clear().env("PATH", "");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

#[test]
fn home_flag_never_touches_borg_home_or_config_from_the_environment() {
    let root = tmp("isolate");
    let (home, real_store, real_cfg) = (root.join("home"), root.join("real-store"), root.join("real-cfg.json"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(&real_cfg, r#"{"collectives":[{"name":"corp","paths":["/w"]}]}"#).unwrap();
    let env = [("BORG_HOME", real_store.as_path()), ("BORG_CONFIG", real_cfg.as_path()), ("BORG_BIN", root.as_path())];

    let out = borg(&["doctor", "--home", home.to_str().unwrap()], &env);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!real_store.exists(), "doctor probed the real store:\n{text}");
    assert!(!text.contains("real-cfg") && !text.contains("BORG_BIN="), "{text}");
    assert!(text.contains(&home.join(".local/share/borg").display().to_string()), "{text}");

    let out = borg(&["status", "--home", home.to_str().unwrap()], &env);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("real-store") && !text.contains("real-cfg"), "{text}");
    assert!(!real_store.exists());
}

#[test]
fn install_refuses_a_binary_not_named_borg() {
    let home = tmp("borg-ai");
    let bin = home.join("borg-ai");
    for _ in 0..3 {
        let out = borg(&["install", "claude", "--apply", "--home", home.to_str().unwrap()], &[("BORG_BIN", &bin)]);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("not named borg"));
    }
    assert!(!home.join(".claude/settings.json").exists());
}
