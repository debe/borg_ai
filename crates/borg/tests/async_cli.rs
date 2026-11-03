//! The detached turn-end check (0.7.7, check_async.rs): with `BORG_ASYNC` on, the stop writes the turn to
//! a job file, starts `borg check --job` and returns without waiting for the model; the check's asks reach
//! the drone with the next prompt. The model is the fake `claude` of tests/common, a blocking one
//! (`blocking_fake`) where a test needs the call to stay open: no test makes a real model call, and every
//! test lets its checks end on their own before it removes its store.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{answer, blocking_fake, borg, context, prompts, release, setup, state};
use serde_json::{Value, json};

const ASYNC: [(&str, &str); 3] = [("BORG_ASYNC", "on"), ("BORG_PREFILTER", "off"), ("BORG_EARLY", "off")];

/// A test's store, let go however the test ends: a failed assertion releases the blocking fake (its
/// check ends on its own) and removes the store, so no check outlives the test.
struct Root(std::path::PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::write(self.0.join("release"), "");
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for Root {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

fn hook(root: &Path, session: &str, event: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut ev = json!({"session_id": session, "cwd": root.join("harbor_orion")});
    ev.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    borg(root, event, &ev, env)
}

fn edit(root: &Path, session: &str, env: &[(&str, &str)]) {
    hook(root, session, "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}), env);
}

fn stop(root: &Path, session: &str, env: &[(&str, &str)]) -> String {
    hook(root, session, "stop", json!({"last_assistant_message": "I drop the budget token with a reset arc.", "stop_hook_active": false}), env)
}

fn dir(root: &Path) -> std::path::PathBuf {
    root.join("store/collectives/corp/state")
}

/// Waits up to 20 s for `cond`; fails the test past it.
fn until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The session's job files (`<s>.job.*`), by name.
fn jobs(root: &Path, session: &str) -> Vec<String> {
    let prefix = format!("{session}.job.");
    let mut v: Vec<String> = std::fs::read_dir(dir(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n.starts_with(&prefix))
        .collect();
    v.sort();
    v
}

/// The checks the session's stops started, in order (`#borg-checking @<check>`).
fn checks(root: &Path, session: &str) -> Vec<String> {
    state(root, session, "ctx").lines().filter_map(|l| l.strip_prefix("#borg-checking @")).map(String::from).collect()
}

/// The session's last check result (`<s>.checked`).
fn checked(root: &Path, session: &str) -> Value {
    serde_json::from_str(&state(root, session, "checked")).unwrap_or(Value::Null)
}

/// Waits until `check` wrote its result.
fn checked_by(root: &Path, session: &str, check: &str) -> Value {
    until(&format!("{check}'s result"), || checked(root, session)["check"] == check);
    checked(root, session)
}

/// Waits until no check of the session runs: no job file left and its check lock free.
fn quiet(root: &Path, session: &str) {
    until("the checks to end", || {
        jobs(root, session).is_empty()
            && std::fs::OpenOptions::new()
                .write(true)
                .open(dir(root).join(format!("{session}.checking.lock")))
                .map_or(true, |f| f.try_lock().map(|()| f.unlock()).is_ok())
    });
}

fn outcomes(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The stop exits while the model is still answering: no block, the turn ends at the stop
/// (`#borg-checking`). Once the model answers, the check queues its ask, and the next prompt delivers it
/// as a question about the turn it judged, marked and recorded as an ask on channel live.
#[test]
fn the_stop_returns_while_the_model_still_runs() {
    let root = Root(setup("async-stop"));
    blocking_fake(&root);
    hook(&root, "a1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "a1", &ASYNC);
    let out = stop(&root, "a1", &ASYNC);
    // The fake cannot answer before `release`: a stop that waited for its check would find it done.
    assert!(state(&root, "a1", "checked").is_empty() && state(&root, "a1", "queued").is_empty(), "the stop returned before the check answered");
    assert!(!out.contains("block"), "no block: {out}");
    let ctx = state(&root, "a1", "ctx");
    assert!(ctx.contains("#borg-checking @"), "the turn ends at the stop: {ctx}");
    let ids = checks(&root, "a1");
    let [check] = &ids[..] else { panic!("one check: {ctx}") };
    until("the model call", || root.join("started").exists());
    assert!(state(&root, "a1", "queued").is_empty(), "nothing queued before the answer");
    let lock = std::fs::OpenOptions::new().write(true).open(dir(&root).join("a1.checking.lock")).unwrap();
    assert!(matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)), "the check runs, holding the session's check lock");
    release(&root);
    let done = checked_by(&root, "a1", check);
    assert_eq!(done["asks"], json!(["Mdrain"]), "{done}");
    assert_eq!(state(&root, "a1", "queued"), format!("live Mdrain @{check}\n"));
    let next = context(&hook(&root, "a1", "prompt", json!({"prompt": "now the retries"}), &ASYNC));
    assert!(next.contains("[Mdrain]") && next.contains("I drop the budget token"), "a question naming the turn: {next}");
    let ctx = state(&root, "a1", "ctx");
    assert!(ctx.contains(&format!("#borg-asked Mdrain @{check}\n#borg-live Mdrain ms=")), "{ctx}");
    let edits = state(&root, "a1", "edits");
    let lines: Vec<&str> = edits.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("#borg-asked Mdrain")).expect(&edits);
    assert!(lines[at - 1].starts_with("I drop the budget token"), "the judged message is the work before the question: {edits}");
    assert!(state(&root, "a1", "queued").is_empty(), "delivered once");
    quiet(&root, "a1");
    assert!(state(&root, "a1", "live.lock").is_empty(), "the session lock was let go");
}

/// `BORG_ASYNC=off`: the stop makes its own call and blocks, as before 0.7.7; no job, no turn marker of
/// a detached check.
#[test]
fn async_off_blocks_as_before() {
    let root = Root(setup("async-off"));
    let env = [("BORG_ASYNC", "off"), ASYNC[1], ASYNC[2]];
    hook(&root, "o1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    edit(&root, "o1", &env);
    let out = stop(&root, "o1", &env);
    assert!(out.contains("\"decision\":\"block\"") && out.contains("[Mdrain]"), "{out}");
    assert!(!state(&root, "o1", "ctx").contains("#borg-checking"));
    assert!(jobs(&root, "o1").is_empty() && state(&root, "o1", "checked").is_empty());
    assert_eq!(prompts(&root).len(), 1, "the stop's own call");
}

/// A newer check grades an ask that still waits against the newer work: graded 1 now, the waiting ask
/// leaves the queue, settled by later work (its `delivery` record), and the 1 goes free.
#[test]
fn a_newer_check_settles_a_waiting_ask() {
    let root = Root(setup("async-settle"));
    let env = [ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")];
    hook(&root, "n1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    edit(&root, "n1", &env);
    stop(&root, "n1", &env);
    let first = checks(&root, "n1")[0].clone();
    checked_by(&root, "n1", &first);
    assert_eq!(state(&root, "n1", "queued"), format!("live Mdrain @{first}\n"));
    quiet(&root, "n1");
    // A second turn end with no prompt or tool call between (a tool call would hand the ask over).
    answer(&root, &[("Mdrain", 1)]);
    stop(&root, "n1", &env);
    let second = checks(&root, "n1")[1].clone();
    let done = checked_by(&root, "n1", &second);
    assert_eq!(done["superseded"], json!(["Mdrain"]), "{done}");
    let queue = state(&root, "n1", "queued");
    assert!(!queue.contains("live "), "the waiting ask left the queue: {queue}");
    assert!(queue.contains(&format!("Mdrain @{second}")), "the 1 goes free: {queue}");
    let route = outcomes(&root)
        .into_iter()
        .find(|o| o["kind"] == "delivery" && o["check"] == first.as_str())
        .unwrap_or_else(|| panic!("a delivery record of {first}: {:?}", outcomes(&root)));
    assert_eq!(
        (route["lessons"].clone(), route["channel"].as_str(), route["route"].as_str(), route["reason"].as_str()),
        (json!(["Mdrain"]), Some("live"), Some("dropped_done"), Some("settled by later work")),
        "{route}"
    );
    let graded = outcomes(&root).into_iter().find(|o| o["kind"] == "graded" && o["check"] == second.as_str()).expect("graded");
    assert_eq!(graded["channel"], "live", "a detached check decides on channel live: {graded}");
    quiet(&root, "n1");
}

/// While one check's model call runs, two more stops leave one job waiting, the newest; the one between
/// is superseded and never calls the model. The running check takes the waiting job when it is done.
#[test]
fn a_second_stop_while_a_check_runs_queues_one_job() {
    let root = Root(setup("async-next"));
    blocking_fake(&root);
    hook(&root, "w1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "w1", &ASYNC);
    stop(&root, "w1", &ASYNC);
    until("the first model call", || root.join("started").exists());
    stop(&root, "w1", &ASYNC);
    stop(&root, "w1", &ASYNC);
    let ids = checks(&root, "w1");
    assert_eq!(ids.len(), 3, "{ids:?}");
    let next = dir(&root).join("w1.job.next");
    until("one job waiting, the newest", || {
        let names = jobs(&root, "w1");
        let waiting: Value = serde_json::from_str(&std::fs::read_to_string(&next).unwrap_or_default()).unwrap_or(Value::Null);
        names.len() == 2 && names.iter().any(|n| n.starts_with(&format!("w1.job.{}.", ids[0]))) && waiting["check"] == ids[2].as_str()
    });
    assert_eq!(prompts(&root).len(), 1, "one model call runs");
    release(&root);
    checked_by(&root, "w1", &ids[2]);
    quiet(&root, "w1");
    assert_eq!(prompts(&root).len(), 2, "the first check's call, then the newest's; the superseded one never called");
}

/// The session ends while its check's model call runs: the check records its decision, routes every
/// lesson nowhere ("session ended"), and leaves no queue line, no job file and no session lock.
#[test]
fn a_check_finishing_after_the_end_enqueues_nothing() {
    let root = Root(setup("async-end"));
    blocking_fake(&root);
    let env = [ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")];
    hook(&root, "e1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    edit(&root, "e1", &env);
    stop(&root, "e1", &env);
    let check = checks(&root, "e1")[0].clone();
    until("the model call", || root.join("started").exists());
    // The end hook ends the session (presence.rs); without learning it starts no `borg learn`, which
    // would outlive the test.
    hook(&root, "e1", "end", json!({}), &[ASYNC[0], ("BORG_LEARN", "off")]);
    release(&root);
    checked_by(&root, "e1", &check);
    quiet(&root, "e1");
    assert!(state(&root, "e1", "queued").is_empty(), "nothing queued: {}", state(&root, "e1", "queued"));
    assert!(state(&root, "e1", "live.lock").is_empty());
    let log = outcomes(&root);
    assert!(log.iter().any(|o| o["kind"] == "graded" && o["check"] == check.as_str()), "the decision is recorded: {log:?}");
    let dropped = log.iter().find(|o| o["kind"] == "delivery" && o["check"] == check.as_str()).expect("a delivery record");
    assert_eq!((dropped["route"].as_str(), dropped["reason"].as_str()), (Some("dropped_done"), Some("session ended")), "{dropped}");
}

/// A delivery open at the stop and settled by the session-end judge while the check's call ran keeps the
/// session-end judge's verdict: the check records none of its own on it.
#[test]
fn pending_settled_by_the_end_judge_is_not_judged_twice() {
    let root = Root(setup("async-judged"));
    blocking_fake(&root);
    hook(&root, "j1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    std::fs::write(dir(&root).join("j1.queued"), "Mdrain @0123456789ab\n").unwrap();
    let given = context(&hook(&root, "j1", "prompt", json!({"prompt": "now the budget place"}), &ASYNC));
    assert!(given.contains("drain the budget place"), "handed over: {given}");
    edit(&root, "j1", &ASYNC);
    let verdict =
        json!({"structured_output": {"moves": [], "applies": [], "resolved": [{"id": "Mdrain", "verdict": "changed", "reason": "r"}]}, "total_cost_usd": 0.05});
    std::fs::write(root.join("answer.json"), verdict.to_string()).unwrap();
    stop(&root, "j1", &ASYNC);
    let check = checks(&root, "j1")[0].clone();
    until("the model call", || root.join("started").exists());
    assert!(prompts(&root)[0].contains("[Mdrain] lesson:"), "the check judges the open delivery: {}", prompts(&root)[0]);
    // What the session-end judge writes when it settles the delivery meanwhile.
    let ctx = dir(&root).join("j1.ctx");
    let mut text = std::fs::read_to_string(&ctx).unwrap();
    text.push_str("#borg-judged Mdrain=ignored\n#borg-end-judged Mdrain\n");
    std::fs::write(&ctx, text).unwrap();
    release(&root);
    checked_by(&root, "j1", &check);
    quiet(&root, "j1");
    let ctx = state(&root, "j1", "ctx");
    assert_eq!(ctx.lines().filter(|l| l.starts_with("#borg-judged Mdrain=")).collect::<Vec<_>>(), ["#borg-judged Mdrain=ignored"], "{ctx}");
}

/// A detached check's `graded` record keeps the model's moves and its why per apply, says it ran
/// detached, and names the lesson line its prompt showed: `a2` by default, each lesson with the
/// engineer's correction the store holds for it (a lesson without one reads as `a1`); its `check` event
/// carries the call's limit and the process's peak memory. `BORG_LESSON_LINE=a1` keeps the line before
/// 0.7.7, under a prompt hash of its own.
#[test]
fn a_detached_checks_record_keeps_moves_whys_and_its_lesson_line() {
    let root = Root(setup("async-record"));
    let store = root.join("store/collectives/corp");
    let drain = json!({"id": "Mdrain", "origin_ms": 1, "scope": ["Orion"], "episodes": ["gone-1", "e1"], "question": "Does the Orion net drain the budget place with an explicit consumer instead of a reset arc?", "consequence": "a reset arc drops the budget token"});
    let mut lessons = std::fs::read_to_string(store.join("lessons.jsonl")).unwrap();
    lessons.push_str(&format!("{drain}\n"));
    std::fs::write(store.join("lessons.jsonl"), lessons).unwrap();
    let episode = json!({"id": "e1", "ms": 1, "repo": "r", "session": "s0", "task": "t", "agent": "I reset the arc.", "rejected_tool": null, "correction": "no,\n drain it with a consumer"});
    std::fs::write(store.join("episodes.jsonl"), format!("{episode}\n")).unwrap();
    let said = json!({"structured_output": {"moves": ["reset a token place under a budget", "drop a consumer"], "applies": [{"id": "Mdrain", "why": "the arc resets the budget", "grade": 3}]}, "total_cost_usd": 0.05});
    std::fs::write(root.join("answer.json"), said.to_string()).unwrap();
    let graded = |session: &str, line: &[(&str, &str)]| -> Value {
        let env: Vec<(&str, &str)> = [ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")].into_iter().chain(line.iter().copied()).collect();
        hook(&root, session, "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
        edit(&root, session, &env);
        stop(&root, session, &env);
        let check = checks(&root, session)[0].clone();
        checked_by(&root, session, &check);
        quiet(&root, session);
        outcomes(&root).into_iter().find(|o| o["kind"] == "graded" && o["check"] == check.as_str()).expect("graded")
    };
    let a2 = graded("r2", &[]);
    let prompt = prompts(&root).pop().unwrap();
    assert!(prompt.contains("a reset arc drops the budget token | the engineer's correction: \"no, drain it with a consumer\""), "{prompt}");
    assert!(
        !prompt.contains("[G0] Did I check the websocket handling for the websocket edge case and the websocket limits? -> websocket breaks |"),
        "no correction, no a2 tail: {prompt}"
    );
    assert_eq!(
        (&a2["moves"], &a2["whys"], &a2["lessons"]),
        (&json!(["reset a token place under a budget", "drop a consumer"]), &json!(["the arc resets the budget"]), &json!(["Mdrain"])),
        "{a2}"
    );
    assert_eq!((a2["run"].as_str(), a2["line"].as_str(), a2["channel"].as_str()), (Some("async"), Some("a2"), Some("live")), "{a2}");
    let events = std::fs::read_to_string(store.join("stats/events.jsonl")).unwrap_or_default();
    let check: Value = events.lines().filter_map(|l| serde_json::from_str(l).ok()).find(|e: &Value| e["kind"] == "check").expect(&events);
    assert!(check["mode"] == "async" && check["limit_ms"].as_u64() > Some(0) && check["rss_kb"].as_u64() > Some(0), "{check}");
    let a1 = graded("r1", &[("BORG_LESSON_LINE", "a1")]);
    assert!(!prompts(&root).pop().unwrap().contains("the engineer's correction"));
    assert_eq!(a1["line"].as_str(), Some("a1"), "{a1}");
    assert_ne!(a1["prompt_hash"], a2["prompt_hash"], "a1 and a2 never mix in what is learned of the grader");
}

fn events(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("store/collectives/corp/stats/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The stop never waits on the session lock: one held by a live process (a session-end judge's) leaves
/// the stop's answer, its turn marker and its job as they are with the lock free; the check then leaves
/// the open deliveries to that holder.
#[test]
fn a_stop_returns_while_the_session_lock_is_held() {
    let root = Root(setup("async-held"));
    blocking_fake(&root);
    hook(&root, "h1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "h1", &ASYNC);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let lock = dir(&root).join("h1.live.lock");
    std::fs::write(&lock, format!("{} {now}\n", std::process::id())).unwrap();
    let out = stop(&root, "h1", &ASYNC);
    assert!(!out.contains("block") && state(&root, "h1", "checked").is_empty(), "returned at once: {out}");
    let ids = checks(&root, "h1");
    assert_eq!(ids.len(), 1, "the turn ends at the stop: {ids:?}");
    until("the model call", || root.join("started").exists());
    // The check's write waits for the lock (it holds no other meanwhile): let it go, then the answer lands.
    let _ = std::fs::remove_file(&lock);
    release(&root);
    checked_by(&root, "h1", &ids[0]);
    quiet(&root, "h1");
}

/// A job a dead check left waiting (`<s>.job.next`, from an older stop) never runs after a newer one:
/// superseded and counted, so the session's latest check stays the newest turn's.
#[test]
fn an_older_waiting_job_never_runs_after_a_newer_one() {
    let root = Root(setup("async-older"));
    let env = [ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")];
    hook(&root, "o2", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    edit(&root, "o2", &env);
    let old = json!({"v": 1, "session": "o2", "check": "0000000old00", "stop_ms": 1, "harness": "claude", "cwd": root.join("harbor_orion"),
        "offset": 0, "message": "older work", "proposal": "older work", "changed": true, "repo": "harbor_orion"});
    std::fs::write(dir(&root).join("o2.job.next"), old.to_string()).unwrap();
    stop(&root, "o2", &env);
    let check = checks(&root, "o2")[0].clone();
    checked_by(&root, "o2", &check);
    quiet(&root, "o2");
    assert_eq!(checked(&root, "o2")["check"], check.as_str(), "the newest turn's check is the latest");
    assert_eq!(prompts(&root).len(), 1, "the older job never called the model");
    assert!(events(&root).iter().any(|e| e["kind"] == "check_dropped" && e["why"] == "superseded"), "counted: {:?}", events(&root));
}

/// A model call that fails leaves the check failed (`<s>.checked`), nothing queued, and no job file or
/// session lock behind; the stop had returned already.
#[test]
fn a_failed_call_leaves_nothing_behind() {
    let root = Root(setup("async-failed"));
    let fake = root.join("bin/claude");
    std::fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nexit 1\n").unwrap();
    hook(&root, "f1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "f1", &ASYNC);
    stop(&root, "f1", &ASYNC);
    let check = checks(&root, "f1")[0].clone();
    let done = checked_by(&root, "f1", &check);
    quiet(&root, "f1");
    assert_eq!((done["failed"].as_bool(), done["asks"].clone()), (Some(true), json!([])), "{done}");
    assert!(state(&root, "f1", "queued").is_empty() && state(&root, "f1", "live.lock").is_empty());
    assert!(jobs(&root, "f1").is_empty());
}

/// A stop removes what a gone check left: its claimed job and a result it was writing, by their dead
/// pid; a live process's claimed job stays.
#[test]
fn a_stop_sweeps_a_dead_checks_files() {
    let root = Root(setup("async-sweep"));
    let mut done = std::process::Command::new("true").spawn().unwrap();
    let dead = done.id();
    done.wait().unwrap();
    let me = std::process::id();
    for name in [format!("s3.job.00000000dead.{dead}"), format!("s3.checked.{dead}"), format!("s3.job.000000000live.{me}")] {
        std::fs::create_dir_all(dir(&root)).unwrap();
        std::fs::write(dir(&root).join(name), "{}").unwrap();
    }
    hook(&root, "s3", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "s3", &ASYNC);
    stop(&root, "s3", &ASYNC);
    let check = checks(&root, "s3")[0].clone();
    checked_by(&root, "s3", &check);
    assert!(!dir(&root).join(format!("s3.job.00000000dead.{dead}")).exists() && !dir(&root).join(format!("s3.checked.{dead}")).exists());
    assert!(dir(&root).join(format!("s3.job.000000000live.{me}")).exists(), "a live process's job stays");
    std::fs::remove_file(dir(&root).join(format!("s3.job.000000000live.{me}"))).unwrap();
    quiet(&root, "s3");
}

/// A check judges the turn its stop saw: a delivery written to the .edits file after that stop (the next
/// turn's early lesson) is neither judged by it nor counted as work since an earlier delivery.
#[test]
fn a_check_judges_none_of_the_next_turns_deliveries() {
    let root = Root(setup("async-edits"));
    blocking_fake(&root);
    hook(&root, "d1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "d1", &ASYNC);
    stop(&root, "d1", &ASYNC);
    until("the first model call", || root.join("started").exists());
    // A second turn end while the first check runs: its job waits, with the .edits file as it is now.
    stop(&root, "d1", &ASYNC);
    let ids = checks(&root, "d1");
    until("the second job waiting", || dir(&root).join("d1.job.next").exists());
    // Then the next turn hands an early lesson over.
    let edits = dir(&root).join("d1.edits");
    let mut text = std::fs::read_to_string(&edits).unwrap_or_default();
    text.push_str("the next turn's work\n#borg-early G0 @0123456789ab\n");
    std::fs::write(&edits, text).unwrap();
    release(&root);
    checked_by(&root, "d1", &ids[1]);
    quiet(&root, "d1");
    let all = prompts(&root);
    assert_eq!(all.len(), 2, "both checks called the model");
    assert!(!all[1].contains("[G0] lesson:"), "the next turn's delivery is not judged: {}", all[1]);
}

/// The session ends while its check's model call runs: the check records no verdict on the deliveries it
/// judged (the session-end judge has them, and `borg learn` may have settled them already).
#[test]
fn a_check_finishing_after_the_end_records_no_verdict() {
    let root = Root(setup("async-end-verdict"));
    blocking_fake(&root);
    let env = [ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")];
    hook(&root, "v1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    std::fs::write(dir(&root).join("v1.queued"), "Mdrain @0123456789ab\n").unwrap();
    hook(&root, "v1", "prompt", json!({"prompt": "now the budget place"}), &env);
    edit(&root, "v1", &env);
    let verdict =
        json!({"structured_output": {"moves": [], "applies": [], "resolved": [{"id": "Mdrain", "verdict": "changed", "reason": "r"}]}, "total_cost_usd": 0.05});
    std::fs::write(root.join("answer.json"), verdict.to_string()).unwrap();
    stop(&root, "v1", &env);
    let check = checks(&root, "v1")[0].clone();
    until("the model call", || root.join("started").exists());
    assert!(prompts(&root)[0].contains("[Mdrain] lesson:"), "the check judges the open delivery");
    hook(&root, "v1", "end", json!({}), &[ASYNC[0], ("BORG_LEARN", "off")]);
    release(&root);
    checked_by(&root, "v1", &check);
    quiet(&root, "v1");
    let ctx = state(&root, "v1", "ctx");
    assert!(!ctx.contains("#borg-judged Mdrain="), "{ctx}");
}

/// Asks a wake reserved (`<s>.queued.wake`) with no waiter left to deliver them go back to the queue at
/// the next prompt, which delivers them.
#[test]
fn a_reserved_ask_with_no_waiter_is_delivered_by_the_next_prompt() {
    let root = Root(setup("async-reclaim"));
    hook(&root, "r1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "r1", &ASYNC);
    stop(&root, "r1", &ASYNC);
    let check = checks(&root, "r1")[0].clone();
    checked_by(&root, "r1", &check);
    quiet(&root, "r1");
    // What a waiter that died after reserving the ask leaves.
    std::fs::rename(dir(&root).join("r1.queued"), dir(&root).join("r1.queued.wake")).unwrap();
    let next = context(&hook(&root, "r1", "prompt", json!({"prompt": "now the retries"}), &ASYNC));
    assert!(next.contains("[Mdrain]"), "delivered: {next}");
    assert!(state(&root, "r1", "queued.wake").is_empty() && state(&root, "r1", "queued").is_empty());
    assert!(state(&root, "r1", "ctx").contains(&format!("#borg-asked Mdrain @{check}")));
}

/// `borg check --job <job>` in the foreground, with the routed store and state a stop passes on; fails
/// the test when it has not ended by itself within 20 s (stopped then with SIGINT, as an engineer would).
fn check_job(root: &Path, job: &Path, env: &[(&str, &str)]) {
    let (home, state) = (root.join("store/collectives/corp"), dir(root));
    let (home, state) = (home.to_string_lossy().into_owned(), state.to_string_lossy().into_owned());
    let mut all: Vec<(&str, &str)> = env.to_vec();
    all.extend([("BORG_HOME", home.as_str()), ("BORG_STATE", state.as_str())]);
    let mut child = common::command(root, &["check", "--job", &job.to_string_lossy()], &all)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            // SAFETY: kill(2) on the check this test started.
            unsafe { libc::kill(i32::try_from(child.id()).unwrap(), libc::SIGINT) };
            let _ = child.wait();
            panic!("the check never ended by itself");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A job of session `s` written by hand where a stop writes it, its turn the state file as it is now.
fn job_file(root: &Path, s: &str, check: &str, stop_ms: i64) -> std::path::PathBuf {
    let offset = state(root, s, "ctx").len();
    let job = json!({"v": 1, "session": s, "check": check, "stop_ms": stop_ms, "harness": "claude", "cwd": root.join("harbor_orion"),
        "offset": offset, "message": "I drop the budget token with a reset arc.", "proposal": "I drop the budget token with a reset arc.",
        "changed": true, "repo": "harbor_orion"});
    let path = dir(root).join(format!("{s}.job.{check}"));
    std::fs::write(&path, job.to_string()).unwrap();
    path
}

/// A waiting job no check can take (a format this binary does not read) is removed and counted, never
/// looked at again: the running check ends after its own job.
#[test]
fn a_waiting_job_that_cannot_be_read_is_dropped_not_spun_on() {
    let root = Root(setup("async-spin"));
    hook(&root, "x1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "x1", &ASYNC);
    let job = job_file(&root, "x1", "000000000001", 1);
    std::fs::write(dir(&root).join("x1.job.next"), "{}").unwrap();
    // Learning on: the drop is counted (stats.rs `record`).
    check_job(&root, &job, &[ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")]);
    assert_eq!(checked(&root, "x1")["check"], "000000000001", "its own job ran");
    assert!(jobs(&root, "x1").is_empty(), "no job file left: {:?}", jobs(&root, "x1"));
    assert!(events(&root).iter().any(|e| e["kind"] == "check_dropped" && e["why"] == "unplaced"), "counted: {:?}", events(&root));
}

/// A job whose session ended after its stop (the session-end judge's line past the job's offset) calls
/// no model and queues nothing; counted as gone.
#[test]
fn a_check_whose_session_ended_before_the_call_calls_no_model() {
    let root = Root(setup("async-gone"));
    blocking_fake(&root);
    hook(&root, "g1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "g1", &ASYNC);
    let job = job_file(&root, "g1", "000000000002", 1);
    let ctx = dir(&root).join("g1.ctx");
    let mut text = std::fs::read_to_string(&ctx).unwrap();
    text.push_str("#borg-end-judged Mdrain\n");
    std::fs::write(&ctx, text).unwrap();
    check_job(&root, &job, &[ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")]);
    assert!(!root.join("started").exists(), "no model call");
    assert!(events(&root).iter().any(|e| e["kind"] == "check_dropped" && e["why"] == "gone"), "counted: {:?}", events(&root));
    assert!(state(&root, "g1", "queued").is_empty() && !job.exists() && jobs(&root, "g1").is_empty());
}

/// A reserved ask whose release cannot be written to the queue stays reserved, never lost: the next
/// prompt that can write it delivers it.
#[test]
fn a_reserved_ask_whose_release_fails_stays_reserved() {
    let root = Root(setup("async-release"));
    hook(&root, "l1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &ASYNC);
    edit(&root, "l1", &ASYNC);
    stop(&root, "l1", &ASYNC);
    let check = checks(&root, "l1")[0].clone();
    checked_by(&root, "l1", &check);
    quiet(&root, "l1");
    // What a waiter that died after reserving the ask leaves, and a queue that cannot be written: a
    // link to a directory that is not there.
    std::fs::rename(dir(&root).join("l1.queued"), dir(&root).join("l1.queued.wake")).unwrap();
    std::os::unix::fs::symlink(root.join("missing/queued"), dir(&root).join("l1.queued")).unwrap();
    let first = context(&hook(&root, "l1", "prompt", json!({"prompt": "now the retries"}), &ASYNC));
    assert!(!first.contains("[Mdrain]"), "{first}");
    assert_eq!(state(&root, "l1", "queued.wake"), format!("live Mdrain @{check}\n"), "still reserved");
    let next = context(&hook(&root, "l1", "prompt", json!({"prompt": "and the backoff"}), &ASYNC));
    assert!(next.contains("[Mdrain]"), "delivered: {next}");
    assert!(state(&root, "l1", "queued.wake").is_empty() && state(&root, "l1", "queued").is_empty());
}

/// A newer check that asks a waiting ask again takes the older line off the queue as superseded (its
/// own `delivery` route, outcome.rs `ROUTE_SUPERSEDED`): no undelivered question in what is learned.
#[test]
fn a_newer_check_asking_a_waiting_ask_again_supersedes_it() {
    let root = Root(setup("async-supersede"));
    let env = [ASYNC[0], ASYNC[1], ASYNC[2], ("BORG_LEARN", "on")];
    hook(&root, "u1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    edit(&root, "u1", &env);
    stop(&root, "u1", &env);
    let first = checks(&root, "u1")[0].clone();
    checked_by(&root, "u1", &first);
    quiet(&root, "u1");
    stop(&root, "u1", &env);
    let second = checks(&root, "u1")[1].clone();
    let done = checked_by(&root, "u1", &second);
    quiet(&root, "u1");
    assert_eq!((done["superseded"].clone(), done["asks"].clone()), (json!(["Mdrain"]), json!(["Mdrain"])), "{done}");
    assert_eq!(state(&root, "u1", "queued"), format!("live Mdrain @{second}\n"));
    let route = outcomes(&root).into_iter().find(|o| o["kind"] == "delivery" && o["check"] == first.as_str()).expect("a delivery record");
    assert_eq!(
        (route["route"].as_str(), route["reason"].as_str(), route["channel"].as_str()),
        (Some("superseded"), Some("superseded"), Some("live")),
        "{route}"
    );
}
