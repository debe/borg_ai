//! The hive through the binary (hive/mod.rs): `borg hive` builds a goal and its board, drones join,
//! claim and publish contracts, and the hooks add the claim, the board's changes and contracts at a
//! prompt or tool call and gate the stop on the goal's oracle. A session in no goal sees no change.
//! No model call: the stop runs with word matching only and no prediction model.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Every hook's stop runs without a model.
const NO_MODEL: [(&str, &str); 2] = [("BORG_MATCH", "bm25"), ("BORG_PREDICT_MODEL", "off")];

fn work(root: &Path) -> PathBuf {
    root.join("harbor_orion")
}

fn store(root: &Path) -> PathBuf {
    root.join("store/collectives/corp")
}

fn command(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .env("BORG_LEARN", "off")
        .env("BORG_VECTORS", "off")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// `borg hive <args> --dir <work>`: (stdout, stderr, exit code).
fn hive(root: &Path, args: &[&str]) -> (String, String, i32) {
    let dir = work(root).display().to_string();
    let all: Vec<&str> = ["hive"].iter().copied().chain(args.iter().copied()).chain(["--dir", dir.as_str()]).collect();
    let out = command(root, &all, &[]).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code().unwrap_or(-1))
}

fn ok(root: &Path, args: &[&str]) -> String {
    let (out, err, code) = hive(root, args);
    assert_eq!(code, 0, "borg hive {args:?}: {out}{err}");
    out
}

fn hook(root: &Path, event: &str, session: &str, extra: Value, env: &[(&str, &str)]) -> Option<Value> {
    hook_as(root, "claude", event, session, extra, env)
}

fn hook_as(root: &Path, harness: &str, event: &str, session: &str, extra: Value, env: &[(&str, &str)]) -> Option<Value> {
    let mut input = json!({"session_id": session, "cwd": work(root)});
    input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let env: Vec<(&str, &str)> = NO_MODEL.iter().copied().chain(env.iter().copied()).collect();
    let out = common::borg_as(root, harness, event, &input, &env);
    (!out.trim().is_empty()).then(|| serde_json::from_str(out.trim()).unwrap())
}

fn prompt(root: &Path, session: &str, env: &[(&str, &str)]) -> String {
    hook(root, "prompt", session, json!({"prompt": "carry on with the goal"}), env).map(|v| common::context(&v.to_string())).unwrap_or_default()
}

fn stop(root: &Path, session: &str, again: bool) -> Option<Value> {
    hook(root, "stop", session, json!({"last_assistant_message": "I made the change.", "stop_hook_active": again}), &[])
}

fn reason(v: &Option<Value>) -> String {
    v.as_ref().and_then(|v| v["reason"].as_str()).unwrap_or_default().to_string()
}

/// A goal `g1` whose oracle passes once `ok` exists in the drone's directory, with `tasks`.
fn init(root: &Path, tasks: &[&str]) {
    let mut args = vec!["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=test -f ok", "--budget", "5"];
    for t in tasks {
        args.extend(["--task", t]);
    }
    ok(root, &args);
}

/// The board as `borg hive board --json`: task id -> (status, holder).
fn board(root: &Path) -> std::collections::BTreeMap<String, (String, String)> {
    let v: Value = serde_json::from_str(&ok(root, &["board", "g1", "--json"])).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .map(|t| (t["id"].as_str().unwrap().to_string(), (t["status"].as_str().unwrap().to_string(), t["by"].as_str().unwrap().to_string())))
        .collect()
}

/// A drone `session` with a presence record (its first prompt), joined to g1.
fn join(root: &Path, session: &str) {
    prompt(root, session, &[]);
    ok(root, &["join", "g1", "--session", session]);
}

#[test]
fn a_non_hive_drone_is_untouched() {
    let run = |root: &Path| -> Vec<String> {
        let edit = json!({"tool_name": "Edit", "tool_input": {"file_path": work(root).join("src/net.rs"), "new_string": "drain the Orion budget place"}});
        let outs = vec![
            hook(root, "prompt", "s1", json!({"prompt": "fix the Orion net budget drain"}), &[]),
            hook(root, "post_tool", "s1", edit, &[]),
            stop(root, "s1", false),
            stop(root, "s1", true),
        ];
        let r = root.display().to_string();
        outs.into_iter().map(|o| o.map(|v| v.to_string().replace(&r, "<root>")).unwrap_or_default()).collect()
    };
    let plain = common::setup("hive-plain");
    let with_goal = common::setup("hive-plain-goal");
    init(&with_goal, &["t1|work|Add the parser"]);
    let a = run(&plain);
    assert_eq!(a, run(&with_goal), "a goal in the store changes nothing for a drone that never joined it");
    assert!(!a[2].is_empty(), "the comparison covers a stop that answered");
    assert!(!store(&with_goal).join("state/s1.hive").exists(), "no membership");
    for r in [plain, with_goal] {
        let _ = std::fs::remove_dir_all(r);
    }
}

#[test]
fn init_join_claim_board_roundtrip() {
    let root = common::setup("hive-roundtrip");
    let (_, err, code) = hive(&root, &["init", "g1", "--spec", "x", "--budget", "1"]);
    assert_eq!(code, 2, "done needs an oracle: {err}");
    init(&root, &["t1|work|Add the parser|src/parse/*.rs", "t2|work|Use it|src/main.rs|t1"]);
    assert!(hive(&root, &["init", "g1", "--spec", "x", "--oracle", "a=true", "--budget", "1"]).2 != 0, "a goal is made once");
    join(&root, "s1");
    let ctx = prompt(&root, "s1", &[]);
    assert!(ctx.starts_with("[borg hive g1]"), "{ctx}");
    assert!(ctx.contains("oracle gates done"), "the first hive prompt says the oracle cannot be gamed usefully: {ctx}");
    assert!(ctx.contains("No claim. Next open: t1 (work) Add the parser. Claim it: `") && ctx.contains("hive claim 't1' --session 's1' --dir"), "{ctx}");
    let out = ok(&root, &["claim", "--session", "s1"]);
    assert!(out.starts_with("claimed t1 for s1. Your claim: t1 (work) Add the parser; files src/parse/*.rs; done when: ok pass"), "{out}");
    assert_eq!(board(&root)["t1"], ("claimed".into(), "s1".into()));
    assert_eq!(board(&root)["t2"], ("open".into(), String::new()));
    let (_, err, code) = hive(&root, &["claim", "t2", "--session", "s1"]);
    assert!(code == 1 && err.contains("holds t1 already"), "{err}");
    let ctx = prompt(&root, "s1", &[]);
    assert!(ctx.contains("Your claim: t1 (work) Add the parser."), "the claim command described it in full already: {ctx}");
    assert!(!ctx.contains("oracle gates done"), "said once");
    let human = ok(&root, &["board", "g1"]);
    assert!(human.contains("claimed (1)") && human.contains("t1") && human.contains("after t1"), "{human}");
    ok(&root, &["release", "--session", "s1"]);
    assert_eq!(board(&root)["t1"].0, "open");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_stop_blocks_while_the_oracle_fails() {
    let root = common::setup("hive-oracle");
    init(&root, &["t1|work|Add the parser"]);
    // A drone launched into the goal joins it at its first hook.
    prompt(&root, "s1", &[("BORG_HIVE", "g1")]);
    assert!(store(&root).join("state/s1.hive").exists());
    ok(&root, &["claim", "t1", "--session", "s1"]);
    for (attempt, again) in [(1, false), (2, true), (3, true)] {
        let r = reason(&stop(&root, "s1", again));
        assert!(r.starts_with("[borg hive] t1 is not done: ok failed") && r.contains(&format!("(attempt {attempt} of 3)")), "{r}");
        assert!(r.contains("hive done --session 's1'"), "{r}");
    }
    let r = reason(&stop(&root, "s1", true));
    assert!(!r.contains("is not done"), "after 3 blocks the drone may stop: {r}");
    assert_eq!(board(&root)["t1"].0, "blocked");
    assert!(std::fs::read_dir(store(&root).join("hive/g1/oracle")).unwrap().count() >= 3, "failed runs are logged");
    // The engineer reopens it, the drone claims it again, and now the oracle passes.
    ok(&root, &["open", "g1", "--id", "t1", "--again"]);
    ok(&root, &["claim", "t1", "--session", "s1"]);
    std::fs::write(work(&root).join("ok"), "").unwrap();
    let r = reason(&stop(&root, "s1", false));
    assert!(!r.contains("is not done"), "{r}");
    let b = board(&root);
    assert_eq!(b["t1"], ("done".into(), "s1".into()));
    assert_eq!(b["t1.v1"].0, "open", "a verify task is opened for every done");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn on_pass_the_next_open_task_is_handed_over() {
    let root = common::setup("hive-handover");
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "s1");
    ok(&root, &["claim", "--session", "s1"]);
    assert!(reason(&stop(&root, "s1", false)).contains("attempt 1 of 3"));
    std::fs::write(work(&root).join("ok"), "").unwrap();
    // The rework round after the block: the handover still reaches the drone.
    let r = reason(&stop(&root, "s1", true));
    assert!(r.starts_with("[borg hive] t1 done, verify task t1.v1 opened. Next: Your claim: t2 (work) Document it"), "{r}");
    let b = board(&root);
    assert_eq!((b["t1"].0.as_str(), &b["t2"]), ("done", &("claimed".to_string(), "s1".to_string())));
    // The author never verifies its own work: another drone may.
    let (_, err, code) = hive(&root, &["claim", "t1.v1", "--session", "s1"]);
    assert_ne!(code, 0, "{err}");
    join(&root, "s2-verifier");
    assert!(ok(&root, &["claim", "--session", "s2-verifier"]).starts_with("claimed t1.v1"));
    ok(&root, &["verify", "--reject", "misses the empty input", "--session", "s2-verifier"]);
    let b = board(&root);
    assert_eq!(b["t1"].0, "open", "a rejection reopens the work");
    let v: Value = serde_json::from_str(&ok(&root, &["board", "g1", "--json"])).unwrap();
    assert_eq!(v.as_array().unwrap().iter().find(|t| t["id"] == "t1").unwrap()["rework"], 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn prompt_injects_claim_changes_and_contracts_once() {
    let root = common::setup("hive-prompt");
    init(&root, &["t1|work|Add the parser|src/api/*.rs", "t2|work|Write the client|client/*.rs"]);
    join(&root, "s1");
    join(&root, "s2");
    ok(&root, &["claim", "t1", "--session", "s1"]);
    prompt(&root, "s1", &[]);
    ok(&root, &["claim", "t2", "--session", "s2"]);
    ok(&root, &["contract", "GET /v1/items returns a page of 50", "--scope", "src/api/**", "--session", "s2"]);
    ok(&root, &["progress", "client skeleton up", "--session", "s2"]);
    let ctx = prompt(&root, "s1", &[]);
    assert!(ctx.contains("Board changes:\n- drone s2 claim t2\n- drone s2 progress t2: client skeleton up"), "{ctx}");
    assert!(ctx.contains("Contracts in your claim's scope:\ncontract c") && ctx.contains("GET /v1/items returns a page of 50 [scope src/api/**]"), "{ctx}");
    assert!(ctx.contains("not the user's own instruction"), "peer data is labelled: {ctx}");
    let again = prompt(&root, "s1", &[]);
    assert!(!again.contains("Board changes") && !again.contains("GET /v1/items"), "each change and contract once: {again}");
    assert!(again.contains("Your claim: t1"), "{again}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn post_tool_shows_a_contract_its_edit_lands_in() {
    let root = common::setup("hive-post-tool");
    init(&root, &["t1|work|Add the parser|src/parse/*.rs", "t2|work|Write the client|client/*.rs"]);
    join(&root, "s1");
    join(&root, "s2");
    ok(&root, &["claim", "t2", "--session", "s2"]);
    ok(&root, &["contract", "parse_item never panics on empty input", "--scope", "id:parse_item", "--session", "s2"]);
    ok(&root, &["claim", "t1", "--session", "s1"]);
    let edit = |file: &str| json!({"tool_name": "Edit", "tool_input": {"file_path": work(&root).join(file), "new_string": "pub fn parse_item(s: &str) {}"}});
    let ctx = hook(&root, "post_tool", "s1", edit("src/parse/item.rs"), &[]).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(ctx.starts_with("[borg hive g1] This edit lands in contract c") && ctx.contains("parse_item never panics on empty input"), "{ctx}");
    assert!(!ctx.contains("outside"), "inside its own scope: {ctx}");
    let ctx = hook(&root, "post_tool", "s1", edit("src/main.rs"), &[]).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(!ctx.contains("lands in"), "a contract is shown once: {ctx}");
    assert!(ctx.contains("src/main.rs is outside t1's scope (src/parse/*.rs)"), "{ctx}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_paused_drone_is_not_blocked_by_the_oracle() {
    let root = common::setup("hive-paused");
    init(&root, &["t1|work|Add the parser"]);
    join(&root, "s1");
    ok(&root, &["claim", "t1", "--session", "s1"]);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let pause = json!({"id": "p1", "collective": "corp", "key": "ab".repeat(32), "engineer": "bob@x", "target": {"node": "", "session": "s1"},
        "kind": "pause", "text": "", "ms": now, "sig": "00", "role": "steer"});
    let inbox = store(&root).join("steer/inbox.jsonl");
    std::fs::create_dir_all(inbox.parent().unwrap()).unwrap();
    std::fs::write(&inbox, format!("{pause}\n")).unwrap();
    let r = reason(&stop(&root, "s1", false));
    assert!(r.contains("PAUSE") && !r.contains("is not done"), "paused is waiting, not unfinished: {r}");
    assert_eq!(stop(&root, "s1", false), None);
    assert_eq!(board(&root)["t1"].0, "claimed");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn sim_harness_without_prompt_context_gets_it_at_the_stop() {
    let root = common::setup("hive-sim-caps");
    init(&root, &["t1|work|Add the parser"]);
    let env = [("BORG_TEST_HARNESS", "1"), ("BORG_SIM_CAPS", ""), ("BORG_HIVE", "g1")];
    let out = hook_as(&root, "sim", "prompt", "s1", json!({"prompt": "go"}), &env);
    assert_eq!(out, None, "no prompt context for this harness");
    let r = reason(&hook_as(&root, "sim", "stop", "s1", json!({"last_assistant_message": "ok", "stop_hook_active": false}), &env));
    assert!(r.contains("[borg hive g1]") && r.contains("No claim. Next open: t1"), "{r}");
    let r = reason(&hook_as(&root, "sim", "stop", "s1", json!({"last_assistant_message": "ok", "stop_hook_active": false}), &env));
    assert!(!r.contains("Next open"), "delivered once: {r}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn three_sim_drones_on_one_board() {
    let root = common::setup("hive-three");
    init(&root, &["t1|work|One", "t2|work|Two", "t3|work|Three"]);
    let sessions = ["drone-one", "drone-two", "drone-three"];
    for s in sessions {
        hook_as(&root, "sim", "prompt", s, json!({"prompt": "go"}), &[("BORG_TEST_HARNESS", "1"), ("BORG_HIVE", "g1")]);
    }
    // All three claim at once: each ends up with a task of its own.
    let dir = work(&root).display().to_string();
    let children: Vec<_> = sessions.iter().map(|s| command(&root, &["hive", "claim", "--session", s, "--dir", &dir], &[]).spawn().unwrap()).collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    }
    let b = board(&root);
    let mut holders: Vec<&str> = b
        .values()
        .map(|(status, by)| {
            assert_eq!(status, "claimed");
            by.as_str()
        })
        .collect();
    holders.sort();
    let mut want = sessions.to_vec();
    want.sort();
    assert_eq!(holders, want, "one holder per task, one task per drone");
    let claims = std::fs::read_to_string(store(&root).join("hive/g1/tasks.jsonl")).unwrap().lines().filter(|l| l.contains("\"ev\":\"claim\"")).count();
    assert!(claims >= 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_drone_outside_the_goal_or_without_a_session_is_refused() {
    let root = common::setup("hive-refused");
    init(&root, &["t1|work|Add the parser"]);
    let (_, err, code) = hive(&root, &["claim", "--session", "nobody-here"]);
    assert!(code == 1 && err.contains("no drone or session"), "{err}");
    prompt(&root, "s1", &[]);
    let (_, err, code) = hive(&root, &["claim", "--session", "s1"]);
    assert!(code == 1 && err.contains("in no hive goal"), "{err}");
    let (_, err, code) = hive(&root, &["board", "nope"]);
    assert!(code == 1 && err.contains("no hive goal nope"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}
