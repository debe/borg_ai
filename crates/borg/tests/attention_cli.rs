//! How many lessons a graded check asks is a cut on the model's rank set from outcomes (attention.rs),
//! not a fixed rule: the prior cut without outcomes, the conformal cut once checks are labelled.
//! No model call is real (the fake `claude` of common/mod.rs).

mod common;

use common::*;
use serde_json::json;

const PROMPT: &str = "Tidy the budget handling of the net";
const LAST: &str = "Retries use a fixed delay; the budget place is reset.";

fn env(attention: &'static str) -> [(&'static str, &'static str); 4] {
    [("BORG_STOP_POOL", "all"), ("BORG_PREFILTER", "off"), ("BORG_EARLY", "off"), ("BORG_STOP_ATTENTION", attention)]
}

fn asked(root: &std::path::Path, session: &str) -> Vec<String> {
    let ctx = state(root, session, "ctx");
    ctx.lines().rev().find_map(|l| l.strip_prefix("#borg-asked ")).unwrap_or_else(|| panic!("{ctx}")).split(',').map(String::from).collect()
}

fn attention(root: &std::path::Path, session: &str) -> Option<String> {
    state(root, session, "ctx").lines().find_map(|l| l.strip_prefix("#borg-attention ").map(String::from))
}

/// Twelve lessons the store knows; the fake model grades the first 3 and the rest 1.
fn twelve(root: &std::path::Path) -> Vec<String> {
    let lessons = root.join("store/collectives/corp/lessons.jsonl");
    let mut more = std::fs::read_to_string(&lessons).unwrap();
    for i in 0..12 {
        more += &format!(
            "{}\n",
            json!({"id": format!("K{i:02}"), "origin_ms": 100 + i, "question": format!("Is the K{i} invariant kept?"), "consequence": "k breaks"})
        );
    }
    std::fs::write(&lessons, more).unwrap();
    (0..12).map(|i| format!("K{i:02}")).collect()
}

fn answer_ranked(root: &std::path::Path, ids: &[String]) {
    let a: Vec<(&str, u8)> = ids.iter().enumerate().map(|(i, id)| (id.as_str(), if i == 0 { 3 } else { 1 })).collect();
    answer(root, &a);
}

#[test]
fn without_outcomes_the_prior_cut_asks_ranks_zero_to_ten() {
    let root = setup("attention-cold");
    let _ = std::fs::remove_file(root.join("store/collectives/corp/outcomes.jsonl"));
    let ids = twelve(&root);
    answer_ranked(&root, &ids);
    let out = turn(&root, "c1", PROMPT, &[], LAST, &env("on"));
    assert!(out.contains("block"), "{out}");
    let mut got = asked(&root, "c1");
    got.sort();
    assert_eq!(got, ids[..11], "{}", state(&root, "c1", "ctx"));
    let line = attention(&root, "c1").unwrap();
    assert!(line.starts_with("k=11 n=0 ") && line.contains(" source=prior ") && line.ends_with(" applied=12"), "{line}");
    assert!(state(&root, "c1", "queued").contains("K11"), "the cut hands the rest over free");
    // The fixed rule asks the 3 and the 1s up to its cap of ten, and logs no cut.
    answer_ranked(&root, &ids);
    turn(&root, "f1", PROMPT, &[], LAST, &env("off"));
    let mut fixed = asked(&root, "f1");
    fixed.sort();
    assert_eq!(fixed, ids[..10]);
    assert_eq!(attention(&root, "f1"), None);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn checks_whose_needed_lesson_is_always_first_cut_to_rank_zero() {
    let root = setup("attention");
    // 30 explored checks grading Mdrain first, Mdrain changing the work afterwards. In 10 of them it is a
    // 1 before ten 3s, which the fixed rule leaves out: α = 10/30, and k = 1 never misses.
    let outcomes: String = (0..30)
        .flat_map(|i| {
            let s = format!("h{i}");
            let (lessons, grades): (Vec<String>, Vec<u8>) = match i < 10 {
                true => std::iter::once(("Mdrain".to_string(), 1)).chain((0..10).map(|j| (format!("T{j}"), 3))).unzip(),
                false => (vec!["Mdrain".into(), "New".into()], vec![3, 3]),
            };
            let ranks: Vec<usize> = (0..lessons.len()).collect();
            [
                json!({"kind": "graded", "session": s, "lessons": lessons, "grades": grades, "ranks": ranks, "applied": lessons.len(), "chosen": lessons, "explore": true, "ms": 1000 + i, "repo": "harbor_orion"}),
                json!({"kind": "asked_then_changed", "session": s, "lessons": ["Mdrain"], "ms": 2000 + i, "repo": "harbor_orion"}),
            ]
        })
        .map(|v| format!("{v}\n"))
        .collect();
    std::fs::write(root.join("store/collectives/corp/outcomes.jsonl"), outcomes).unwrap();
    answer(&root, &[("Mdrain", 3), ("New", 3)]);
    let out = turn(&root, "a1", PROMPT, &[], LAST, &env("on"));
    assert!(out.contains("block"), "{out}");
    assert_eq!(asked(&root, "a1"), ["Mdrain"], "{}", state(&root, "a1", "ctx"));
    let line = attention(&root, "a1").unwrap();
    assert!(line.starts_with("k=1 n=30 alpha=0.3333 source=crc "), "{line}");
    assert!(state(&root, "a1", "queued").contains("New"), "{}", state(&root, "a1", "queued"));
    // Off (the default), the fixed rule asks both 3s.
    answer(&root, &[("Mdrain", 3), ("New", 3)]);
    turn(&root, "a2", PROMPT, &[], LAST, &env("off"));
    let mut both = asked(&root, "a2");
    both.sort();
    assert_eq!(both, ["Mdrain", "New"]);
    assert_eq!(attention(&root, "a2"), None);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn every_tenth_check_explores_and_is_logged_as_such() {
    let root = setup("attention-explore");
    // Nine graded checks in the store, none labelled: the tenth asks every apply, up to the rail.
    let outcomes: String = (0..9)
        .map(|i| json!({"kind": "graded", "session": format!("e{i}"), "lessons": ["K00"], "grades": [3], "chosen": ["K00"], "ms": 1000 + i, "repo": "harbor_orion"}))
        .map(|v| format!("{v}\n"))
        .collect();
    std::fs::write(root.join("store/collectives/corp/outcomes.jsonl"), outcomes).unwrap();
    let ids = twelve(&root);
    let learn = [env("on").as_slice(), &[("BORG_LEARN", "on")]].concat();
    let graded = |root: &std::path::Path| -> serde_json::Value {
        let store = std::fs::read_to_string(root.join("store/collectives/corp/outcomes.jsonl")).unwrap();
        store.lines().rev().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|o| o["kind"] == "graded").unwrap()
    };
    answer_ranked(&root, &ids);
    turn(&root, "x1", PROMPT, &[], LAST, &learn);
    let mut got = asked(&root, "x1");
    got.sort();
    assert_eq!(got, ids, "{}", state(&root, "x1", "ctx"));
    let line = attention(&root, "x1").unwrap();
    assert!(line.starts_with("k=20 n=0 ") && line.contains(" source=explore "), "{line}");
    let g = graded(&root);
    assert_eq!((g["session"].as_str(), g["explore"].as_bool(), g["chosen"].as_array().map(Vec::len)), (Some("x1"), Some(true), Some(12)), "{g}");
    // The next check is not explored: the prior cut, and no flag on its record.
    answer_ranked(&root, &ids);
    turn(&root, "x2", PROMPT, &[], LAST, &learn);
    assert_eq!(asked(&root, "x2").len(), 11);
    let g = graded(&root);
    assert_eq!((g["session"].as_str(), g.get("explore")), (Some("x2"), None), "{g}");
    let _ = std::fs::remove_dir_all(root);
}
