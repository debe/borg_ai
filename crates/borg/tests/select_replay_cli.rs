//! `borg debug select`: the selection policy's pre-registered checks replayed from a log, through the
//! CLI: a fixed-rule log reproduced, a changed decision caught, predictions without the future, the
//! value model's calibration passed and failed, a planted world ranked, an evaluation's recall kept and
//! lost, the learned policy whatever the environment, the routed store, and bad usage. A check that
//! tested nothing says NOT TESTED and, when decisive, fails the command.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-select-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn borg(args: &[&str], root: &Path) -> Output {
    borg_with(args, root, &[])
}

/// `borg` with `env` set on top of the cleared environment.
fn borg_with(args: &[&str], root: &Path, env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", "")
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn lines(v: &[Value]) -> String {
    v.iter().map(|l| format!("{l}\n")).collect()
}

/// A graded check logged by the fixed rule before select.rs: `chosen` and `p` as the hook wrote them.
fn graded(session: &str, ms: i64, channel: &str, lessons: &[&str], grades: &[u8], chosen: &[&str]) -> Value {
    let p: Vec<f64> = lessons.iter().map(|l| if chosen.contains(l) { 1.0 } else { 0.0 }).collect();
    json!({"kind": "graded", "session": session, "lessons": lessons, "ms": ms, "repo": "r", "channel": channel, "grades": grades,
           "ranks": (0..lessons.len()).collect::<Vec<_>>(), "applied": lessons.len(), "chosen": chosen, "p": p})
}

fn result(kind: &str, session: &str, lesson: &str, ms: i64, channel: &str) -> Value {
    json!({"kind": kind, "session": session, "lessons": [lesson], "ms": ms, "repo": "r", "channel": channel})
}

/// The command's exit code for a JSON report: 1 when a decisive check (P0–P4) failed or tested
/// nothing, else 0.
fn exit_of(v: &Value) -> Option<i32> {
    let c = &v["report"]["checks"];
    Some(i32::from(["p0", "p1", "p2", "p3", "p4"].iter().any(|p| c[p]["verdict"] == "fail" || c[p]["verdict"] == "not_tested")))
}

fn run(root: &Path, log: &str, extra: &[&str]) -> (String, Value, Option<i32>) {
    run_with(root, log, extra, &[])
}

/// `run` with `env` set for the command.
fn run_with(root: &Path, log: &str, extra: &[&str], env: &[(&str, &str)]) -> (String, Value, Option<i32>) {
    let file = root.join("outcomes.jsonl");
    std::fs::write(&file, log).unwrap();
    let args: Vec<&str> = ["debug", "select", "--replay", "--file", file.to_str().unwrap()].into_iter().chain(extra.iter().copied()).collect();
    let out = borg_with(&args, root, env);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let json_args: Vec<&str> = args.iter().copied().chain(["--json"]).collect();
    let v: Value = serde_json::from_slice(&borg_with(&json_args, root, env).stdout).unwrap_or(Value::Null);
    (text, v, out.status.code())
}

/// A stop block (a 3 asks the 1 too), a live check without a 3 (both handed over free), and a check
/// logged with its id, policy and free propensities.
fn fixed_log() -> String {
    let mut with_id = graded("s2", 30, "live", &["E", "F"], &[2, 1], &[]);
    with_id["check"] = json!("0123456789ab");
    with_id["policy"] = json!("fixed");
    with_id["free"] = json!(["E", "F"]);
    with_id["pf"] = json!([1.0, 1.0]);
    lines(&[
        graded("s1", 10, "stop", &["A", "B"], &[3, 1], &["A", "B"]),
        graded("s1", 20, "live", &["C", "D"], &[1, 2], &[]),
        with_id,
        result("asked_then_changed", "s1", "A", 15, "stop"),
        result("asked_ignored", "s1", "D", 25, "queued"),
    ])
}

#[test]
fn a_fixed_rule_log_is_reproduced_with_its_propensities() {
    let root = tmp("fixed");
    let (text, v, code) = run(&root, &fixed_log(), &[]);
    assert_eq!(code, exit_of(&v), "{text}");
    assert!(
        text.starts_with("file ") && text.contains("\nrules 2026-10-28, amendment 2026-10-29 (docs/evidence.md#lesson-selection: "),
        "the rules it checked, where to read them: {text}"
    );
    assert!(text.contains(" here, the checks replay the learned policy; summary v"), "{text}");
    assert!(text.contains("graded checks: 3 (3 fixed)"), "{text}");
    assert!(text.contains("P0 baseline identity: PASS: 3 of 3 checks at gate budget 0"), "{text}");
    assert!(text.contains("P1 determinism: PASS: 3 of 3 replayed checks reproduce their decision and propensities within 0.01 (max |Δ| 0.0000)"), "{text}");
    assert!(text.contains("incremental fold = the whole fold"), "{text}");
    // No session ended, so no fit and no deviation: the gate was never put to the test.
    assert!(
        text.contains("P3 gate conduct: NOT TESTED (no decision left the fixed rule, so the gate never had to hold one back): cold-start replay of 3 checks (0 refits, one after each session end), 0 deviations"),
        "{text}"
    );
    assert_eq!(
        (v["report"]["checks"]["p3"]["verdict"].as_str(), code),
        (Some("not_tested"), Some(1)),
        "a decisive check that tested nothing fails the command"
    );
    assert!(text.contains("no budget set for the collective: spend reported, not gated; the policy's pools spend 18000 tokens per session"), "{text}");
    assert!(text.contains("spend, policy: 3 checks, 2 questions, 1 blocks at the stop, 4 free, pool tokens not logged"), "{text}");
    assert!(text.contains("no check has judged units of one grade on two routes yet"), "{text}");
    assert!(text.contains("P4 full-information recall: NOT RUN: pass --eval RUNS_DIR --qrels FILE"), "{text}");
    assert!(text.contains("P5 off-policy: REPORTED (non-decisive): 2 judged units"), "{text}");
    let c = &v["report"]["checks"];
    assert_eq!((c["p1"]["replayed"].as_u64(), c["p1"]["max_delta"].as_f64(), c["p0"]["differ"].as_u64()), (Some(3), Some(0.0), Some(0)), "{v}");
    assert_eq!((&v["report"]["rules"], &v["report"]["amendment"]), (&json!("2026-10-28"), &json!("2026-10-29")));
    // Without --replay: the state only, under the policy this process decides by.
    let out = borg(&["debug", "select", "--file", root.join("outcomes.jsonl").to_str().unwrap()], &root);
    let state = String::from_utf8_lossy(&out.stdout);
    let default = borg::select::precedence(None, None, false).name();
    assert!(
        out.status.success() && state.contains(&format!("policy: {default}; summary v{}", borg::select::summary::SV)) && state.contains("run with --replay"),
        "{state}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn p0_and_p1_fail_when_a_logged_decision_differs_from_the_fixed_rule() {
    let root = tmp("differs");
    // A block that asked only the 3: the fixed rule asks the 1 as well.
    let log = lines(&[graded("s1", 10, "stop", &["A", "B"], &[3, 1], &["A"]), graded("s1", 20, "live", &["C"], &[2], &[])]);
    let (text, v, code) = run(&root, &log, &[]);
    assert_eq!(code, Some(1), "a failed check fails the command: {text}");
    assert!(text.contains("P0 baseline identity: FAIL: 1 of 2 checks"), "{text}");
    assert!(text.contains("session s1 check - ms 10: gate 0 asks [\"A\", \"B\"]"), "the difference is listed: {text}");
    assert!(text.contains("P1 determinism: FAIL: 1 of 2 replayed checks"), "{text}");
    assert_eq!(v["report"]["checks"]["p0"]["verdict"], "fail");
    let _ = std::fs::remove_dir_all(&root);
}

/// Every result lands after every check: a leak-free prediction has no past, so neither model learned
/// anything (both say 1/2, a log-loss of ln 2 each) and P2 tested nothing: NOT TESTED, never a pass.
/// Results between the checks give the later ones a past.
#[test]
fn predictions_see_no_result_from_after_their_check() {
    let root = tmp("leak");
    let checks = |n: i64| (0..n).map(|i| graded(&format!("s{i}"), 10 * i, "live", &["A"], &[1], &[])).collect::<Vec<_>>();
    let late: Vec<Value> = (0..6).map(|i| result("asked_then_changed", &format!("s{i}"), "A", 1000 + i, "queued")).collect();
    let (text, v, code) = run(&root, &lines(&[checks(6), late].concat()), &[]);
    assert!(
        text.contains(
            "P2 calibration: NOT TESTED: no judged unit has a label before its check, so neither model learned anything; grade-only leave-future-out log-loss 0.6931 over 6 judged units in 6 sessions, 0 with a label before their check"
        ),
        "nothing to learn from: {text}"
    );
    assert_eq!((code, exit_of(&v)), (Some(1), Some(1)), "a decisive NOT TESTED fails the command: {text}");
    let p2 = &v["report"]["checks"]["p2"];
    assert_eq!((p2["verdict"].as_str(), p2["informed"].as_u64()), (Some("not_tested"), Some(0)), "{p2}");
    assert_eq!(p2["grade_only"].as_f64().map(|x| (x * 1e4).round()), Some(6931.0));
    let between: Vec<Value> = (0..6)
        .flat_map(|i| [graded(&format!("s{i}"), 10 * i, "live", &["A"], &[1], &[]), result("asked_then_changed", &format!("s{i}"), "A", 10 * i + 5, "queued")])
        .collect();
    let (_, v, _) = run(&root, &lines(&between), &[]);
    let ll = v["report"]["checks"]["p2"]["grade_only"].as_f64().unwrap();
    assert!(ll < 0.6, "the later units learn from the earlier results: {ll}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Checks with a 3 and two 1s, logged under randomisation: each 1 asked or handed over free with
/// probability 1/2. Free 1s change the work 4 times in 5, asked ones 1 in 5.
fn planted(n: usize) -> String {
    let mut log = Vec::new();
    for i in 0..n {
        let (s, ms) = (format!("s{}", i / 2), 100 * i as i64);
        let (asked, free) = if i % 2 == 0 { ("A1", "A2") } else { ("A2", "A1") };
        let mut c = graded(&s, ms, "live", &["T", "A1", "A2"], &[3, 1, 1], &["T", asked]);
        let check = format!("{i:012x}");
        c["check"] = json!(check);
        c["policy"] = json!("learned");
        c["free"] = json!([free]);
        c["p"] = json!([1.0, 0.5, 0.5]);
        c["pf"] = json!([0.0, 0.5, 0.5]);
        log.push(c);
        let (free_changed, asked_changed) = (i % 5 != 0, i % 5 == 0);
        let verdict = |changed: bool| if changed { "asked_then_changed" } else { "asked_ignored" };
        for (lesson, channel, changed) in [(free, "queued", free_changed), (asked, "live", asked_changed)] {
            let mut r = result(verdict(changed), &s, lesson, ms + 50, channel);
            r["check"] = json!(check);
            log.push(r);
        }
    }
    lines(&log)
}

#[test]
fn a_planted_world_where_free_beats_asked_for_grade_1_ranks_free_above_the_fixed_rule() {
    let root = tmp("planted");
    let (text, v, _) = run(&root, &planted(60), &[]);
    let c = &v["report"]["checks"];
    let rows = c["p5"]["rows"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|r| r["policy"] == name).unwrap_or_else(|| panic!("{name}: {text}"));
    let free = &row("fixed, grade 1 free")["dr"];
    assert!(free["value"].as_f64().unwrap() > 0.4 && free["lo"].as_f64().unwrap() > 0.0, "handing grade 1 over free ranks above the fixed rule: {text}");
    assert_eq!(row("fixed, grade 1 free")["coverage"].as_f64(), Some(1.0), "the randomised log covers it");
    assert_eq!(row("learned")["dr"]["n"].as_u64(), Some(120), "the learned policy is estimated on every unit: {text}");
    // The readout's within-check contrast says the same: asking a 1 changes less than handing it over.
    let qf = c["readout"]["contrasts"].as_array().unwrap().iter().find(|x| x["grade"] == 1 && x["routes"] == "Q−F").unwrap();
    assert!(qf["est"]["value"].as_f64().unwrap() < 0.0 && qf["est"]["hi"].as_f64().unwrap() < 0.0, "{text}");
    assert_eq!(c["readout"]["randomised"].as_u64(), Some(120));
    assert!(
        text.contains("readout: due (120 of 300 randomised labelled units, 30 of 30 sessions under the learned policy)"),
        "30 sessions make it due: {text}"
    );
    assert!(text.contains("  grade 1 Q−F: -") && text.contains("over 60 checks"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// `planted`, but the world changes its mind every `period` checks: then asked 1s change the work 4
/// times in 5 and free ones 1 in 5, then back. Every check has one change and one not, so grade only
/// stays at 1/2, while the value model chases the route it last saw and is wrong at every turn.
fn fickle(n: usize, period: usize) -> String {
    let mut log: Vec<Value> = serde_json::Deserializer::from_str(&planted(n)).into_iter::<Value>().map(Result::unwrap).collect();
    let flip = |v: &str| if v == "asked_then_changed" { "asked_ignored" } else { "asked_then_changed" };
    for r in log.iter_mut().filter(|r| r["kind"] != "graded" && r["ms"].as_i64().is_some_and(|ms| (ms as usize / 100 / period) % 2 == 1)) {
        r["kind"] = json!(flip(r["kind"].as_str().unwrap()));
    }
    lines(&log)
}

/// P2 decides on data that could fail it: the value model, which knows the route, beats grade only
/// where the route matters; where the world keeps turning it predicts worse than grade only, fails,
/// and the pre-registered rerun by the pruned model (it keeps the route) misses again.
#[test]
fn p2_passes_a_value_model_that_predicts_better_and_fails_one_that_predicts_worse() {
    let root = tmp("calibration");
    let (text, v, _) = run(&root, &planted(60), &[]);
    let p2 = &v["report"]["checks"]["p2"];
    assert_eq!(p2["verdict"], "pass", "{text}");
    assert!(p2["model"].as_f64().unwrap() < p2["grade_only"].as_f64().unwrap() && p2["diff"]["hi"].as_f64().unwrap() < 0.0, "{p2}");
    assert!(p2["informed"].as_u64().unwrap() > 100 && p2["pruned"].is_null(), "informative, and a pass is not rerun: {p2}");
    assert!(text.contains("P2 calibration: PASS: leave-future-out log-loss model "), "{text}");
    let (text, v, code) = run(&root, &fickle(60, 6), &[]);
    let p2 = &v["report"]["checks"]["p2"];
    assert_eq!((p2["verdict"].as_str(), code), (Some("fail"), Some(1)), "{text}");
    assert!(p2["model"].as_f64().unwrap() > p2["grade_only"].as_f64().unwrap() && p2["diff"]["hi"].as_f64().unwrap() > 0.005, "{p2}");
    assert_eq!(p2["pruned"]["verdict"], "fail", "{p2}");
    assert!(
        text.contains("  pre-registered rerun, the model pruned to grade, route and dilution: log-loss ") && text.contains(": FAIL: a second miss fails P2"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The checks replay the learned policy whatever this process's policy is: the same report under
/// `BORG_SELECT=fixed` and both evaluation switches, which only the header's policy names.
#[test]
fn the_checks_replay_the_learned_policy_whatever_the_environment_sets() {
    let root = tmp("env");
    let (_, plain, _) = run(&root, &harmful(4), &[]);
    let switches = [("BORG_SELECT", "fixed"), ("BORG_STOP_ASK_CAP", "1"), ("BORG_STOP_ATTENTION", "on")];
    let (text, set, _) = run_with(&root, &harmful(4), &[], &switches);
    assert_eq!(set["report"]["policy"], "fixed", "{text}");
    assert!(text.contains("policy: fixed here, the checks replay the learned policy;"), "{text}");
    assert_eq!(plain["report"]["checks"], set["report"]["checks"], "{text}");
    assert!(plain["report"]["checks"]["p3"]["deviations"].as_u64().unwrap() > 0, "the learned policy decided: {}", plain["report"]["checks"]["p3"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A store whose labels say handing a 1 over hurts: handed over free it is ignored, while judged
/// blindly as a placebo (not delivered) the work changes anyway; 2s handed over free change it. Logged
/// under the learned policy at cold start: the fixed rule, its two leftovers of a check drawn as
/// placebos; each delivery and verdict joined by check id, each session settled at its end.
fn harmful(sessions: usize) -> String {
    let mut log = Vec::new();
    let mut ms = 0;
    for s in 0..sessions {
        let session = format!("h{s}");
        for c in 0..6usize {
            ms += 100;
            let check = format!("{:012x}", 100 * s + c);
            // Two 2s alone, or with ten 1s rotated so every 1 is handed over at some checks and drawn at others.
            let ones: Vec<String> = if c % 2 == 0 { Vec::new() } else { (0..10).map(|k| format!("W{}", (k + s + c) % 10)).collect() };
            let lessons: Vec<String> = ["G0", "G1"].iter().map(|g| g.to_string()).chain(ones).collect();
            let grades: Vec<u8> = (0..lessons.len()).map(|j| if j < 2 { 2 } else { 1 }).collect();
            let (free, placebo) = lessons.split_at(lessons.len().min(10));
            let ids: Vec<&str> = lessons.iter().map(String::as_str).collect();
            let mut g = graded(&session, ms, "live", &ids, &grades, &[]);
            g["check"] = json!(check);
            g["policy"] = json!("learned");
            g["source"] = json!("fixed");
            g["free"] = json!(free);
            g["pf"] = json!(lessons.iter().map(|l| f64::from(u8::from(free.contains(l)))).collect::<Vec<_>>());
            if !placebo.is_empty() {
                g["placebo"] = json!(placebo);
                g["psi"] = json!(1.0);
            }
            log.push(g);
            let mut judged = |lesson: &str, route: &str, channel: &str, changed: bool| {
                ms += 1;
                let mut d = result("delivery", &session, lesson, ms, "");
                (d["check"], d["route"]) = (json!(check), json!(route));
                let mut r = result(if changed { "asked_then_changed" } else { "asked_ignored" }, &session, lesson, ms, channel);
                (r["check"], r["judge"]) = (json!(check), json!(2));
                log.extend([d, r]);
            };
            for (k, l) in free.iter().enumerate() {
                let changed = if k < 2 { (s + c + k) % 5 != 0 } else { (s + c + k) % 8 == 0 };
                judged(l, "free", "queued", changed);
            }
            for (k, l) in placebo.iter().enumerate() {
                judged(l, "placebo", "placebo", (s + c + k) % 5 != 0);
            }
        }
        ms += 10;
        let mut settle = result("asked_not_corrected", &session, "", ms, "");
        (settle["lessons"], settle["reason"]) = (json!([]), json!("judged"));
        log.push(settle);
    }
    lines(&log)
}

/// P4 on the same evaluation (every run four stop checks, each with the right lesson R as a 1) under
/// two stores: fitted on `planted` the learned policy keeps R and P4 passes; fitted on `harmful` it
/// leaves the stop's 1s undelivered, P4 fails, and so does the command, while the fixed rule hands R
/// over in every run and the n = 0 prior's arm passes under both. The harmful store passes P0–P3 on
/// its own, so its exit is P4's.
#[test]
fn p4_fails_the_command_when_the_stores_posterior_loses_the_right_lesson() {
    let root = tmp("harm");
    let runs = root.join("runs");
    for case in ["rf01", "rf02"] {
        for seed in 0..2 {
            eval_run(&runs, case, seed, &[("R:1:0,X:1:1", false); 4]);
        }
    }
    let qrels = root.join("qrels.jsonl");
    std::fs::write(&qrels, lines(&[json!({"case": "rf01", "challenge": "R", "grade": 2}), json!({"case": "rf02", "challenge": "R", "grade": 2})])).unwrap();
    let eval = ["--eval", runs.to_str().unwrap(), "--qrels", qrels.to_str().unwrap()];
    let (text, v, _) = run(&root, &planted(60), &eval);
    let p4 = &v["report"]["checks"]["p4"];
    assert_eq!(p4["verdict"], "pass", "a posterior that keeps the right lesson: {text}");
    assert!(p4["posterior"]["deviations"].as_u64().unwrap() > 0 && p4["prior"]["pass"] == true, "{p4}");
    let log = harmful(8);
    let (text, v, code) = run(&root, &log, &[]);
    for p in ["p0", "p1", "p2", "p3"] {
        assert_eq!(v["report"]["checks"][p]["verdict"], "pass", "{p}: {text}");
    }
    assert_eq!(code, Some(0), "P4 not run decides nothing: {text}");
    let (text, v, code) = run(&root, &log, &eval);
    let p4 = &v["report"]["checks"]["p4"];
    assert_eq!((p4["verdict"].as_str(), code), (Some("fail"), Some(1)), "{text}");
    assert!(text.contains("P4 full-information recall: FAIL\n") && text.contains("fixed rule: recall 100.0%"), "{text}");
    let post = &p4["posterior"];
    assert!(post["recall"].as_f64().unwrap() < 0.5 && post["pass"] == false && post["deviations"].as_u64().unwrap() > 0, "{post}");
    assert_eq!(p4["prior"]["pass"], true, "the n = 0 prior keeps the right lesson: {}", p4["prior"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A run directory as borg-eval writes it: `<set>/<case>-s<seed>/borg-state/<session>.ctx` with the
/// graded lines, and the session's cost log saying which checks were live.
fn eval_run(runs: &Path, case: &str, seed: u32, checks: &[(&str, bool)]) {
    let state = runs.join("set").join(format!("{case}-s{seed}")).join("borg-state");
    std::fs::create_dir_all(&state).unwrap();
    let (mut ctx, mut cost) = (String::new(), String::new());
    for (items, live) in checks {
        ctx.push_str(&format!("#borg-predict-cost 0.1000\n#borg-graded n={} {items}\n", items.split(',').count()));
        if *live {
            cost.push_str("live usd=0.1000 lessons=3\n");
        }
        cost.push_str("graded applied=1 ask=0 free=0 dropped\n");
    }
    std::fs::write(state.join("sess.ctx"), ctx).unwrap();
    std::fs::write(state.join("sess.cost"), cost).unwrap();
}

#[test]
fn p4_compares_recall_on_an_evaluations_graded_lines() {
    let root = tmp("eval");
    let runs = root.join("runs");
    // rf01: the right lesson R is handed over free in one seed and missed in the other; rf02: a stop
    // block asks it in both.
    eval_run(&runs, "rf01", 0, &[("X:1:0", true), ("R:2:0", true)]);
    eval_run(&runs, "rf01", 1, &[("X:3:0", true)]);
    eval_run(&runs, "rf02", 0, &[("Q:3:0,X:1:1", false)]);
    eval_run(&runs, "rf02", 1, &[("Q:3:0,X:1:1", false)]);
    let qrels = root.join("qrels.jsonl");
    std::fs::write(
        &qrels,
        lines(&[
            json!({"case": "rf01", "challenge": "R", "grade": 2}),
            json!({"case": "rf02", "challenge": "Q", "grade": 2}),
            json!({"case": "rf02", "challenge": "X", "grade": 0}),
        ]),
    )
    .unwrap();
    let (text, v, code) = run(&root, &fixed_log(), &["--eval", runs.to_str().unwrap(), "--qrels", qrels.to_str().unwrap()]);
    // Decided either way (the learned core's recall is its own); the fixed rule's is the log's.
    let p4 = &v["report"]["checks"]["p4"];
    assert!(matches!(p4["verdict"].as_str(), Some("pass" | "fail")), "{p4}");
    assert_eq!(code, exit_of(&v), "{text}");
    assert!(text.contains("2 of 2 cases, 4 runs (2–2 per case), 5 checks, $0.1000 per check"), "{text}");
    assert!(text.contains("fixed rule: recall 75.0%; per check 1.00 questions, 0.40 free, 0.40 blocks at the stop"), "{text}");
    for arm in ["learned, n = 0 prior: recall ", "learned, store posterior (fit "] {
        assert!(text.contains(arm) && text.contains(" points (case-paired 95% CI); per check "), "{arm}: {text}");
    }
    assert_eq!((p4["runs"].as_u64(), p4["checks"].as_u64()), (Some(4), Some(5)), "{p4}");
    // A seed still missing: incomplete, not decided.
    std::fs::remove_dir_all(runs.join("set/rf02-s1")).unwrap();
    let (text, _, _) = run(&root, &fixed_log(), &["--eval", runs.to_str().unwrap(), "--qrels", qrels.to_str().unwrap()]);
    assert!(text.contains("P4 full-information recall: NOT RUN: cases have 1 to 2 runs: the evaluation is incomplete"), "{text}");
    // A case the qrels name without runs: the pre-registered set is not covered.
    std::fs::write(&qrels, lines(&[json!({"case": "rf01", "challenge": "R", "grade": 2}), json!({"case": "rf09", "challenge": "Z", "grade": 2})])).unwrap();
    let (text, v, code) = run(&root, &fixed_log(), &["--eval", runs.to_str().unwrap(), "--qrels", qrels.to_str().unwrap()]);
    assert_eq!(code, exit_of(&v), "NOT RUN fails nothing");
    assert!(text.contains("P4 full-information recall: NOT RUN: runs cover 1 of the 2 cases the qrels name"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn select_reads_the_routed_store_and_refuses_bad_usage() {
    let root = tmp("route");
    let (corp, off) = (root.join("corp"), root.join("elsewhere"));
    for d in [&corp, &off] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(root.join("collectives.json"), json!({"default": "off", "collectives": [{"name": "corp", "paths": [corp], "share": "local"}]}).to_string())
        .unwrap();
    let store = borg::collective::store(&root.join("store"), "corp");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("outcomes.jsonl"), fixed_log()).unwrap();
    let out = borg(&["debug", "select", "--replay", "--dir", corp.to_str().unwrap()], &root);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("collective corp") && text.contains("P0 baseline identity: PASS: 3 of 3"), "{text}");
    // The engineer's budget for the collective bounds the replayed pools: 18,000 pool tokens per session
    // (two sessions, three checks at the baseline's 12,000) is over a budget of 1,000.
    let config = |budget: u32| {
        let c = json!({"default": "off", "collectives": [{"name": "corp", "paths": [corp], "share": "local", "budget": budget}]});
        std::fs::write(root.join("collectives.json"), c.to_string()).unwrap();
    };
    config(1000);
    let out = borg(&["debug", "select", "--replay", "--dir", corp.to_str().unwrap()], &root);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("P3 gate conduct: FAIL: ")
            && text.contains("budget of 1000 pool tokens per session: the policy's pools spend 18000 per session, over it"),
        "{text}"
    );
    assert_eq!(out.status.code(), Some(1), "a failed check fails the command: {text}");
    config(18_000);
    let out = borg(&["debug", "select", "--replay", "--dir", corp.to_str().unwrap()], &root);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("P3 gate conduct: NOT TESTED (") && text.contains("spend 18000 per session, within it"), "within it, the gate untested: {text}");
    let out = borg(&["debug", "select", "--rebuild", "--dir", corp.to_str().unwrap()], &root);
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains(&format!("summary v{} at byte", borg::select::summary::SV)), "{out:?}");
    let out = borg(&["debug", "select", "--dir", off.to_str().unwrap()], &root);
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).starts_with("off:"));
    for bad in [
        &["debug", "select", "--zz-probe"][..],
        &["debug", "select", "--dir"],
        &["debug", "select", "--dir", "a", "--file", "b"],
        &["debug", "select", "--eval", "runs"],
        &["debug", "select", "--qrels", "q.jsonl"],
    ] {
        let out = borg(bad, &root);
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("usage: borg debug select"), "its own usage: {bad:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
