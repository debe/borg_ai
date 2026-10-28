//! Team outcomes through the binary (outcome.rs "Team outcomes"): `borg learn` publishes a redacted
//! projection of the local outcomes to `outcomes-team.jsonl`, and `borg debug memory` counts a teammate's
//! synced result once and this machine's own published copy not at all. No model call
//! (BORG_LEARN_MODEL=off), no real store.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn setup(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-outcome-share-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = root.join("store/collectives/corp");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(store.join("lessons.jsonl"), format!("{}\n", json!({"id": "L1", "origin_ms": 1, "question": "Is the lock order kept?"}))).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("corp")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let o = |kind: &str, ms: i64, extra: Value| {
        let mut v = json!({"kind": kind, "session": "sess-raw-1", "lessons": ["L1"], "ms": ms, "repo": "secret-repo", "groups": [["L1", "L2"]]});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        format!("{v}\n")
    };
    let log = o("asked", now - 2, json!({"p": [0.25], "grades": [3], "ranks": [0], "applied": 1}))
        + &o("asked_then_changed", now - 1, json!({}))
        + &o("graded", now - 3, json!({"chosen": ["L1"], "p": [0.25]}));
    std::fs::write(store.join("outcomes.jsonl"), log).unwrap();
    root
}

fn borg(root: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store/collectives/corp"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN_MODEL", "off")
        .env("BORG_VECTORS", "off")
        .envs(env.iter().copied())
        .current_dir(root)
        .stderr(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into()
}

fn team(root: &Path) -> String {
    std::fs::read_to_string(root.join("store/collectives/corp/outcomes-team.jsonl")).unwrap_or_default()
}

fn changed(root: &Path, env: &[(&str, &str)]) -> u64 {
    let lessons = root.join("store/collectives/corp/lessons.jsonl");
    let out = borg(root, &["debug", "memory", lessons.to_str().unwrap()], env);
    let v: Value = serde_json::from_str(out.lines().next().unwrap_or_else(|| panic!("no memory: {out:?}"))).unwrap();
    v["changed"].as_u64().unwrap()
}

#[test]
fn learn_publishes_redacted_outcomes_with_their_propensity() {
    let root = setup("publish");
    borg(&root, &["learn"], &[]);
    let text = team(&root);
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"asked") && kinds.contains(&"asked_then_changed"), "{text}");
    assert!(!kinds.contains(&"graded"), "graded stays local: {text}");
    for raw in ["sess-raw-1", "secret-repo", "groups", "grades", "chosen"] {
        assert!(!text.contains(raw), "{raw} leaked: {text}");
    }
    let asked = lines.iter().find(|l| l["kind"] == "asked").unwrap();
    assert_eq!(asked["p"], json!([0.25]), "{asked}");
    assert_eq!(asked["s"].as_str().unwrap().len(), 32);
    // A second run adds nothing; off, nothing is published.
    borg(&root, &["learn"], &[]);
    assert_eq!(team(&root), text);
    let off = setup("publish-off");
    borg(&off, &["learn"], &[("BORG_SHARE_OUTCOMES", "off")]);
    assert_eq!(team(&off), "");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(off);
}

#[test]
fn memory_counts_a_teammate_s_result_once_and_its_own_copy_never() {
    let root = setup("memory");
    assert_eq!(changed(&root, &[]), 1);
    borg(&root, &["learn"], &[]);
    assert!(team(&root).contains("asked_then_changed"));
    assert_eq!(changed(&root, &[]), 1, "this machine's published copy does not count again");
    // A teammate's change of L1, synced twice (from two nodes' copies).
    let remote = |node: &str| json!({"kind": "asked_then_changed", "s": "0123456789abcdef0123456789abcdef", "lessons": ["L1"], "ms": 5, "node": node});
    let more = format!("{}{}\n{}\n", team(&root), remote("n2"), remote("n3"));
    std::fs::write(root.join("store/collectives/corp/outcomes-team.jsonl"), more).unwrap();
    assert_eq!(changed(&root, &[]), 2, "the teammate's session counts once");
    assert_eq!(changed(&root, &[("BORG_SHARE_OUTCOMES", "off")]), 1, "off: this machine's outcomes only");
    let _ = std::fs::remove_dir_all(root);
}
