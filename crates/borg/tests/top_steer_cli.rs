//! Presence, `borg top` and steering through the binary: hooks keep a presence record per session,
//! `borg top` lists the drones, a directive in the inbox is shown once and a pause refuses tools,
//! and an off repo is left without a single file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-top-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn borg(root: &Path, args: &[&str], stdin: &str) -> (String, Option<i32>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_MATCH", "bm25") // word matching only: no model call from a test
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "off")
        .env("BORG_QUEEN", "off")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), out.status.code())
}

fn hook(root: &Path, harness: &str, event: &str, input: Value) -> Option<Value> {
    let (out, code) = borg(root, &["hook", harness, event], &input.to_string());
    assert_eq!(code, Some(0));
    (!out.trim().is_empty()).then(|| serde_json::from_str(out.trim()).unwrap())
}

fn repo(path: &Path, email: &str) {
    std::fs::create_dir_all(path).unwrap();
    let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(path).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["config", "user.email", email]);
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            out.extend(files(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = tmp(name);
    let (work, private, secret) = (root.join("work/app"), root.join("private/diary"), root.join("secret/x"));
    repo(&work, "alice@x");
    repo(&private, "alice@home");
    std::fs::create_dir_all(&secret).unwrap();
    let cfg = json!({"default": "off", "off": [root.join("secret")],
        "collectives": [{"name": "corp", "paths": [root.join("work")], "share": "team"},
                        {"name": "mine", "paths": [root.join("private")], "share": "local"}]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    (root, work, private, secret)
}

fn store(root: &Path, name: &str) -> PathBuf {
    root.join("base/collectives").join(name)
}

#[test]
fn hooks_keep_presence_and_top_lists_the_drones() {
    let (root, work, private, _) = setup("top");
    let w = work.display().to_string();
    hook(&root, "claude", "prompt", json!({"session_id": "s-one", "cwd": w, "prompt": "fix the login redirect\nsecond line"}));
    hook(&root, "claude", "post_tool", json!({"session_id": "s-one", "cwd": w, "tool_name": "Edit", "tool_input": {"file_path": format!("{w}/src/login.rs")}}));
    hook(&root, "omp", "prompt", json!({"session_id": "s-two", "cwd": w, "prompt": "write docs", "pid": 1}));
    hook(&root, "opencode", "prompt", json!({"session_id": "s-three", "cwd": private.display().to_string(), "prompt": "diary"}));

    let p: Value = serde_json::from_slice(&std::fs::read(store(&root, "corp").join("presence/s-one.json")).unwrap()).unwrap();
    assert_eq!((p["engineer"].as_str(), p["repo"].as_str(), p["collective"].as_str()), (Some("alice@x"), Some("app"), Some("corp")));
    assert_eq!(p["prompt"], "fix the login redirect");
    assert_eq!(p["files"], json!(["src/login.rs"]));
    assert_eq!(p["event"], "post_tool");

    let (out, code) = borg(&root, &["top"], "");
    assert_eq!(code, Some(0));
    assert!(out.contains("corp (team): 2 drone(s), 2 active, 0 remote"), "{out}");
    assert!(out.contains("mine (local, never leaves this machine): 1 drone(s)"), "{out}");
    let one = out.lines().find(|l| l.contains("s-one")).unwrap();
    assert!(one.contains("One of Two") && one.contains("alice@x") && one.contains("fix the login redirect") && one.contains("src/login.rs"), "{one}");

    let (out, _) = borg(&root, &["top", "--json", "--dir", &w], "");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1, "--dir: only the collective it routes to");
    assert_eq!(v[0]["collective"], "corp");
    assert_eq!(v[0]["drones"][1]["session"], "s-two");
    assert_eq!(v[0]["drones"][1]["designation"], "Two of Two");
    assert_eq!(v[0]["drones"][1]["harness"], "omp");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_off_repo_writes_nothing() {
    let (root, _, _, secret) = setup("off");
    let s = secret.display().to_string();
    for (ev, extra) in [
        ("prompt", json!({"prompt": "hi"})),
        ("pre_tool", json!({"tool_name": "Bash"})),
        ("post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": format!("{s}/a")}})),
        ("stop", json!({"last_assistant_message": "done"})),
        ("end", json!({})),
    ] {
        let mut input = json!({"session_id": "s1", "cwd": s});
        input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert_eq!(hook(&root, "claude", ev, input), None);
    }
    assert!(files(&root.join("base")).is_empty(), "{:?}", files(&root.join("base")));
    let _ = std::fs::remove_dir_all(&root);
}

fn accepted(id: &str, kind: &str, session: &str, text: &str, ms: i64) -> String {
    json!({"id": id, "collective": "corp", "key": "ab".repeat(32), "engineer": "bob@x", "target": {"node": "", "session": session},
        "kind": kind, "text": text, "ms": ms, "sig": "00", "role": "steer"})
    .to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

#[test]
fn a_directive_is_shown_once_and_a_pause_holds_until_resumed() {
    let (root, work, _, _) = setup("steer");
    let w = work.display().to_string();
    let ev = |extra: Value| {
        let mut v = json!({"session_id": "s-one", "cwd": w});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    hook(&root, "claude", "prompt", ev(json!({"prompt": "go"})));
    assert_eq!(hook(&root, "claude", "pre_tool", ev(json!({"tool_name": "Bash"}))), None, "nobody steers: nothing");
    let inbox = store(&root, "corp").join("steer/inbox.jsonl");
    std::fs::create_dir_all(inbox.parent().unwrap()).unwrap();
    std::fs::write(&inbox, accepted("d1", "say", "s-one", "the staging db is down, use the mock", now()) + "\n").unwrap();

    let out = hook(&root, "claude", "post_tool", ev(json!({"tool_name": "Read", "tool_input": {"file_path": "x"}}))).unwrap();
    let ctx = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.starts_with("[borg] Steering from bob@x [key abababababab] (signed, role steer): the staging db is down, use the mock"), "{ctx}");
    assert!(ctx.contains("not the user's own instruction"));
    assert_eq!(hook(&root, "claude", "post_tool", ev(json!({"tool_name": "Read"}))), None, "once");

    // Pause: tools refused, the stop announces it once and then lets the drone wait.
    std::fs::write(&inbox, std::fs::read_to_string(&inbox).unwrap() + &accepted("p1", "pause", "s-one", "", now()) + "\n").unwrap();
    let deny = hook(&root, "claude", "pre_tool", ev(json!({"tool_name": "Bash"}))).unwrap();
    assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
    let stop = hook(&root, "claude", "stop", ev(json!({"last_assistant_message": "done", "stop_hook_active": false}))).unwrap();
    assert!(stop["reason"].as_str().unwrap().contains("PAUSE"));
    assert_eq!(hook(&root, "claude", "stop", ev(json!({"last_assistant_message": "done", "stop_hook_active": false}))), None);
    assert!(hook(&root, "claude", "pre_tool", ev(json!({"tool_name": "Edit"}))).is_some(), "still paused");
    std::fs::write(&inbox, std::fs::read_to_string(&inbox).unwrap() + &accepted("r1", "resume", "s-one", "", now() + 1) + "\n").unwrap();
    assert_eq!(hook(&root, "claude", "pre_tool", ev(json!({"tool_name": "Edit"}))), None, "resumed");
    let (out, _) = borg(&root, &["top", "--json"], "");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["drones"][0]["paused"], false);
    let _ = std::fs::remove_dir_all(&root);
}
