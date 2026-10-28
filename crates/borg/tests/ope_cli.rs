//! `borg debug ope`: off-policy estimates of the belief's weights from a log, through the CLI: a file alone,
//! the routed collective's store, an off repo, and bad usage.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::json;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-ope-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn borg(args: &[&str], root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", "")
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .output()
        .unwrap()
}

/// Two logged Thompson checks over a pool of three unscoped lessons and three slots (each asked, p = 1,
/// as the current weights replay it), each asked lesson with a result.
fn log() -> String {
    let ask = |s: &str, ms: i64| {
        json!({"kind": "asked", "session": s, "lessons": ["A", "B", "C"], "ms": ms, "repo": "r", "p": [1.0, 1.0, 1.0],
               "pool": ["A", "B", "C"], "base": [1.0, 1.0, 1.0], "scoped": [false, false, false]})
    };
    let res = |kind: &str, s: &str, l: &str, ms: i64| json!({"kind": kind, "session": s, "lessons": [l], "ms": ms, "repo": "r"});
    [
        json!({"kind": "vetoed", "session": "h", "lessons": ["D"], "ms": 1, "repo": "r"}),
        ask("s1", 10),
        res("asked_then_changed", "s1", "A", 11),
        res("asked_ignored", "s1", "B", 11),
        res("asked_ignored", "s1", "C", 11),
        ask("s2", 20),
        res("asked_then_changed", "s2", "A", 21),
        res("asked_then_changed", "s2", "B", 21),
        res("asked_ignored", "s2", "C", 21),
        json!({"kind": "asked", "session": "s3", "lessons": ["A"], "ms": 30, "repo": "r"}),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect()
}

#[test]
fn ope_reads_a_file_and_prints_a_table_per_weight() {
    let root = tmp("file");
    let file = root.join("outcomes.jsonl");
    std::fs::write(&file, log()).unwrap();
    let out = borg(&["debug", "ope", "--file", file.to_str().unwrap()], &root);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.starts_with("file "), "{text}");
    assert!(text.contains("decisions: 2 with a pool") && text.contains("1 lessons without p"), "{text}");
    assert!(text.contains("grid: each weight ×0.5, ×1, ×2 around its current value, the others held"), "{text}");
    for knob in [
        "veto (current 3, evidence 1 records)",
        "verified (current 1",
        "rejected",
        "hacked",
        "regret (current 1",
        "verifier_regret (current 0.5",
        "tested (current 0.5",
    ] {
        assert!(text.contains(knob), "{knob}: {text}");
    }
    assert!(text.contains("value") && text.contains("estimate") && text.contains("95% CI") && text.contains("ESS"), "{text}");
    assert!(text.trim_end().ends_with("constants unchanged"), "{text}");
    let out = borg(&["debug", "ope", "--file", file.to_str().unwrap(), "--json"], &root);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((v["report"]["decisions"].as_u64(), v["report"]["rows"].as_array().map(Vec::len)), (Some(2), Some(21)), "{v}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ope_reads_the_routed_store_and_off_has_none() {
    let root = tmp("route");
    let (corp, off) = (root.join("corp"), root.join("elsewhere"));
    for d in [&corp, &off] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(root.join("collectives.json"), json!({"default": "off", "collectives": [{"name": "corp", "paths": [corp], "share": "local"}]}).to_string())
        .unwrap();
    let store = borg::collective::store(&root.join("store"), "corp");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("outcomes.jsonl"), log()).unwrap();
    let out = borg(&["debug", "ope", "--dir", corp.to_str().unwrap()], &root);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.starts_with("collective corp") && text.contains("decisions: 2"), "{text}");
    let out = borg(&["debug", "ope", "--dir", off.to_str().unwrap()], &root);
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).starts_with("off:"));
    // An empty store: too thin, nothing changed.
    std::fs::write(store.join("outcomes.jsonl"), "").unwrap();
    let text = String::from_utf8_lossy(&borg(&["debug", "ope", "--dir", corp.to_str().unwrap()], &root).stdout).into_owned();
    assert!(text.contains("too thin") && text.contains("constants unchanged"), "{text}");
    for bad in [&["debug", "ope", "--bogus"][..], &["debug", "ope", "--dir"], &["debug", "ope", "--dir", "a", "--file", "b"]] {
        assert_eq!(borg(bad, &root).status.code(), Some(2), "{bad:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
