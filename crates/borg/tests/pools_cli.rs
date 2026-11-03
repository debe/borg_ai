//! The pool's shadow log (0.7.7, pools.rs): every detached check writes one line per check to
//! `pools.jsonl` with each eligible lesson's role, exact chance to be shown and the candidate orders'
//! scores; none of it reaches the prompt. The model is the fake `claude` of tests/common; every test lets
//! its checks end on their own before it removes its store.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{borg, prompts, setup, state};
use serde_json::{Value, json};

/// The detached check, learning on (records and the log), no prefilter or early delivery.
const ENV: [(&str, &str); 4] = [("BORG_ASYNC", "on"), ("BORG_PREFILTER", "off"), ("BORG_EARLY", "off"), ("BORG_LEARN", "on")];

fn hook(root: &Path, session: &str, event: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut ev = json!({"session_id": session, "cwd": root.join("harbor_orion")});
    ev.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    borg(root, event, &ev, env)
}

fn corp(root: &Path) -> std::path::PathBuf {
    root.join("store/collectives/corp")
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Waits up to 20 s for `cond`; fails the test past it.
fn until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One turn with a detached check: prompt, an edit, the stop; then waits until the check wrote its
/// result and let go of its lock. The check's id.
fn turn(root: &Path, session: &str, prompt: &str, env: &[(&str, &str)]) -> String {
    hook(root, session, "prompt", json!({"prompt": prompt}), env);
    hook(root, session, "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": "/x/net.rs", "new_string": "reset arc"}}), env);
    hook(root, session, "stop", json!({"last_assistant_message": "I drop the budget token with a reset arc.", "stop_hook_active": false}), env);
    let check = state(root, session, "ctx").lines().rev().find_map(|l| l.strip_prefix("#borg-checking @")).unwrap().to_string();
    until("the check's result", || serde_json::from_str::<Value>(&state(root, session, "checked")).is_ok_and(|v| v["check"] == check.as_str()));
    let lock = corp(root).join(format!("state/{session}.checking.lock"));
    until("the check to end", || std::fs::OpenOptions::new().write(true).open(&lock).map_or(true, |f| f.try_lock().map(|()| f.unlock()).is_ok()));
    check
}

fn floats(v: &Value) -> Vec<Option<f64>> {
    v.as_array().unwrap().iter().map(Value::as_f64).collect()
}

/// A pool its budget cuts: the line names every eligible lesson once in fill order, the prefix the
/// record shows, the boundary draw and the tail sample past it as the cost line counts them, each row's
/// exact chance (1 in the prefix; pt, the record's, exact as k / n), the shortlist and scope flags even
/// for a pool the record never shortlisted, and the applies' grades; the record's `xtokens` holds what the
/// draws showed past the pool.
#[test]
fn a_detached_check_logs_its_pool_with_exact_chances() {
    let root = setup("pools-log");
    let env: Vec<(&str, &str)> = ENV.into_iter().chain([("BORG_STOP_POOL_TOKENS", "90")]).collect();
    let check = turn(&root, "p1", "Tidy the budget handling of the Orion net", &env);
    let g = lines(&corp(&root).join("outcomes.jsonl")).into_iter().find(|o| o["kind"] == "graded" && o["check"] == check.as_str()).expect("graded");
    let pools = lines(&corp(&root).join("pools.jsonl"));
    let [p] = &pools[..] else { panic!("one line per check: {pools:?}") };
    assert_eq!(
        (p["v"].as_u64(), p["check"].as_str(), p["session"].as_str(), p["idx"].as_u64(), p["failed"].as_bool()),
        (Some(1), Some(check.as_str()), Some("p1"), Some(0), Some(false)),
        "{p}"
    );
    let ids: Vec<&str> = p["ids"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    assert_eq!(ids.len() as u64, g["eligible"].as_u64().unwrap(), "every eligible lesson: {p}");
    assert_eq!(ids.iter().collect::<std::collections::HashSet<_>>().len(), ids.len(), "once each");
    for column in ["ver", "role", "pb", "pi", "bm25q", "bm25f", "cos", "w", "trust", "act", "q", "cue", "mv", "rrf", "flags", "grade"] {
        assert_eq!(p[column].as_array().map(Vec::len), Some(ids.len()), "{column} aligned with ids: {p}");
    }
    let role: Vec<u64> = p["role"].as_array().unwrap().iter().map(|r| r.as_u64().unwrap()).collect();
    let count = |r: u64| role.iter().filter(|x| **x == r).count();
    let prefix = p["prefix"].as_u64().unwrap() as usize;
    assert_eq!((prefix as u64, count(1)), (g["shown"].as_u64().unwrap(), prefix), "the record's pool is the prefix: {p}");
    assert!(role[..prefix].iter().all(|r| *r == 1) && role[prefix..].iter().all(|r| *r != 1), "in fill order: {role:?}");
    let tail: Vec<&str> = g["tail"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    assert!(!tail.is_empty() && count(3) == tail.len() && tail.iter().all(|t| role[ids.iter().position(|i| i == t).unwrap()] == 3), "{p}\n{g}");
    let cost = state(&root, "p1", "cost");
    assert!(cost.contains(&format!("tail={} boundary={}", tail.len(), count(2))), "{cost}");
    // pt exact: k of the n the prefix left.
    let (k, n, pt) = (p["k"].as_u64().unwrap(), p["n_rem"].as_u64().unwrap(), p["pt"].as_f64().unwrap());
    assert_eq!((k as usize, n as usize, pt), (tail.len(), ids.len() - prefix, g["pt"].as_f64().unwrap()), "{p}");
    assert_eq!(pt, k as f64 / n as f64);
    let (pi, pb) = (floats(&p["pi"]), floats(&p["pb"]));
    assert!(pi[..prefix].iter().all(|x| *x == Some(1.0)) && pb[..prefix].iter().all(Option::is_none), "{p}");
    assert!(pi[prefix..].iter().all(|x| x.is_some_and(|x| x >= (pt * 1e4).round() / 1e4 - 1e-9 && x <= 1.0)), "pt + (1 − pt)·π_b: {p}");
    assert_eq!((p["s"].as_f64(), p["budget"].as_u64()), (Some(0.15), Some(90)));
    // Every row is in scope; rrf only where the model was shown the lesson; no vectors in tests.
    let flags: Vec<u64> = p["flags"].as_array().unwrap().iter().map(|f| f.as_u64().unwrap()).collect();
    assert!(flags.iter().all(|f| f & 4 != 0), "{p}");
    assert!(flags.iter().zip(&ids).any(|(f, id)| f & 1 != 0 && *id == "Mdrain"), "the shortlist flag, though the record shortlisted nothing: {p}");
    let rrf = floats(&p["rrf"]);
    assert!(role.iter().zip(&rrf).all(|(r, x)| (*r == 0) == x.is_none()), "{p}");
    assert!(floats(&p["cos"]).iter().all(Option::is_none) && floats(&p["cue"]).iter().all(Option::is_none) && p["moves_from"].is_null());
    // The fake applies Mdrain at grade 3.
    let at = ids.iter().position(|i| *i == "Mdrain").unwrap();
    assert_eq!(p["grade"][at].as_u64(), Some(3), "{p}");
    assert!(p["bm25q"][at].as_f64().unwrap() > 0.0 && p["bm25f"][at].as_f64().unwrap() > 0.0, "{p}");
    // The draws' tokens, past the pool's.
    assert!(g["xtokens"].as_u64().unwrap() > 0, "{g}");
    let _ = std::fs::remove_dir_all(root);
}

/// The shadow scores never reach the prompt: with cues and an earlier check's moves the model reads the
/// very same prompts as without them, while the log scores them (`cue`, `mv`, `moves_from`).
#[test]
fn shadow_scores_never_reach_the_prompt() {
    let run = |with: bool| -> (Vec<String>, Vec<Value>) {
        let root = setup(if with { "pools-shadow-with" } else { "pools-shadow-without" });
        let moves: Vec<&str> = if with { vec!["drain a budget place with a consumer", "retry with backoff"] } else { vec![] };
        let said = json!({"structured_output": {"moves": moves, "applies": [{"id": "Mdrain", "why": "w", "grade": 3}]}, "total_cost_usd": 0.05});
        std::fs::write(root.join("answer.json"), said.to_string()).unwrap();
        if with {
            // Cues for every lesson of the store, keyed by its text (cues.rs `key`).
            let cues: String = lines(&corp(&root).join("lessons.jsonl"))
                .iter()
                .map(|l| {
                    let s = |k: &str| l[k].as_str().unwrap_or_default().to_string();
                    let sit: Vec<String> = l["situations"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
                    let key = borg::cues::key(&s("question"), &sit, &s("consequence"));
                    let text = format!("budget place consumer {}", s("id"));
                    format!("{}\n", json!({"id": s("id"), "version": 1, "key": key, "cues": [{"kind": "move", "text": text}], "cost_usd": 0.001, "ms": 1}))
                })
                .collect();
            std::fs::write(corp(&root).join("cues.jsonl"), cues).unwrap();
        }
        let first = turn(&root, "s1", "Tidy the budget handling of the Orion net", &ENV);
        turn(&root, "s1", "Now the retries of the Orion net", &ENV);
        let pools = lines(&corp(&root).join("pools.jsonl"));
        assert_eq!(pools.len(), 2, "{pools:?}");
        if with {
            assert_eq!(pools[1]["moves_from"].as_str(), Some(first.as_str()), "the latest earlier check's moves: {}", pools[1]);
            assert!(floats(&pools[1]["mv"]).iter().any(|x| x.is_some_and(|x| x > 0.0)), "{}", pools[1]);
            assert!(floats(&pools[0]["cue"]).iter().any(|x| x.is_some_and(|x| x > 0.0)), "{}", pools[0]);
        } else {
            assert!(pools.iter().all(|p| p["moves_from"].is_null() && floats(&p["cue"]).iter().all(Option::is_none)));
        }
        let seen = prompts(&root);
        let _ = std::fs::remove_dir_all(root);
        (seen, pools)
    };
    let (with, _) = run(true);
    let (without, _) = run(false);
    assert_eq!(with.len(), 2);
    assert_eq!(with, without, "byte-identical prompts");
}
