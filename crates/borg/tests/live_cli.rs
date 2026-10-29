//! Live mode (live.rs) end to end through the binary: a change in PostToolUse starts a detached `borg
//! check`, its verdict waits in `.queued`, the next tool call delivers it as "borg check of your work so
//! far", and a stop whose changes a check already covered costs no model call. The model is the fake
//! `claude` of tests/common: no test makes a real model call. omp and opencode send no transcript: their
//! checks read the turn's tool calls (`.turn`).

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{answer, borg, borg_as, context, prompts, setup, state};
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

/// A `.queued` file's lines without the check each names (` @<check>`, hook.rs, a 12-hex check id): what
/// is queued. Each line must name its check.
fn queued(text: &str) -> String {
    text.lines()
        .map(|l| {
            let (head, check) = l.rsplit_once(" @").unwrap_or_else(|| panic!("a queued line names its check: {l:?}"));
            assert!(check.len() == borg::select::CHECK_HEX && check.chars().all(|c| c.is_ascii_hexdigit()), "{l:?}");
            head.to_string() + "\n"
        })
        .collect()
}

/// A context file's lines with the check a marker names (` @<check>`) left out: the markers as they read
/// before checks had ids.
fn unchecked(text: &str) -> String {
    let is_check = |c: &str| c.len() == borg::select::CHECK_HEX && c.chars().all(|x| x.is_ascii_hexdigit());
    text.lines().map(|l| l.rsplit_once(" @").filter(|(_, c)| is_check(c)).map_or(l, |(head, _)| head).to_string() + "\n").collect()
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
    let q = wait_for(&root, "l1", "queued", |q| q.contains("live "));
    assert_eq!(queued(&q), "live Mdrain\n", "a 3 is asked, at the next tool call");
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
    let ctx = unchecked(&state(&root, "l1", "ctx"));
    assert!(ctx.contains("#borg-asked Mdrain\n#borg-live Mdrain ms="), "{ctx}");
    let edits = unchecked(&state(&root, "l1", "edits"));
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
    // The same call judges what the drone did with it, blind to how it was delivered; its answer at the
    // covered stop is the evidence, with the drone's declaration for the half of questions not stripped.
    let raw = state(&root, "l1", "ctx");
    let check = raw.lines().find_map(|l| l.strip_prefix("#borg-asked Mdrain @")).expect("the live check's ask");
    let judged = p[1].lines().find(|l| l.starts_with("[Mdrain] lesson: ")).unwrap_or_else(|| panic!("{}", p[1]));
    assert_eq!(judged.ends_with(" (declared: applies)"), !borg::hook::stripped(check, "Mdrain"), "{judged}");
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
    assert_eq!(queued(&wait_for(&root, "w1", "queued", |q| q.contains("live "))), "live Mdrain\n");
    wait_for(&root, "w1", "live.mark", |m| !m.is_empty());
    let stop = |last: &str, active: bool| {
        borg(&root, "stop", &json!({"session_id": "w1", "cwd": cwd, "last_assistant_message": last, "stop_hook_active": active}), &ENV)
    };
    // Covered, so no model call: but the waiting question is asked, as the stop would have.
    let out = stop("Done: the budget place is drained.", false);
    assert!(out.contains("block") && out.contains("[Mdrain]"), "{out}");
    assert_eq!(prompts(&root).len(), 1, "no second model call");
    assert!(!root.join("store/collectives/corp/state/w1.queued").exists(), "taken by the stop");
    assert!(unchecked(&state(&root, "w1", "ctx")).contains("\n#borg-asked Mdrain\n"), "{}", state(&root, "w1", "ctx"));
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
    assert!(!p[1].contains("[Mdrain] lesson: "), "not judged before the drone answered it: {}", p[1]);
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
    // A debounce no test run outlasts: only moving the last check's start back passes it.
    let env = [("BORG_MODE", "live"), ("BORG_EARLY", "off"), ("BORG_LIVE_DEBOUNCE_S", "3600")];
    let call = |name: &str, input: Value| {
        let ev = json!({"session_id": "g1", "cwd": cwd, "transcript_path": transcript, "tool_name": name, "tool_input": input});
        borg(&root, "post_tool", &ev, &env)
    };

    call("Edit", json!({"file_path": "/x/a.rs", "old_string": "a", "new_string": "b"}));
    let mark = wait_for(&root, "g1", "live.mark", |m| m.starts_with("1 "));
    assert!(mark.starts_with("1 "), "the check ran despite the gate and covered the change: {mark}");
    assert!(!state(&root, "g1", "ctx").contains("#borg-live-skip"), "{}", state(&root, "g1", "ctx"));
    assert_eq!(prompts(&root).len(), 1);

    // A second change inside the debounce starts nothing; a read once it has passed checks it. The debounce
    // passes when the mark's start (live.rs `Mark`: covered, started ms, turn) moves back by it.
    call("Edit", json!({"file_path": "/x/a.rs", "old_string": "b", "new_string": "c"}));
    // The first check has let go of its lock (it writes the mark just before).
    assert_eq!(wait_for(&root, "g1", "live.lock", str::is_empty), "");
    let mut parts: Vec<String> = mark.split_whitespace().map(str::to_string).collect();
    parts[1] = (parts[1].parse::<i64>().unwrap() - 3_600_000).to_string();
    std::fs::write(root.join("store/collectives/corp/state/g1.live.mark"), parts.join(" ") + "\n").unwrap();
    call("Read", json!({"file_path": "/x/a.rs"}));
    let mark = wait_for(&root, "g1", "live.mark", |m| m.starts_with("2 "));
    assert!(mark.starts_with("2 "), "the held-back change was checked at the next tool call: {mark}");
    assert_eq!(prompts(&root).len(), 2, "one model call per check: the change inside the debounce started none");
    assert_eq!(state(&root, "g1", "live.n").lines().count(), 2, "a read is no change");
}

/// Off Claude Code the model call is pinned to the fake `claude`: it would otherwise go through the
/// harness's own CLI.
const PINNED: (&str, &str) = ("BORG_PREDICT_MODEL", "claude:sonnet");

#[test]
fn omp_and_opencode_run_live_without_a_transcript() {
    for harness in ["omp", "opencode"] {
        let root = setup(&format!("live-{harness}"));
        let cwd = root.join("harbor_orion");
        let env = [ENV[0], ENV[1], ENV[2], PINNED];
        let hook = |event: &str, extra: Value| {
            let mut ev = json!({"session_id": "o1", "cwd": cwd});
            ev.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            borg_as(&root, harness, event, &ev, &env)
        };
        hook("prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}));
        answer(&root, &[("Mdrain", 3)]);
        let out = hook("post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "old_string": "a", "new_string": "reset arc"}}));
        assert!(context(&out).is_empty(), "{harness}: {out}");
        assert_eq!(queued(&wait_for(&root, "o1", "queued", |q| q.contains("live "))), "live Mdrain\n", "{harness}");
        wait_for(&root, "o1", "live.mark", |m| !m.is_empty());
        let p = &prompts(&root)[0];
        assert!(p.contains("[Edit] /x/net.rs") && p.contains("reset arc"), "{harness}: the turn's tool calls are the work so far: {p}");

        let next = context(&hook("post_tool", json!({"tool_name": "Read", "tool_input": {"file_path": "/x/budget.rs"}})));
        assert!(next.contains("borg check of your work so far") && next.contains("[Mdrain]"), "{harness}: {next}");
        assert!(state(&root, "o1", "edits").contains("[Edit] /x/net.rs"), "{harness}: the judge's evidence: {}", state(&root, "o1", "edits"));

        let out = hook("stop", json!({"last_assistant_message": "Done: the budget place is drained.", "stop_hook_active": false}));
        assert!(!out.contains("block"), "{harness}: {out}");
        assert_eq!(prompts(&root).len(), 1, "{harness}: covered, no second model call");
        assert!(state(&root, "o1", "ctx").ends_with("#borg-skip live_covered\n"), "{harness}: {}", state(&root, "o1", "ctx"));
        // The next prompt starts a new turn: its work so far starts empty.
        hook("prompt", json!({"prompt": "now the retries"}));
        assert_eq!(state(&root, "o1", "turn"), "", "{harness}");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn graded_2s_wait_for_the_next_prompt_on_omp() {
    let root = setup("live-graded-omp");
    let cwd = root.join("harbor_orion");
    let env = [("BORG_MODE", "hook"), ENV[1], ENV[2], PINNED];
    let hook = |event: &str, extra: Value| {
        let mut ev = json!({"session_id": "q1", "cwd": cwd});
        ev.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        borg_as(&root, "omp", event, &ev, &env)
    };
    hook("prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}));
    hook("post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}));
    answer(&root, &[("Mdrain", 2)]);
    let out = hook("stop", json!({"last_assistant_message": "I dropped the budget token with a reset arc.", "stop_hook_active": false}));
    assert!(!out.contains("block"), "a 2 does not block: {out}");
    assert!(state(&root, "q1", "cost").contains("queued"), "{}", state(&root, "q1", "cost"));
    let next = context(&hook("prompt", json!({"prompt": "now the retries"})));
    assert!(next.contains("drain the budget place"), "the 2 arrives with the next prompt: {next}");
    assert!(state(&root, "q1", "ctx").contains("#borg-queued Mdrain"), "{}", state(&root, "q1", "ctx"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn stop_wait_and_model_limit_fit_the_harness_budget() {
    let root = setup("live-budget");
    let cwd = root.join("harbor_orion");
    let env = [ENV[0], ENV[1], ENV[2], PINNED, ("BORG_STOP_BUDGET_MS", "28000")];
    borg_as(&root, "omp", "prompt", &json!({"session_id": "b1", "cwd": cwd, "prompt": "Tidy the budget handling"}), &env);
    // A check that never finishes: its process (this test) lives and its lock is fresh.
    let lock = root.join("store/collectives/corp/state/b1.live.lock");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    std::fs::write(&lock, format!("{} {now}\n", std::process::id())).unwrap();
    answer(&root, &[("Mdrain", 3)]);
    let started = Instant::now();
    let out = borg_as(
        &root,
        "omp",
        "stop",
        &json!({"session_id": "b1", "cwd": cwd, "last_assistant_message": "I drop the budget token with a reset arc.", "stop_hook_active": false}),
        &env,
    );
    let took = started.elapsed();
    assert!(out.contains("block") && out.contains("[Mdrain]"), "the stop still checks: {out}");
    assert!(took >= Duration::from_millis(6500) && took < Duration::from_secs(12), "a quarter of omp's 28 s, not 15 s: {took:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_live_ask_at_the_next_prompt_keeps_the_last_turns_work_as_evidence_on_omp() {
    let root = setup("live-prompt-evidence");
    let cwd = root.join("harbor_orion");
    let env = [("BORG_MODE", "hook"), ENV[1], ENV[2], PINNED];
    let hook = |event: &str, extra: Value| {
        let mut ev = json!({"session_id": "e1", "cwd": cwd});
        ev.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        borg_as(&root, "omp", event, &ev, &env)
    };
    hook("prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}));
    hook("post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}));
    // A live check that finished after the stop: its ask waits for the next prompt.
    std::fs::write(root.join("store/collectives/corp/state/e1.queued"), "live Mdrain\n").unwrap();
    let next = context(&hook("prompt", json!({"prompt": "now the retries"})));
    assert!(next.contains("[Mdrain]"), "{next}");
    let edits = state(&root, "e1", "edits");
    let lines: Vec<&str> = edits.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("#borg-asked Mdrain")).expect(&edits);
    assert!(at > 0 && lines[at - 1].contains("[Edit] /x/net.rs"), "the judge's evidence is the last turn's work: {edits}");
    assert_eq!(state(&root, "e1", "turn"), "", "the new turn starts empty");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_check_with_no_lesson_to_ask_and_none_to_judge_makes_no_model_call() {
    let root = setup("live-empty");
    let cwd = root.join("harbor_orion");
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), "").unwrap();
    let lines = [
        json!({"type": "user", "message": {"role": "user", "content": "Tidy the budget handling"}}),
        json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "/x/net.rs"}}]}}),
    ];
    std::fs::write(root.join("transcript.jsonl"), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    borg(&root, "prompt", &json!({"session_id": "n1", "cwd": cwd, "prompt": "Tidy the budget handling"}), &ENV);
    // A change starts a live check; with no lesson in its pool it asks no model.
    tool(&root, "n1", "Edit", json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "reset arc"}));
    let ctx = wait_for(&root, "n1", "ctx", |c| c.contains("#borg-live-skip"));
    assert!(ctx.contains("#borg-live-skip empty") && !ctx.contains("#borg-predict-cost"), "{ctx}");
    // Nor does the stop.
    let out = borg(&root, "stop", &json!({"session_id": "n1", "cwd": cwd, "last_assistant_message": "Done.", "stop_hook_active": false}), &ENV);
    assert!(!out.contains("block"), "{out}");
    assert!(prompts(&root).is_empty(), "no model call: {:?}", prompts(&root));
    let _ = std::fs::remove_dir_all(root);
}
