//! What the records say was delivered is what reached the drone, end to end through the binary: one
//! check decides each lesson (a lesson still waiting at the stop to be asked or handed over stays out of
//! the stop's pool)
//! and every `asked` record names the check that routed its lessons; the queue hands a lesson over once
//! and decides a placebo by what was shown; a live check's line from before 0.7.6 is handed over free;
//! a subagent's tool call takes nothing from the queue; a delivery's work line is no work after an
//! earlier one; a stripped question's declaration reaches no part of the judge's prompt. Queues are
//! written directly. The fake `claude` of tests/common: no real model call.

mod common;

use std::path::{Path, PathBuf};

use common::{answer, borg, context, prompts, setup, state};
use serde_json::{Value, json};

const HOOK: [(&str, &str); 3] = [("BORG_PREFILTER", "off"), ("BORG_EARLY", "off"), ("BORG_LEARN", "on")];

/// A check id as hook.rs tags a marker with it (select.rs `CHECK_HEX` hex digits).
fn check(n: u64) -> String {
    format!("{n:012x}")
}

fn state_dir(root: &Path) -> PathBuf {
    root.join("store/collectives/corp/state")
}

/// The session's `.queued` file, as checks that finished leave it (hook.rs `queued_file`).
fn queue(root: &Path, session: &str, text: &str) {
    std::fs::create_dir_all(state_dir(root)).unwrap();
    std::fs::write(state_dir(root).join(format!("{session}.queued")), text).unwrap();
}

fn outcomes(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl")).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The records of `kind` holding `lesson`.
fn records(root: &Path, kind: &str, lesson: &str) -> Vec<Value> {
    outcomes(root).into_iter().filter(|o| o["kind"] == kind && o["lessons"].as_array().is_some_and(|l| l.iter().any(|x| x == lesson))).collect()
}

/// `lesson`'s `delivery` records, as (check, route, reason).
fn routes(root: &Path, lesson: &str) -> Vec<(String, String, String)> {
    let s = |o: &Value, k: &str| o[k].as_str().unwrap_or_default().to_string();
    records(root, "delivery", lesson).iter().map(|o| (s(o, "check"), s(o, "route"), s(o, "reason"))).collect()
}

/// `lesson`'s `asked` records, as (channel, check).
fn asked(root: &Path, lesson: &str) -> Vec<(String, String)> {
    let s = |o: &Value, k: &str| o[k].as_str().unwrap_or_default().to_string();
    records(root, "asked", lesson).iter().map(|o| (s(o, "channel"), s(o, "check"))).collect()
}

fn hook(root: &Path, session: &str, event: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut input = json!({"session_id": session, "cwd": root.join("harbor_orion")});
    input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    borg(root, event, &input, env)
}

fn stop(root: &Path, session: &str, last: &str, env: &[(&str, &str)]) -> String {
    hook(root, session, "stop", json!({"last_assistant_message": last, "stop_hook_active": false}), env)
}

fn read(root: &Path, session: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut ev = json!({"tool_name": "Read", "tool_input": {"file_path": "/x/budget.rs"}, "transcript_path": root.join("transcript.jsonl")});
    ev.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    hook(root, session, "post_tool", ev, env)
}

/// A live check (before 0.7.6) graded Mdrain 3 and its ask still waits at the stop of a session that
/// runs across the upgrade: decided already, so it stays out of the stop's pool, no stop blocks for it,
/// and the next prompt hands it over, recorded as the ask the live check decided (channel live), so the
/// selection fold joins it to that check's apply.
#[test]
fn a_live_ask_from_before_the_upgrade_waiting_at_the_stop_is_handed_over_by_its_own_check() {
    let root = setup("routes-wait");
    hook(&root, "w1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &HOOK);
    let live = check(0xc1);
    queue(&root, "w1", &format!("live Mdrain @{live}\n"));
    // The stop's model would grade Mdrain 3 again, were it shown.
    answer(&root, &[("Mdrain", 3)]);
    let out = stop(&root, "w1", "Done: the budget place is drained.", &HOOK);
    assert!(!out.contains("block"), "{out}");
    let p = prompts(&root);
    assert_eq!(p.len(), 1, "the stop asked its own model");
    assert!(!p[0].contains("[Mdrain] Does the Orion"), "decided already, so not in the stop's pool: {}", p[0]);
    assert!(records(&root, "graded", "Mdrain").is_empty(), "the stop's check did not grade it: {:?}", outcomes(&root));
    let out = context(&hook(&root, "w1", "prompt", json!({"prompt": "Next: the Orion net timers"}), &HOOK));
    assert!(out.contains("explicit consumer"), "{out}");
    assert_eq!(asked(&root, "Mdrain"), [("live".to_string(), live.clone())], "{:?}", outcomes(&root));
    assert_eq!(routes(&root, "Mdrain"), [(live, "asked".to_string(), String::new())], "{:?}", outcomes(&root));
    let _ = std::fs::remove_dir_all(root);
}

/// A check's free line still waits at a stop that asks its own model: an earlier stop's at a harness
/// without prompt context. The lesson is decided already, so it stays out of the stop's pool,
/// the stop never hands it over a second time, and the next prompt delivers it once, under the check
/// that queued it, with no copy dropped as queued twice (which counted a miss against the stop's free
/// route although the lesson arrived).
#[test]
fn a_free_lesson_waiting_at_the_stop_is_decided_by_its_own_check_alone() {
    let env = &HOOK;
    let root = setup("routes-free-wait");
    hook(&root, "f1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), env);
    let free = check(0xd1);
    queue(&root, "f1", &format!("Mdrain @{free}\n"));
    // The stop asks its model, which would grade Mdrain 2 again, were it shown.
    answer(&root, &[("Mdrain", 2)]);
    let out = stop(&root, "f1", "Done: the budget place is drained.", env);
    assert!(!out.contains("block"), "{out}");
    let p = prompts(&root);
    assert_eq!(p.len(), 1, "the stop asked its own model");
    assert!(!p[0].contains("[Mdrain] Does the Orion"), "decided already, so not in the stop's pool: {}", p[0]);
    assert!(records(&root, "graded", "Mdrain").is_empty(), "the stop's check did not grade it: {:?}", outcomes(&root));
    assert_eq!(state(&root, "f1", "queued"), format!("Mdrain @{free}\n"), "no second copy queued");
    let out = context(&hook(&root, "f1", "prompt", json!({"prompt": "Next: the Orion net timers"}), env));
    assert!(out.contains("explicit consumer"), "{out}");
    assert_eq!(routes(&root, "Mdrain"), [(free, "free".to_string(), String::new())], "{:?}", outcomes(&root));
    let _ = std::fs::remove_dir_all(root);
}

/// A subagent's tool call (`agent_id`, the session's id) gets no context: Claude gives it to the
/// subagent, so nothing is taken from the queue or recorded delivered there; the session's own next
/// tool call delivers it.
#[test]
fn a_subagents_tool_call_takes_nothing_from_the_queue() {
    let root = setup("routes-subagent");
    hook(&root, "s1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &HOOK);
    let line = format!("Mdrain @{}\nG0 @{}\n", check(0xc3), check(0xc4));
    queue(&root, "s1", &line);
    let out = read(&root, "s1", json!({"agent_id": "a1", "agent_type": "Explore"}), &HOOK);
    assert!(context(&out).is_empty(), "{out}");
    assert_eq!(state(&root, "s1", "queued"), line, "the queue waits for the session itself");
    assert!(!state(&root, "s1", "ctx").contains("#borg-queued"));
    assert!(outcomes(&root).iter().all(|o| o["kind"] != "asked" && o["kind"] != "delivery"), "{:?}", outcomes(&root));
    let out = context(&read(&root, "s1", json!({}), &HOOK));
    assert!(out.contains("explicit consumer") && out.contains("websocket"), "{out}");
    assert_eq!(routes(&root, "Mdrain"), [(check(0xc3), "free".to_string(), String::new())]);
    let _ = std::fs::remove_dir_all(root);
}

/// A stop's free line came before a live check's ask of the same lesson (a 0.7.5 queue): the live
/// check's ask is handed over first, recorded as its ask, and the free line is the one dropped.
#[test]
fn a_lesson_queued_twice_is_handed_over_once_under_the_line_that_owes_it() {
    let root = setup("routes-order");
    hook(&root, "o1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &HOOK);
    let (free, live) = (check(0xc5), check(0xc6));
    queue(&root, "o1", &format!("Mdrain @{free}\nlive Mdrain @{live}\n"));
    let out = context(&read(&root, "o1", json!({}), &HOOK));
    assert!(out.starts_with("[borg] Peer lessons") && out.matches("explicit consumer").count() == 1, "handed over once: {out}");
    let ctx = state(&root, "o1", "ctx");
    assert!(ctx.contains(&format!("#borg-asked Mdrain @{live}\n#borg-live Mdrain ms=")) && !ctx.contains("#borg-queued Mdrain"), "{ctx}");
    assert_eq!(
        routes(&root, "Mdrain"),
        [(live, "asked".to_string(), String::new()), (free, "dropped_done".to_string(), "queued twice".to_string())],
        "{:?}",
        outcomes(&root)
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Lessons whose lines fill one free delivery's budget (`conscience::EARLY_CHARS`) by the third: the
/// fourth is cut. Another check's placebo of that fourth lesson stays a placebo: it was not shown.
#[test]
fn a_placebo_of_a_lesson_the_budget_cut_stays_a_placebo() {
    let root = setup("routes-placebo");
    let long = |id: &str| json!({"id": id, "origin_ms": 1, "question": format!("Did I check {id}: {}?", "the long handling ".repeat(17)), "consequence": format!("{id} {}", "breaks badly ".repeat(16))});
    let lessons: String = ["L1", "L2", "L3", "L4"].iter().map(|id| format!("{}\n", long(id))).collect();
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), lessons).unwrap();
    hook(&root, "p1", "prompt", json!({"prompt": "Tidy the handling"}), &HOOK);
    let (free, placebo) = (check(0xc7), check(0xc8));
    queue(&root, "p1", &format!("L1,L2,L3,L4 @{free}\nplacebo L4 @{placebo}\n"));
    let out = context(&read(&root, "p1", json!({}), &HOOK));
    assert!(out.contains("Did I check L1") && !out.contains("Did I check L4"), "{out}");
    assert!(state(&root, "p1", "ctx").contains(&format!("#borg-placebo L4 @{placebo}")), "{}", state(&root, "p1", "ctx"));
    assert_eq!(
        routes(&root, "L4"),
        [(placebo, "placebo".to_string(), String::new()), (free, "dropped_chars".to_string(), "budget".to_string())],
        "{:?}",
        outcomes(&root)
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Free lessons handed over in place of questions the fixed rule asks (hook.rs `INSTEAD_NEXT`: the
/// question-count test's or the evidence rule's on a check with a 3) all reach the drone, though four of their lines are past
/// what one free delivery's budget (`conscience::EARLY_CHARS`) holds; another check's free lessons
/// waiting beside them keep the budget they had alone, the three it holds, and the fourth is cut and
/// says so.
#[test]
fn hand_overs_in_place_of_questions_are_never_cut_by_the_free_budget() {
    let root = setup("routes-instead");
    let long = |id: &str| json!({"id": id, "origin_ms": 1, "question": format!("Did I check {id}: {}?", "the long handling ".repeat(17)), "consequence": format!("{id} {}", "breaks badly ".repeat(16))});
    let ids = ["T1", "T2", "T3", "T4", "F1", "F2", "F3", "F4"];
    let lessons: String = ids.iter().map(|id| format!("{}\n", long(id))).collect();
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), lessons).unwrap();
    hook(&root, "i1", "prompt", json!({"prompt": "Tidy the handling"}), &HOOK);
    let (test, other) = (check(0xd1), check(0xd2));
    // The other check's line first: the queue's order is the checks', delivery's is the route's.
    queue(&root, "i1", &format!("F1,F2,F3,F4 @{other}\ninstead-next T1,T2,T3,T4 @{test}\n"));
    let out = context(&read(&root, "i1", json!({}), &HOOK));
    let shown: Vec<&str> = ids.iter().copied().filter(|id| out.contains(&format!("Did I check {id}:"))).collect();
    assert_eq!(shown, ["T1", "T2", "T3", "T4", "F1", "F2", "F3"], "{out}");
    let ctx = state(&root, "i1", "ctx");
    assert!(ctx.contains(&format!("#borg-queued T1,T2,T3,T4 @{test}")) && ctx.contains(&format!("#borg-queued F1,F2,F3 @{other}")), "{ctx}");
    for id in ["T1", "T4"] {
        assert_eq!(routes(&root, id), [(test.clone(), "free".to_string(), String::new())], "{id}: {:?}", outcomes(&root));
        assert_eq!(asked(&root, id), [("queued".to_string(), test.clone())], "{id}");
    }
    assert_eq!(routes(&root, "F4"), [(other, "dropped_chars".to_string(), "budget".to_string())], "{:?}", outcomes(&root));
    let _ = std::fs::remove_dir_all(root);
}

fn transcript(root: &Path, texts: &[&str]) {
    let mut lines = vec![json!({"type": "user", "message": {"role": "user", "content": "Tidy the budget handling"}})];
    lines.extend(texts.iter().map(|t| json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": t}]}})));
    std::fs::write(root.join("transcript.jsonl"), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
}

/// A free delivery, then another free delivery of another lesson: the second's work line is the work
/// before it, never work after the first delivery (its judge's "work since").
#[test]
fn a_later_deliverys_work_line_is_no_work_after_an_earlier_delivery() {
    let root = setup("routes-work");
    hook(&root, "k1", "prompt", json!({"prompt": "Tidy the budget handling"}), &HOOK);
    transcript(&root, &["I read the budget code first."]);
    queue(&root, "k1", &format!("G0 @{}\n", check(0xc9)));
    assert!(context(&read(&root, "k1", json!({}), &HOOK)).contains("websocket"));
    transcript(&root, &["I read the budget code first.", "Then I renamed the helper."]);
    queue(&root, "k1", &format!("Mdrain @{}\n", check(0xca)));
    assert!(context(&read(&root, "k1", json!({}), &HOOK)).contains("explicit consumer"));
    let edits = state(&root, "k1", "edits");
    let lines: Vec<&str> = edits.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("#borg-queued Mdrain")).unwrap_or_else(|| panic!("{edits}"));
    assert!(lines[at - 1].starts_with("#borg-work ") && lines[at - 1].contains("renamed the helper"), "{edits}");
    // The stop's judge: G0's evidence after its delivery holds none of the later delivery's work line.
    answer(&root, &[]);
    stop(&root, "k1", "Done.", &HOOK);
    let p = prompts(&root);
    let block = p[0].split("### Work").find(|b| b.contains("[G0] lesson:")).unwrap_or_else(|| panic!("{}", p[0]));
    let after: Vec<&str> = block.lines().skip_while(|l| !l.starts_with("after: ")).collect();
    assert!(!after.is_empty() && after.iter().all(|l| !l.contains("renamed the helper")), "{block}");
    let _ = std::fs::remove_dir_all(root);
}

/// A live question (asked before 0.7.6, in a session that runs across the upgrade) answered at a stop
/// that asks its own model, and judged without its declaration (hook.rs `stripped`): the stop's own
/// message is the answer, and no part of the prompt shows the declaration, neither the evidence nor the
/// current work.
#[test]
fn a_stripped_live_question_answered_at_the_stop_shows_no_declaration() {
    let root = setup("routes-strip");
    hook(&root, "d1", "prompt", json!({"prompt": "Tidy the budget handling of the Orion net"}), &HOOK);
    // One check in two is stripped (a hash bit): the first of 64 that is.
    let live = (0..64).map(check).find(|c| borg::hook::stripped(c, "Mdrain")).expect("a stripped check among 64");
    // What a 0.7.5 live ask left: its markers in the state file, and in .edits after the work so far.
    let markers = format!("#borg-asked Mdrain @{live}\n#borg-live Mdrain ms=1\n");
    let ctx = state_dir(&root).join("d1.ctx");
    std::fs::write(&ctx, format!("{}{markers}", std::fs::read_to_string(&ctx).unwrap())).unwrap();
    std::fs::write(state_dir(&root).join("d1.edits"), format!("#borg-work I read the budget code.\n{markers}")).unwrap();
    answer(&root, &[]);
    stop(&root, "d1", "Fixed the arc with an explicit consumer. Mdrain: applies", &HOOK);
    let p = prompts(&root);
    assert_eq!(p.len(), 1, "the stop's own model call judges it");
    assert!(p[0].contains("[Mdrain] lesson:") && p[0].contains("explicit consumer"), "{}", p[0]);
    assert!(!p[0].to_lowercase().contains("mdrain: applies") && !p[0].contains("(declared:"), "{}", p[0]);
    let _ = std::fs::remove_dir_all(root);
}
