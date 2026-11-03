//! R1 end to end through the binary: the Stop block names each lesson's id; the drone's answer and its
//! work after the block are evidence; the consequence model's next call at a later Stop of the session
//! (a fake `claude` here) judges them into `asked_then_changed` / `asked_ignored`, and `borg debug memory`
//! derives activation and usefulness from them. The word rule and the drone's declaration alone never
//! label. A temp BORG_CONFIG and either BORG_MATCH=bm25 or a fake `claude` on PATH: no real model call,
//! no real store.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn setup(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-memory-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = root.join("store/collectives/corp");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(root.join("corp")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let lessons = [
        json!({"id": "Llock", "origin_ms": 1, "question": "Is the websocket send lock held across the await on the delivery queue?",
            "consequence": "a slow client stalls every sender"}),
        json!({"id": "Ldrain", "origin_ms": 1, "question": "Is the delivery queue drained by one writer thread instead of a lock?",
            "consequence": "frames interleave on the socket"}),
        json!({"id": "Lspec", "origin_ms": 1, "question": "Does the delivery queue follow the websocket spec close handshake?",
            "consequence": "clients reconnect in a loop"}),
        json!({"id": "Lcss", "origin_ms": 1, "question": "Do the stylesheet tokens follow the design system?"}),
        json!({"id": "Lgit", "origin_ms": 1, "question": "Is the release branch tagged before the merge?"}),
        json!({"id": "Ldocs", "origin_ms": 1, "question": "Is the changelog entry written for users?"}),
    ];
    std::fs::write(store.join("lessons.jsonl"), lessons.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("corp")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    // The fake consequence model: records each prompt and schema it gets, answers with answer.json.
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("calls")).unwrap();
    let calls = root.join("calls");
    let fake = root.join("bin/claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\np=$(cat)\nn=$(( $(ls {c} | wc -l) / 2 ))\nprintf '%s' \"$p\" > {c}/$n.prompt\nprintf '%s' \"$9\" > {c}/$n.schema\ncat {a}\n",
            c = calls.display(),
            a = root.join("answer.json").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

/// What the fake model answers next: these lessons apply, these verdicts on pending lessons.
fn answer(root: &Path, applies: &[&str], resolved: Value) {
    let applies: Vec<Value> = applies.iter().map(|i| json!({"id": i, "why": "w", "grade": 3})).collect();
    let v = json!({"structured_output": {"moves": [], "applies": applies, "resolved": resolved}, "total_cost_usd": 0.01});
    std::fs::write(root.join("answer.json"), v.to_string()).unwrap();
}

/// The fake model's calls so far: (prompt, schema).
fn calls(root: &Path) -> Vec<(String, String)> {
    let n = std::fs::read_dir(root.join("calls")).unwrap().count() / 2;
    (0..n)
        .map(|i| {
            let r = |ext: &str| std::fs::read_to_string(root.join(format!("calls/{i}.{ext}"))).unwrap_or_default();
            (r("prompt"), r("schema"))
        })
        .collect()
}

/// Runs borg; `model` selects the default match with the fake on PATH (prefilter off unless `env` sets
/// it), otherwise BORG_MATCH=bm25.
fn borg(root: &Path, args: &[&str], stdin: &str, model: bool, env: &[(&str, &str)]) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        // The stop's own check and its block (check_async.rs `on`), as these tests pin.
        .env("BORG_ASYNC", "off")
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        // The fake `claude` alone answers: a failed answer would otherwise fall through to the next model
        // CLI on PATH (runner.rs `pick`), a real call.
        .env("BORG_PREDICT_MODEL", "claude:")
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_VECTORS", "off")
        .env("BORG_EARLY", "off")
        // The fixed rule's delivery (select.rs `Policy::Fixed`), as tests/common pins it.
        .env("BORG_SELECT", "fixed")
        .env("BORG_STOP_ATTENTION", "off")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if model {
        c.env("BORG_PREFILTER", "off");
    } else {
        c.env("BORG_MATCH", "bm25");
    }
    c.envs(env.iter().copied());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into()
}

fn stop(root: &Path, session: &str, active: bool, msg: &str, model: bool, env: &[(&str, &str)]) -> String {
    let v = json!({"session_id": session, "cwd": root.join("corp"), "stop_hook_active": active, "last_assistant_message": msg});
    borg(root, &["hook", "claude", "stop"], &v.to_string(), model, env)
}

fn edit(root: &Path, session: &str, new: &str) {
    let v = json!({"session_id": session, "cwd": root.join("corp"), "tool_name": "Edit",
        "tool_input": {"file_path": "/w/relay/Delivery.java", "old_string": "synchronized", "new_string": new}});
    borg(root, &["hook", "claude", "post_tool"], &v.to_string(), false, &[]);
}

fn store_file(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join("store/collectives/corp").join(name)).unwrap_or_default()
}

fn outcomes(root: &Path) -> Vec<Value> {
    store_file(root, "outcomes.jsonl").lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// Whether the judge's result of kind `k` for `lesson` was judged without its declaration (hook.rs
/// `stripped`, a seeded coin per check and lesson).
fn stripped(root: &Path, k: &str, lesson: &str) -> bool {
    let o = outcomes(root).into_iter().find(|o| o["kind"] == k && o["lessons"].as_array().unwrap().iter().any(|l| l == lesson));
    o.unwrap_or_else(|| panic!("no {k} for {lesson}"))["stripped"].as_bool().unwrap_or(false)
}

fn kind(root: &Path, k: &str) -> Vec<String> {
    let mut v: Vec<String> = outcomes(root)
        .iter()
        .filter(|o| o["kind"] == k)
        .flat_map(|o| o["lessons"].as_array().unwrap().clone())
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

/// The asked ids, without each line's ` @<check>` tag.
fn asked(root: &Path, session: &str) -> Vec<String> {
    store_file(root, &format!("state/{session}.ctx"))
        .lines()
        .filter_map(|l| l.strip_prefix("#borg-asked "))
        .flat_map(|l| l.split(" @").next().unwrap_or_default().split(','))
        .filter(|i| !i.is_empty())
        .map(String::from)
        .collect()
}

const PLAN: &str = "Plan: hold the websocket send lock across the await on the delivery queue, a lock instead of anything else; close per the spec handshake.";

/// A first Stop the fake model blocks (it predicts Llock and Ldrain), then the drone's answer turn.
fn blocked_and_answered(root: &Path, session: &str, answer_msg: &str) -> Vec<String> {
    answer(root, &["Llock", "Ldrain"], json!([]));
    let out = stop(root, session, false, PLAN, true, &[]);
    let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("no block: {out:?}"));
    assert!(v["reason"].as_str().unwrap().contains("[Llock]"), "{v}");
    let ids = asked(root, session);
    assert!(ids.contains(&"Llock".to_string()) && ids.contains(&"Ldrain".to_string()), "{ids:?}");
    edit(root, session, "one writer thread drains the delivery queue; no lock held across the await");
    assert_eq!(stop(root, session, true, answer_msg, true, &[]).trim(), "");
    ids
}

#[test]
fn the_next_model_call_judges_earlier_questions_into_changed_and_ignored() {
    let root = setup("judge");
    let ids = blocked_and_answered(&root, "s1", "Revised: no lock across the await.\nLlock: applies\n- Ldrain: skip\n");
    let first = calls(&root);
    assert_eq!(first.len(), 1, "the answer turn makes no model call");
    assert!(!first[0].0.contains("from earlier in this session") && !first[0].1.contains("resolved"), "nothing pending yet");
    // The declaration alone labels nothing.
    assert!(kind(&root, "asked_then_changed").is_empty() && kind(&root, "asked_ignored").is_empty(), "{:?}", outcomes(&root));

    // A later turn end: the same model call also judges the questions asked at the block. Unknown ids
    // are dropped, a repeated id counts once, an unclear verdict leaves the lesson pending.
    let mut resolved = vec![
        json!({"id": "Llock", "verdict": "changed", "avoids_consequence": true}),
        json!({"id": "Llock", "verdict": "unchanged", "avoids_consequence": false}),
        json!({"id": "Ldrain", "verdict": "unchanged", "avoids_consequence": false}),
        json!({"id": "Lnope", "verdict": "changed", "avoids_consequence": true}),
    ];
    resolved
        .extend(ids.iter().filter(|i| !["Llock", "Ldrain"].contains(&i.as_str())).map(|i| json!({"id": i, "verdict": "unclear", "avoids_consequence": false})));
    answer(&root, &[], json!(resolved));
    assert_eq!(stop(&root, "s1", false, "Done: the writer thread owns the socket.", true, &[]).trim(), "");
    let all = calls(&root);
    assert_eq!(all.len(), 2, "no extra call: the judge rides on the consequence model's call");
    let (prompt, schema) = &all[1];
    assert!(prompt.contains("## Lessons from earlier in this session") && schema.contains("\"resolved\""), "{schema}");
    assert!(prompt.contains("[Llock] lesson: Is the websocket send lock"), "{prompt}");
    assert!(prompt.contains("before: Plan: hold the websocket send lock"), "{prompt}");
    assert!(prompt.contains("one writer thread drains the delivery queue") && prompt.contains("this turn: Done: the writer thread"), "{prompt}");
    assert_eq!(prompt.matches("### Work").count(), 1, "one block, one shared evidence");
    assert_eq!(kind(&root, "asked_then_changed"), ["Llock"]);
    assert_eq!(kind(&root, "asked_ignored"), ["Ldrain"]);
    // Each declaration reaches the judge unless its question's coin stripped it (the result says which):
    // then neither the annotation nor the answer's line shows it.
    for (k, id, end, word) in [
        ("asked_then_changed", "Llock", "a slow client stalls every sender", "applies"),
        ("asked_ignored", "Ldrain", "frames interleave on the socket", "skip"),
    ] {
        let shown = format!("{end} (declared: {word})");
        match stripped(&root, k, id) {
            true => assert!(!prompt.contains(&shown) && !prompt.contains(&format!("{id}: {word}")), "{id} stripped: {prompt}"),
            false => assert!(prompt.contains(&shown), "{id}: {prompt}"),
        }
    }
    let state = store_file(&root, "state/s1.ctx");
    assert!(state.contains("#borg-judged Llock=changed") && state.contains("#borg-judged Ldrain=ignored"), "{state}");
    if ids.contains(&"Lspec".to_string()) {
        assert!(state.contains("#borg-judged Lspec=pending"), "{state}");
    }
    // The cost log: which call judged how many, and the characters it added.
    let cost = store_file(&root, "state/s1.cost");
    let line = cost.lines().find(|l| l.starts_with("judge ")).unwrap_or_else(|| panic!("{cost}"));
    assert!(line.starts_with(&format!("judge call=2 pending={} resolved=2 chars=", ids.len())), "{line}");
    let field = |k: &str| line.split_whitespace().find_map(|w| w.strip_prefix(k)).unwrap_or_else(|| panic!("{line}"));
    let chars: usize = field("chars=").parse().unwrap();
    assert!(chars > 300 && prompt.len() > chars, "{chars}");
    // What the answer named, each id once, and the one no pending lesson has: zero tokens, no prompt text.
    let mut want = vec!["Llock", "Ldrain", "Lnope"];
    want.extend(ids.iter().filter(|i| !["Llock", "Ldrain"].contains(&i.as_str())).map(String::as_str));
    assert_eq!((field("returned="), field("unmatched=")), (want.join(",").as_str(), "Lnope"), "{line}");

    // Judged once: later calls do not send them again (an unclear one may come back once more).
    answer(&root, &[], json!([{"id": "Llock", "verdict": "unchanged", "avoids_consequence": false}]));
    stop(&root, "s1", false, "Anything else?", true, &[]);
    assert!(!calls(&root)[2].0.contains("[Llock] lesson"));
    assert_eq!(kind(&root, "asked_then_changed"), ["Llock"]);

    // `borg debug memory`: the changed lesson now weighs more than the ignored one, and more than a new one (1).
    let lessons = root.join("store/collectives/corp/lessons.jsonl");
    let memory = || -> Vec<Value> {
        borg(&root, &["debug", "memory", lessons.to_str().unwrap()], "", false, &[]).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    };
    let m = |mem: &[Value], id: &str| mem.iter().find(|m| m["id"] == id).unwrap().clone();
    let mem = memory();
    assert_eq!((m(&mem, "Llock")["changed"].as_u64(), m(&mem, "Ldrain")["ignored"].as_u64()), (Some(1), Some(1)));
    assert!(m(&mem, "Llock")["weight"].as_f64() > m(&mem, "Ldrain")["weight"].as_f64());
    assert!(m(&mem, "Llock")["weight"].as_f64().unwrap() > 1.0, "{:?}", m(&mem, "Llock"));

    // The engineer corrects the same concern later in the session (`borg learn` merges it into Llock):
    // the correction overrides the judged change.
    let corrected = json!({"kind": "asked_then_corrected", "session": "s1", "lessons": ["Llock"], "ms": 4_000_000_000_000i64, "repo": "r"});
    std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("store/collectives/corp/outcomes.jsonl"))
        .unwrap()
        .write_all(format!("{corrected}\n").as_bytes())
        .unwrap();
    let mem = memory();
    assert_eq!((m(&mem, "Llock")["changed"].as_u64(), m(&mem, "Llock")["corrected"].as_u64()), (Some(0), Some(1)));
    assert!(m(&mem, "Llock")["weight"].as_f64().unwrap() < 1.0);
    assert_eq!(m(&mem, "Ldrain")["regretted"].as_f64(), Some(0.0));

    // A later fix of a decision Ldrain was asked before: half a correction at most, shown as `regretted`.
    let before = m(&mem, "Ldrain")["mean"].as_f64().unwrap();
    let regretted = json!({"kind": "regretted", "session": "s1", "lessons": ["Ldrain"], "ms": 4_000_000_000_001i64, "repo": "r", "cause": "R1", "credit": 0.5});
    std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("store/collectives/corp/outcomes.jsonl"))
        .unwrap()
        .write_all(format!("{regretted}\n").as_bytes())
        .unwrap();
    let mem = memory();
    assert_eq!(m(&mem, "Ldrain")["regretted"].as_f64(), Some(0.5));
    assert!(m(&mem, "Ldrain")["mean"].as_f64().unwrap() < before);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn unclear_or_missing_verdicts_stay_pending_and_are_sent_twice_at_most() {
    let root = setup("unclear");
    blocked_and_answered(&root, "s1", "Llock: applies");
    // No `resolved` at all: everything stays pending.
    std::fs::write(root.join("answer.json"), json!({"structured_output": {"moves": [], "applies": []}, "total_cost_usd": 0.01}).to_string()).unwrap();
    stop(&root, "s1", false, "Next step done.", true, &[]);
    // Unclear, and a change that still leads to the consequence (judge v2's answer shape): pending too.
    answer(
        &root,
        &[],
        json!([{"id": "Llock", "verdict": "changed", "avoids_consequence": false}, {"id": "Ldrain", "verdict": "unclear", "avoids_consequence": true}]),
    );
    stop(&root, "s1", false, "Another step done.", true, &[]);
    assert!(kind(&root, "asked_then_changed").is_empty() && kind(&root, "asked_ignored").is_empty(), "{:?}", outcomes(&root));
    // Measured, not a result, each under its own kind (a missing verdict writes nothing).
    assert_eq!(kind(&root, "asked_unclear"), ["Ldrain"], "{:?}", outcomes(&root));
    assert_eq!(kind(&root, "asked_consequence_remains"), ["Llock"], "{:?}", outcomes(&root));
    let state = store_file(&root, "state/s1.ctx");
    assert_eq!(state.matches("#borg-judged Llock=pending").count(), 2, "{state}");
    // Twice sent without a verdict: not sent again (borg learn settles it as asked_not_corrected).
    answer(&root, &[], json!([{"id": "Llock", "verdict": "changed", "avoids_consequence": true}]));
    stop(&root, "s1", false, "Last step.", true, &[]);
    let c = calls(&root);
    assert_eq!(c.len(), 4);
    assert!(c[1].0.contains("[Llock] lesson") && c[2].0.contains("[Llock] lesson") && !c[3].0.contains("from earlier in this session"), "{}", c[3].0);
    assert!(kind(&root, "asked_then_changed").is_empty());
    let _ = std::fs::remove_dir_all(root);
}

/// Judge v3 (0.7.5): the schema asks for five verdicts with a reason; a point that never came up
/// (`not_arisen`) is recorded under its own kind with the judge's reason (redacted, on one line), a
/// remaining consequence (`consequence_remains`) too; at a check both stay open (the point may still
/// come up later in the session) and are sent again, and only the session-end judge's `not_arisen`
/// settles; the cost line counts the verdicts by word and the reasons' characters; and the belief does
/// not move for the point that never came up.
#[test]
fn the_judge_gives_five_verdicts_each_with_its_reason() {
    let root = setup("verdicts");
    blocked_and_answered(&root, "s1", "Llock: applies");
    let lessons = root.join("store/collectives/corp/lessons.jsonl");
    let memory = || -> Vec<Value> {
        borg(&root, &["debug", "memory", lessons.to_str().unwrap()], "", false, &[]).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    };
    let before = memory();
    answer(
        &root,
        &[],
        json!([
            {"id": "Llock", "verdict": "not_arisen", "reason": "the turn only touched\nthe changelog; token AKIAIOSFODNN7EXAMPLE"},
            {"id": "Ldrain", "verdict": "consequence_remains", "reason": "one writer now, but frames still interleave on reconnect"}
        ]),
    );
    stop(&root, "s1", false, "Updated the changelog.", true, &[]);
    let (_, schema) = &calls(&root)[1];
    let schema: Value = serde_json::from_str(schema).unwrap();
    let item = &schema["properties"]["resolved"]["items"];
    assert_eq!(item["properties"]["verdict"]["enum"], json!(["changed", "ignored", "not_arisen", "consequence_remains", "unclear"]), "{schema}");
    assert_eq!(item["required"], json!(["id", "verdict", "reason"]));
    let rec = |k: &str| outcomes(&root).into_iter().find(|o| o["kind"] == k).unwrap_or_else(|| panic!("no {k}: {:?}", outcomes(&root)));
    let na = rec("asked_not_arisen");
    assert_eq!((na["lessons"].clone(), na["channel"].clone(), na["judge"].clone()), (json!(["Llock"]), json!("stop"), json!(3)), "{na}");
    let reason = na["reasons"][0].as_str().unwrap();
    assert!(reason.starts_with("the turn only touched the changelog; token ") && !reason.contains("AKIA"), "redacted, one line: {reason}");
    let remains = rec("asked_consequence_remains");
    assert_eq!(remains["reasons"], json!(["one writer now, but frames still interleave on reconnect"]), "{remains}");
    assert!(kind(&root, "asked_unclear").is_empty(), "{:?}", outcomes(&root));
    let state = store_file(&root, "state/s1.ctx");
    assert!(state.contains("#borg-judged Llock=pending") && state.contains("#borg-judged Ldrain=pending"), "a check's not arisen stays open: {state}");
    let cost = store_file(&root, "state/s1.cost");
    let line = cost.lines().find(|l| l.starts_with("judge ")).unwrap_or_else(|| panic!("{cost}"));
    assert!(line.contains(" resolved=1 ") && line.contains(" verdicts=changed:0,ignored:0,not_arisen:1,consequence_remains:1,unclear:"), "{line}");
    let chars: usize = line.split_whitespace().find_map(|w| w.strip_prefix("reason_chars=")).unwrap().parse().unwrap();
    assert_eq!(chars, reason.chars().count() + "one writer now, but frames still interleave on reconnect".len(), "{line}");
    // Both open: sent again.
    answer(&root, &[], json!([{"id": "Ldrain", "verdict": "changed", "reason": "the reconnect path is serialised too"}]));
    stop(&root, "s1", false, "Serialised the reconnect.", true, &[]);
    let (third, _) = &calls(&root)[2];
    assert!(third.contains("[Llock] lesson") && third.contains("[Ldrain] lesson"), "{third}");
    assert_eq!(kind(&root, "asked_then_changed"), ["Ldrain"]);
    // The session-end judge sees Llock once more, with the whole session; its not arisen settles.
    let end = json!({"structured_output": {"resolved": [{"id": "Llock", "verdict": "not_arisen", "reason": "no lock in the session's work"}]}, "total_cost_usd": 0.01});
    std::fs::write(root.join("answer.json"), end.to_string()).unwrap();
    learn_end(&root, "s1", &[]);
    let c = calls(&root);
    assert_eq!(c.len(), 4, "one call at the session's end");
    assert!(c[3].0.contains("[Llock] lesson") && !c[3].0.contains("[Ldrain] lesson"), "{}", c[3].0);
    let state = store_file(&root, "state/s1.ctx");
    assert!(state.contains("#borg-judged Llock=not_arisen\n#borg-end-judged Llock\n"), "{state}");
    assert!(outcomes(&root).iter().any(|o| o["kind"] == "asked_not_arisen" && o["at"] == "end"), "{:?}", outcomes(&root));
    // Censored for the belief: Llock's memory is as before its verdict.
    let m = |mem: &[Value], id: &str| mem.iter().find(|m| m["id"] == id).unwrap().clone();
    assert_eq!(m(&memory(), "Llock"), m(&before, "Llock"));
    let _ = std::fs::remove_dir_all(root);
}

/// `borg learn --session` as the end hook starts it: in the session's collective, with its state.
fn learn_end(root: &Path, session: &str, env: &[(&str, &str)]) {
    let corp = root.join("store/collectives/corp");
    let (home, state) = (corp.display().to_string(), corp.join("state").display().to_string());
    let mut all = vec![
        ("BORG_HOME", home.as_str()),
        ("BORG_STATE", state.as_str()),
        ("BORG_HARNESS", "claude"),
        ("BORG_TRANSCRIPT_CAPTURE", "off"),
        ("BORG_REVIEW_FIX", "off"),
    ];
    all.extend_from_slice(env);
    borg(root, &["learn", "--session", session], "", true, &all);
}

/// The session-end judge (0.7.5): `borg learn --session` judges once, in one call with the whole
/// session's work, every delivery no verdict settled (a question whose check tries were spent on unclear
/// verdicts included), never one a check settled; records the verdict as a check's judge does, marked
/// `at` end, with its cost line and a `model_call` with why `judge`; and never judges again. A session
/// with nothing open, `BORG_LEARN=off` and a consequence model turned off make no call.
#[test]
fn the_session_end_judges_every_open_delivery_once_with_the_whole_session() {
    let root = setup("end");
    blocked_and_answered(&root, "s1", "Llock: applies");
    // The checks settle Ldrain and spend Llock's tries on unclear verdicts.
    answer(&root, &[], json!([{"id": "Llock", "verdict": "unclear", "reason": "r"}, {"id": "Ldrain", "verdict": "changed", "reason": "one writer"}]));
    stop(&root, "s1", false, "Next step done.", true, &[]);
    answer(&root, &[], json!([{"id": "Llock", "verdict": "unclear", "reason": "r"}]));
    stop(&root, "s1", false, "Another step done.", true, &[]);
    assert_eq!(calls(&root).len(), 3);
    let end = json!({"structured_output": {"resolved": [{"id": "Llock", "verdict": "changed", "reason": "no lock is held across the await any more"}]}, "total_cost_usd": 0.02});
    std::fs::write(root.join("answer.json"), end.to_string()).unwrap();
    learn_end(&root, "s1", &[]);
    let c = calls(&root);
    assert_eq!(c.len(), 4, "one call at the session's end");
    let (prompt, schema) = &c[3];
    assert!(prompt.starts_with("You are the consequence model of an engineering team. A coding agent's session has ended."), "{prompt}");
    assert!(prompt.contains("## The session's work") && prompt.contains("one writer thread drains the delivery queue"), "the whole session: {prompt}");
    assert!(prompt.contains("[Llock] lesson") && !prompt.contains("[Ldrain] lesson"), "only what stays open: {prompt}");
    let schema: Value = serde_json::from_str(schema).unwrap();
    assert_eq!(schema["required"], json!(["resolved"]), "{schema}");
    let rec = outcomes(&root).into_iter().find(|o| o["kind"] == "asked_then_changed" && o["lessons"] == json!(["Llock"])).expect("Llock judged");
    assert_eq!((rec["at"].clone(), rec["channel"].clone(), rec["judge"].clone()), (json!("end"), json!("stop"), json!(3)), "{rec}");
    assert_eq!(rec["reasons"], json!(["no lock is held across the await any more"]));
    let checked = outcomes(&root).into_iter().find(|o| o["kind"] == "asked_then_changed" && o["lessons"] == json!(["Ldrain"])).unwrap();
    assert!(checked.get("at").is_none(), "a check's verdict has no `at`: {checked}");
    let state = store_file(&root, "state/s1.ctx");
    assert!(state.contains("#borg-judged Llock=changed\n#borg-end-judged Llock\n"), "{state}");
    let cost = store_file(&root, "state/s1.cost");
    let line = cost.lines().last().unwrap_or_default();
    // No evidence of the judge's effective length yet: the start budget, or on a drawn share of passes
    // the step above it (judge_length.rs), the chance logged.
    let start = line.contains(" model=claude:sonnet at=end batch=1/1 budget=72000 psi=0.2000 explore=0 cut=none");
    let explored = line.contains(" model=claude:sonnet at=end batch=1/1 budget=108000 psi=0.2000 explore=1 cut=none");
    assert!(line.starts_with("judge call=4 pending=1 resolved=1 ") && (start || explored), "{cost}");
    let check = cost.lines().find(|l| l.starts_with("judge ")).unwrap_or_default();
    let prompt = check.split_once(" model=claude:sonnet prompt=").map(|(_, n)| n.parse::<usize>());
    assert!(matches!(prompt, Some(Ok(n)) if n > 0), "a check's judge line names its model and its prompt's size too: {check}");
    let judge: Vec<Value> = store_file(&root, "stats/events.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|e| e["kind"] == "model_call" && e["why"] == "judge")
        .collect();
    assert_eq!(judge.len(), 1, "{judge:?}");
    assert_eq!((judge[0]["session"].clone(), judge[0]["usd"].clone()), (json!("s1"), json!(0.02)));
    // Settled at the end: a second run judges nothing again.
    learn_end(&root, "s1", &[]);
    assert_eq!(calls(&root).len(), 4, "never judged twice");
    assert_eq!(kind(&root, "asked_then_changed"), ["Ldrain", "Llock"]);
    // Nothing open: no call.
    learn_end(&root, "s2", &[]);
    assert_eq!(calls(&root).len(), 4);
    // Open, but learning or the consequence model is off: no call; on again, the call comes.
    blocked_and_answered(&root, "s3", "Llock: applies");
    let n = calls(&root).len();
    learn_end(&root, "s3", &[("BORG_LEARN", "off")]);
    learn_end(&root, "s3", &[("BORG_PREDICT_MODEL", "off")]);
    assert_eq!(calls(&root).len(), n, "off: nothing runs");
    assert!(!store_file(&root, "state/s3.ctx").contains("#borg-end-judged"));
    std::fs::write(root.join("answer.json"), json!({"structured_output": {"resolved": []}}).to_string()).unwrap();
    learn_end(&root, "s3", &[]);
    assert_eq!(calls(&root).len(), n + 1);
    assert!(store_file(&root, "state/s3.ctx").contains("#borg-end-judged "), "judged without a verdict: settled there as well");
    let _ = std::fs::remove_dir_all(root);
}

/// A session whose end hook never ran settles once idle (outcome.rs `idle_sessions`) in any later
/// `borg learn`: the session-end judge gives its open deliveries their last verdict there too, in one
/// call, before the settle, which then leaves the judged lesson alone; once.
#[test]
fn an_idle_session_gets_the_session_end_judge_before_it_settles() {
    let root = setup("idle");
    blocked_and_answered(&root, "s1", "Llock: applies");
    answer(&root, &[], json!([{"id": "Llock", "verdict": "unclear", "reason": "r"}, {"id": "Ldrain", "verdict": "changed", "reason": "one writer"}]));
    stop(&root, "s1", false, "Next step done.", true, &[]);
    let n = calls(&root).len();
    // Two days quiet: its records and its state file.
    let corp = root.join("store/collectives/corp");
    let old = |v: &mut Value| {
        if let Some(ms) = v["ms"].as_i64() {
            v["ms"] = json!(ms - 2 * 86_400_000);
        }
    };
    for f in ["outcomes.jsonl", "episodes.jsonl"] {
        let Ok(text) = std::fs::read_to_string(corp.join(f)) else { continue };
        let aged: String = text
            .lines()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                old(&mut v);
                format!("{v}\n")
            })
            .collect();
        std::fs::write(corp.join(f), aged).unwrap();
    }
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    std::fs::File::options().append(true).open(corp.join("state/s1.ctx")).unwrap().set_modified(then).unwrap();
    std::fs::write(
        root.join("answer.json"),
        json!({"structured_output": {"resolved": [{"id": "Llock", "verdict": "ignored", "reason": "the lock still spans the await"}]}}).to_string(),
    )
    .unwrap();
    // Another session's end runs `borg learn`: s1 is idle.
    learn_end(&root, "s2", &[]);
    let c = calls(&root);
    assert_eq!(c.len(), n + 1, "one session-end call for the idle session");
    assert!(c[n].0.contains("## The session's work") && c[n].0.contains("[Llock] lesson"), "{}", c[n].0);
    let rec = outcomes(&root).into_iter().find(|o| o["kind"] == "asked_ignored" && o["lessons"] == json!(["Llock"])).expect("Llock judged");
    assert_eq!(rec["at"], json!("end"), "{rec}");
    let settled: Vec<Value> = outcomes(&root).into_iter().filter(|o| o["kind"] == "asked_not_corrected" && o["lessons"] == json!(["Llock"])).collect();
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["reason"], json!("judged"), "settled as judged: {settled:?}");
    assert!(settled[0]["ms"].as_i64() >= rec["ms"].as_i64(), "after the verdict: {settled:?} {rec}");
    learn_end(&root, "s2", &[]);
    assert_eq!(calls(&root).len(), n + 1, "once");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_turn_end_the_prefilter_skips_leaves_questions_pending() {
    let root = setup("skip");
    blocked_and_answered(&root, "s1", "Llock: applies");
    // A threshold nothing reaches: the free gate skips the turn end, no model call, nothing judged.
    answer(&root, &[], json!([{"id": "Llock", "verdict": "changed", "avoids_consequence": true}]));
    stop(&root, "s1", false, "Renamed a CSS token.", true, &[("BORG_PREFILTER", "1000000")]);
    assert_eq!(calls(&root).len(), 1);
    assert!(store_file(&root, "state/s1.ctx").contains("#borg-skip"));
    assert!(outcomes(&root).iter().all(|o| ["asked", "graded", "delivery"].contains(&o["kind"].as_str().unwrap())), "nothing judged: {:?}", outcomes(&root));
    assert!(!store_file(&root, "state/s1.cost").contains("judge "));
    // The next turn end that reaches the model judges them.
    stop(&root, "s1", false, "Wired the writer thread.", true, &[]);
    assert_eq!(calls(&root).len(), 2);
    assert_eq!(kind(&root, "asked_then_changed"), ["Llock"]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_word_rule_and_a_declaration_alone_write_no_outcome() {
    // BORG_MATCH=bm25: no model call ever, so the questions stay pending; the edits carry the lesson's
    // words and the drone declares, and still no change or ignore is recorded.
    let root = setup("words");
    let out = stop(&root, "s1", false, PLAN, false, &[]);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[Llock] Is the websocket send lock") && reason.contains("`<id>: applies`"), "{reason}");
    edit(&root, "s1", "one writer thread drains the delivery queue");
    assert_eq!(stop(&root, "s1", true, "Revised: no lock across the await.\nLlock: applies\nLdrain: skip\n", false, &[]).trim(), "");
    stop(&root, "s1", false, "Done.", false, &[]);
    assert!(calls(&root).is_empty());
    assert!(outcomes(&root).iter().all(|o| ["asked", "graded", "delivery"].contains(&o["kind"].as_str().unwrap())), "nothing judged: {:?}", outcomes(&root));
    let edits = store_file(&root, "state/s1.edits");
    assert!(edits.contains("#borg-asked Llock") || edits.contains(",Llock"), "{edits}");
    assert_eq!(edits.matches("#borg-answer ").count(), 1, "one answer per block: {edits}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_order_questions_are_shown_in_is_stable_per_session_and_varies_across_sessions() {
    let root = setup("order");
    let plan = "Plan: websocket send lock, delivery queue writer thread, spec close handshake.";
    let order = |s: &str| {
        let out = stop(&root, s, false, plan, false, &[]);
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        let reason = v["reason"].as_str().unwrap().to_string();
        let mut ids: Vec<(usize, &str)> = ["Llock", "Ldrain", "Lspec"].iter().filter_map(|i| reason.find(&format!("[{i}]")).map(|p| (p, *i))).collect();
        ids.sort();
        ids.into_iter().map(|(_, i)| i.to_string()).collect::<Vec<_>>()
    };
    let firsts: std::collections::HashSet<String> = (0..12).map(|i| order(&format!("o{i}"))[0].clone()).collect();
    assert!(firsts.len() >= 2, "the same lesson always first: {firsts:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn bm25_asks_log_slot_propensities() {
    // Eight unscoped lessons on the plan's words compete for GENERIC_CAP slots: each asked one carries its
    // chance of a slot under the session's draws (conscience.rs `propensity`), in (0, 1].
    let root = setup("propensity");
    let mut lessons = store_file(&root, "lessons.jsonl");
    for i in 0..5 {
        lessons += &format!(
            "{}\n",
            json!({"id": format!("Lws{i}"), "origin_ms": 1, "question": format!("Is the websocket delivery queue lock {i} released before the await?")})
        );
    }
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), lessons).unwrap();
    let out = stop(&root, "s1", false, PLAN, false, &[]);
    assert!(out.contains("block"), "{out}");
    let a = outcomes(&root).into_iter().find(|o| o["kind"] == "asked").unwrap();
    let p: Vec<f64> = a["p"].as_array().unwrap_or_else(|| panic!("{a}")).iter().map(|x| x.as_f64().unwrap()).collect();
    assert_eq!(p.len(), a["lessons"].as_array().unwrap().len(), "{a}");
    assert!(p.iter().all(|&x| x > 0.0 && x <= 1.0), "{a}");
    assert!(p.iter().any(|&x| x < 1.0), "the pool overflows the slots: {a}");
    assert!(store_file(&root, "state/s1.ctx").lines().any(|l| l.starts_with("#borg-p ")), "{}", store_file(&root, "state/s1.ctx"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn teammates_sessions_age_a_team_lesson_and_a_repo_convention_only_in_its_repo() {
    // Disuse counts the sessions a lesson was deliverable in, on any node (memory.rs "Whose sessions
    // count"): a teammate's sessions age the team lesson Lgit, and the repo convention Lconv only when
    // they work in its repo. This node's own three sessions keep both active.
    let root = setup("peer-clock");
    let store = root.join("store/collectives/corp");
    let mut lessons = store_file(&root, "lessons.jsonl");
    lessons += &format!("{}\n", json!({"id": "Lconv", "origin_ms": 1, "question": "Is the deploy run through make ship?", "reach": "repo", "repo": "corp"}));
    std::fs::write(store.join("lessons.jsonl"), lessons).unwrap();
    let local: String =
        (0..3).map(|i| format!("{}\n", json!({"kind": "asked", "session": format!("own{i}"), "lessons": ["Llock"], "ms": 10 + i, "repo": "corp"}))).collect();
    std::fs::write(store.join("outcomes.jsonl"), local).unwrap();
    let peers = |repo: &str| -> String {
        (0..60)
            .map(|i| format!("{}\n", json!({"kind": "asked", "s": borg::outcome::tag(&format!("peer{i}")), "lessons": ["Ldrain"], "ms": 100 + i, "node": "n2", "r": borg::outcome::tag(repo)})))
            .collect()
    };
    let dormant = |root: &Path| -> std::collections::HashMap<String, bool> {
        let path = store.join("lessons.jsonl");
        borg(root, &["debug", "memory", path.to_str().unwrap()], "", false, &[])
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .map(|v| (v["id"].as_str().unwrap().to_string(), v["dormant"].as_bool().unwrap()))
            .collect()
    };
    let alone = dormant(&root);
    assert_eq!((alone["Lgit"], alone["Lconv"]), (false, false), "{alone:?}");
    std::fs::write(store.join("outcomes-team.jsonl"), peers("elsewhere")).unwrap();
    let busy_elsewhere = dormant(&root);
    assert_eq!((busy_elsewhere["Lgit"], busy_elsewhere["Lconv"]), (true, false), "{busy_elsewhere:?}");
    std::fs::write(store.join("outcomes-team.jsonl"), peers("corp")).unwrap();
    let busy_here = dormant(&root);
    assert_eq!((busy_here["Lgit"], busy_here["Lconv"]), (true, true), "{busy_here:?}");
    // Kept to this machine's view, the teammates' sessions age nothing.
    let path = store.join("lessons.jsonl");
    let off = borg(&root, &["debug", "memory", path.to_str().unwrap()], "", false, &[("BORG_SHARE_OUTCOMES", "off")]);
    assert!(off.lines().all(|l| l.contains("\"dormant\":false")), "{off}");
    let _ = std::fs::remove_dir_all(root);
}
