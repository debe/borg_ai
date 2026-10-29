//! What a graded check decided and what delivery did with it, end to end through the hook and `borg
//! check`: the check's id on its `graded` record and on every marker it routes (`@<check>`), the record
//! of every check that called the model (one without applies and a failed one included), the free route's
//! propensity carried to the free delivery, a `delivery` record per check and lesson with the route it
//! actually took (asked, free, placebo, dropped and why), a live grade 3 on a lesson handed over earlier
//! asked and never dropped, and a placebo judged blindly that no belief, no statistic and no team line
//! reads. The fake `claude` of tests/common: no test makes a real model call. Every check here is run
//! to its end in the foreground (`borg check` is called, never spawned), so no test waits on a clock.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use common::{answer, borg, context, prompts, setup, state};
use serde_json::{Value, json};

/// The fake `claude` alone answers: a failed answer must never fall through to another model CLI on
/// PATH (runner.rs `pick`), so the runner is pinned.
const ENV: [(&str, &str); 4] = [("BORG_LEARN", "on"), ("BORG_PREFILTER", "off"), ("BORG_EARLY", "off"), ("BORG_PREDICT_MODEL", "claude:")];
const PROMPT: &str = "Tidy the budget handling of the Orion net";
const LAST: &str = "Retries use a fixed delay; the budget place is reset.";

fn store(root: &Path) -> std::path::PathBuf {
    root.join("store/collectives/corp")
}

fn outcomes(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(store(root).join("outcomes.jsonl")).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The records of `kind`, in log order.
fn of(root: &Path, kind: &str) -> Vec<Value> {
    outcomes(root).into_iter().filter(|o| o["kind"] == kind).collect()
}

fn lessons(o: &Value) -> Vec<String> {
    o["lessons"].as_array().map(|a| a.iter().map(|l| l.as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// The `delivery` records as (check, route, reason, lessons), in log order.
fn routes(root: &Path) -> Vec<(String, String, String, Vec<String>)> {
    let s = |o: &Value, k: &str| o[k].as_str().unwrap_or_default().to_string();
    of(root, "delivery").iter().map(|o| (s(o, "check"), s(o, "route"), s(o, "reason"), lessons(o))).collect()
}

fn is_check(c: &str) -> bool {
    c.len() == borg::select::CHECK_HEX && c.bytes().all(|b| b.is_ascii_hexdigit())
}

fn hook(root: &Path, session: &str, event: &str, extra: Value, env: &[(&str, &str)]) -> String {
    let mut input = json!({"session_id": session, "cwd": root.join("harbor_orion")});
    input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    borg(root, event, &input, &[&ENV[..], env].concat())
}

fn prompt(root: &Path, session: &str, text: &str, env: &[(&str, &str)]) -> String {
    hook(root, session, "prompt", json!({"prompt": text}), env)
}

fn edit(root: &Path, session: &str, file: &str, text: &str) -> String {
    hook(root, session, "post_tool", json!({"tool_name": "Edit", "tool_input": {"file_path": file, "old_string": "a", "new_string": text}}), &[])
}

fn stop(root: &Path, session: &str, last: &str, env: &[(&str, &str)]) -> String {
    hook(root, session, "stop", json!({"last_assistant_message": last, "stop_hook_active": false}), env)
}

/// One live check of `session`'s work so far, run to its end (live.rs): the change a tool call counted in
/// live mode, then `borg check` as the hook would spawn it, with the collective's store and state.
fn live_check(root: &Path, session: &str) {
    let state = store(root).join("state");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join(format!("{session}.live.n")))
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"Edit\n"))
        .unwrap();
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["check", "--session", session, "--cwd", root.join("harbor_orion").to_str().unwrap()])
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", store(root))
        .env("BORG_STATE", &state)
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_HARNESS", "claude")
        .env("BORG_MODE", "live")
        .env("BORG_SELECT", "fixed")
        .env("BORG_STOP_ATTENTION", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .envs(ENV)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// `borg debug memory` of the collective's lessons, as JSON lines.
fn memory(root: &Path) -> Vec<Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["debug", "memory", store(root).join("lessons.jsonl").to_str().unwrap()])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_SHARE_OUTCOMES", "off")
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The fake model's next answer: `applies` as (id, grade), `resolved` its verdicts on pending lessons.
fn verdicts(root: &Path, applies: &[(&str, u8)], resolved: Value) {
    let a: Vec<Value> = applies.iter().map(|(id, g)| json!({"id": id, "why": "w", "grade": g})).collect();
    let out = json!({"structured_output": {"moves": [], "applies": a, "resolved": resolved}, "total_cost_usd": 0.05});
    std::fs::write(root.join("answer.json"), out.to_string()).unwrap();
}

/// Adds lessons to the store: `ids`, each unscoped, its question `q(id)`.
fn add(root: &Path, ids: &[String], q: impl Fn(&str) -> String) {
    add_by(root, ids, q, "");
}

/// `add`, each lesson written by `engineer` (none when empty).
fn add_by(root: &Path, ids: &[String], q: impl Fn(&str) -> String, engineer: &str) {
    let path = store(root).join("lessons.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    for (i, id) in ids.iter().enumerate() {
        let mut l = json!({"id": id, "origin_ms": 100 + i, "question": q(id), "consequence": format!("the {id} invariant breaks")});
        if !engineer.is_empty() {
            l["engineers"] = json!([engineer]);
        }
        text.push_str(&format!("{l}\n"));
    }
    std::fs::write(path, text).unwrap();
}

/// `borg learn --session <session>` on the collective's store, as the session's end runs it: it settles
/// the session (outcome.rs `settle_with`). No model call: no episode waits, and the learn model is off.
fn learn(root: &Path, session: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["learn", "--session", session])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("BORG_HOME", store(root))
        .env("BORG_STATE", store(root).join("state"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "on")
        .env("BORG_LEARN_MODEL", "off")
        .env("BORG_REVIEW_FIX", "off")
        .env("BORG_VECTORS", "off")
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// The lesson ids of a prompt's lesson block (whole layout: between `## Lessons` and `## Current work`),
/// in the order shown.
fn shown_in(prompt: &str) -> Vec<String> {
    let start = prompt.find("## Lessons\n").expect("lesson block") + 11;
    let end = prompt.find("\n## Current work\n").expect("work block");
    prompt[start..end].lines().filter_map(|l| l.strip_prefix('[')?.split(']').next().map(String::from)).collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

#[test]
fn a_stop_block_logs_its_check_on_the_record_and_on_every_marker() {
    let root = setup("select-stop");
    prompt(&root, "s1", PROMPT, &[]);
    answer(&root, &[("Mdrain", 3)]);
    assert!(stop(&root, "s1", LAST, &[]).contains("block"));
    let graded = of(&root, "graded");
    assert_eq!(graded.len(), 1, "{:?}", outcomes(&root));
    let g = &graded[0];
    let check = g["check"].as_str().unwrap().to_string();
    assert!(is_check(&check), "{g}");
    assert_eq!(
        (g["policy"].as_str(), g["chosen"].clone(), g["p"].clone(), g["pf"].clone()),
        (Some("fixed"), json!(["Mdrain"]), json!([1.0]), json!([0.0])),
        "{g}"
    );
    // The pool and the call, so the check can be replayed.
    let n = |k: &str| g[k].as_u64().unwrap_or(0);
    assert!(n("eligible") >= n("shown") && n("shown") > 0 && n("tokens") > 0, "{g}");
    assert!(g["fill"].as_array().is_some_and(|f| f.len() == 1 && f[0].as_u64().unwrap() < n("shown")), "{g}");
    assert_eq!((g["harness"].as_str(), g["version"].as_str()), (Some("claude"), Some(env!("CARGO_PKG_VERSION"))), "{g}");
    assert!(g["prompt_hash"].as_str().is_some_and(is_check) && g.get("failed").is_none(), "{g}");
    // The state the decision read and what set its routes, so a replay decides from them (select/replay.rs P1).
    assert_eq!(g["source"].as_str(), Some("fixed"), "{g}");
    assert!(g["seen"].as_str().and_then(|k| k.split_once('-')).is_some_and(|(at, hash)| at.parse::<u64>().is_ok() && hash.len() <= 8), "{g}");
    // `#borg-graded` keeps its format; the decision's lines follow in the same write, tagged.
    let ctx = state(&root, "s1", "ctx");
    let at = ctx.find("#borg-graded n=1 Mdrain:3:0\n#borg-select policy=fixed source=fixed fit=").unwrap_or_else(|| panic!("{ctx}"));
    let lines: Vec<&str> = ctx[at..].lines().take(3).collect();
    assert!(lines[1].ends_with(&format!(" applied=1 @{check}")), "{ctx}");
    assert_eq!(lines[2], format!("#borg-p Mdrain=1 @{check}"), "{ctx}");
    assert!(ctx.contains(&format!("\n#borg-asked Mdrain @{check}\n")), "{ctx}");
    assert!(state(&root, "s1", "edits").contains(&format!("\n#borg-asked Mdrain @{check}\n")));
    // The ask names its check; delivery says what it did with the decision.
    let asked = of(&root, "asked");
    assert_eq!((asked[0]["channel"].as_str(), asked[0]["check"].as_str()), (Some("stop"), Some(check.as_str())), "{asked:?}");
    assert_eq!(routes(&root), [(check, "asked".to_string(), String::new(), vec!["Mdrain".to_string()])]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_check_without_a_three_hands_over_free_at_its_propensity_and_logs_what_did_not_fit() {
    let root = setup("select-free");
    let long: Vec<String> = (0..8).map(|i| format!("W{i}")).collect();
    add(&root, &long, |id| format!("Does the {id} rule hold for {}?", "every watched budget place and its consumer ".repeat(6)));
    prompt(&root, "f1", PROMPT, &[]);
    let applies: Vec<(&str, u8)> = long.iter().map(|id| (id.as_str(), 2)).collect();
    answer(&root, &applies);
    assert!(!stop(&root, "f1", LAST, &[]).contains("block"));
    let g = &of(&root, "graded")[0];
    let check = g["check"].as_str().unwrap().to_string();
    assert_eq!((g["free"].clone(), g["pf"].clone()), (json!(long), json!(vec![1.0; 8])), "{g}");
    assert_eq!(state(&root, "f1", "queued"), format!("{} @{check}\n", long.join(",")));
    assert!(state(&root, "f1", "ctx").contains(&format!("#borg-pf {} @{check}", long.iter().map(|id| format!("{id}=1")).collect::<Vec<_>>().join(","))));

    // The next prompt hands over what one early delivery fits; the rest is dropped, and says so.
    let out = context(&prompt(&root, "f1", "next: the invoices", &[]));
    let shown: Vec<String> = long.iter().filter(|id| out.contains(&format!("Does the {id} rule"))).cloned().collect();
    assert!(!shown.is_empty() && shown.len() < long.len(), "{out}");
    assert_eq!(shown, long[..shown.len()], "in the check's order");
    let ctx = state(&root, "f1", "ctx");
    let marker = format!("#borg-queued {} @{check}", shown.join(","));
    assert!(ctx.contains(&format!("\n{marker}\n")), "{ctx}");
    // The judge's evidence before a free delivery is the work so far, as before a live question.
    let edits = state(&root, "f1", "edits");
    let at = edits.find(&marker).unwrap_or_else(|| panic!("{edits}"));
    assert!(edits[..at].lines().last().is_some_and(|l| l.starts_with("#borg-work ")), "{edits}");
    let asked = &of(&root, "asked")[0];
    assert_eq!((asked["channel"].as_str(), asked["check"].as_str(), asked["p"].clone()), (Some("queued"), Some(check.as_str()), json!(vec![1.0; shown.len()])));
    let rest: Vec<String> = long[shown.len()..].to_vec();
    assert_eq!(routes(&root), [(check.clone(), "free".into(), String::new(), shown), (check, "dropped_chars".into(), "budget".into(), rest)]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_block_under_the_cut_hands_the_rest_over_free_at_the_cuts_propensity() {
    let root = setup("select-cut");
    let ids: Vec<String> = (0..12).map(|i| format!("K{i:02}")).collect();
    add(&root, &ids, |id| format!("Is the {id} invariant kept?"));
    let applies: Vec<(&str, u8)> = ids.iter().enumerate().map(|(i, id)| (id.as_str(), if i == 0 { 3 } else { 1 })).collect();
    answer(&root, &applies);
    // Attention wins over the default policy (BORG_SELECT any value but fixed, select.rs `precedence`).
    let cut = [("BORG_SELECT", "learned"), ("BORG_STOP_ATTENTION", "on"), ("BORG_STOP_POOL", "all")];
    prompt(&root, "c1", PROMPT, &cut);
    assert!(stop(&root, "c1", LAST, &cut).contains("block"));
    let g = &of(&root, "graded")[0];
    let check = g["check"].as_str().unwrap().to_string();
    assert_eq!((g["policy"].as_str(), g["free"].clone()), (Some("attention"), json!(["K11"])), "{g}");
    // The prior cut asks ranks 0 to 10; rank 11 goes free unless the check explores (1 in 10).
    assert_eq!(g["pf"][11], json!(0.9), "{g}");
    let ctx = state(&root, "c1", "ctx");
    assert!(ctx.contains("#borg-attention k=11 ") && ctx.contains("#borg-select policy=attention source=prior "), "{ctx}");
    assert!(ctx.contains(&format!("#borg-pf K11=0.9 @{check}")), "{ctx}");
    prompt(&root, "c1", "next: the invoices", &cut);
    let queued = of(&root, "asked").into_iter().find(|o| o["channel"] == "queued").unwrap();
    assert_eq!((lessons(&queued), queued["p"].clone(), queued["check"].as_str()), (vec!["K11".to_string()], json!([0.9]), Some(check.as_str())), "{queued}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_live_check_logs_its_check_and_its_ask_arrives_tagged_with_it() {
    let root = setup("select-live");
    prompt(&root, "l1", PROMPT, &[]);
    edit(&root, "l1", "/x/net.rs", "drop the budget token with a reset arc");
    answer(&root, &[("Mdrain", 3)]);
    live_check(&root, "l1");
    let g = &of(&root, "graded")[0];
    let check = g["check"].as_str().unwrap().to_string();
    assert_eq!((g["channel"].as_str(), g["chosen"].clone()), (Some("live"), json!(["Mdrain"])), "{g}");
    assert_eq!(state(&root, "l1", "queued"), format!("live Mdrain @{check}\n"));
    assert!(!state(&root, "l1", "ctx").contains("#borg-asked Mdrain"), "asked at delivery, not before");
    // The next tool call delivers it, tagged with the check that asked.
    let out = context(&edit(&root, "l1", "/x/other.rs", "unrelated"));
    assert!(out.contains("explicit consumer"), "{out}");
    let ctx = state(&root, "l1", "ctx");
    assert!(ctx.contains(&format!("\n#borg-asked Mdrain @{check}\n#borg-live Mdrain ms=")), "{ctx}");
    let asked = &of(&root, "asked")[0];
    assert_eq!((asked["channel"].as_str(), asked["check"].as_str(), asked["p"].clone()), (Some("live"), Some(check.as_str()), json!([1.0])), "{asked}");
    assert_eq!(routes(&root), [(check, "asked".to_string(), String::new(), vec!["Mdrain".to_string()])]);
    let _ = std::fs::remove_dir_all(root);
}

/// The bug this slice fixes: a live check may grade a lesson handed over earlier and not acted on (its
/// pool leaves only done lessons out), and delivery dropped any lesson ever handed over, so a grade 3
/// was logged chosen with p 1 and never reached the drone. One definition of done now serves both.
#[test]
fn a_live_three_on_a_lesson_handed_over_earlier_is_asked_never_dropped() {
    let root = setup("select-bug");
    prompt(&root, "b1", PROMPT, &[]);
    // A 2 at the stop: handed over free at the next prompt.
    answer(&root, &[("Mdrain", 2)]);
    assert!(!stop(&root, "b1", LAST, &[]).contains("block"));
    assert!(context(&prompt(&root, "b1", "next: the invoices", &[])).contains("explicit consumer"));
    assert!(state(&root, "b1", "ctx").contains("\n#borg-queued Mdrain @"));
    // The drone does not act on it; a live check grades it 3.
    edit(&root, "b1", "/x/invoices.md", "fix a typo in the heading");
    answer(&root, &[("Mdrain", 3)]);
    live_check(&root, "b1");
    let live = of(&root, "graded").into_iter().find(|g| g["channel"] == "live").unwrap();
    let check = live["check"].as_str().unwrap().to_string();
    assert_eq!(live["chosen"], json!(["Mdrain"]), "{live}");
    // Delivered as the question it was decided to be.
    let out = context(&edit(&root, "b1", "/x/invoices.md", "the next heading"));
    assert!(out.contains("explicit consumer"), "the question reaches the drone: {out}");
    assert!(state(&root, "b1", "ctx").contains(&format!("\n#borg-asked Mdrain @{check}\n")), "{}", state(&root, "b1", "ctx"));
    let asked: Vec<String> = of(&root, "asked").iter().map(|o| o["channel"].as_str().unwrap().to_string()).collect();
    assert_eq!(asked, ["queued", "live"]);
    let ours: Vec<_> = routes(&root).into_iter().filter(|r| r.0 == check).collect();
    assert_eq!(ours, [(check, "asked".to_string(), String::new(), vec!["Mdrain".to_string()])], "never dropped_done");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_check_without_applies_and_a_failed_check_are_logged_and_a_failed_judge_leaves_its_lessons_pending() {
    let root = setup("select-zero");
    prompt(&root, "z1", PROMPT, &[]);
    answer(&root, &[]);
    assert!(!stop(&root, "z1", LAST, &[]).contains("block"));
    let g = &of(&root, "graded")[0];
    assert!(is_check(g["check"].as_str().unwrap()) && lessons(g).is_empty() && g.get("failed").is_none(), "a check without applies: {g}");
    assert!(g["shown"].as_u64().unwrap() > 0, "its pool is logged: {g}");

    // A block, its answer, then a call that fails: nothing asked (fail open), the question stays pending.
    prompt(&root, "z1", PROMPT, &[]);
    answer(&root, &[("Mdrain", 3)]);
    assert!(stop(&root, "z1", LAST, &[]).contains("block"));
    hook(&root, "z1", "stop", json!({"last_assistant_message": "Mdrain: applies.", "stop_hook_active": true}), &[]);
    std::fs::write(root.join("answer.json"), "no answer").unwrap();
    assert!(!stop(&root, "z1", "Done.", &[]).contains("block"));
    let failed = of(&root, "graded").into_iter().last().unwrap();
    assert!(failed["failed"] == true && lessons(&failed).is_empty(), "{failed}");
    let ctx = state(&root, "z1", "ctx");
    assert!(ctx.contains("\n#borg-unjudged Mdrain why=failed\n") && !ctx.contains("#borg-judged Mdrain"), "no try counted: {ctx}");
    // The next call judges it.
    verdicts(&root, &[], json!([{"id": "Mdrain", "verdict": "changed", "avoids_consequence": true}]));
    stop(&root, "z1", "Done again.", &[]);
    assert_eq!(of(&root, "asked_then_changed").len(), 1, "{:?}", outcomes(&root));
    let _ = std::fs::remove_dir_all(root);
}

/// A placebo: an apply the check routed nowhere, judged blindly where a free delivery would be. Its
/// "changed" is the judge's floor; no belief, no statistic and no team line counts it.
#[test]
fn a_placebo_is_judged_blind_and_counts_for_nothing() {
    let root = setup("select-placebo");
    prompt(&root, "p1", PROMPT, &[]);
    // A check left G1 and G2 as its placebos (select.rs draws them among the applies routed nowhere).
    let check = "0123456789ab";
    std::fs::write(store(&root).join("state/p1.queued"), format!("placebo G1,G2 @{check}\n")).unwrap();
    let out = context(&prompt(&root, "p1", "next: the invoices", &[]));
    assert!(!out.contains("tomatoes") && !out.contains("invoices handling"), "the drone sees nothing of a placebo: {out}");
    let marker = format!("#borg-placebo G1,G2 @{check}");
    assert!(state(&root, "p1", "ctx").contains(&format!("\n{marker}\n")));
    let edits = state(&root, "p1", "edits");
    assert!(edits.contains(&format!("\n{marker}\n")) && edits[..edits.find(&marker).unwrap()].lines().last().unwrap().starts_with("#borg-work "), "{edits}");
    assert_eq!(routes(&root), [(check.to_string(), "placebo".to_string(), String::new(), vec!["G1".to_string(), "G2".to_string()])]);
    assert!(of(&root, "asked").is_empty(), "never asked: trust and regrets never see it; settle closes it on the placebo channel");

    // The next judge call sees each as a free delivery; its results are the placebo channel's.
    verdicts(
        &root,
        &[],
        json!([{"id": "G1", "verdict": "changed", "avoids_consequence": true}, {"id": "G2", "verdict": "unchanged", "avoids_consequence": false}]),
    );
    stop(&root, "p1", "Done with the invoices.", &[]);
    let judged = prompts(&root).last().unwrap().clone();
    assert!(judged.contains("[G1] lesson:") && judged.contains("[G2] lesson:") && !judged.contains("(declared:"), "{judged}");
    for kind in ["asked_then_changed", "asked_ignored"] {
        let r = of(&root, kind);
        assert_eq!(r.len(), 1, "{kind}: {:?}", outcomes(&root));
        assert_eq!((r[0]["channel"].as_str(), r[0]["check"].as_str(), r[0]["judge"].clone()), (Some("placebo"), Some(check), json!(2)), "{kind}");
    }
    // Excluded from beliefs (memory.rs), from the statistics' results, and from the team file.
    let mem = memory(&root);
    for id in ["G1", "G2"] {
        let m = mem.iter().find(|m| m["id"] == id).unwrap();
        assert_eq!((m["changed"].as_u64(), m["ignored"].as_u64()), (Some(0), Some(0)), "{m}");
    }
    let events = std::fs::read_to_string(store(&root).join("stats/events.jsonl")).unwrap_or_default();
    assert!(!events.contains("\"kind\":\"changed\"") && !events.contains("\"kind\":\"ignored\""), "{events}");
    assert_eq!(borg::outcome::publish(&store(&root)), 0, "nothing of it is shared");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_early_delivery_names_its_own_check_on_its_marker_and_record() {
    let root = setup("select-early");
    let out = prompt(&root, "e1", "Fix the Orion net: the budget place and its reset arc", &[("BORG_EARLY", "on")]);
    assert!(context(&out).contains("explicit consumer"), "{out}");
    let asked = &of(&root, "asked")[0];
    let check = asked["check"].as_str().unwrap().to_string();
    assert!(is_check(&check) && asked["channel"] == "early", "{asked}");
    let marker = format!("#borg-early Mdrain @{check}");
    assert!(state(&root, "e1", "ctx").contains(&format!("{marker}\n")));
    let edits = state(&root, "e1", "edits");
    assert!(edits.contains(&format!("#borg-work \n{marker}\n")), "the work so far (none yet) before it: {edits}");
    // Its score as a share of the best on-topic match, for a learned early rule: it is the best.
    assert_eq!(asked["rel"], json!([1.0]), "{asked}");
    let _ = std::fs::remove_dir_all(root);
}

/// A lesson the session had (handed over free earlier) is graded again: it may still be asked, but it
/// never takes a free slot again, and the check's record names the applies that could not go free
/// (`nofree`), so the record alone holds what its decision read of the session.
#[test]
fn a_lesson_the_session_had_never_goes_free_again_and_the_record_says_so() {
    let root = setup("select-nofree");
    prompt(&root, "n1", PROMPT, &[]);
    answer(&root, &[("Mdrain", 2)]);
    assert!(!stop(&root, "n1", LAST, &[]).contains("block"));
    assert!(context(&prompt(&root, "n1", "next: the invoices", &[])).contains("explicit consumer"));
    assert!(of(&root, "graded")[0].get("nofree").is_none(), "the first check had nothing to hold back");
    // The drone does not act on it; the next stop grades it again beside a lesson the session never had.
    answer(&root, &[("Mdrain", 2), ("G2", 1)]);
    assert!(!stop(&root, "n1", "Done with the invoices.", &[]).contains("block"));
    let g = of(&root, "graded").into_iter().last().unwrap();
    assert_eq!(lessons(&g), ["Mdrain", "G2"], "{g}");
    assert_eq!((g["nofree"].clone(), g["free"].clone()), (json!(["Mdrain"]), json!(["G2"])), "{g}");
    assert_eq!(state(&root, "n1", "queued"), format!("G2 @{}\n", g["check"].as_str().unwrap()), "only G2 waits to go free");
    let _ = std::fs::remove_dir_all(root);
}

/// The fixed rule's placebos (select.rs `placebo` under `Fixed`, the default): a check hands over its
/// first `CAP` 1s and judges the rest blindly. End to end: the stop's record names them, the next prompt
/// marks them and shows the drone nothing of them, the next call judges them where a free delivery
/// would be, and each verdict is recorded on the placebo channel with the check that drew it; `borg
/// learn` closes the one the judge never reached on that channel. No belief, no trust, no statistic
/// and no team line counts any of it, while a free delivery judged in the same call counts as ever.
#[test]
fn the_fixed_rules_placebos_make_the_whole_round_trip_and_count_for_nothing() {
    let root = setup("select-fixed-placebo");
    let free: Vec<String> = (0..10).map(|i| format!("W{i}")).collect();
    let placebos = vec!["P10".to_string(), "P11".to_string()];
    add_by(&root, &free, |id| format!("Does the {id} rule hold for {}?", "every watched budget place and its consumer ".repeat(6)), "eng-free");
    add_by(&root, &placebos, |id| format!("Is the {id} ledger balanced?"), "eng-placebo");
    prompt(&root, "f1", PROMPT, &[]);
    let applies: Vec<(&str, u8)> = free.iter().chain(&placebos).map(|id| (id.as_str(), 1)).collect();
    answer(&root, &applies);
    assert!(!stop(&root, "f1", LAST, &[]).contains("block"));
    let g = &of(&root, "graded")[0];
    let check = g["check"].as_str().unwrap().to_string();
    assert_eq!(
        (g["policy"].as_str(), g["free"].clone(), g["placebo"].clone(), g["psi"].clone()),
        (Some("fixed"), json!(free), json!(placebos), json!(1.0)),
        "{g}"
    );
    assert!(state(&root, "f1", "queued").contains(&format!("placebo P10,P11 @{check}\n")), "{}", state(&root, "f1", "queued"));

    // The next prompt hands over what fits and marks the placebos; the drone sees nothing of them.
    let out = context(&prompt(&root, "f1", "next: the invoices", &[]));
    assert!(!out.contains("ledger balanced"), "{out}");
    let shown: Vec<String> = free.iter().filter(|id| out.contains(&format!("Does the {id} rule"))).cloned().collect();
    assert!(!shown.is_empty(), "{out}");
    assert!(state(&root, "f1", "ctx").contains(&format!("\n#borg-placebo P10,P11 @{check}\n")), "{}", state(&root, "f1", "ctx"));
    let marked: Vec<_> = routes(&root).into_iter().filter(|r| r.1 == "placebo").collect();
    assert_eq!(marked, [(check.clone(), "placebo".to_string(), String::new(), placebos.clone())]);

    // The next call judges both blindly beside the free deliveries: P10 changed the work (the judge's
    // floor), P11 gets no verdict; of the free ones the first changed it.
    verdicts(
        &root,
        &[],
        json!([{"id": "P10", "verdict": "changed", "avoids_consequence": true}, {"id": shown[0], "verdict": "changed", "avoids_consequence": true}]),
    );
    stop(&root, "f1", "Done with the invoices.", &[]);
    let judged = prompts(&root).last().unwrap().clone();
    assert!(judged.contains("[P10] lesson:") && judged.contains("[P11] lesson:"), "{judged}");
    let on = |kind: &str, channel: &str| -> Vec<(Vec<String>, String)> {
        let on = |o: &&Value| o["channel"].as_str().unwrap_or_default() == channel;
        of(&root, kind).iter().filter(on).map(|o| (lessons(o), o["check"].as_str().unwrap_or_default().to_string())).collect()
    };
    assert_eq!(on("asked_then_changed", "placebo"), [(vec!["P10".to_string()], check.clone())]);
    assert_eq!(on("asked_then_changed", "queued"), [(vec![shown[0].clone()], check.clone())]);
    // The session's end closes the placebo never judged on the placebo channel, the free ones as ever.
    learn(&root, "f1");
    assert_eq!(on("asked_not_corrected", "placebo"), [(vec!["P11".to_string()], check.clone())]);
    assert!(on("asked_not_corrected", "").iter().any(|(l, _)| l.contains(&shown[0])), "{:?}", of(&root, "asked_not_corrected"));

    // Beliefs (memory.rs): the free lesson's change counts, the placebo's not.
    let mem = memory(&root);
    let m = |id: &str| mem.iter().find(|m| m["id"] == id).cloned().unwrap_or_else(|| panic!("{id}: {mem:?}"));
    assert_eq!(m(&shown[0])["changed"].as_u64(), Some(1), "{}", m(&shown[0]));
    for id in &placebos {
        assert_eq!((m(id)["changed"].as_u64(), m(id)["ignored"].as_u64()), (Some(0), Some(0)), "{}", m(id));
    }
    // Trust (trust.rs): the free lessons' author was followed, the placebos' author has no evidence.
    let trust = borg::trust::of_store_with(&store(&root), &borg::collective::Aliases::default());
    let of_author = |e: &str| trust.rows().filter(|(x, _, _)| *x == e).map(|(_, _, ev)| ev).fold((0, 0), |(f, c), ev| (f + ev.followed, c + ev.corrected));
    assert!(of_author("eng-free").0 > 0, "{:?}", trust.rows().collect::<Vec<_>>());
    assert_eq!(of_author("eng-placebo"), (0, 0), "{:?}", trust.rows().collect::<Vec<_>>());
    // The statistics' results: the free lesson's change, nothing of a placebo's.
    let events = std::fs::read_to_string(store(&root).join("stats/events.jsonl")).unwrap_or_default();
    let changed: Vec<Value> = events.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|e| e["kind"] == "changed").collect();
    assert!(changed.iter().any(|e| strings(&e["lessons"]).contains(&shown[0])), "{events}");
    assert!(changed.iter().all(|e| strings(&e["lessons"]).iter().all(|l| !placebos.contains(l))), "{events}");
    // The team file: the free lesson's results, never a placebo's.
    borg::outcome::publish(&store(&root));
    let team = std::fs::read_to_string(store(&root).join(borg::outcome::TEAM_FILE)).unwrap_or_default();
    let shared: Vec<Value> = team.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
    assert!(shared.iter().any(|x| x["kind"] == "asked_then_changed" && strings(&x["lessons"]) == [shown[0].clone()]), "{team}");
    assert!(shared.iter().all(|x| strings(&x["lessons"]).iter().all(|l| !placebos.contains(l))), "{team}");
    let _ = std::fs::remove_dir_all(root);
}

/// The tail sample (whole.rs `tail`), under every policy: a pool the budget cut shows the model a few of
/// the lessons it dropped, after the pool; the check's record names them (`tail`), each one's chance (k
/// of the n dropped, `pt`) and, for those that apply, their place past the budget's in `fill`, never a
/// place the budget showed. A tail lesson that applies then makes a free delivery's whole round trip:
/// queued, handed over at the next prompt, judged by the next call, its verdict on its check's route.
#[test]
fn a_pool_the_budget_cut_shows_a_tail_sample_and_logs_it() {
    let root = setup("select-tail");
    let eligible = ["Mdrain", "New", "G0", "G1", "G2", "G3", "G4", "G5"];
    for (session, policy) in [("t1", "fixed"), ("t2", "learned")] {
        // Room for about two lines of the eight eligible: six dropped, two of them sampled.
        let env = [("BORG_STOP_POOL_TOKENS", "60"), ("BORG_SELECT", policy)];
        prompt(&root, session, "Look at the telescope and glaciers code", &env);
        answer(&root, &eligible.map(|id| (id, 1)));
        stop(&root, session, "The telescope limits and the telescope edge case, plus glaciers.", &env);
        let g = of(&root, "graded").into_iter().find(|g| g["session"] == session).unwrap();
        let p = prompts(&root).last().unwrap().clone();
        let tail = strings(&g["tail"]);
        assert_eq!(shown_in(&p), [vec!["G3".to_string(), "G4".to_string()], tail.clone()].concat(), "the budget's pool, then the tail: {g}");
        assert!(tail.len() == 2 && tail.iter().all(|t| eligible.contains(&t.as_str()) && t != "G3" && t != "G4"), "{g}");
        assert_eq!((g["shown"].as_u64(), g["eligible"].as_u64(), g["pt"].as_f64()), (Some(2), Some(8), Some(2.0 / 6.0)), "{g}");
        // Every shown lesson applies: the budget's at their places, the tail's past them, apart.
        let fill: Vec<u64> = g["fill"].as_array().unwrap().iter().map(|f| f.as_u64().unwrap()).collect();
        let at = |id: &str| fill[lessons(&g).iter().position(|l| l == id).unwrap()];
        assert!(at("G3") < 2 && at("G4") < 2, "{g}");
        assert!(tail.iter().all(|t| (2..8).contains(&at(t))) && at(&tail[0]) != at(&tail[1]), "{g}");
        // After the applies' places, each tail lesson's, aligned with `tail` (outcome.rs `fill`).
        let n = lessons(&g).len();
        assert_eq!(fill[n..], tail.iter().map(|t| at(t)).collect::<Vec<_>>(), "{g}");
        assert!(state(&root, session, "cost").contains("shown=2 budget=full tail=2"), "{}", state(&root, session, "cost"));
    }
    // The selection summary folds each tail lesson into the pool's yield at its place (select/summary.rs
    // `tail_of`), as a hook loads it: two checks, each two of the pool and two of the tail, all applied.
    let v = select_summary(&root);
    let bins: Vec<(u64, u64)> = v["pool"]["bins"].as_array().unwrap().iter().map(|b| (b[0].as_u64().unwrap(), b[1].as_u64().unwrap())).collect();
    let past: (u64, u64) = bins.iter().skip(1).fold((0, 0), |s, b| (s.0 + b.0, s.1 + b.1));
    assert_eq!((bins[0], past), ((2, 2), (6, 6)), "place 0 the pool's; past it the pool's second and the tails' four: {bins:?}");

    // A tail lesson's round trip, as any free apply's: the fixed rule hands every 1 over free, the tail's
    // too; the next prompt delivers them tagged with their check, the next call judges one, and its
    // verdict lands on the queued channel with that check. The summary keeps each tail apply on its
    // route, delivered and labelled: the yield's `fill < shown` skips them in the pool's bins alone.
    let env = [("BORG_STOP_POOL_TOKENS", "60"), ("BORG_SELECT", "fixed")];
    let g = of(&root, "graded").into_iter().find(|g| g["session"] == "t1").unwrap();
    let (check, tail) = (g["check"].as_str().unwrap().to_string(), strings(&g["tail"]));
    assert!(tail.iter().all(|t| strings(&g["free"]).contains(t)), "{g}");
    let queued = state(&root, "t1", "queued");
    assert!(queued.lines().any(|l| l.ends_with(&format!(" @{check}")) && tail.iter().all(|t| l.split([' ', ',']).any(|x| x == t))), "{queued}");
    let out = context(&prompt(&root, "t1", "next: the invoices", &env));
    let ctx = state(&root, "t1", "ctx");
    let marker = ctx.lines().find(|l| l.starts_with("#borg-queued ") && l.ends_with(&format!(" @{check}"))).unwrap_or_else(|| panic!("{ctx}"));
    let handed: Vec<String> = marker["#borg-queued ".len()..].split(' ').next().unwrap().split(',').map(String::from).collect();
    assert!(tail.iter().all(|t| handed.contains(t)), "both tail lessons handed over: {marker}");
    let text = std::fs::read_to_string(store(&root).join("lessons.jsonl")).unwrap();
    let question =
        |id: &str| text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|l| l["id"] == id).unwrap()["question"].as_str().unwrap().to_string();
    assert!(tail.iter().all(|t| out.contains(&question(t))), "the drone sees them: {out}");
    let free: Vec<_> = routes(&root).into_iter().filter(|r| r.0 == check && r.1 == "free").collect();
    assert!(free.len() == 1 && tail.iter().all(|t| free[0].3.contains(t)), "{free:?}");
    let asked = of(&root, "asked").into_iter().find(|o| o["channel"] == "queued").unwrap();
    assert!(asked["check"] == check.as_str() && tail.iter().all(|t| lessons(&asked).contains(t)), "{asked}");
    // The next call judges both tail lessons; the model says the first changed the work.
    verdicts(&root, &[], json!([{"id": tail[0], "verdict": "changed", "avoids_consequence": true}]));
    stop(&root, "t1", "Done with the invoices.", &env);
    let judged = prompts(&root).last().unwrap().clone();
    assert!(tail.iter().all(|t| judged.contains(&format!("[{t}] lesson:"))), "{judged}");
    let changed: Vec<(Vec<String>, String, String)> = of(&root, "asked_then_changed")
        .iter()
        .map(|o| (lessons(o), o["channel"].as_str().unwrap_or_default().to_string(), o["check"].as_str().unwrap_or_default().to_string()))
        .collect();
    assert_eq!(changed, [(vec![tail[0].clone()], "queued".to_string(), check.clone())]);
    // `borg debug select`: each tail apply is free and delivered; the judged one is labelled changed.
    let v = select_summary(&root);
    let name = |k: &str, i: &Value| v[k][i.as_u64().unwrap() as usize].clone();
    let apply = |id: &str| {
        let mut found =
            v["applies"].as_array().unwrap().iter().filter(|a| name("checks", &a["check"])["id"] == check.as_str() && name("lessons", &a["lesson"]) == id);
        found.next().cloned().unwrap_or_else(|| panic!("{id} of {check}: {v}"))
    };
    let fate = |a: &Value| (a["route"].clone(), a["delivered"].clone(), a.get("label").cloned().unwrap_or(json!("open")));
    assert_eq!(fate(&apply(&tail[0])), (json!("free"), json!("yes"), json!("changed")), "{}", apply(&tail[0]));
    assert_eq!(fate(&apply(&tail[1])), (json!("free"), json!("yes"), json!("open")), "{}", apply(&tail[1]));
    let _ = std::fs::remove_dir_all(root);
}

/// `borg debug select --file <outcomes> --json`: the store's selection summary, as a hook loads it.
fn select_summary(root: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["debug", "select", "--file", store(root).join("outcomes.jsonl").to_str().unwrap(), "--json"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .current_dir(root)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["report"]["summary"].clone()
}
