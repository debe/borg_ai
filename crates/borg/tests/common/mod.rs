//! Shared by the CLI tests that drive the hooks against a fake model: a store, a collectives config and
//! a fake `claude` on PATH that saves each prompt and predicts `Mdrain` at grade 3 (or answers with
//! `answer.json` when a test writes one). No test makes a real model call.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// A Orion repo (`harbor_orion`, system orion), a store with: a Orion-scoped lesson, an Beacon-scoped
/// one (another system), one scoped to a name the work never mentions, an old lesson superseded by a
/// newer one, and six generic ones.
pub fn setup(name: &str) -> PathBuf {
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
            "#!/bin/sh\nn=$(( $(ls {d} | wc -l) ))\nprintf '%s' \"$2\" > {d}/$n.txt\nif [ -f {a} ]; then cat {a}; else echo '{{\"structured_output\":{{\"moves\":[],\"applies\":[{{\"id\":\"Mdrain\",\"why\":\"w\",\"grade\":3}}]}},\"total_cost_usd\":0.05}}'; fi\n",
            d = root.join("prompts").display(),
            a = root.join("answer.json").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

pub fn borg(root: &Path, event: &str, input: &Value, env: &[(&str, &str)]) -> String {
    borg_as(root, "claude", event, input, env)
}

/// `borg` as the hook of `harness` (claude, omp, opencode). Off Claude Code, pass
/// `BORG_PREDICT_MODEL=claude:…`: the model call would otherwise go through that harness's real CLI.
pub fn borg_as(root: &Path, harness: &str, event: &str, input: &Value, env: &[(&str, &str)]) -> String {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", harness, event])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .env("BORG_LEARN", "off")
        .env("BORG_VECTORS", "off")
        // The fake `claude` alone answers: an answer it fails would otherwise fall through to the next
        // model CLI on PATH (runner.rs `pick`), a real call. Off Claude Code a test passes its own.
        .env("BORG_PREDICT_MODEL", "claude:")
        // These tests pin delivery, the fixed rule's (select.rs `Policy::Fixed`, not the default), so a
        // BORG_SELECT of the shell running them changes nothing; select_cli.rs and attention_cli.rs pass
        // their own.
        .env("BORG_SELECT", "fixed")
        .env("BORG_STOP_ATTENTION", "off")
        // These tests pin the turn-end behaviour; the live ones pass BORG_MODE=live.
        .env("BORG_MODE", "hook")
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
pub fn turn(root: &Path, session: &str, prompt: &str, tools: &[(&str, Value)], last: &str, env: &[(&str, &str)]) -> String {
    turn_as(root, "claude", session, prompt, tools, last, env)
}

/// `turn` through the hooks of `harness` (see `borg_as`).
pub fn turn_as(root: &Path, harness: &str, session: &str, prompt: &str, tools: &[(&str, Value)], last: &str, env: &[(&str, &str)]) -> String {
    let cwd = root.join("harbor_orion");
    borg_as(root, harness, "prompt", &json!({"session_id": session, "cwd": cwd, "prompt": prompt}), env);
    for (tool, input) in tools {
        borg_as(root, harness, "post_tool", &json!({"session_id": session, "cwd": cwd, "tool_name": tool, "tool_input": input}), env);
    }
    borg_as(root, harness, "stop", &json!({"session_id": session, "cwd": cwd, "last_assistant_message": last, "stop_hook_active": false}), env)
}

/// The prompts the fake model received, in order.
pub fn prompts(root: &Path) -> Vec<String> {
    let dir = root.join("prompts");
    let n = std::fs::read_dir(&dir).unwrap().count();
    (0..n).map(|i| std::fs::read_to_string(dir.join(format!("{i}.txt"))).unwrap()).collect()
}

pub fn state(root: &Path, session: &str, ext: &str) -> String {
    std::fs::read_to_string(root.join(format!("store/collectives/corp/state/{session}.{ext}"))).unwrap_or_default()
}

/// The fake model's next answers: `applies` as (id, grade) pairs.
pub fn answer(root: &Path, applies: &[(&str, u8)]) {
    let a: Vec<Value> = applies.iter().map(|(id, g)| json!({"id": id, "why": "w", "grade": g})).collect();
    let out = json!({"structured_output": {"moves": [], "applies": a}, "total_cost_usd": 0.05});
    std::fs::write(root.join("answer.json"), out.to_string()).unwrap();
}

pub fn context(out: &str) -> String {
    serde_json::from_str::<Value>(out).ok().and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)).unwrap_or_default()
}
