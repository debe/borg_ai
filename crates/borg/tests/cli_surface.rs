//! The CLI's surface agrees with itself: `borg help`, `borg debug help`, the completions and the
//! dispatch name the same commands (completions.rs's one table), and the commands 0.3.0 removed or
//! moved under `borg debug` exit non-zero at their old names.
//!
//! Every probe runs in an empty home whose collectives config routes everything off, with fake model
//! CLIs first on PATH: no probe writes outside it or makes a model call.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-cli-surface-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home")).unwrap();
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::write(d.join("collectives.json"), r#"{"default": "off", "collectives": []}"#).unwrap();
    for model in ["claude", "codex", "omp", "opencode"] {
        let fake = d.join("bin").join(model);
        std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    d
}

/// `borg <args>` in `root`'s sandbox: (stdout, stderr, exit code).
fn borg(root: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let path = format!("{}:/usr/bin:/bin", root.join("bin").display());
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_STATE", root.join("state"))
        .env("BORG_LEARN", "off")
        .env("BORG_NOTIFY", "off")
        .env("BORG_VECTORS", "off")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code())
}

/// The commands a help text lists: the first word of each indented row.
fn listed(help: &str) -> BTreeSet<String> {
    help.lines().filter(|l| l.starts_with("  ")).filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
}

/// The words bash completes for `case` (`""`: the top-level names).
fn completed(bash: &str, case: &str) -> BTreeSet<String> {
    let line = match case {
        "" => bash.lines().find(|l| l.contains("compgen -W \"") && !l.contains("$words")).unwrap().split('"').nth(1).unwrap().to_string(),
        c => bash.lines().find(|l| l.trim_start().starts_with(&format!("{c}) words="))).unwrap().split('"').nth(1).unwrap().to_string(),
    };
    line.split_whitespace().filter(|w| !w.starts_with('-')).map(str::to_string).collect()
}

/// A harmless call of each listed command: it reaches the command's own handler (usage, an unknown
/// flag of its own, or an off repo) without starting a server, the network or a model.
const PROBES: &[(&str, &[&str])] = &[
    ("setup", &["--zz-probe"]),
    ("install", &[]),
    ("uninstall", &[]),
    ("status", &[]),
    ("doctor", &[]),
    ("collective", &["."]),
    ("vectors", &[]),
    ("completions", &[]),
    ("learn", &[]),
    ("no", &[]),
    ("assimilate", &["--zz-probe"]),
    ("reach", &["--zz-probe"]),
    ("facets", &["--zz-probe"]),
    ("queen", &[]),
    ("why", &[]),
    ("notes", &["--zz-probe"]),
    ("regress", &["--zz-probe"]),
    ("review-fix", &["--zz-probe"]),
    ("trust", &["--zz-probe"]),
    ("top", &["--once"]),
    ("web", &["--help"]),
    ("mesh", &["--help"]),
    ("steer", &[]),
];

const DEBUG_PROBES: &[(&str, &[&str])] = &[
    ("recall", &["missing.jsonl"]),
    ("resolve", &["missing.jsonl"]),
    ("prefilter", &["missing.jsonl"]),
    ("memory", &["missing.jsonl"]),
    ("expert", &["missing-repo"]),
    ("vectors", &["--query", "--store", "missing.jsonl"]),
    ("ope", &["--zz-probe"]),
];

#[test]
fn help_completions_and_dispatch_name_the_same_commands() {
    let root = tmp("agree");
    let (help, _, code) = borg(&root, &["help"]);
    assert_eq!(code, Some(0));
    let (bash, _, _) = borg(&root, &["completions", "bash"]);
    let main = listed(&help);
    let mut top = completed(&bash, "");
    assert!(top.remove("debug"), "debug completes: {top:?}");
    assert_eq!(main, top, "borg help and the completions list the same commands");
    for group in ["Setup", "Learning", "Decisions", "Watching", "Team"] {
        assert!(help.lines().any(|l| l == group), "group {group} in borg help:\n{help}");
    }
    assert!(help.contains("borg debug help"), "the main help points at the debug tools");
    // Every listed command has a probe, and its probe reaches its handler.
    assert_eq!(main, PROBES.iter().map(|(c, _)| c.to_string()).collect(), "a new command needs a probe here");
    for (cmd, rest) in PROBES {
        let args: Vec<&str> = std::iter::once(*cmd).chain(rest.iter().copied()).collect();
        let (out, err, _) = borg(&root, &args);
        assert!(!err.contains("borg: unknown command") && !err.contains("Usage: borg <command>"), "borg {args:?} is not dispatched: {out}{err}");
    }

    let (debug, _, code) = borg(&root, &["debug", "help"]);
    assert_eq!(code, Some(0));
    let mut debug_completed = completed(&bash, "debug");
    assert!(debug_completed.remove("help"));
    assert_eq!(listed(&debug), debug_completed, "borg debug help and its completions agree");
    assert_eq!(listed(&debug), DEBUG_PROBES.iter().map(|(c, _)| c.to_string()).collect());
    for (cmd, rest) in DEBUG_PROBES {
        let args: Vec<&str> = ["debug", cmd].into_iter().chain(rest.iter().copied()).collect();
        let (out, err, _) = borg(&root, &args);
        assert!(!err.contains("Usage: borg debug <command>"), "borg {args:?} is not dispatched: {out}{err}");
    }
    let (_, err, code) = borg(&root, &["debug", "nope"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("Usage: borg debug <command>"), "{err}");

    // The internal commands dispatch, and are never listed.
    for internal in ["hook", "check"] {
        assert!(!main.contains(internal) && !top.contains(internal), "{internal} is internal");
        let (_, err, _) = borg(&root, &[internal]);
        assert!(!err.contains("borg: unknown command"), "{internal}: {err}");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn removed_and_moved_commands_exit_non_zero_at_their_old_names() {
    let root = tmp("removed");
    // Each with the refusal that says it is gone: an unknown command, a flag its old command no longer
    // takes, or the command's usage. Any other failure (an off repo, a missing file) would pass a bare
    // non-zero check while the command still ran.
    let gone: &[(&[&str], &str)] = &[
        (&["top", "--watch"], "borg top: unknown flag --watch"),
        (&["top", "--once", "--watch"], "borg top: unknown flag --watch"),
        (&["queen", "next", "--json"], "usage: borg queen"),
        (&["queen", "notify"], "usage: borg queen"),
        (&["recall", "missing.jsonl"], "borg: unknown command recall"),
        (&["resolve", "missing.jsonl"], "borg: unknown command resolve"),
        (&["prefilter", "missing.jsonl"], "borg: unknown command prefilter"),
        (&["memory", "missing.jsonl"], "borg: unknown command memory"),
        (&["expert", "missing-repo", "--at", "HEAD", "--q", "x"], "borg: unknown command expert"),
        (&["ope"], "borg: unknown command ope"),
        (&["vectors", "--query"], "borg vectors: unknown flag --query"),
        (&["hive", "join"], "borg: unknown command hive"),
        (&["nope"], "borg: unknown command nope"),
    ];
    for (args, refusal) in gone {
        let (out, err, code) = borg(&root, args);
        assert_eq!(code, Some(2), "borg {args:?} still runs: {out}{err}");
        assert!(out.is_empty(), "borg {args:?} printed to stdout: {out}");
        assert!(err.starts_with(refusal), "borg {args:?} is not refused as gone ({refusal}): {err}");
    }
    // The usage refusals show the command's usage.
    for args in [&["queen", "next", "--json"][..], &["queen", "notify"]] {
        let (_, err, _) = borg(&root, args);
        assert!(err.contains("usage: borg "), "borg {args:?}: {err}");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn completions_leave_out_the_removed_commands() {
    let root = tmp("completions");
    let (bash, _, _) = borg(&root, &["completions", "bash"]);
    assert!(!bash.lines().any(|l| l.trim_start().starts_with("hive) words=")), "borg hive is gone");
    assert!(!bash.lines().find(|l| l.trim_start().starts_with("top) words=")).unwrap().contains("--watch"));
    assert!(!bash.lines().find(|l| l.trim_start().starts_with("queen) words=")).unwrap().contains("next"));
    let debug = completed(&bash, "debug");
    assert!(!debug.contains("hive") && debug.contains("recall"), "{debug:?}");
    let (fish, _, _) = borg(&root, &["completions", "fish"]);
    assert!(!fish.contains("__fish_seen_subcommand_from hive"), "{fish}");
    let _ = std::fs::remove_dir_all(root);
}
