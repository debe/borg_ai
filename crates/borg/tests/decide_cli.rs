//! A learned decision is carried out only when the gate it spends can be debited for it, end to end
//! through the stop's check: on a store whose fit prefers asking a 2 to handing it over free and whose
//! gate has room, a stop with learning on deviates and records it; with learning off (no record at all), or with
//! `BORG_CONSCIENCE` at another store's lessons (its gate is folded from a log this hook never writes),
//! the same check takes the fixed rule and hands it over, and a record it does write says why. The fake `claude`
//! of tests/common: no real model call.

mod common;

use std::path::{Path, PathBuf};

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

/// A selection summary beside `lessons` whose fit is sure that asking beats handing over, with a gate
/// open wide: a learned stop asks its 2, which the fixed rule hands over free.
fn fitted(lessons: &Path) {
    let at = |name: &str| policy::NAMES.iter().position(|n| *n == name).unwrap();
    let mut beta = vec![0.0; policy::P];
    beta[at("intercept")] = -0.5;
    beta[at("ask")] = 3.0;
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

/// A prompt, an edit, then the turn's stop under the learned policy (`BORG_SELECT=learned`), with `env`
/// on top; the stop's answer.
fn learned_stop(root: &Path, session: &str, env: &[(&str, &str)]) -> String {
    let hooks = [&[("BORG_PREFILTER", "off"), ("BORG_EARLY", "off"), ("BORG_SELECT", "learned")][..], env].concat();
    let cwd = root.join("harbor_orion");
    borg(root, "prompt", &json!({"session_id": session, "cwd": cwd, "prompt": PROMPT}), &hooks);
    let edit = json!({"file_path": "/x/net.rs", "old_string": "a", "new_string": "drop the budget token with a reset arc"});
    borg(root, "post_tool", &json!({"session_id": session, "cwd": cwd, "tool_name": "Edit", "tool_input": edit}), &hooks);
    answer(root, &[("Mdrain", 2)]);
    let stop = json!({"session_id": session, "cwd": cwd, "last_assistant_message": "I drop the budget token with a reset arc.", "stop_hook_active": false});
    borg(root, "stop", &stop, &hooks)
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
    let out = learned_stop(&root, "r1", &[("BORG_LEARN", "on")]);
    assert!(out.contains("block") && out.contains("[Mdrain]"), "asked, not handed over: {out}");
    assert_eq!(selected(&root, "r1"), format!("policy=learned source={}", policy::EVIDENCE));
    let g = graded(&store(&root).join("outcomes.jsonl"));
    assert_eq!((g.len(), g[0]["chosen"].clone(), g[0]["reason"].clone()), (1, json!(["Mdrain"]), Value::Null), "{g:?}");
    assert_eq!(state(&root, "r1", "queued"), "", "nothing handed over");
    let _ = std::fs::remove_dir_all(&root);

    // Learning off: nothing is recorded, so nothing would debit the gate; the fixed rule hands the 2 over.
    let root = setup("decide-learn-off");
    fitted(&store(&root).join("lessons.jsonl"));
    let out = learned_stop(&root, "o1", &[("BORG_LEARN", "off")]);
    assert_eq!(selected(&root, "o1"), "policy=fixed source=fixed");
    assert!(!out.contains("block"), "handed over: {out}");
    assert!(state(&root, "o1", "queued").starts_with("Mdrain @"), "handed over free: {}", state(&root, "o1", "queued"));
    assert!(graded(&store(&root).join("outcomes.jsonl")).is_empty());
    let _ = std::fs::remove_dir_all(&root);

    // Another store's lessons and gate (BORG_CONSCIENCE): the record lands in this store's log, never in
    // the one that gate is folded from; the fixed rule, and its record says why.
    let root = setup("decide-elsewhere");
    let copy = root.join("copy/lessons.jsonl");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(store(&root).join("lessons.jsonl"), &copy).unwrap();
    fitted(&copy);
    let out = learned_stop(&root, "e1", &[("BORG_LEARN", "on"), ("BORG_CONSCIENCE", copy.to_str().unwrap())]);
    assert_eq!(selected(&root, "e1"), "policy=fixed source=fixed");
    assert!(!out.contains("block"), "handed over: {out}");
    let g = graded(&store(&root).join("outcomes.jsonl"));
    assert_eq!((g.len(), g[0]["policy"].clone(), g[0]["reason"].clone()), (1, json!("fixed"), json!("unrecorded")), "{g:?}");
    assert!(graded(&copy.with_file_name("outcomes.jsonl")).is_empty(), "the other store's log is as it was");
    let _ = std::fs::remove_dir_all(&root);
}
