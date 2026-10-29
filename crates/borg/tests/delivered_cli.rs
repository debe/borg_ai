//! Free deliveries learn as questions do, end to end through the binary: a lesson delivered early or
//! handed over free (`#borg-queued`) gets an `asked` record with its channel and propensity, the next
//! Stop's one model call judges it into `asked_then_changed` / `asked_ignored` for that channel, and a
//! correction `borg learn` merges into it later in the session finds it asked (`asked_then_corrected`),
//! the rest settled at the session's end. Questions keep their judge slots before free deliveries. A
//! fake `claude` on PATH (tests/common): no real model call.

mod common;

use std::path::Path;

use common::{borg, prompts, setup, state};
use serde_json::{Value, json};

const ENV: [(&str, &str); 2] = [("BORG_PREFILTER", "off"), ("BORG_LEARN", "on")];

fn outcomes(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl")).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The records of `kind` for `lesson`, as (channel, p).
fn records(root: &Path, kind: &str, lesson: &str) -> Vec<(String, Value)> {
    outcomes(root)
        .into_iter()
        .filter(|o| o["kind"] == kind && o["lessons"].as_array().unwrap().iter().any(|l| l == lesson))
        .map(|o| (o["channel"].as_str().unwrap_or_default().to_string(), o["p"].clone()))
        .collect()
}

/// The fake model's next answer: `applies` as (id, grade), `resolved` its verdicts on pending lessons.
fn answer(root: &Path, applies: &[(&str, u8)], resolved: Value) {
    let a: Vec<Value> = applies.iter().map(|(id, g)| json!({"id": id, "why": "w", "grade": g})).collect();
    let out = json!({"structured_output": {"moves": [], "applies": a, "resolved": resolved}, "total_cost_usd": 0.05});
    std::fs::write(root.join("answer.json"), out.to_string()).unwrap();
}

fn verdict(id: &str, changed: bool) -> Value {
    json!([{"id": id, "verdict": if changed { "changed" } else { "unchanged" }, "avoids_consequence": changed}])
}

fn hook(root: &Path, session: &str, event: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut input = json!({"session_id": session, "cwd": root.join("harbor_orion")});
    input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    borg(root, event, &input, &[&ENV[..], env].concat())
}

fn stop(root: &Path, session: &str, last: &str, active: bool, env: &[(&str, &str)]) -> String {
    hook(root, session, "stop", json!({"last_assistant_message": last, "stop_hook_active": active}), env)
}

#[test]
fn an_early_lesson_is_asked_judged_at_the_next_stop_and_found_by_a_merged_correction() {
    let root = setup("early-judged");
    let out = hook(&root, "e1", "prompt", json!({"prompt": "Fix the Orion net: the budget place and its reset arc"}), &[]);
    assert!(out.contains("explicit consumer"), "delivered early: {out}");
    assert_eq!(records(&root, "asked", "Mdrain"), [("early".to_string(), json!([1.0]))], "{:?}", outcomes(&root));

    hook(&root, "e1", "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "old_string": "a", "new_string": "b"}}), &[]);
    answer(&root, &[], verdict("Mdrain", true));
    assert!(!stop(&root, "e1", "Done: the budget place has its own consumer.", false, &[]).contains("block"));
    let p = prompts(&root);
    assert_eq!(p.len(), 1, "the judge rides on the Stop's one model call");
    assert!(p[0].contains("## Lessons from earlier in this session") && p[0].contains("[Mdrain] lesson:"), "{}", p[0]);
    // Route-blind (judge v2): a free delivery's evidence has a question's shape, and nothing says how it came.
    assert!(p[0].contains("\nafter: work since: ") && p[0].contains("\nthis turn: ") && !p[0].contains("answer:"), "{}", p[0]);
    assert!(!p[0].contains("(declared:"), "a free delivery declares nothing: {}", p[0]);
    assert_eq!(records(&root, "asked_then_changed", "Mdrain"), [("early".to_string(), Value::Null)]);
    assert!(state(&root, "e1", "ctx").contains("#borg-judged Mdrain=changed"));
    // Judged once: the next Stop does not send it again.
    answer(&root, &[], json!([]));
    stop(&root, "e1", "Anything else?", false, &[]);
    assert!(!prompts(&root)[1].contains("[Mdrain] lesson:"));

    // `borg learn` merges a later correction of the session into Mdrain: the early delivery was asked.
    let log = borg::outcome::read(&root.join("store/collectives/corp/outcomes.jsonl"));
    assert_eq!(borg::outcome::contradicted(&log, "e1", i64::MAX, Some("Mdrain"), &[]), ["Mdrain"]);
    assert!(borg::outcome::contradicted(&log, "never", i64::MAX, Some("Mdrain"), &[]).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_queued_lesson_is_asked_with_p_1_and_judged_for_its_channel() {
    let root = setup("queued-judged");
    let off = [("BORG_EARLY", "off")];
    hook(&root, "q1", "prompt", json!({"prompt": "Tidy the budget handling of the net"}), &off);
    // A 2: no block; the lesson waits for the next prompt. Nothing was delivered yet: nothing to judge.
    answer(&root, &[("Mdrain", 2)], json!([]));
    assert!(!stop(&root, "q1", "Done: the budget place is drained.", false, &off).contains("block"));
    assert!(!prompts(&root)[0].contains("## Questions asked earlier"), "{}", prompts(&root)[0]);
    assert!(!state(&root, "q1", "cost").contains("judge "), "{}", state(&root, "q1", "cost"));
    assert!(records(&root, "asked", "Mdrain").is_empty());

    let out = hook(&root, "q1", "prompt", json!({"prompt": "next: the invoices"}), &off);
    assert!(out.contains("explicit consumer"), "{out}");
    assert_eq!(records(&root, "asked", "Mdrain"), [("queued".to_string(), json!([1.0]))], "{:?}", outcomes(&root));
    let asked = outcomes(&root).into_iter().find(|o| o["kind"] == "asked").unwrap();
    assert_eq!(asked["grades"], json!([2]), "the graded check's grade comes along: {asked}");

    answer(&root, &[], verdict("Mdrain", false));
    stop(&root, "q1", "Invoices done.", false, &off);
    assert_eq!(prompts(&root).len(), 2, "one model call per Stop");
    assert!(prompts(&root)[1].contains("[Mdrain] lesson:"));
    assert_eq!(records(&root, "asked_ignored", "Mdrain"), [("queued".to_string(), Value::Null)]);
    // The result names the check that handed it over and the judge's version.
    let check = outcomes(&root).into_iter().find(|o| o["kind"] == "graded").unwrap()["check"].clone();
    let result = outcomes(&root).into_iter().find(|o| o["kind"] == "asked_ignored").unwrap();
    assert_eq!((result["check"].clone(), result["judge"].clone()), (check, json!(2)), "{result}");
    assert!(state(&root, "q1", "cost").contains("judge call=2 pending=1 resolved=1"), "{}", state(&root, "q1", "cost"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_lesson_delivered_early_then_asked_at_a_stop_is_judged_for_each() {
    let root = setup("early-then-asked");
    hook(&root, "a1", "prompt", json!({"prompt": "Fix the Orion net: the budget place and its reset arc"}), &[]);
    // The Stop's call judges the early delivery unchanged and grades the lesson 3: the block asks it.
    answer(&root, &[("Mdrain", 3)], verdict("Mdrain", false));
    let out = stop(&root, "a1", "Done: the reset arc stays.", false, &[]);
    assert!(out.contains("block") && out.contains("[Mdrain]"), "{out}");
    assert_eq!(records(&root, "asked_ignored", "Mdrain"), [("early".to_string(), Value::Null)]);
    let channels: Vec<String> = records(&root, "asked", "Mdrain").into_iter().map(|(c, _)| c).collect();
    assert_eq!(channels, ["early", "stop"]);
    stop(&root, "a1", "Mdrain: applies. Replaced the reset arc with a consumer.", true, &[]);
    // The question is judged on its own evidence at the next Stop.
    answer(&root, &[], verdict("Mdrain", true));
    stop(&root, "a1", "Done: the budget place has its own consumer.", false, &[]);
    let p = prompts(&root);
    assert_eq!(p.len(), 2);
    assert_eq!(records(&root, "asked_then_changed", "Mdrain"), [("stop".to_string(), Value::Null)]);
    // A random half of the questions are judged without their declaration (seeded by the check): the
    // result says which, and the evidence agrees.
    let result = outcomes(&root).into_iter().find(|o| o["kind"] == "asked_then_changed" && o["channel"] == "stop").unwrap();
    match result["stripped"].as_bool().unwrap_or(false) {
        true => assert!(p[1].contains("[Mdrain] lesson:") && !p[1].contains("(declared:") && !p[1].to_lowercase().contains("mdrain: applies"), "{}", p[1]),
        false => assert!(p[1].contains("[Mdrain] lesson:") && p[1].contains("(declared: applies)"), "{}", p[1]),
    }
    let _ = std::fs::remove_dir_all(root);
}

/// The lessons of the `asked` records of `channel`, in log order.
fn asked_by(root: &Path, channel: &str) -> Vec<String> {
    let lessons = |o: Value| o["lessons"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect::<Vec<_>>();
    outcomes(root).into_iter().filter(|o| o["kind"] == "asked" && o["channel"] == channel).flat_map(lessons).collect()
}

/// Adds `n` more Orion lessons, `O0`…, each on its own word of the net.
fn more_orion(root: &Path, n: usize) {
    let words =
        ["guard", "arc", "token", "colour", "inhibitor", "timer", "fork", "join", "siphon", "trap", "marking", "subnet", "firing", "deadline", "priority"];
    let mut text = std::fs::read_to_string(root.join("store/collectives/corp/lessons.jsonl")).unwrap();
    for (i, w) in words.iter().take(n).enumerate() {
        let l = json!({"id": format!("O{i}"), "origin_ms": 100 + i, "scope": ["Orion"], "question": format!("Does the Orion net keep its {w} rule for the budget place?"), "consequence": format!("the {w} breaks")});
        text.push_str(&format!("{l}\n"));
    }
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), text).unwrap();
}

#[test]
fn a_free_delivery_after_a_block_never_pushes_its_questions_out_of_the_judge() {
    let root = setup("questions-first");
    more_orion(&root, 15);
    let off = [("BORG_EARLY", "off")];
    hook(&root, "f1", "prompt", json!({"prompt": "Fix the Orion net: the budget place, its guard, arc and token"}), &off);
    let grades: Vec<(String, u8)> = (0..8).map(|i| (format!("O{i}"), 3)).collect();
    let grades: Vec<(&str, u8)> = grades.iter().map(|(id, g)| (id.as_str(), *g)).collect();
    answer(&root, &grades, json!([]));
    assert!(stop(&root, "f1", "Done: the Orion net budget place is fixed.", false, &off).contains("block"));
    let questions = asked_by(&root, "stop");
    assert!(questions.len() >= 8, "{questions:?}");
    stop(&root, "f1", "All of them apply; the rules are kept.", true, &off);
    // The next prompt delivers more of the net's lessons early: more lessons wait than the judge has slots.
    let out = hook(&root, "f1", "prompt", json!({"prompt": "Now the Orion net subnet, firing, deadline, priority, marking and siphon"}), &[]);
    let early = asked_by(&root, "early");
    assert!(questions.len() + early.len() > 10, "delivered early: {early:?} ({out})");
    answer(&root, &[], json!([]));
    stop(&root, "f1", "Done with the subnet.", false, &[]);
    let p = prompts(&root);
    let judged = p.last().unwrap();
    for q in &questions {
        assert!(judged.contains(&format!("[{q}] lesson:")), "{q} left out of the judge: {judged}");
    }
    assert!(state(&root, "f1", "cost").contains("pending=10"), "{}", state(&root, "f1", "cost"));
    let _ = std::fs::remove_dir_all(root);
}

/// `borg learn` on the collective's store, with the fake model on PATH; `session` ended, if given.
fn learn(root: &Path, session: Option<&str>) {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut args = vec!["learn"];
    args.extend(session.map(|s| ["--session", s]).into_iter().flatten());
    std::process::Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store/collectives/corp"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_VECTORS", "off")
        .env("BORG_LEARN", "on")
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .output()
        .unwrap();
}

/// The setup's lessons with the fields `borg learn` stores with every lesson (learn.rs `Lesson`).
fn as_stored(root: &Path) {
    let path = root.join("store/collectives/corp/lessons.jsonl");
    let mut text = String::new();
    for line in std::fs::read_to_string(&path).unwrap().lines() {
        let mut l: Value = serde_json::from_str(line).unwrap();
        let fields = json!({"repo": "", "situations": [], "support": 1, "episodes": [], "history": [], "version": 1});
        for (k, v) in fields.as_object().unwrap() {
            l.as_object_mut().unwrap().entry(k.clone()).or_insert_with(|| v.clone());
        }
        text.push_str(&format!("{l}\n"));
    }
    std::fs::write(path, text).unwrap();
}

/// A correction in `session`, now, that the fake model merges into `into`.
fn correction(root: &Path, id: &str, session: &str, text: &str, into: &str) {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let e = json!({"id": id, "ms": ms, "repo": "harbor_orion", "session": session, "task": "t", "agent": "claude", "rejected_tool": null, "correction": text});
    let path = root.join("store/collectives/corp/episodes.jsonl");
    let mut log = std::fs::read_to_string(&path).unwrap_or_default();
    log.push_str(&format!("{e}\n"));
    std::fs::write(path, log).unwrap();
    let a = json!({"structured_output": {"is_lesson": true, "question": text, "situations": [], "consequence": "c", "same_as": into, "supersedes": []}, "total_cost_usd": 0.01});
    std::fs::write(root.join("answer.json"), a.to_string()).unwrap();
}

#[test]
fn borg_learn_finds_free_deliveries_corrected_and_settles_the_rest_at_session_end() {
    let root = setup("free-learned");
    // Mdrain early at the prompt; a Stop that does not block grades New and G0 2, handed over free next.
    assert!(hook(&root, "c1", "prompt", json!({"prompt": "Fix the Orion net: the budget place and its reset arc"}), &[]).contains("explicit consumer"));
    answer(&root, &[("New", 2), ("G0", 2)], json!([]));
    assert!(!stop(&root, "c1", "Done: retries and the websocket handling too.", false, &[]).contains("block"));
    hook(&root, "c1", "prompt", json!({"prompt": "next: the invoices"}), &[]);
    assert_eq!(asked_by(&root, "early"), ["Mdrain"]);
    let mut queued = asked_by(&root, "queued");
    queued.sort();
    assert_eq!(queued, ["G0", "New"], "{:?}", outcomes(&root));

    // The engineer still corrects the drone on Mdrain's and New's concerns: neither delivery prevented it.
    as_stored(&root);
    correction(&root, "x1", "c1", "No: the Orion net must drain the budget place with an explicit consumer, not a reset arc", "Mdrain");
    learn(&root, None);
    correction(&root, "x2", "c1", "No: retries need jittered backoff, never a fixed delay", "New");
    learn(&root, Some("c1"));
    let kinds = |lesson: &str| -> Vec<String> {
        let mut k: Vec<String> = outcomes(&root)
            .into_iter()
            .filter(|o| !["asked", "graded", "delivery"].contains(&o["kind"].as_str().unwrap()) && o["lessons"].as_array().unwrap().iter().any(|l| l == lesson))
            .map(|o| o["kind"].as_str().unwrap().to_string())
            .collect();
        k.sort();
        k
    };
    assert_eq!(kinds("Mdrain"), ["asked_then_corrected"], "{:?}", outcomes(&root));
    // New was graded 2 and not chosen: handed over free and still corrected, it stays a miss for the cut.
    assert_eq!(kinds("New"), ["applied_then_corrected", "asked_then_corrected"], "{:?}", outcomes(&root));
    assert_eq!(kinds("G0"), ["asked_not_corrected"], "the session ended, never corrected: {:?}", outcomes(&root));
    let _ = std::fs::remove_dir_all(root);
}
