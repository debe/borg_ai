//! `borg trust` and the status line respect the collective boundary: trust is computed from the
//! routed collective's own store only, and an off repo has none.

use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-trust-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn borg(args: &[&str], root: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", "")
        .env("HOME", root.join("home"))
        .env("BORG_MATCH", "bm25") // word matching only: no model call from a test
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(store: &Path, lessons: &[serde_json::Value], outcomes: &[serde_json::Value]) {
    std::fs::create_dir_all(store).unwrap();
    let lines = |v: &[serde_json::Value]| v.iter().map(|l| format!("{l}\n")).collect::<String>();
    std::fs::write(store.join("lessons.jsonl"), lines(lessons)).unwrap();
    std::fs::write(store.join("outcomes.jsonl"), lines(outcomes)).unwrap();
}

#[test]
fn trust_is_per_collective_store_and_off_has_none() {
    use serde_json::json;
    let root = tmp("routing");
    let (corp, private, off) = (root.join("corp"), root.join("private"), root.join("corp/secret"));
    for d in [&corp, &private, &off] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "off": [off], "collectives": [
            {"name": "corp", "paths": [corp], "share": "team"},
            {"name": "private", "paths": [private], "share": "local"}]})
        .to_string(),
    )
    .unwrap();
    let base = root.join("store");
    // The same engineer, opposite evidence in each collective.
    let lesson = json!({"id": "L1", "origin_ms": 1, "question": "q", "area": "concurrency", "engineers": ["ann@x"]});
    let o = |kind: &str| json!({"kind": kind, "session": "s", "lessons": ["L1"], "ms": 2, "repo": "r"});
    write(&borg::collective::store(&base, "corp"), std::slice::from_ref(&lesson), &[o("asked_not_corrected")]);
    write(&borg::collective::store(&base, "private"), &[lesson], &[o("asked_then_corrected")]);

    let c = borg(&["trust", "--dir", corp.to_str().unwrap()], &root);
    assert!(c.starts_with("collective corp") && c.contains("ann@x") && c.contains("0.67") && !c.contains("0.33"), "{c}");
    assert!(c.contains("sessions on this machine and the team's synced outcomes"), "{c}");
    let p = borg(&["trust", "--dir", private.to_str().unwrap()], &root);
    assert!(p.starts_with("collective private") && p.contains("0.33") && !p.contains("0.67"), "{p}");
    let x = borg(&["trust", "--dir", off.to_str().unwrap()], &root);
    assert!(x.starts_with("off:") && !x.contains("ann@x"), "{x}");
    let unlisted = borg(&["trust", "--dir", root.join("elsewhere").to_str().unwrap()], &root);
    assert!(unlisted.starts_with("off:"), "{unlisted}");

    let status = borg(&["status"], &root);
    assert!(status.contains("trust        highest ann@x in concurrency 0.67"), "{status}");
    assert!(status.contains("trust        highest ann@x in concurrency 0.33"), "{status}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_engineers_emails_count_once_and_a_bad_section_routes_off() {
    use serde_json::json;
    let root = tmp("aliases");
    let corp = root.join("corp");
    std::fs::create_dir_all(&corp).unwrap();
    let config = |engineers: serde_json::Value| {
        let c = json!({"default": "off", "collectives": [{"name": "corp", "paths": [corp]}], "engineers": engineers});
        std::fs::write(root.join("collectives.json"), c.to_string()).unwrap();
    };
    let l = |id: &str, e: &str| json!({"id": id, "origin_ms": 1, "question": "q", "area": "scope", "engineers": [e]});
    write(&borg::collective::store(&root.join("store"), "corp"), &[l("a", "Dana.Example@corp.example"), l("b", "da@home.example")], &[]);

    config(json!([]));
    let status = borg(&["status"], &root);
    assert!(status.contains("trust        no evidence yet (2 engineers neutral"), "no aliases: unchanged\n{status}");

    config(json!([{"name": "dana", "emails": ["dana.example@corp.example", "DA@home.example"]}]));
    let status = borg(&["status"], &root);
    assert!(status.contains("trust        no evidence yet (1 engineers neutral"), "{status}");
    let t = borg(&["trust", "--dir", corp.to_str().unwrap()], &root);
    assert!(t.contains("\ndana") && !t.contains("@"), "{t}");

    config(json!([{"name": "dana", "emails": "da@home.example"}]));
    let status = borg(&["status"], &root);
    assert!(status.contains("COLLECTIVES CONFIG INVALID") && status.contains("every repo routes OFF"), "{status}");
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["trust", "--dir", corp.to_str().unwrap()])
        .env_clear()
        .env("PATH", "")
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.code() == Some(1) && err.contains("invalid collectives config") && err.contains("engineers"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}
