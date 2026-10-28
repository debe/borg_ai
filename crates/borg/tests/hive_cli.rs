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
    // A second drone asking for t1 is told who holds it and that it is not its own to work on.
    join(&root, "s2");
    let (_, err, code) = hive(&root, &["claim", "t1", "--session", "s2"]);
    assert!(code == 1 && err.contains("t1 is not open to s2 (claimed by s1)") && err.contains("It is not yours: do not work on it"), "{err}");
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

/// Appends `lines` (JSON events) to g1's board.
fn append_board(root: &Path, lines: &[Value]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(store(root).join("hive/g1/tasks.jsonl")).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// A goal `g1` (t1, t2) whose oracle passes, and while it runs hands t1 to `s2`: an engineer releases
/// it and s2 claims it, the way a rival takes a claim whose lease ran out during a long suite.
fn init_with_a_rival(root: &Path) {
    let script = root.join("rival.sh");
    let line = |ev: &str, by: &str, at: &str| format!(r#"{{"v":1,"ev":"{ev}","task":"t1","ms":{at},"by":"{by}"}}"#);
    let body = format!(
        "ms=$(perl -MTime::HiRes=time -e 'printf \"%d\", time*1000')\nprintf '%s\\n%s\\n' '{}' '{}' | sed \"s/MS1/$((ms-2))/;s/MS2/$((ms-1))/\" >> {}\n",
        line("released", "engineer:e@x", "MS1"),
        line("claim", "s2", "MS2"),
        store(root).join("hive/g1/tasks.jsonl").display()
    );
    std::fs::write(&script, body).unwrap();
    let check = format!("ok=sh {}", script.display());
    ok(
        root,
        &["init", "g1", "--spec", "Ship the parser", "--oracle", &check, "--budget", "5", "--task", "t1|work|Add the parser", "--task", "t2|work|Document it"],
    );
}

#[test]
fn a_done_after_the_claim_passed_to_a_rival_is_not_a_done() {
    let root = common::setup("hive-done-lost");
    init_with_a_rival(&root);
    join(&root, "s1");
    ok(&root, &["claim", "t1", "--session", "s1"]);
    let (out, err, code) = hive(&root, &["done", "--session", "s1"]);
    assert_eq!(code, 3, "the claim was lost while the oracle ran: {out}{err}");
    assert!(!out.contains("done: the oracle passed") && out.contains("claimed by s2") && out.contains("It is not yours"), "{out}");
    let b = board(&root);
    assert_eq!(b["t1"], ("claimed".into(), "s2".into()));
    assert!(!b.contains_key("t1.v1"), "no verify task for a done the board ignored");
    // The stop's done: the same read-back, no handover announced for work the board did not take.
    let root2 = common::setup("hive-stop-done-lost");
    init_with_a_rival(&root2);
    join(&root2, "s1");
    ok(&root2, &["claim", "t1", "--session", "s1"]);
    let r = reason(&stop(&root2, "s1", false));
    assert!(!r.contains("t1 done") && r.contains("claimed by s2") && r.contains("It is not yours"), "{r}");
    let b = board(&root2);
    assert_eq!((&b["t1"], b["t2"].0.as_str()), (&("claimed".to_string(), "s2".to_string()), "open"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

#[test]
fn a_task_whose_deps_cannot_be_brought_in_is_passed_over_for_the_next() {
    let root = common::setup("hive-deps-skip");
    init(&root, &["t1|work|Add the parser", "t2|work|Use it||t1", "t3|work|Document it", "t4|work|Benchmark it"]);
    // t1 verified, with no snapshot of its work to bring into a tree (a claim made outside git).
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let ev = |ev: &str, task: &str, by: &str, at: i64| json!({"v": 1, "ev": ev, "task": task, "ms": ms + at, "by": by});
    let mut open = ev("open", "t1.v1", "sx", 2);
    open["def"] = json!({"id": "t1.v1", "role": "verify", "title": "verify t1", "of": "t1"});
    append_board(&root, &[ev("claim", "t1", "sx", 1), ev("done", "t1", "sx", 2), open, ev("claim", "t1.v1", "sy", 3), ev("verified", "t1.v1", "sy", 4)]);
    assert_eq!(board(&root)["t1"].0, "verified");
    join(&root, "s3");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let (out, err, code) = hive(&root, &["claim", "--session", "s3"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("skipped t2: t1 has no verified work to bring into this tree") && out.contains("claimed t3 for s3"), "{out}");
    // Offered no more in this session, and passed over again by the stop's handover.
    std::fs::write(work(&root).join("ok"), "").unwrap();
    let r = reason(&stop(&root, "s3", false));
    assert!(r.contains("t3 done") && r.contains("Next: Your claim: t4"), "{r}");
    let b = board(&root);
    assert_eq!((b["t2"].0.as_str(), &b["t4"]), ("open", &("claimed".to_string(), "s3".to_string())));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_board_that_cannot_be_written_is_no_lost_race() {
    use std::os::unix::fs::PermissionsExt;
    let root = common::setup("hive-board-ro");
    init(&root, &["t1|work|Add the parser"]);
    join(&root, "s1");
    let tasks = store(&root).join("hive/g1/tasks.jsonl");
    std::fs::set_permissions(&tasks, std::fs::Permissions::from_mode(0o444)).unwrap();
    let (out, err, code) = hive(&root, &["claim", "--session", "s1"]);
    std::fs::set_permissions(&tasks, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(code, 1, "{out}{err}");
    assert!(!out.contains("another drone claimed it first") && err.contains("could not write the board"), "{out}{err}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_session_in_a_subdirectory_is_scoped_by_repo_paths_and_its_end_releases_the_claim() {
    let root = common::setup("hive-subdir");
    init(&root, &["t1|work|Add the parser|crates/x/src/*"]);
    std::fs::create_dir_all(work(&root).join(".git")).unwrap();
    let sub = work(&root).join("crates/x");
    std::fs::create_dir_all(sub.join("src")).unwrap();
    join(&root, "s1");
    ok(&root, &["claim", "t1", "--session", "s1"]);
    let edit = |f: &str| {
        let input = json!({"cwd": sub, "tool_name": "Edit", "tool_input": {"file_path": sub.join(f), "old_string": "a", "new_string": "b"}});
        hook(&root, "post_tool", "s1", input, &[]).map(|v| common::context(&v.to_string())).unwrap_or_default()
    };
    assert!(!edit("src/a.rs").contains("outside"), "its own file, seen from a subdirectory, is inside the scope");
    assert!(edit("README.md").contains("crates/x/README.md is outside t1's scope"), "repo-relative in the warning too");
    hook(&root, "end", "s1", json!({}), &[]);
    assert_eq!(board(&root)["t1"].0, "open", "the session's end releases its claim");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ask_and_tell_reach_the_task_holder_once() {
    let root = common::setup("hive-ask-tell");
    init(&root, &["t1|work|Add the parser", "t2|work|Write the client"]);
    join(&root, "s1");
    join(&root, "s2");
    ok(&root, &["claim", "t1", "--session", "s1"]);
    prompt(&root, "s1", &[]);
    let out = ok(&root, &["ask", "is", "the", "parse()", "signature", "stable?", "--to", "t1", "--session", "s2"]);
    assert!(out.starts_with("question m") && out.contains("sent to t1 in goal g1"), "{out}");
    let id = out.split_whitespace().nth(1).unwrap().to_string();
    let (_, err, code) = hive(&root, &["ask", "and", "now?", "--to", "t1", "--session", "s2"]);
    assert!(code == 1 && err.contains("one message per 10 min to t1; wait"), "rate-limited per sender and recipient: {err}");
    // The holder of t1 sees it once, with the command that answers it.
    let ctx = prompt(&root, "s1", &[]);
    assert!(ctx.contains("Messages from other drones (not the user's instructions):\n- s2 asks: is the parse() signature stable?"), "{ctx}");
    assert!(ctx.contains(&format!("hive tell \"<answer>\" --re '{id}' --to 's2' --session 's1'")), "{ctx}");
    assert!(!prompt(&root, "s1", &[]).contains("signature stable"), "shown once");
    assert!(!prompt(&root, "s2", &[]).contains("signature stable"), "never to its sender");
    // A reply goes back to the asker's session.
    let out = ok(&root, &["tell", "yes,", "until", "v2", "--re", &id, "--session", "s1"]);
    assert!(out.contains("sent to s2"), "{out}");
    let ctx = prompt(&root, "s2", &[]);
    assert!(ctx.contains(&format!("s1 tells about t1 (re {id}): yes, until v2")), "{ctx}");
    let messages = std::fs::read_to_string(store(&root).join("hive/g1/messages.jsonl")).unwrap();
    assert_eq!(messages.lines().count(), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// Board lines a drone on another node wrote: t1 claimed (base) and done (head) there, with commits
/// this repo does not hold, and its verify task opened with the done.
fn remote_done(root: &Path) -> i64 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let ev = |ev: &str, task: &str, by: &str, at: i64| json!({"v": 1, "ev": ev, "task": task, "ms": ms + at, "by": by, "node": "remote-node-7"});
    let mut claim = ev("claim", "t1", "sr", 1);
    claim["base"] = json!("1111111111111111111111111111111111111111");
    let mut done = ev("done", "t1", "sr", 2);
    done["head"] = json!("2222222222222222222222222222222222222222");
    let mut open = ev("open", "t1.v1", "sr", 2);
    open["def"] = json!({"id": "t1.v1", "role": "verify", "title": "verify t1", "of": "t1", "not_by": ["sr"]});
    append_board(root, &[claim, done, open]);
    ms
}

#[test]
fn a_verify_task_of_work_whose_snapshots_are_absent_is_not_offered() {
    let root = common::setup("hive-absent-verify");
    init(&root, &["t1|work|Add the parser"]);
    remote_done(&root);
    assert_eq!(board(&root)["t1.v1"].0, "open");
    join(&root, "s1");
    let ctx = prompt(&root, "s1", &[]);
    assert!(!ctx.contains("t1.v1 (verify)") && ctx.contains("no task is open to you now"), "{ctx}");
    let (out, err, code) = hive(&root, &["claim", "t1.v1", "--session", "s1"]);
    assert!(code == 1 && err.contains("t1.v1: its work was done on node remote-n and is not in this repo"), "{out}{err}");
    assert!(!std::fs::read_to_string(store(&root).join("hive/g1/tasks.jsonl")).unwrap().contains("\"by\":\"s1\""), "nothing written");
    let _ = std::fs::remove_dir_all(root);
}

/// d3-accept2: verifiers of another node's work, sent to a joins.jsonl that holds only this node's
/// worktrees, found "no change". Its commits here, the claim names them and how to check them out.
#[test]
fn a_verify_claim_of_another_nodes_work_names_its_commits() {
    let root = common::setup("hive-verify-names");
    let git = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(work(&root)).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(work(&root).join("README"), "x\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(work(&root).join("parser.rs"), "fn parse() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "the remote drone's work"]);
    let head = git(&["rev-parse", "HEAD"]);
    git(&["reset", "-q", "--hard", &base]);
    init(&root, &["t1|work|Add the parser"]);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let ev = |ev: &str, task: &str, at: i64| json!({"v": 1, "ev": ev, "task": task, "ms": ms + at, "by": "sr", "node": "remote-node-7"});
    let (mut claim, mut done, mut open) = (ev("claim", "t1", 1), ev("done", "t1", 2), ev("open", "t1.v1", 2));
    claim["base"] = json!(base);
    done["head"] = json!(head);
    open["def"] = json!({"id": "t1.v1", "role": "verify", "title": "verify t1", "of": "t1", "not_by": ["sr"]});
    append_board(&root, &[claim, done, open]);
    join(&root, "s1");
    let out = ok(&root, &["claim", "t1.v1", "--session", "s1"]);
    assert!(out.contains(&format!("Its change is `git diff {base} {head}`")) && !out.contains("joins.jsonl"), "{out}");
    // The printed copy holds the work; the verifier's own tree does not.
    let make = out.split("apart from your tree, `").nth(1).and_then(|s| s.split('`').next()).expect(&out);
    assert!(Command::new("sh").arg("-c").arg(make).current_dir(work(&root)).status().unwrap().success(), "{make}");
    let dir = make.rsplit("tar -x -C ").next().unwrap().trim_matches('\'');
    assert!(Path::new(dir).join("parser.rs").exists() && !work(&root).join("parser.rs").exists());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn integrate_refuses_cleanly_when_an_attempt_is_absent() {
    let root = common::setup("hive-absent-integrate");
    let git = |args: &[&str]| {
        assert!(
            Command::new("git").arg("-C").arg(work(&root)).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap().status.success()
        )
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(work(&root).join("README"), "x\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    init(&root, &["t1|work|Add the parser"]);
    let ms = remote_done(&root);
    let ev = |ev: &str, at: i64| json!({"v": 1, "ev": ev, "task": "t1.v1", "ms": ms + at, "by": "sv", "node": "remote-node-7"});
    append_board(&root, &[ev("claim", 3), ev("verified", 4)]);
    assert_eq!(board(&root)["t1"].0, "verified");
    let before = std::fs::read_to_string(store(&root).join("hive/g1/tasks.jsonl")).unwrap();
    let (out, err, code) = hive(&root, &["integrate", "g1"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("integration needs t1 from node remote-n") && err.contains("integrate on a node that holds these commits"), "{err}");
    assert_eq!(std::fs::read_to_string(store(&root).join("hive/g1/tasks.jsonl")).unwrap(), before, "no board line: no blocked integration");
    let _ = std::fs::remove_dir_all(root);
}

/// hive-accept-1 W1 s0: d2's next session claimed the verify task of d2's own work, which the fold then
/// voided. A fresh session of a runner drone (its membership names no drone yet; the runner's env
/// does) is neither offered it nor writes a claim of it.
#[test]
fn a_fresh_runner_session_is_not_offered_the_verify_task_of_its_drone() {
    let root = common::setup("hive-fresh-drone");
    init(&root, &["t1|work|Add the parser"]);
    let d1 = [("BORG_HIVE", "g1"), ("BORG_HIVE_DRONE", "d1")];
    prompt(&root, "s1-drone-session", &d1);
    ok(&root, &["claim", "t1", "--session", "s1-drone-session"]);
    std::fs::write(work(&root).join("ok"), "").unwrap();
    ok(&root, &["done", "--session", "s1-drone-session"]);
    // The next iteration of d1: joined without a drone in its membership, the runner's env names it.
    let s2 = "s2-drone-session";
    join(&root, s2);
    let only_d1 = [("BORG_HIVE_DRONE", "d1")];
    let ctx = prompt(&root, s2, &only_d1);
    assert!(!ctx.contains("t1.v1") && ctx.contains("no task is open to you now"), "{ctx}");
    // The runner's join record of the iteration: `borg hive claim` with no --session finds it by drone.
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let join = json!({"v": 1, "ms": ms, "goal": "g1", "dir": work(&root), "drone": "d1", "session": s2, "harness": "claude", "by": "run"});
    std::fs::write(store(&root).join("hive/g1/joins.jsonl"), format!("{join}\n")).unwrap();
    let dir = work(&root).display().to_string();
    let out =
        command(&root, &["hive", "claim", "--dir", &dir], &[("BORG_HIVE_DRONE", "d1"), ("BORG_STATE", &store(&root).join("state").display().to_string())])
            .output()
            .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && text.contains("no task is open to s2-drone"), "{text}");
    let lines = std::fs::read_to_string(store(&root).join("hive/g1/tasks.jsonl")).unwrap();
    assert!(!lines.lines().any(|l| l.contains("\"ev\":\"claim\"") && l.contains(s2)), "no claim of s2 at all: {lines}");
    let _ = std::fs::remove_dir_all(root);
}

/// A line of another node's drone `sr` that claims `task` 5 ms before `session`'s latest claim of it:
/// the fold gives it the race, as a sync would deliver it after the local claim looked won.
fn lose_to_remote(root: &Path, session: &str, task: &str) {
    lose_to(root, "sr", session, task);
}

fn lose_to(root: &Path, rival: &str, session: &str, task: &str) {
    let ms = claim_ms(root, session, task);
    let ev = |ev: &str| json!({"v": 1, "ev": ev, "task": task, "ms": ms - 5, "by": rival, "node": "remote-node-7"});
    append_board(root, &[ev("claim"), ev("heartbeat")]);
}

/// The ms of `session`'s latest claim of `task`.
fn claim_ms(root: &Path, session: &str, task: &str) -> i64 {
    let lines = std::fs::read_to_string(store(root).join("hive/g1/tasks.jsonl")).unwrap();
    lines
        .lines()
        .rev()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["ev"] == "claim" && v["by"] == session && v["task"] == task)
        .and_then(|v| v["ms"].as_i64())
        .unwrap()
}

fn bash() -> Value {
    json!({"tool_name": "Bash", "tool_input": {"command": "ls"}})
}

fn edit(root: &Path) -> Value {
    json!({"tool_name": "Edit", "tool_input": {"file_path": work(root).join("src/parse.rs"), "old_string": "a", "new_string": "b"}})
}

fn denied(v: &Option<Value>) -> Option<String> {
    let spec = &v.as_ref()?["hookSpecificOutput"];
    (spec["permissionDecision"] == "deny").then(|| spec["permissionDecisionReason"].as_str().unwrap_or_default().to_string())
}

#[test]
fn a_claim_lost_to_another_node_is_told_at_the_next_tool_call_and_its_edits_are_refused() {
    let root = common::setup("hive-lost-hook");
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "s1");
    ok(&root, &["claim", "t1", "--session", "s1"]);
    assert_eq!(denied(&hook(&root, "pre_tool", "s1", edit(&root), &[])), None, "its own claim: edits run");
    lose_to_remote(&root, "s1", "t1");
    let ctx = hook(&root, "post_tool", "s1", bash(), &[]).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(ctx.starts_with("[borg hive g1] Your claim on t1 did not take effect"), "after any tool, not only an edit: {ctx}");
    let again = hook(&root, "post_tool", "s1", bash(), &[]).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(!again.contains("did not take effect"), "told once: {again}");
    let why = denied(&hook(&root, "pre_tool", "s1", edit(&root), &[])).expect("an edit of a lost claim is refused");
    assert!(why.contains("Your claim on t1 did not take effect") && why.contains("It is not yours"), "{why}");
    assert_eq!(denied(&hook(&root, "pre_tool", "s1", bash(), &[])), None, "only edits and writes are refused");
    assert_eq!(denied(&hook(&root, "pre_tool", "s2", edit(&root), &[])), None, "a session in no goal is untouched");
    ok(&root, &["claim", "t2", "--session", "s1"]);
    assert_eq!(denied(&hook(&root, "pre_tool", "s1", edit(&root), &[])), None, "a new claim lifts it");
    assert_eq!(board(&root)["t1"], ("claimed".into(), "sr".into()));
    let _ = std::fs::remove_dir_all(root);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The work dir as a git repo with `src/parse.rs` committed: the origin of a goal made in it after,
/// and every claim's start there (a checkout off its start claims nothing, hive/start.rs).
fn git_work(root: &Path) -> PathBuf {
    let w = work(root);
    std::fs::create_dir_all(w.join("src")).unwrap();
    std::fs::write(w.join("src/parse.rs"), "fn parse() {}\n").unwrap();
    git(&w, &["init", "-q", "-b", "main"]);
    git(&w, &["add", "-A"]);
    git(&w, &["-c", "user.name=x", "-c", "user.email=x@y", "commit", "-q", "-m", "one"]);
    w
}

fn tool_ctx(root: &Path, session: &str) -> String {
    hook(root, "post_tool", session, bash(), &[]).map(|v| common::context(&v.to_string())).unwrap_or_default()
}

#[test]
fn a_lost_claims_worktree_is_set_aside_at_its_stop_in_a_pinned_snapshot_and_reset_to_its_base() {
    let root = common::setup("hive-lost-reset");
    let w = git_work(&root);
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "lr1");
    ok(&root, &["claim", "t1", "--session", "lr1"]);
    // The loser works on: a tracked file changed, a new file, before the sync tells it.
    std::fs::write(w.join("src/parse.rs"), "fn parse() { todo!() }\n").unwrap();
    std::fs::write(w.join("src/lost.rs"), "lost work\n").unwrap();
    lose_to_remote(&root, "lr1", "t1");
    // A tool call's hook tells it and leaves the tree: opencode and omp give such a hook 2 to 5 s.
    let ctx = tool_ctx(&root, "lr1");
    assert!(ctx.contains("did not take effect") && ctx.contains("set aside when this turn ends"), "{ctx}");
    assert!(w.join("src/lost.rs").exists(), "nothing reset inside a tool call's hook");
    let r = reason(&stop(&root, "lr1", false));
    let snap = r.split("set aside in commit ").nth(1).and_then(|s| s.split_whitespace().next()).expect(&r).to_string();
    assert!(r.contains("your worktree is back at the claim's start"), "{r}");
    assert_eq!(std::fs::read_to_string(w.join("src/parse.rs")).unwrap(), "fn parse() {}\n", "the tracked file is as at the claim");
    assert!(!w.join("src/lost.rs").exists(), "the new file is gone");
    assert_eq!(git(&w, &["show", &format!("{snap}:src/lost.rs")]), "lost work", "the snapshot holds the discarded work");
    assert_eq!(git(&w, &["show", &format!("{snap}:src/parse.rs")]), "fn parse() { todo!() }");
    assert_eq!(git(&w, &["rev-parse", &format!("refs/borg/hive/g1/{snap}")]), snap, "pinned: gc keeps it");
    let lines = std::fs::read_to_string(store(&root).join("hive/g1/tasks.jsonl")).unwrap();
    let base = lines.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|v| v["ev"] == "claim" && v["by"] == "lr1").unwrap()["base"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(git(&w, &["diff", "--name-only", &base, &snap]), "src/lost.rs\nsrc/parse.rs", "the discarded diff, against the base");
    assert_eq!(git(&w, &["status", "--porcelain"]), "", "the worktree equals the base: nothing differs from HEAD");
    // Once only: the next stop neither tells nor resets again.
    std::fs::write(w.join("scratch.txt"), "after\n").unwrap();
    let again = reason(&stop(&root, "lr1", false));
    assert!(!again.contains("set aside") && w.join("scratch.txt").exists(), "{again}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_lost_claim_is_set_aside_at_the_next_claim_before_it_takes_its_base() {
    let root = common::setup("hive-lost-next-claim");
    let w = git_work(&root);
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "ln1");
    ok(&root, &["claim", "t1", "--session", "ln1"]);
    std::fs::write(w.join("src/lost.rs"), "lost work\n").unwrap();
    lose_to_remote(&root, "ln1", "t1");
    tool_ctx(&root, "ln1");
    let out = ok(&root, &["claim", "t2", "--session", "ln1"]);
    assert!(out.contains("set aside in commit") && out.contains("claimed t2"), "{out}");
    assert!(!w.join("src/lost.rs").exists(), "t1's work is not in t2's tree");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_second_loss_of_a_task_claimed_again_sets_aside_only_this_claims_work() {
    let root = common::setup("hive-lost-twice");
    let w = git_work(&root);
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "lt1");
    ok(&root, &["claim", "t1", "--session", "lt1"]);
    lose_to_remote(&root, "lt1", "t1");
    stop(&root, "lt1", false);
    // t2's work, done in the same checkout.
    ok(&root, &["claim", "t2", "--session", "lt1"]);
    std::fs::write(w.join("t2.txt"), "t2's work\n").unwrap();
    std::fs::write(w.join("ok"), "").unwrap();
    ok(&root, &["done", "--session", "lt1"]);
    // The rival lets t1 go. A checkout that holds t2's work is not at t1's start: never t1's base, so
    // refused, the tree untouched; once the engineer cleared it, lt1 claims t1 again and loses once more.
    let ms = claim_ms(&root, "lt1", "t1");
    append_board(&root, &[json!({"v": 1, "ev": "released", "task": "t1", "ms": ms + 1, "by": "sr", "node": "remote-node-7"})]);
    let (_, err, code) = hive(&root, &["claim", "t1", "--session", "lt1"]);
    assert!(code == 1 && err.contains("this checkout is not at t1's start"), "{err}");
    assert!(w.join("t2.txt").exists(), "nothing reset");
    git(&w, &["clean", "-fdq"]);
    ok(&root, &["claim", "t1", "--session", "lt1"]);
    std::fs::write(w.join("t1-again.txt"), "lost again\n").unwrap();
    lose_to(&root, "sr2", "lt1", "t1");
    let r = reason(&stop(&root, "lt1", false));
    assert!(r.contains("set aside in commit"), "{r}");
    assert!(!w.join("t1-again.txt").exists(), "this claim's work is set aside");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_lost_claim_the_fold_gives_back_after_it_was_told_is_released() {
    let root = common::setup("hive-lost-flip");
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "lf1");
    ok(&root, &["claim", "t1", "--session", "lf1"]);
    lose_to_remote(&root, "lf1", "t1");
    assert!(tool_ctx(&root, "lf1").contains("Your claim on t1 did not take effect"));
    // The rival's release, written before s1's claim, syncs late: the fold now gives t1 to s1.
    let ms = claim_ms(&root, "lf1", "t1");
    append_board(&root, &[json!({"v": 1, "ev": "released", "task": "t1", "ms": ms - 4, "by": "sr", "node": "remote-node-7"})]);
    tool_ctx(&root, "lf1");
    assert_eq!(board(&root)["t1"].0, "open", "told it lost, the drone moved on: released, not renewed");
    assert!(denied(&hook(&root, "pre_tool", "lf1", edit(&root), &[])).is_some(), "its edits stay refused");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_refused_edit_of_a_test_file_raises_no_flag() {
    let root = common::setup("hive-lost-no-flag");
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "lg1");
    ok(&root, &["claim", "t1", "--session", "lg1"]);
    lose_to_remote(&root, "lg1", "t1");
    let test = json!({"tool_name": "Edit", "tool_input": {"file_path": work(&root).join("tests/test_parse.py"), "old_string": "a", "new_string": "b"}});
    assert!(denied(&hook(&root, "pre_tool", "lg1", test, &[])).is_some());
    let flags = store(&root).join("hive/g1/flags.jsonl");
    assert_eq!(std::fs::read_to_string(&flags).unwrap_or_default(), "", "the edit never ran: nothing to flag");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_lost_claim_in_a_tree_another_session_holds_a_task_in_is_left_as_it_is() {
    let root = common::setup("hive-lost-shared");
    let w = git_work(&root);
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "lh1");
    join(&root, "lh2");
    ok(&root, &["claim", "t1", "--session", "lh1"]);
    ok(&root, &["claim", "t2", "--session", "lh2"]);
    std::fs::write(w.join("t2.txt"), "s2's work on t2\n").unwrap();
    lose_to_remote(&root, "lh1", "t1");
    let r = reason(&stop(&root, "lh1", false));
    assert!(r.contains("left as it is: another drone holds a task in it"), "{r}");
    assert!(w.join("t2.txt").exists(), "s2's work is untouched");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_lost_claim_learnt_while_holding_the_next_task_resets_nothing() {
    let root = common::setup("hive-lost-held");
    let w = git_work(&root);
    init(&root, &["t1|work|Add the parser", "t2|work|Document it"]);
    join(&root, "s1");
    ok(&root, &["claim", "t1", "--session", "s1"]);
    lose_to_remote(&root, "s1", "t1");
    // Before any hook told it, the session claimed t2 and works on it.
    ok(&root, &["claim", "t2", "--session", "s1"]);
    std::fs::write(w.join("src/parse.rs"), "t2's work\n").unwrap();
    let ctx = hook(&root, "post_tool", "s1", bash(), &[]).map(|v| common::context(&v.to_string())).unwrap_or_default();
    assert!(ctx.contains("Your claim on t1 did not take effect") && !ctx.contains("set aside"), "told, nothing reset: {ctx}");
    assert_eq!(std::fs::read_to_string(w.join("src/parse.rs")).unwrap(), "t2's work\n", "the held task's work is untouched");
    assert_eq!(denied(&hook(&root, "pre_tool", "s1", edit(&root), &[])), None, "it holds t2: edits run");
    let _ = std::fs::remove_dir_all(root);
}
