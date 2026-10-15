//! Membership changes through the binary take effect on steering at once, without a daemon:
//! `revoke`, a demotion by `invite --role`, and `join` of a history that revoked the steerer each
//! lift that steerer's pause on this machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use borg_mesh::identity::Identity;
use borg_mesh::membership::{self, Membership};
use borg_mesh::steer::{Authority, file, new, sign};
use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-mesh-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("work/app")).unwrap();
    let cfg = json!({"default": "off", "collectives": [{"name": "corp", "paths": [d.join("work")], "share": "team"}]});
    std::fs::write(d.join("collectives.json"), cfg.to_string()).unwrap();
    d
}

fn mesh(root: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_borg-mesh"))
        .args(args)
        .args(["--dir", &root.join("work/app").display().to_string()])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into()
}

fn store(root: &Path) -> PathBuf {
    root.join("base/collectives/corp")
}

fn now() -> i64 {
    borg::learn::now_ms()
}

/// Files a pause signed by `by` into `root`'s store under `m`, as a mesh round would.
fn paused_by(root: &Path, m: &Membership, by: &Identity) {
    let home = store(root);
    let me = Identity::load_or_create(&home).unwrap().public();
    let d = sign(by, new("corp", "s@x", Default::default(), borg::steer::PAUSE, "", now()));
    assert_eq!(file(&home, "corp", &Authority::Team { m, me: &me }, &me, true, &d, now()), Ok(true));
}

fn pre_tool(root: &Path) -> Option<Value> {
    let ev = json!({"session_id": "s1", "cwd": root.join("work/app"), "tool_name": "Bash"});
    borg::steer::around(&store(root), true, "pre_tool", &ev, Some(0), now(), || None)
}

#[test]
fn revoke_demote_and_join_lift_the_steerers_pause_without_a_daemon() {
    let root = tmp("revoke");
    mesh(&root, &["init"]);
    let steerer = Identity::load_or_create(&root.join("steerer")).unwrap();
    let key = steerer.public();
    let load = |r: &Path| Membership::load(&store(r)).unwrap().unwrap();

    // Revoke.
    mesh(&root, &["invite", &key, "--role", "steer"]);
    paused_by(&root, &load(&root), &steerer);
    assert!(pre_tool(&root).is_some(), "paused");
    mesh(&root, &["revoke", &key]);
    assert_eq!(pre_tool(&root), None, "the revoked steerer's pause no longer holds");

    // Demote (a fresh key: a revoked one stays revoked).
    let other = Identity::load_or_create(&root.join("other")).unwrap();
    mesh(&root, &["invite", &other.public(), "--role", "steer"]);
    paused_by(&root, &load(&root), &other);
    assert!(pre_tool(&root).is_some(), "paused");
    mesh(&root, &["invite", &other.public(), "--role", "member"]);
    assert_eq!(pre_tool(&root), None, "the demoted steerer's pause no longer holds");

    // Join: a second machine holds a pause by a third steerer, then joins the history that revoked it.
    let third = Identity::load_or_create(&root.join("third")).unwrap();
    mesh(&root, &["invite", &third.public(), "--role", "steer"]);
    let peer = tmp("revoke-peer");
    mesh(&peer, &["id"]);
    paused_by(&peer, &load(&root), &third);
    assert!(pre_tool(&peer).is_some(), "paused on the peer");
    mesh(&root, &["revoke", &third.public()]);
    mesh(&peer, &["join", &membership::history_path(&store(&root)).display().to_string()]);
    assert_eq!(pre_tool(&peer), None, "joining the history that revoked it lifts the pause");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&peer);
}
