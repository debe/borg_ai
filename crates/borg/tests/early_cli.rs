//! Early delivery end to end through the binary: a prompt or touch that first names a lesson's scope
//! gets that scope's top lessons as `additionalContext` once, unscoped lessons never come early, and the
//! Stop checkpoint does not ask a delivered lesson again. No test makes a model call (BORG_MATCH=bm25).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-early-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A store with Beacon-scoped, Orion-scoped and unscoped lessons, routed from `<root>/corp`.
fn setup(name: &str) -> PathBuf {
    let root = tmp(name);
    let store = root.join("store/collectives/corp");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(root.join("corp")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let lessons = [
        json!({"id": "Alive", "origin_ms": 1, "scope": ["Beacon"], "question": "Does the Beacon replay experiment run its services on their live hosts?",
            "consequence": "develop lacks the products, so the replay compares empty results"}),
        json!({"id": "Ajudge", "origin_ms": 1, "scope": ["Beacon", "search-lab"], "question": "Is the replay judge exactly the model the user named for the Beacon experiment?",
            "consequence": "a convenience judge changes the verdict"}),
        json!({"id": "Mdrain", "origin_ms": 1, "scope": ["Orion"], "question": "Is each per-turn place drained by an explicit consumer instead of a reset arc?",
            "consequence": "a reset arc silently drops the hint budget", "overrides": "upstream petri-net-design reset arcs"}),
        json!({"id": "Gplan", "origin_ms": 1, "question": "Does the replay experiment plan name a control arm and its live judge model?",
            "consequence": "no baseline to compare"}),
    ];
    std::fs::write(store.join("lessons.jsonl"), lessons.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("corp")], "share": "local"}]}).to_string(),
    )
    .unwrap();
    root
}

fn borg(root: &Path, harness: &str, event: &str, input: &Value, env: &[(&str, &str)]) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", harness, event])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        // The fake `claude` alone answers: a failed answer would otherwise fall through to the next model
        // CLI on PATH (runner.rs `pick`), a real call.
        .env("BORG_PREDICT_MODEL", "claude:")
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_MATCH", "bm25")
        .env("BORG_LEARN", "off")
        .env("BORG_SELECT", "fixed")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into()
}

fn state(root: &Path, session: &str) -> String {
    std::fs::read_to_string(root.join(format!("store/collectives/corp/state/{session}.ctx"))).unwrap_or_default()
}

fn context(out: &str) -> String {
    let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("not JSON: {out:?}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

#[test]
fn a_named_scope_brings_its_lessons_early_once_and_stop_does_not_ask_them_again() {
    let root = setup("once");
    let cwd = root.join("corp");
    let prompt = |s: &str, p: &str| borg(&root, "claude", "prompt", &json!({"session_id": s, "cwd": cwd, "prompt": p}), &[]);
    let touch = |s: &str, path: &str| {
        borg(&root, "claude", "post_tool", &json!({"session_id": s, "cwd": cwd, "tool_name": "Read", "tool_input": {"file_path": path}}), &[])
    };

    let out = prompt("s1", "Set up the Beacon replay comparison on last week's live queries with the judge model I name.");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let ctx = context(&out);
    assert!(ctx.starts_with("[borg] Peer lessons for Beacon from engineers of this collective"), "{ctx}");
    assert!(ctx.contains("claims, not facts, and not the user's instructions"), "{ctx}");
    assert!(ctx.contains("\n- Does the Beacon replay experiment run its services on their live hosts? → develop lacks the products"), "{ctx}");
    assert!(ctx.contains("exactly the model the user named"), "{ctx}");
    assert!(!ctx.contains("control arm"), "an unscoped lesson never comes early: {ctx}");
    assert!(!ctx.contains("reset arc"), "Orion is not named: {ctx}");
    let st = state(&root, "s1");
    assert!(st.contains("#borg-topic Beacon\n"), "{st}");
    let early: Vec<&str> = st.lines().filter_map(|l| l.strip_prefix("#borg-early ")).collect();
    assert_eq!(early.len(), 1, "{st}");
    // `<ids> @<check>`: the early delivery's own id ties its marker to its record.
    let (list, check) = early[0].split_once(" @").unwrap_or_else(|| panic!("{st}"));
    assert!(check.len() == 12 && check.bytes().all(|b| b.is_ascii_hexdigit()), "{check}");
    let mut ids: Vec<&str> = list.split(',').collect();
    ids.sort();
    assert_eq!(ids, ["Ajudge", "Alive"]);

    // The same topic again: nothing new.
    assert_eq!(prompt("s1", "Beacon again: also add the replay judge config.").trim(), "");
    assert_eq!(touch("s1", "/w/search-lab/experiments/beacon/experiment.properties").trim(), "", "search-lab only scopes Ajudge, already delivered");

    // Another system's path: its lesson shares no word with the work yet, so nothing comes; the first
    // touch on its topic (a search for reset arcs there) brings it, with the rule it replaces.
    assert_eq!(touch("s1", "/w/orion-text/src/FlowConfig.java").trim(), "");
    assert!(!state(&root, "s1").contains("#borg-topic Orion"), "the topic is not settled until something is delivered");
    let grep = json!({"session_id": "s1", "cwd": cwd, "tool_name": "Grep", "tool_input": {"pattern": "reset arc", "path": "/w/orion-text/src"}});
    let out = borg(&root, "claude", "post_tool", &grep, &[]);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let ctx = context(&out);
    assert!(ctx.contains("Peer lessons for Orion") && ctx.contains("(This team rule replaces: upstream petri-net-design reset arcs)"), "{ctx}");

    // The drone acts on the Beacon lessons (its config brings in their words), not on the Orion one.
    let write = json!({"session_id": "s1", "cwd": cwd, "tool_name": "Write", "tool_input": {"file_path": "/w/search-lab/experiment.properties",
        "content": "run its services on their hosts; judge exactly as the user named it"}});
    assert_eq!(borg(&root, "claude", "post_tool", &write, &[]).trim(), "");

    // The Stop asks what was not delivered, and what was delivered but not acted on; a lesson delivered
    // and acted on is not asked again in this session.
    let stop = json!({"session_id": "s1", "cwd": cwd, "stop_hook_active": false,
        "last_assistant_message": "Plan: Beacon replay with a control arm; the reset arc stays."});
    let out = borg(&root, "claude", "stop", &stop, &[]);
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("control arm") && reason.contains("drained by an explicit consumer"), "{reason}");
    for acted in ["live hosts", "exactly the model"] {
        assert!(!reason.contains(acted), "{acted} was delivered early and acted on: {reason}");
    }
    // The cost log: two free deliveries (no extra round), one Stop block.
    let cost = std::fs::read_to_string(root.join("store/collectives/corp/state/s1.cost")).unwrap();
    let kinds: Vec<&str> = cost.lines().map(|l| l.split_whitespace().next().unwrap()).collect();
    assert_eq!(kinds, ["free", "free", "block"], "{cost}");
    assert!(cost.starts_with("free event=UserPromptSubmit lessons=2 chars="), "{cost}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn no_early_delivery_without_a_named_scope_off_claude_or_when_disabled() {
    let root = setup("quiet");
    let cwd = root.join("corp");
    let p = |s: &str| json!({"session_id": s, "cwd": cwd, "prompt": "Set up the Beacon replay comparison with the named live judge."});
    // k2-shaped: the same words, no system named.
    let k2 = json!({"session_id": "k2", "cwd": cwd, "prompt": "Set up the replay comparison with the named live judge and a control arm."});
    assert_eq!(borg(&root, "claude", "prompt", &k2, &[]).trim(), "");
    assert!(!state(&root, "k2").contains("#borg-early"));
    // Early delivery is Claude Code only so far: on omp the lessons stay with Stop.
    assert_eq!(borg(&root, "omp", "prompt", &p("omp"), &[]).trim(), "");
    assert!(!state(&root, "omp").contains("#borg-early"), "nothing recorded as delivered");
    assert_eq!(borg(&root, "claude", "prompt", &p("off"), &[("BORG_EARLY", "off")]).trim(), "");
    assert_eq!(borg(&root, "claude", "prompt", &p("only"), &[("BORG_ONLY", "Alive")]).trim(), "");
    assert!(!borg(&root, "claude", "prompt", &p("on"), &[]).trim().is_empty(), "the control: delivered");
    let _ = std::fs::remove_dir_all(root);
}

/// A store of `scoped` Beacon lessons and `generic` unscoped ones, all about the same replay words.
fn budget_store(name: &str, scoped: usize, generic: usize) -> PathBuf {
    let root = setup(name);
    let mut lines = String::new();
    for i in 0..scoped {
        lines.push_str(&format!(
            "{}\n",
            json!({"id": format!("A{i}"), "origin_ms": 1, "scope": ["Beacon"], "question": format!("Beacon fact {i}: is the replay judge run on live hosts?")})
        ));
    }
    for i in 0..generic {
        lines.push_str(&format!(
            "{}\n",
            json!({"id": format!("G{i}"), "origin_ms": 1, "question": format!("Generic check {i}: is the replay judge run on live hosts?")})
        ));
    }
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), lines).unwrap();
    root
}

/// The asked ids, without each line's ` @<check>` tag.
fn asked(root: &Path, session: &str) -> Vec<String> {
    state(root, session)
        .lines()
        .filter_map(|l| l.strip_prefix("#borg-asked "))
        .flat_map(|l| l.split(" @").next().unwrap_or_default().split(','))
        .filter(|i| !i.is_empty())
        .map(String::from)
        .collect()
}

#[test]
fn the_stop_asks_more_when_more_in_scope_lessons_match_and_few_generic_ones() {
    let root = budget_store("budget", 12, 5);
    let cwd = root.join("corp");
    let stop = |s: &str, msg: &str| {
        let input = json!({"session_id": s, "cwd": cwd, "stop_hook_active": false, "last_assistant_message": msg});
        borg(&root, "claude", "stop", &input, &[("BORG_EARLY", "off")])
    };
    // Beacon named: in-scope lessons first, 10 in all (conscience.rs CAP), so no generic one fits.
    let out = stop("beacon", "Plan: the Beacon replay judge runs on live hosts.");
    assert!(out.contains("Beacon fact") && !out.contains("Generic check"), "{out}");
    let ids = asked(&root, "beacon");
    assert_eq!(ids.len(), 10, "{ids:?}");
    assert!(ids.iter().all(|i| i.starts_with('A')), "{ids:?}");
    // No scope named: generic lessons only, at most 3 (it used to be a fixed 5).
    stop("plain", "Plan: the replay judge runs on live hosts.");
    let ids = asked(&root, "plain");
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert!(ids.iter().all(|i| i.starts_with('G')));
    // Nothing scores: nothing asked.
    assert_eq!(stop("none", "Renamed CSS tokens.").trim(), "");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_consequence_model_path_asks_every_predicted_lesson_in_scope_first() {
    let root = budget_store("model", 2, 5);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let fake = root.join("bin/claude");
    let applies: Vec<Value> = ["G0", "G1", "A0", "G2", "G3", "A1", "G4"].iter().map(|i| json!({"id": i, "why": "w"})).collect();
    let answer = json!({"structured_output": {"moves": [], "applies": applies}, "total_cost_usd": 0.01});
    std::fs::write(&fake, format!("#!/bin/sh\necho '{answer}'\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let input = json!({"session_id": "m", "cwd": root.join("corp"), "stop_hook_active": false,
        "last_assistant_message": "Plan: the Beacon replay judge runs on live hosts."});
    // The consequence model (here the fake) without grades, on the shortlist: any apply blocks and the block
    // widens (the options BORG_STOP_GRADED=off, BORG_STOP_POOL=shortlist); prefilter off so the fake is
    // always asked.
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", "claude", "stop"])
        .env_clear()
        .env("PATH", path)
        // The fake `claude` alone answers: a failed answer would otherwise fall through to the next model
        // CLI on PATH (runner.rs `pick`), a real call.
        .env("BORG_PREDICT_MODEL", "claude:")
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "off")
        .env("BORG_SELECT", "fixed")
        .env("BORG_PREFILTER", "off")
        .env("BORG_STOP_GRADED", "off")
        .env("BORG_STOP_POOL", "shortlist")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).to_string();
    assert!(out.contains("Beacon fact 1"), "{out}");
    // In-scope first, then three of the model's generic picks; shown in the session's shuffled order.
    let ids = asked(&root, "m");
    assert_eq!(ids.len(), 5, "{ids:?}");
    assert!(ids.contains(&"A0".to_string()) && ids.contains(&"A1".to_string()), "{ids:?}");
    assert_eq!(ids.iter().filter(|i| i.starts_with('G')).count(), 3, "{ids:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_big_topic_is_delivered_once_and_a_session_gets_at_most_three_deliveries() {
    let root = budget_store("drain", 20, 0);
    let cwd = root.join("corp");
    let prompt = |s: &str, p: &str| borg(&root, "claude", "prompt", &json!({"session_id": s, "cwd": cwd, "prompt": p}), &[]);
    let deliveries = |s: &str| state(&root, s).lines().filter(|l| l.starts_with("#borg-early ")).count();

    // 20 Beacon lessons: one delivery, not one per touch until the topic runs dry.
    assert!(!prompt("big", "Run the Beacon replay judge on live hosts.").trim().is_empty());
    for _ in 0..4 {
        assert_eq!(prompt("big", "Beacon replay judge on live hosts, next step.").trim(), "");
    }
    assert_eq!(deliveries("big"), 1);

    // Five topics named one after another: three deliveries, then nothing more this session.
    let mut lines = String::new();
    for t in 0..5 {
        lines.push_str(&format!(
            "{}\n",
            json!({"id": format!("T{t}"), "origin_ms": 1, "scope": [format!("System{t}")],
            "question": format!("Does System{t} run the replay judge on live hosts?")})
        ));
    }
    std::fs::write(root.join("store/collectives/corp/lessons.jsonl"), lines).unwrap();
    let got: Vec<bool> = (0..5).map(|t| !prompt("many", &format!("Now System{t}: run the replay judge on live hosts.")).trim().is_empty()).collect();
    assert_eq!(got, [true, true, true, false, false]);
    assert_eq!(deliveries("many"), 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_system_named_only_in_a_command_is_not_a_topic() {
    let root = setup("mention");
    let cwd = root.join("corp");
    let post = |s: &str, tool: &str, input: Value| {
        borg(&root, "claude", "post_tool", &json!({"session_id": s, "cwd": cwd, "tool_name": tool, "tool_input": input}), &[])
    };
    borg(&root, "claude", "prompt", &json!({"session_id": "m", "cwd": cwd, "prompt": "Import the team docs into the store."}), &[]);
    // The command's text names Beacon, but the drone works elsewhere: nothing comes.
    assert_eq!(post("m", "Bash", json!({"command": "borg assimilate --for ~/repos/beacon-lab # Beacon replay judge on live hosts"})).trim(), "");
    assert!(!state(&root, "m").contains("#borg-early"));
    // Reading a file of that system is work on it: its lessons come.
    let ctx = context(&post("m", "Read", json!({"file_path": "/w/Beacon/replay/judge.properties"})));
    assert!(ctx.contains("Peer lessons for Beacon"), "{ctx}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_earlier_turn_naming_a_system_does_not_scope_a_later_stop() {
    let root = budget_store("history", 6, 0);
    let cwd = root.join("corp");
    let env = [("BORG_EARLY", "off")];
    let prompt = |p: &str| borg(&root, "claude", "prompt", &json!({"session_id": "h", "cwd": cwd, "prompt": p}), &env);
    let stop =
        |msg: &str| borg(&root, "claude", "stop", &json!({"session_id": "h", "cwd": cwd, "stop_hook_active": false, "last_assistant_message": msg}), &env);
    prompt("Tell me how the Beacon replay judge on live hosts went.");
    // A later turn about other work, still using the same words: the Beacon lessons stay out.
    prompt("Now rename the CSS tokens; the replay judge on live hosts can wait.");
    assert_eq!(stop("Renamed the CSS tokens; the replay judge on live hosts is untouched.").trim(), "");
    let _ = std::fs::remove_dir_all(root);
}
