//! A learned decision is carried out only when the gate it spends can be debited for it, end to end
//! through `borg check`: on a store whose fit prefers handing a live 3 over free and whose gate has room,
//! a live check with learning on deviates and records it; with learning off (no record at all), or with
//! `BORG_CONSCIENCE` at another store's lessons (its gate is folded from a log this hook never writes),
//! the same check takes the fixed rule and asks, and a record it does write says why. The fake `claude`
//! of tests/common: no real model call. Each check runs to its end in the foreground.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use borg::select::{policy, summary};
use common::{answer, borg, setup, state};
use serde_json::{Value, json};

const PROMPT: &str = "Tidy the budget handling of the Orion net";

fn store(root: &Path) -> PathBuf {
    root.join("store/collectives/corp")
}

fn graded(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log).unwrap_or_default().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|o| o["kind"] == "graded").collect()
}

/// A selection summary beside `lessons` whose fit is sure that handing over beats asking, with a gate
/// open wide (hook.rs `decide_store`): a learned live check hands its 3 over free.
fn fitted(lessons: &Path) {
    let at = |name: &str| policy::NAMES.iter().position(|n| *n == name).unwrap();
    let mut beta = vec![0.0; policy::P];
    beta[at("intercept")] = -0.5;
    beta[at("ask")] = -3.0;
    beta[at("not")] = -1.0;
    let mut chol = vec![0.0; policy::P * policy::P];
    for k in 0..policy::P {
        chol[k * policy::P + k] = 0.05;
    }
    let model = policy::Model { beta, chol, sd: vec![1.0; policy::P], spread: [0.1; 3], ..policy::Model::default() };
    let rates = policy::Rates { free_stop: 1.0, free_live: 1.0, later: 0.0, wait: 0.0, ask_live: 1.0 };
    let fit = policy::Fit { key: "9-abcdef01".into(), model: Some(model), rates, ..policy::Fit::default() };
    let gate = summary::Gate { gain: vec![0.0; policy::DRAWS], allowance: 5.0, ..summary::Gate::default() };
    std::fs::write(lessons.with_file_name("outcomes.jsonl"), "").unwrap();
    summary::save(lessons, &summary::Summary { fit, gate, ..summary::Summary::default() }).unwrap();
}

/// A prompt, an edit, then one live check of the turn under the learned policy (`BORG_SELECT=learned`),
/// as the hook would spawn it, with `env` on top.
fn learned_live_check(root: &Path, session: &str, env: &[(&str, &str)]) {
    let hooks = [&[("BORG_PREFILTER", "off"), ("BORG_EARLY", "off")][..], env].concat();
    let cwd = root.join("harbor_orion");
    borg(root, "prompt", &json!({"session_id": session, "cwd": cwd, "prompt": PROMPT}), &hooks);
    let edit = json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "drop the budget token with a reset arc"});
    borg(root, "post_tool", &json!({"session_id": session, "cwd": cwd, "tool_name": "Edit", "tool_input": edit}), &hooks);
    let state = store(root).join("state");
    std::fs::write(state.join(format!("{session}.live.n")), "Edit\n").unwrap();
    answer(root, &[("Mdrain", 3)]);
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["check", "--session", session, "--cwd", cwd.to_str().unwrap()])
        .env_clear()
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", store(root))
        .env("BORG_STATE", &state)
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_HARNESS", "claude")
        .env("BORG_MODE", "live")
        .env("BORG_SELECT", "learned")
        .env("BORG_STOP_ATTENTION", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_PREFILTER", "off")
        .env("BORG_EARLY", "off")
        .env("BORG_PREDICT_MODEL", "claude:")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// The `#borg-select` line of the session's check: its policy and source.
fn selected(root: &Path, session: &str) -> String {
    let ctx = state(root, session, "ctx");
    let line = ctx.lines().find(|l| l.starts_with("#borg-select ")).unwrap_or_else(|| panic!("no decision: {ctx}"));
    line.split(' ').filter(|w| w.starts_with("policy=") || w.starts_with("source=")).collect::<Vec<_>>().join(" ")
}

#[test]
fn a_learned_check_whose_record_cannot_reach_its_gate_takes_the_fixed_rule() {
    // Learning on, the lessons in the store the hook records to: the learned policy deviates, and its
    // record is the gate's debit (not vacuous: the same check below would deviate too).
    let root = setup("decide-recorded");
    fitted(&store(&root).join("lessons.jsonl"));
    learned_live_check(&root, "r1", &[("BORG_LEARN", "on")]);
    assert_eq!(selected(&root, "r1"), format!("policy=learned source={}", policy::EVIDENCE));
    let g = graded(&store(&root).join("outcomes.jsonl"));
    assert_eq!((g.len(), g[0]["free"].clone(), g[0]["reason"].clone()), (1, json!(["Mdrain"]), Value::Null), "{g:?}");
    // In place of the question the fixed rule asks: the evidence rule's hand-over (hook.rs `INSTEAD_NEXT`).
    assert!(state(&root, "r1", "queued").starts_with("instead-next Mdrain @"), "handed over free: {}", state(&root, "r1", "queued"));
    let _ = std::fs::remove_dir_all(&root);

    // Learning off: nothing is recorded, so nothing would debit the gate; the fixed rule asks the 3.
    let root = setup("decide-learn-off");
    fitted(&store(&root).join("lessons.jsonl"));
    learned_live_check(&root, "o1", &[("BORG_LEARN", "off")]);
    assert_eq!(selected(&root, "o1"), "policy=fixed source=fixed");
    assert!(state(&root, "o1", "queued").starts_with("live Mdrain @"), "asked: {}", state(&root, "o1", "queued"));
    assert!(graded(&store(&root).join("outcomes.jsonl")).is_empty());
    let _ = std::fs::remove_dir_all(&root);

    // Another store's lessons and gate (BORG_CONSCIENCE): the record lands in this store's log, never in
    // the one that gate is folded from; the fixed rule, and its record says why.
    let root = setup("decide-elsewhere");
    let copy = root.join("copy/lessons.jsonl");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(store(&root).join("lessons.jsonl"), &copy).unwrap();
    fitted(&copy);
    learned_live_check(&root, "e1", &[("BORG_LEARN", "on"), ("BORG_CONSCIENCE", copy.to_str().unwrap())]);
    assert_eq!(selected(&root, "e1"), "policy=fixed source=fixed");
    let g = graded(&store(&root).join("outcomes.jsonl"));
    assert_eq!((g.len(), g[0]["policy"].clone(), g[0]["reason"].clone()), (1, json!("fixed"), json!("unrecorded")), "{g:?}");
    assert!(graded(&copy.with_file_name("outcomes.jsonl")).is_empty(), "the other store's log is as it was");
    let _ = std::fs::remove_dir_all(&root);
}
