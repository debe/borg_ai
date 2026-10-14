//! Live mode (live.rs) end to end through the binary: a change in PostToolUse starts a detached `borg
//! check`, its verdict waits in `.queued`, the next tool call delivers it as "borg check of your work so
//! far", and a stop whose changes a check already covered costs no model call. The model is the fake
//! `claude` of tests/common: no test makes a real model call.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{answer, borg, context, prompts, setup, state};
use serde_json::{Value, json};

const ENV: [(&str, &str); 3] = [("BORG_MODE", "live"), ("BORG_PREFILTER", "off"), ("BORG_EARLY", "off")];

/// Waits (at most 20 s) until the session's state file `ext` satisfies `done`.
fn wait_for(root: &Path, session: &str, ext: &str, done: impl Fn(&str) -> bool) -> String {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let text = state(root, session, ext);
        if done(&text) || Instant::now() > until {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn tool(root: &Path, session: &str, name: &str, input: Value) -> String {
    let transcript = root.join("transcript.jsonl");
    let ev = json!({"session_id": session, "cwd": root.join("harbor_orion"), "transcript_path": transcript, "tool_name": name, "tool_input": input});
    borg(root, "post_tool", &ev, &ENV)
}

#[test]
fn a_change_starts_a_check_whose_asks_arrive_with_the_next_tool_call_and_cover_the_stop() {
    let root = setup("live");
    let cwd = root.join("harbor_orion");
    // This turn so far, as the check reads it (learn.rs `turn_so_far`).
    let lines = [
        json!({"type": "user", "message": {"role": "user", "content": "Tidy the budget handling of the Orion net"}}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "text", "text": "I will drop the budget token with a reset arc."},
            {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "/x/net.rs"}}]}}),
    ];
    std::fs::write(root.join("transcript.jsonl"), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    borg(&root, "prompt", &json!({"session_id": "l1", "cwd": cwd, "prompt": "Tidy the budget handling of the Orion net"}), &ENV);
    answer(&root, &[("Mdrain", 3)]);

    // A read changes nothing: no check.
    tool(&root, "l1", "Read", json!({"file_path": "/x/net.rs"}));
    assert_eq!(state(&root, "l1", "live.n"), "");
    // An edit starts one; the hook itself returns at once with nothing to say yet.
    let out = tool(&root, "l1", "Edit", json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "reset arc"}));
    assert!(context(&out).is_empty(), "{out}");
    let queued = wait_for(&root, "l1", "queued", |q| q.contains("live "));
    assert_eq!(queued, "live Mdrain\n", "a 3 is asked, at the next tool call");
    let mark = wait_for(&root, "l1", "live.mark", |m| !m.is_empty());
    assert!(mark.starts_with("1 "), "the check covered the one change: {mark}");
    let p = &prompts(&root)[0];
    assert!(p.contains("I will drop the budget token with a reset arc.") && p.contains("[Edit] /x/net.rs"), "the turn so far: {p}");
    let ctx = state(&root, "l1", "ctx");
    assert!(ctx.contains("#borg-predict-cost 0.0500") && !ctx.contains("#borg-asked") && !ctx.contains("#borg-skip"), "{ctx}");
    assert!(state(&root, "l1", "cost").contains("live usd=0.0500 lessons="), "{}", state(&root, "l1", "cost"));

    // The next tool call delivers it, as a question to answer; asked from then on.
    let next = context(&tool(&root, "l1", "Read", json!({"file_path": "/x/budget.rs"})));
    assert!(next.starts_with("[borg] borg check of your work so far:") && next.contains("[Mdrain]"), "{next}");
    let ctx = state(&root, "l1", "ctx");
    assert!(ctx.contains("#borg-asked Mdrain\n#borg-live Mdrain ms="), "{ctx}");
    let edits = state(&root, "l1", "edits");
    assert!(
        edits.contains("a reset arc. [Edit] /x/net.rs\n#borg-asked Mdrain\n#borg-live Mdrain ms="),
        "the work so far is the judge's evidence before the ask: {edits}"
    );
    assert!(!root.join("store/collectives/corp/state/l1.queued").exists(), "delivered once");
    assert!(context(&tool(&root, "l1", "Read", json!({"file_path": "/x/c.rs"}))).is_empty());

    // The stop: the check covered every change and none came since, so no model call and no block.
    let stop = |last: &str| borg(&root, "stop", &json!({"session_id": "l1", "cwd": cwd, "last_assistant_message": last, "stop_hook_active": false}), &ENV);
    let out = stop("Done: the budget place is drained. Mdrain: applies");
    assert!(!out.contains("block"), "{out}");
    assert_eq!(prompts(&root).len(), 1);
    assert!(state(&root, "l1", "ctx").ends_with("#borg-skip live_covered\n"), "{}", state(&root, "l1", "ctx"));

    // Next turn: an edit within the debounce starts no check, so the stop is not covered and asks the
    // model itself; the lesson asked live is not asked again.
    borg(&root, "prompt", &json!({"session_id": "l1", "cwd": cwd, "prompt": "now the retries"}), &ENV);
    tool(&root, "l1", "Edit", json!({"file_path": "/x/retry.rs", "old_string": "a", "new_string": "fixed delay"}));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(prompts(&root).len(), 1, "debounced");
    answer(&root, &[("New", 3)]);
    let out = stop("Retries use a fixed delay.");
    assert!(out.contains("block") && out.contains("[New]"), "{out}");
    let p = prompts(&root);
    assert_eq!(p.len(), 2);
    assert!(!p[1].contains("[Mdrain] Does the Orion"), "asked live, so done: {}", p[1]);
    // The same call judges what the drone did with it; its answer at the covered stop is the evidence.
    assert!(p[1].contains("[Mdrain] question: ") && p[1].contains("(declared: applies)"), "{}", p[1]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_waiting_ask_blocks_the_stop_and_a_check_covers_its_own_turn_only() {
    let root = setup("live-wait");
    let cwd = root.join("harbor_orion");
    let lines = [
        json!({"type": "user", "message": {"role": "user", "content": "Tidy the budget handling"}}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "/x/net.rs"}}]}}),
    ];
    std::fs::write(root.join("transcript.jsonl"), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    borg(&root, "prompt", &json!({"session_id": "w1", "cwd": cwd, "prompt": "Tidy the budget handling"}), &ENV);
    answer(&root, &[("Mdrain", 3)]);
    // The last tool call of the turn is a change; its check's 3 waits, and no tool call follows.
    tool(&root, "w1", "Edit", json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "reset arc"}));
    assert_eq!(wait_for(&root, "w1", "queued", |q| q.contains("live ")), "live Mdrain\n");
    wait_for(&root, "w1", "live.mark", |m| !m.is_empty());
    let stop = |last: &str, active: bool| {
        borg(&root, "stop", &json!({"session_id": "w1", "cwd": cwd, "last_assistant_message": last, "stop_hook_active": active}), &ENV)
    };
    // Covered, so no model call: but the waiting question is asked, as the stop would have.
    let out = stop("Done: the budget place is drained.", false);
    assert!(out.contains("block") && out.contains("[Mdrain]"), "{out}");
    assert_eq!(prompts(&root).len(), 1, "no second model call");
    assert!(!root.join("store/collectives/corp/state/w1.queued").exists(), "taken by the stop");
    assert!(state(&root, "w1", "ctx").contains("\n#borg-asked Mdrain\n"), "{}", state(&root, "w1", "ctx"));
    stop("Mdrain: applies", true);
    // The next turn only talks: the earlier turn's check does not cover it, so the stop asks the model.
    borg(&root, "prompt", &json!({"session_id": "w1", "cwd": cwd, "prompt": "what about the retries?"}), &ENV);
    answer(&root, &[]);
    let out = stop("The retries use a fixed delay.", false);
    assert!(!out.contains("block"), "{out}");
    assert_eq!(prompts(&root).len(), 2, "this turn's message is checked");
    assert!(!state(&root, "w1", "ctx").contains("live_covered"), "{}", state(&root, "w1", "ctx"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_later_check_in_the_same_turn_leaves_an_unanswered_live_ask_to_the_stop() {
    let root = setup("live-judge");
    let cwd = root.join("harbor_orion");
    std::fs::write(root.join("transcript.jsonl"), format!("{}\n", json!({"type": "user", "message": {"role": "user", "content": "Tidy it"}}))).unwrap();
    borg(&root, "prompt", &json!({"session_id": "j1", "cwd": cwd, "prompt": "Tidy it"}), &ENV);
    answer(&root, &[("Mdrain", 3)]);
    let env = [ENV[0], ENV[1], ENV[2], ("BORG_LIVE_DEBOUNCE_S", "0")];
    let tool = |name: &str, input: Value| {
        let ev = json!({"session_id": "j1", "cwd": cwd, "transcript_path": root.join("transcript.jsonl"), "tool_name": name, "tool_input": input});
        borg(&root, "post_tool", &ev, &env)
    };
    tool("Edit", json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "reset arc"}));
    wait_for(&root, "j1", "queued", |q| q.contains("live "));
    wait_for(&root, "j1", "live.mark", |m| !m.is_empty());
    assert!(context(&tool("Read", json!({"file_path": "/x/a.rs"}))).contains("[Mdrain]"));
    answer(&root, &[]);
    tool("Edit", json!({"file_path": "/x/b.rs", "old_string": "a", "new_string": "b"}));
    wait_for(&root, "j1", "live.mark", |m| m.starts_with("2 "));
    let p = prompts(&root);
    assert_eq!(p.len(), 2, "a second check ran");
    assert!(!p[1].contains("[Mdrain] question: "), "not judged before the drone answered it: {}", p[1]);
    assert!(!state(&root, "j1", "ctx").contains("#borg-judged"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn hook_mode_and_word_matching_start_no_check() {
    let root = setup("live-off");
    let cwd = root.join("harbor_orion");
    let edit = |env: &[(&str, &str)]| {
        let ev = json!({"session_id": "h1", "cwd": cwd, "tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}});
        borg(&root, "post_tool", &ev, env)
    };
    edit(&[("BORG_MODE", "hook"), ("BORG_PREFILTER", "off")]);
    edit(&[("BORG_MODE", "live"), ("BORG_MATCH", "bm25")]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(prompts(&root).is_empty());
    assert_eq!(state(&root, "h1", "live.n"), "", "neither counts a change for a check");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_live_check_has_no_word_gate_and_a_later_tool_call_checks_a_change_the_debounce_held_back() {
    let root = setup("live-gate");
    let cwd = root.join("harbor_orion");
    // Words that touch no lesson: the stop's gate would skip this turn; a live check must not.
    let lines = [
        json!({"type": "user", "message": {"role": "user", "content": "Rename the helper"}}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "/x/a.rs"}}]}}),
    ];
    let transcript = root.join("transcript.jsonl");
    std::fs::write(&transcript, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    answer(&root, &[("Mdrain", 3)]);
    let env = [("BORG_MODE", "live"), ("BORG_EARLY", "off"), ("BORG_LIVE_DEBOUNCE_S", "1")];
    let call = |name: &str, input: Value| {
        let ev = json!({"session_id": "g1", "cwd": cwd, "transcript_path": transcript, "tool_name": name, "tool_input": input});
        borg(&root, "post_tool", &ev, &env)
    };

    call("Edit", json!({"file_path": "/x/a.rs", "old_string": "a", "new_string": "b"}));
    let mark = wait_for(&root, "g1", "live.mark", |m| m.starts_with("1 "));
    assert!(mark.starts_with("1 "), "the check ran despite the gate and covered the change: {mark}");
    assert!(!state(&root, "g1", "ctx").contains("#borg-live-skip"), "{}", state(&root, "g1", "ctx"));
    assert_eq!(prompts(&root).len(), 1);

    // A second change inside the debounce starts nothing; a read once it has passed checks it.
    call("Edit", json!({"file_path": "/x/a.rs", "old_string": "b", "new_string": "c"}));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(prompts(&root).len(), 1, "debounced");
    std::thread::sleep(Duration::from_millis(1000));
    call("Read", json!({"file_path": "/x/a.rs"}));
    let mark = wait_for(&root, "g1", "live.mark", |m| m.starts_with("2 "));
    assert!(mark.starts_with("2 "), "the held-back change was checked at the next tool call: {mark}");
    assert_eq!(state(&root, "g1", "live.n").lines().count(), 2, "a read is no change");
}
