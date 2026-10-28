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
            "#!/bin/sh\nn=$(( $(ls {c} | wc -l) / 2 ))\nprintf '%s' \"$2\" > {c}/$n.prompt\nprintf '%s' \"${{10}}\" > {c}/$n.schema\ncat {a}\n",
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
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_VECTORS", "off")
        .env("BORG_EARLY", "off")
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

fn asked(root: &Path, session: &str) -> Vec<String> {
    store_file(root, &format!("state/{session}.ctx"))
        .lines()
        .filter_map(|l| l.strip_prefix("#borg-asked "))
        .flat_map(|l| l.split(','))
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
    assert!(!first[0].0.contains("asked earlier") && !first[0].1.contains("resolved"), "nothing pending yet");
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
    assert!(prompt.contains("## Questions asked earlier in this session") && schema.contains("\"resolved\""), "{schema}");
    assert!(
        prompt.contains("[Llock] question: Is the websocket send lock") && prompt.contains("a slow client stalls every sender (declared: applies)"),
        "{prompt}"
    );
    assert!(prompt.contains("frames interleave on the socket (declared: skip)"), "{prompt}");
    assert!(prompt.contains("before: Plan: hold the websocket send lock"), "{prompt}");
    assert!(prompt.contains("one writer thread drains the delivery queue") && prompt.contains("this turn: Done: the writer thread"), "{prompt}");
    assert_eq!(prompt.matches("### Asked together").count(), 1, "one block, one shared evidence");
    assert_eq!(kind(&root, "asked_then_changed"), ["Llock"]);
    assert_eq!(kind(&root, "asked_ignored"), ["Ldrain"]);
    let state = store_file(&root, "state/s1.ctx");
    assert!(state.contains("#borg-judged Llock=changed") && state.contains("#borg-judged Ldrain=ignored"), "{state}");
    if ids.contains(&"Lspec".to_string()) {
        assert!(state.contains("#borg-judged Lspec=pending"), "{state}");
    }
    // The cost log: which call judged how many, and the characters it added.
    let cost = store_file(&root, "state/s1.cost");
    let line = cost.lines().find(|l| l.starts_with("judge ")).unwrap_or_else(|| panic!("{cost}"));
    assert!(line.starts_with(&format!("judge call=2 pending={} resolved=2 chars=", ids.len())), "{line}");
    let chars: usize = line.rsplit("chars=").next().unwrap().parse().unwrap();
    assert!(chars > 300 && prompt.len() > chars, "{chars}");

    // Judged once: later calls do not send them again (an unclear one may come back once more).
    answer(&root, &[], json!([{"id": "Llock", "verdict": "unchanged", "avoids_consequence": false}]));
    stop(&root, "s1", false, "Anything else?", true, &[]);
    assert!(!calls(&root)[2].0.contains("[Llock] question"));
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
    // Unclear, and a change that still leads to the consequence: pending too.
    answer(
        &root,
        &[],
        json!([{"id": "Llock", "verdict": "changed", "avoids_consequence": false}, {"id": "Ldrain", "verdict": "unclear", "avoids_consequence": true}]),
    );
    stop(&root, "s1", false, "Another step done.", true, &[]);
    assert!(kind(&root, "asked_then_changed").is_empty() && kind(&root, "asked_ignored").is_empty(), "{:?}", outcomes(&root));
    let state = store_file(&root, "state/s1.ctx");
    assert_eq!(state.matches("#borg-judged Llock=pending").count(), 2, "{state}");
    // Twice sent without a verdict: not sent again (borg learn settles it as asked_not_corrected).
    answer(&root, &[], json!([{"id": "Llock", "verdict": "changed", "avoids_consequence": true}]));
    stop(&root, "s1", false, "Last step.", true, &[]);
    let c = calls(&root);
    assert_eq!(c.len(), 4);
    assert!(c[1].0.contains("[Llock] question") && c[2].0.contains("[Llock] question") && !c[3].0.contains("asked earlier"), "{}", c[3].0);
    assert!(kind(&root, "asked_then_changed").is_empty());
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
    assert!(outcomes(&root).iter().all(|o| o["kind"] == "asked" || o["kind"] == "graded"), "nothing judged: {:?}", outcomes(&root));
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
    assert!(outcomes(&root).iter().all(|o| o["kind"] == "asked" || o["kind"] == "graded"), "nothing judged: {:?}", outcomes(&root));
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
