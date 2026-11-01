//! Live mode is gone (0.7.6, docs/delivery.md "Live mode (removed in 0.7.6)"): `BORG_MODE=live` and
//! `"mode": "live"` behave as no mode, a tool call starts no check, and the stop makes its own call. What
//! stays of live.rs is the session lock the stop holds; a queue line a live check wrote before the
//! upgrade is handed over at the next prompt. The model is the fake `claude` of tests/common: no test makes a real
//! model call.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{answer, borg_as, context, prompts, setup, state};
use serde_json::{Value, json};

const ENV: [(&str, &str); 2] = [("BORG_PREFILTER", "off"), ("BORG_EARLY", "off")];

/// Off Claude Code the model call is pinned to the fake `claude`: it would otherwise go through the
/// harness's own CLI.
const PINNED: (&str, &str) = ("BORG_PREDICT_MODEL", "claude:sonnet");

/// The hook of `harness` for session `session`, with `extra` added to the event.
fn hook(root: &Path, harness: &str, session: &str, event: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut ev = json!({"session_id": session, "cwd": root.join("harbor_orion")});
    ev.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    borg_as(root, harness, event, &ev, env)
}

/// `BORG_MODE=live` and a collective's `"mode": "live"` are accepted and ignored: an edit counts no
/// change and starts no check (the hook used to write `.live.n` before it spawned one), and the stop
/// makes its own model call and blocks for a 3, as with no mode at all.
#[test]
fn a_live_mode_left_set_behaves_like_no_mode() {
    for (harness, by_config) in [("claude", false), ("claude", true), ("omp", false), ("opencode", true)] {
        let root = setup(&format!("live-gone-{harness}-{by_config}"));
        if by_config {
            let cfg = json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("harbor_orion")], "share": "local", "mode": "live"}]});
            std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
        }
        let mut env = vec![ENV[0], ENV[1], PINNED];
        if !by_config {
            env.push(("BORG_MODE", "live"));
        }
        let tag = format!("{harness} by_config={by_config}");
        hook(&root, harness, "m1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
        let out =
            hook(&root, harness, "m1", "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}), &env);
        assert!(context(&out).is_empty(), "{tag}: {out}");
        for ext in ["live.n", "live.mark", "live.lock", "queued"] {
            assert!(state(&root, "m1", ext).is_empty(), "{tag}: no live check bookkeeping ({ext})");
        }
        assert!(prompts(&root).is_empty(), "{tag}: no model call during the turn");
        let out =
            hook(&root, harness, "m1", "stop", json!({"last_assistant_message": "I drop the budget token with a reset arc.", "stop_hook_active": false}), &env);
        assert!(out.contains("block") && out.contains("[Mdrain]"), "{tag}: the stop makes its call: {out}");
        assert_eq!(prompts(&root).len(), 1, "{tag}: one model call, the stop's");
        let ctx = state(&root, "m1", "ctx");
        assert!(!ctx.contains("live_covered") && !ctx.contains("#borg-live"), "{tag}: {ctx}");
        assert!(state(&root, "m1", "live.lock").is_empty(), "{tag}: the stop let its lock go");
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The stop takes the session lock for its check, never waits for it: while another holds it (the
/// session-end judge of a resumed session) the stop still checks and asks, at once, and leaves the
/// holder's lock as it was.
#[test]
fn a_stop_never_waits_for_the_session_lock_and_leaves_anothers_alone() {
    let root = setup("live-lock");
    let env = [ENV[0], ENV[1], PINNED, ("BORG_STOP_BUDGET_MS", "28000")];
    hook(&root, "omp", "b1", "prompt", json!({"prompt": "Tidy the budget handling"}), &env);
    // A holder that never finishes: its process (this test) lives and its lock is fresh.
    let lock = root.join("store/collectives/corp/state/b1.live.lock");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let held = format!("{} {now}\n", std::process::id());
    std::fs::write(&lock, &held).unwrap();
    answer(&root, &[("Mdrain", 3)]);
    let started = Instant::now();
    let out = hook(&root, "omp", "b1", "stop", json!({"last_assistant_message": "I drop the budget token with a reset arc.", "stop_hook_active": false}), &env);
    let took = started.elapsed();
    assert!(out.contains("block") && out.contains("[Mdrain]"), "the stop still checks: {out}");
    // Live mode's stop waited a quarter of omp's 28 s (7 s) for a running check; none waits now.
    assert!(took < Duration::from_secs(6), "no wait for the lock: {took:?}");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), held, "another's lock is left as it was");
    let _ = std::fs::remove_dir_all(root);
}

/// While another holds the session lock (the session-end judge of a resumed session, which judges every
/// open delivery), a stop never asks a lesson whose delivery is still open: the judge's verdict on that
/// hand-over, and its `#borg-end-judged` line, would land on the new question and settle it unjudged.
/// With the lock the same stop asks it.
#[test]
fn a_stop_without_the_lock_leaves_open_deliveries_to_the_end_judge() {
    for held in [false, true] {
        let root = setup(&format!("live-open-{held}"));
        let env = [ENV[0], ENV[1], PINNED];
        hook(&root, "omp", "o1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
        hook(&root, "omp", "o1", "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}), &env);
        let dir = root.join("store/collectives/corp/state");
        std::fs::write(dir.join("o1.queued"), "Mdrain @0123456789ab\n").unwrap();
        let next = context(&hook(&root, "omp", "o1", "prompt", json!({"prompt": "now the retries"}), &env));
        assert!(next.contains("drain the budget place"), "handed over: {next}");
        if held {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
            std::fs::write(dir.join("o1.live.lock"), format!("{} {now}\n", std::process::id())).unwrap();
        }
        answer(&root, &[("Mdrain", 3)]);
        let out = hook(&root, "omp", "o1", "stop", json!({"last_assistant_message": "Retries done.", "stop_hook_active": false}), &env);
        assert_eq!(out.contains("[Mdrain]"), !held, "held={held}: {out}");
        assert_eq!(state(&root, "o1", "ctx").contains("#borg-asked Mdrain"), !held, "held={held}: {}", state(&root, "o1", "ctx"));
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A session that runs across the upgrade: the lines a 0.7.5 live check left in the queue are handed over
/// at the next prompt, with the last turn's work as the judge's evidence, and no stop blocks for them.
/// Each keeps the route its check decided, so the selection fold joins it: the check's ask (`live <ids>`)
/// is recorded asked on channel live (`#borg-asked` then `#borg-live`, as 0.7.5 did), its free lesson
/// (`instead-next <ids>`) free on channel queued with its own free propensity (`#borg-pf`).
#[test]
fn a_live_checks_queue_lines_from_before_the_upgrade_keep_their_routes() {
    let root = setup("live-legacy");
    let env = [ENV[0], ENV[1], PINNED, ("BORG_LEARN", "on")];
    let c = "0123456789ab";
    hook(&root, "omp", "e1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    hook(&root, "omp", "e1", "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}), &env);
    let dir = root.join("store/collectives/corp/state");
    let mut ctx = std::fs::read_to_string(dir.join("e1.ctx")).unwrap_or_default();
    ctx.push_str(&format!("#borg-p Mdrain=1,Pbeacon=0.3333 @{c}\n#borg-pf Pbeacon=0.6667 @{c}\n"));
    std::fs::write(dir.join("e1.ctx"), ctx).unwrap();
    std::fs::write(dir.join("e1.queued"), format!("live Mdrain @{c}\ninstead-next Pbeacon @{c}\n")).unwrap();
    let next = context(&hook(&root, "omp", "e1", "prompt", json!({"prompt": "now the retries"}), &env));
    assert!(next.starts_with("[borg] Peer lessons") && next.contains("drain the budget place") && next.contains("Beacon replay"), "a hand-over: {next}");
    let ctx = state(&root, "e1", "ctx");
    assert!(ctx.contains(&format!("#borg-asked Mdrain @{c}\n#borg-live Mdrain ms=")), "the ask keeps its route: {ctx}");
    assert!(ctx.contains(&format!("#borg-queued Pbeacon @{c}")) && !ctx.contains("#borg-queued Mdrain"), "{ctx}");
    let outcomes: Vec<Value> = std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let find = |kind: &str, lesson: &str| -> Value {
        outcomes.iter().find(|o| o["kind"] == kind && o["lessons"] == json!([lesson])).cloned().unwrap_or_else(|| panic!("{kind} {lesson}: {outcomes:?}"))
    };
    let (asked, delivered) = (find("asked", "Mdrain"), find("delivery", "Mdrain"));
    assert_eq!((asked["channel"].as_str(), asked["check"].as_str(), &asked["p"]), (Some("live"), Some(c), &json!([1.0])), "{asked}");
    assert_eq!(
        (delivered["channel"].as_str(), delivered["route"].as_str(), delivered["check"].as_str()),
        (Some("live"), Some("asked"), Some(c)),
        "{delivered}"
    );
    let (free, routed) = (find("asked", "Pbeacon"), find("delivery", "Pbeacon"));
    assert_eq!((free["channel"].as_str(), free["check"].as_str(), &free["p"]), (Some("queued"), Some(c), &json!([0.6667])), "{free}");
    assert_eq!((routed["channel"].as_str(), routed["route"].as_str()), (Some("queued"), Some("free")), "{routed}");
    let edits = state(&root, "e1", "edits");
    let lines: Vec<&str> = edits.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("#borg-asked Mdrain")).expect(&edits);
    assert!(at > 0 && lines[at - 1].contains("[Edit] /x/net.rs"), "the judge's evidence is the last turn's work: {edits}");
    assert_eq!(state(&root, "e1", "turn"), "", "the new turn starts empty");
    assert!(state(&root, "e1", "queued").is_empty(), "delivered once");
    answer(&root, &[("Mdrain", 3)]);
    let out = hook(&root, "omp", "e1", "stop", json!({"last_assistant_message": "Retries done.", "stop_hook_active": false}), &env);
    assert!(!out.contains("[Mdrain]"), "the ask is not asked again: {out}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn graded_2s_wait_for_the_next_prompt_on_omp() {
    let root = setup("live-graded-omp");
    let env = [ENV[0], ENV[1], PINNED];
    hook(&root, "omp", "q1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &env);
    hook(&root, "omp", "q1", "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}), &env);
    answer(&root, &[("Mdrain", 2)]);
    let out =
        hook(&root, "omp", "q1", "stop", json!({"last_assistant_message": "I dropped the budget token with a reset arc.", "stop_hook_active": false}), &env);
    assert!(!out.contains("block"), "a 2 does not block: {out}");
    assert!(state(&root, "q1", "cost").contains("queued"), "{}", state(&root, "q1", "cost"));
    let next = context(&hook(&root, "omp", "q1", "prompt", json!({"prompt": "now the retries"}), &env));
    assert!(next.contains("drain the budget place"), "the 2 arrives with the next prompt: {next}");
    assert!(state(&root, "q1", "ctx").contains("#borg-queued Mdrain"), "{}", state(&root, "q1", "ctx"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_stop_with_no_lesson_to_ask_and_none_to_judge_makes_no_model_call() {
    let root = setup("live-empty");
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), "").unwrap();
    let env = [ENV[0], ENV[1], PINNED];
    hook(&root, "claude", "n1", "prompt", json!({"prompt": "Tidy the budget handling"}), &env);
    hook(&root, "claude", "n1", "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}), &env);
    let out = hook(&root, "claude", "n1", "stop", json!({"last_assistant_message": "Done.", "stop_hook_active": false}), &env);
    assert!(!out.contains("block"), "{out}");
    assert!(prompts(&root).is_empty(), "no model call: {:?}", prompts(&root));
    assert!(state(&root, "n1", "ctx").contains("#borg-skip empty"), "{}", state(&root, "n1", "ctx"));
    let _ = std::fs::remove_dir_all(root);
}
