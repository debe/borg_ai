//! The whole-store Stop pool (`BORG_STOP_POOL=all`, whole.rs) and the edits gate (`BORG_STOP_GATE=edits`)
//! end to end through the binary. The model is a fake `claude` on PATH that saves each prompt and predicts
//! `Mdrain`, or answers with `answer.json` when a test writes one: no test makes a real model call.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// A Orion repo (`harbor_orion`, system orion), a store with: a Orion-scoped lesson, an Beacon-scoped
/// one (another system), one scoped to a name the work never mentions, an old lesson superseded by a
/// newer one, and six generic ones.
fn setup(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-whole-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = root.join("store/collectives/corp");
    std::fs::create_dir_all(&store).unwrap();
    for d in ["harbor_orion", "home", "bin", "prompts"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let mut lessons = vec![
        json!({"id": "Mdrain", "origin_ms": 1, "scope": ["Orion"], "question": "Does the Orion net drain the budget place with an explicit consumer instead of a reset arc?", "consequence": "a reset arc drops the budget token"}),
        json!({"id": "Pbeacon", "origin_ms": 2, "scope": ["Beacon"], "question": "Is the Beacon replay judged by exactly the named model?", "consequence": "the judge drifts"}),
        json!({"id": "Zother", "origin_ms": 3, "scope": ["Zephyrpipe"], "question": "Does the Zephyrpipe export keep its schema version?", "consequence": "consumers break"}),
        json!({"id": "Old", "origin_ms": 4, "question": "Do retries use a fixed delay?", "consequence": "thundering herd"}),
        json!({"id": "New", "origin_ms": 5, "source": "queen", "supersedes": ["Old"], "question": "Do retries use jittered backoff, never a fixed delay?", "consequence": "thundering herd"}),
    ];
    for (i, w) in ["websocket", "tomatoes", "invoices", "telescope", "glaciers", "harpsichord"].iter().enumerate() {
        lessons.push(json!({"id": format!("G{i}"), "origin_ms": 10 + i, "question": format!("Did I check the {w} handling for the {w} edge case and the {w} limits?"), "consequence": format!("{w} breaks")}));
    }
    std::fs::write(store.join("lessons.jsonl"), lessons.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("harbor_orion")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    let fake = root.join("bin/claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nn=$(ls {d} | wc -l)\nprintf '%s' \"$2\" > {d}/$n.txt\nif [ -f {a} ]; then cat {a}; else echo '{{\"structured_output\":{{\"moves\":[],\"applies\":[{{\"id\":\"Mdrain\",\"why\":\"w\",\"grade\":3}}]}},\"total_cost_usd\":0.05}}'; fi\n",
            d = root.join("prompts").display(),
            a = root.join("answer.json").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

fn borg(root: &Path, event: &str, input: &Value, env: &[(&str, &str)]) -> String {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", "claude", event])
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into()
}

/// One turn: a prompt, the given tool calls, then the Stop with `last`. Returns the Stop's output.
fn turn(root: &Path, session: &str, prompt: &str, tools: &[(&str, Value)], last: &str, env: &[(&str, &str)]) -> String {
    let cwd = root.join("harbor_orion");
    borg(root, "prompt", &json!({"session_id": session, "cwd": cwd, "prompt": prompt}), env);
    for (tool, input) in tools {
        borg(root, "post_tool", &json!({"session_id": session, "cwd": cwd, "tool_name": tool, "tool_input": input}), env);
    }
    borg(root, "stop", &json!({"session_id": session, "cwd": cwd, "last_assistant_message": last, "stop_hook_active": false}), env)
}

/// The prompts the fake model received, in order.
fn prompts(root: &Path) -> Vec<String> {
    let dir = root.join("prompts");
    let n = std::fs::read_dir(&dir).unwrap().count();
    (0..n).map(|i| std::fs::read_to_string(dir.join(format!("{i}.txt"))).unwrap()).collect()
}

/// The lesson ids in a prompt's lesson block (whole layout: between `## Lessons` and `## Current work`).
fn shown(prompt: &str) -> Vec<String> {
    let start = prompt.find("## Lessons\n").expect("lesson block") + 11;
    let end = prompt.find("\n## Current work\n").expect("work block");
    prompt[start..end].lines().filter_map(|l| l.strip_prefix('[')?.split(']').next().map(String::from)).collect()
}

fn state(root: &Path, session: &str, ext: &str) -> String {
    std::fs::read_to_string(root.join(format!("store/collectives/corp/state/{session}.{ext}"))).unwrap_or_default()
}

const ALL: [(&str, &str); 2] = [("BORG_STOP_POOL", "all"), ("BORG_PREFILTER", "off")];

#[test]
fn the_whole_pool_is_every_allowed_lesson_lessons_first_and_done_ones_drop_out() {
    let root = setup("pool");
    let out = turn(&root, "w1", "Tidy the budget handling of the net", &[], "Done: the budget place is drained.", &ALL);
    assert!(out.contains("block") && out.contains("explicit consumer"), "the model's pick is asked: {out}");
    let p = &prompts(&root)[0];
    assert!(p.starts_with("You are the consequence model") && p.find("## Lessons") < p.find("## Current work"), "{p}");
    assert!(
        p.contains(
            "[Mdrain] Does the Orion net drain the budget place with an explicit consumer instead of a reset arc? -> a reset arc drops the budget token"
        ),
        "{p}"
    );
    // Store order; Beacon is another system, Zephyrpipe is never named, Old is superseded.
    assert_eq!(shown(p), ["Mdrain", "New", "G0", "G1", "G2", "G3", "G4", "G5"]);
    assert!(state(&root, "w1", "cost").contains("pool mode=all eligible=8 shown=8 budget=fits"), "{}", state(&root, "w1", "cost"));

    // The next turn end: Mdrain was asked (done), so it is no longer shown; the prefix before the work
    // is otherwise the same lesson block.
    turn(&root, "w1", "and the tests", &[], "Tests added.", &ALL);
    let p2 = &prompts(&root)[1];
    assert_eq!(shown(p2), ["New", "G0", "G1", "G2", "G3", "G4", "G5"]);

    // The default is the whole pool, compact and graded (reports/whole-store.md, "Graded follow-up").
    turn(&root, "w3", "Tidy the budget handling of the net", &[], "Done: the budget place is drained.", &[("BORG_PREFILTER", "off")]);
    let p3 = &prompts(&root)[2];
    assert_eq!(shown(p3), ["Mdrain", "New", "G0", "G1", "G2", "G3", "G4", "G5"]);
    assert!(p3.contains("Grade each lesson you return"), "{p3}");
    // BORG_STOP_POOL=shortlist keeps the shortlist layout (work first, situations form).
    turn(
        &root,
        "w2",
        "Tidy the budget handling of the net",
        &[],
        "Done: the budget place is drained.",
        &[("BORG_PREFILTER", "off"), ("BORG_STOP_POOL", "shortlist")],
    );
    let p4 = &prompts(&root)[3];
    assert!(p4.find("## Current work") < p4.find("## Lessons"), "{p4}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_classic_format_asks_about_the_whole_pool_in_the_shortlist_layout() {
    let root = setup("classic");
    let env = [("BORG_STOP_POOL", "all"), ("BORG_STOP_FORMAT", "classic"), ("BORG_PREFILTER", "off")];
    let out = turn(&root, "c1", "Tidy the budget handling of the net", &[], "Done: the budget place is drained.", &env);
    assert!(out.contains("block") && out.contains("explicit consumer"), "the model's pick is asked: {out}");
    let p = &prompts(&root)[0];
    // predict::prompt's layout: the work first, then the lessons as `situations: ... -> consequence: ...`.
    assert!(p.find("## Current work") < p.find("## Lessons"), "{p}");
    assert!(p.contains("[Mdrain] situations: ") && p.contains("-> consequence: a reset arc drops the budget token"), "{p}");
    // The lesson set is the whole pool, in store order, as with the compact layout.
    let block = &p[p.find("## Lessons\n").unwrap() + 11..];
    let ids: Vec<&str> = block.lines().filter_map(|l| l.strip_prefix('[')?.split(']').next()).collect();
    assert_eq!(ids, ["Mdrain", "New", "G0", "G1", "G2", "G3", "G4", "G5"]);
    assert!(state(&root, "c1", "cost").contains("pool mode=all eligible=8 shown=8"), "{}", state(&root, "c1", "cost"));

    // With BORG_STOP_POOL=shortlist, BORG_STOP_FORMAT changes nothing: the shortlist (no whole-pool log line).
    turn(
        &root,
        "c2",
        "Tidy the budget handling of the net",
        &[],
        "Done: the budget place is drained.",
        &[("BORG_STOP_FORMAT", "classic"), ("BORG_STOP_POOL", "shortlist"), ("BORG_PREFILTER", "off")],
    );
    assert!(!state(&root, "c2", "cost").contains("pool mode=all"), "{}", state(&root, "c2", "cost"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn over_the_budget_the_shortlist_order_picks_what_fits() {
    let root = setup("budget");
    // Room for about two lines: the BM25 best matches of this turn win (telescope, then glaciers).
    let env = [("BORG_STOP_POOL", "all"), ("BORG_PREFILTER", "off"), ("BORG_STOP_POOL_TOKENS", "60")];
    turn(&root, "b1", "Look at the telescope and glaciers code", &[], "The telescope limits and the telescope edge case, plus glaciers.", &env);
    let ids = shown(&prompts(&root)[0]);
    assert_eq!(ids, ["G3", "G4"], "best matches, in store order");
    assert!(state(&root, "b1", "cost").contains("shown=2 budget=full"), "{}", state(&root, "b1", "cost"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_edits_gate_asks_only_after_a_turn_that_changed_something() {
    let root = setup("edits");
    let env = [("BORG_STOP_GATE", "edits")];
    let read = ("Read", json!({"file_path": "/x/net.rs"}));
    // Read-only, though its words match a lesson strongly: no call.
    turn(
        &root,
        "e1",
        "Does the Orion net drain the budget place?",
        std::slice::from_ref(&read),
        "The Orion net drains the budget place with a reset arc.",
        &env,
    );
    assert_eq!(prompts(&root).len(), 0);
    assert!(state(&root, "e1", "ctx").contains("#borg-skip noedit"), "{}", state(&root, "e1", "ctx"));
    // A read-only shell command: no call either.
    turn(&root, "e1", "look", &[("Bash", json!({"command": "ls -la && git status"}))], "ok", &env);
    assert_eq!(prompts(&root).len(), 0);
    // An edit with chat-only words, which the lexical gate would skip: the model is asked.
    let out = turn(&root, "e1", "go", &[("Edit", json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "b"}))], "done", &env);
    assert_eq!(prompts(&root).len(), 1);
    assert!(out.contains("block"), "{out}");
    // The rework after the block, then the second Stop (stop_hook_active): the marker is consumed there.
    let cwd = root.join("harbor_orion");
    borg(&root, "post_tool", &json!({"session_id": "e1", "cwd": cwd, "tool_name": "Write", "tool_input": {"file_path": "/x/a"}}), &env);
    borg(&root, "stop", &json!({"session_id": "e1", "cwd": cwd, "last_assistant_message": "reworked", "stop_hook_active": true}), &env);
    turn(&root, "e1", "thanks", &[], "you're welcome", &env);
    assert_eq!(prompts(&root).len(), 1, "a talk-only turn after the rework does not call");
    // A state-changing command calls.
    turn(&root, "e1", "build it", &[("Bash", json!({"command": "cargo build --release"}))], "built", &env);
    assert_eq!(prompts(&root).len(), 2);
    // Without the option, the lexical gate decides as before: the edit with chat words is skipped.
    turn(&root, "e2", "go", &[("Edit", json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "b"}))], "ok thanks", &[]);
    assert_eq!(prompts(&root).len(), 2);
    assert!(!root.join("store/collectives/corp/state/e2.changes").exists(), "no marker file without the option");
    let _ = std::fs::remove_dir_all(root);
}

/// The fake model's next answers: `applies` as (id, grade) pairs.
fn answer(root: &Path, applies: &[(&str, u8)]) {
    let a: Vec<Value> = applies.iter().map(|(id, g)| json!({"id": id, "why": "w", "grade": g})).collect();
    let out = json!({"structured_output": {"moves": [], "applies": a}, "total_cost_usd": 0.05});
    std::fs::write(root.join("answer.json"), out.to_string()).unwrap();
}

fn context(out: &str) -> String {
    serde_json::from_str::<Value>(out).ok().and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)).unwrap_or_default()
}

#[test]
fn graded_blocks_only_for_a_three_and_hands_twos_and_ones_over_free_once() {
    let root = setup("graded");
    let env = [("BORG_STOP_POOL", "all"), ("BORG_PREFILTER", "off"), ("BORG_STOP_GRADED", "on"), ("BORG_EARLY", "off")];
    let cwd = root.join("harbor_orion");
    // A 2 and a 1: no block; both wait for the next prompt or touch.
    answer(&root, &[("Mdrain", 2), ("G0", 1)]);
    let out = turn(&root, "g1", "Tidy the budget handling of the net", &[], "Done: the budget place is drained.", &env);
    assert!(!out.contains("block"), "2s and 1s alone never block: {out}");
    let p = &prompts(&root)[0];
    assert!(p.contains("Grade each lesson you return: 3 = this work clearly runs into the consequence") && p.find("Grade each") < p.find("## Lessons"), "{p}");
    assert!(state(&root, "g1", "cost").contains("graded applied=2 ask=0 free=2 queued"), "{}", state(&root, "g1", "cost"));
    // The next prompt carries them, as free context; the next touch does not repeat them.
    let free = context(&borg(&root, "prompt", &json!({"session_id": "g1", "cwd": cwd, "prompt": "next: the invoices"}), &env));
    assert!(free.contains("explicit consumer") && free.contains("websocket handling"), "{free}");
    assert!(state(&root, "g1", "ctx").contains("#borg-queued Mdrain,G0"), "{}", state(&root, "g1", "ctx"));
    assert!(state(&root, "g1", "cost").contains("free event=UserPromptSubmit lessons=2"), "{}", state(&root, "g1", "cost"));
    let again = borg(&root, "post_tool", &json!({"session_id": "g1", "cwd": cwd, "tool_name": "Read", "tool_input": {"file_path": "/x/inv.rs"}}), &env);
    assert!(context(&again).is_empty(), "{again}");

    // A 3 blocks: the block asks the 3, the 2 and the 1, and nothing the model did not grade (no widening).
    answer(&root, &[("G1", 1), ("New", 3), ("G2", 2)]);
    let out = turn(&root, "g1", "now the retries", &[], "Retries use a fixed delay, also for invoices and tomatoes.", &env);
    assert!(out.contains("block"), "{out}");
    let asked = state(&root, "g1", "ctx").lines().rev().find_map(|l| l.strip_prefix("#borg-asked ")).unwrap().to_string();
    let mut ids: Vec<&str> = asked.split(',').collect();
    ids.sort();
    assert_eq!(ids, ["G1", "G2", "New"]);
    assert!(!root.join("store/collectives/corp/state/g1.queued").exists(), "a blocking turn queues nothing");

    // A lone 3 is asked alone: the graded block does not widen over the low bar...
    let last = |s: &str| state(&root, s, "ctx").lines().rev().find_map(|l| l.strip_prefix("#borg-asked ")).unwrap().to_string();
    let work = "Retries use a fixed delay; the invoices handling for the invoices edge case and the invoices limits.";
    answer(&root, &[("New", 3)]);
    turn(&root, "g2", "now the retries", &[], work, &env);
    assert_eq!(last("g2"), "New");
    // ...while with BORG_STOP_GRADED=off the grades are ignored and the block widens as before.
    turn(&root, "g3", "now the retries", &[], work, &[("BORG_STOP_GRADED", "off"), ("BORG_PREFILTER", "off"), ("BORG_EARLY", "off")]);
    assert!(last("g3").split(',').count() > 1 && last("g3").contains("New"), "{}", last("g3"));
    assert!(!prompts(&root).last().unwrap().contains("Grade each"));
    let _ = std::fs::remove_dir_all(root);
}
