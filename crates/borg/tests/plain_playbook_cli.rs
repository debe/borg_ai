//! A plain session's playbooks, no hive, end to end through the binary and every harness's hooks: a
//! session that ended clean (it landed a commit, its own last test run passed, and the engineer neither
//! corrected nor vetoed it) has `borg learn` ask the playbook reflection once and queue its edit for the
//! engineer; a session with a correction asks nothing; and once promoted, a later prompt naming most of
//! the shape (`queen::STATED`), not all of it, is shown the playbook. Its task is its first prompt, not a
//! later "commit it", and a reflection the model did not answer waits for a later `borg learn`. The model
//! is a fake `claude`: no test makes a real model call.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{borg_as, context, prompts, setup};
use serde_json::{Value, json};

const HARNESSES: [&str; 4] = ["claude", "omp", "opencode", "codex"];

/// The reflection's answer: one step for the shape "tidy the orion budget handling" (no quote: the fake
/// echoes it single-quoted).
const STEP: &str = "Run the Orion budget tests before calling it done";

fn store(root: &Path) -> std::path::PathBuf {
    root.join("store/collectives/corp")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@x", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `common::setup` with the Orion repo under git, and a fake `claude` that saves each prompt as the
/// common one does and answers the playbook reflection with `STEP` (or fails it once while `fail-once`
/// exists in `root`), a correction with "not a lesson", and anything else with the common prediction.
fn setup_git(name: &str) -> std::path::PathBuf {
    let root = setup(name);
    let repo = root.join("harbor_orion");
    std::fs::write(repo.join("net.txt"), "budget: reset\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "start"]);
    let edit = json!({"structured_output": {"shape": "tidy the orion budget handling", "op": "add", "step": "", "text": STEP}, "total_cost_usd": 0});
    let not_lesson = json!({"structured_output": {"is_lesson": false, "question": "", "situations": [], "consequence": "", "same_as": "",
        "area": "other", "supersedes": [], "reach": "team"}, "total_cost_usd": 0});
    let predict = json!({"structured_output": {"moves": [], "applies": [{"id": "Mdrain", "why": "w", "grade": 3}]}, "total_cost_usd": 0.05});
    let fake = root.join("bin/claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nn=$(( $(ls {d} | wc -l) ))\nprintf '%s' \"$2\" > {d}/$n.txt\ncase \"$2\" in\n*\"You curate the team's playbooks\"*) if [ -e {f} ]; then rm {f}; exit 1; fi; echo '{edit}' ;;\n*\"An engineer corrected a coding agent\"*) echo '{not_lesson}' ;;\n*) echo '{predict}' ;;\nesac\n",
            d = root.join("prompts").display(),
            f = root.join("fail-once").display(),
        ),
    )
    .unwrap();
    root
}

/// The env of every hook call: learning on, and every model call through the fake `claude`.
fn env(harness: &str) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![("BORG_LEARN", "on"), ("BORG_PREFILTER", "off"), ("BORG_LEARN_MODEL", "claude:fake")];
    if harness != "claude" {
        env.push(("BORG_PREDICT_MODEL", "claude:sonnet"));
    }
    env
}

/// A passing test run as `harness` reports it (tested_cli.rs `test_run`).
fn passing_run(harness: &str) -> (&'static str, Value) {
    let out = "test result: ok. 3 passed; 0 failed";
    match harness {
        "claude" => ("post_tool", json!({"tool_response": {"stdout": out, "stderr": "", "interrupted": false}})),
        "omp" | "opencode" => ("post_tool", json!({"tool_response": out})),
        _ => ("post_tool", json!({"tool_response": format!("Exit code: 0\nOutput:\n{out}")})),
    }
}

/// A session through `harness`: its prompt, then each of `later` as a further prompt (a correction, or
/// "commit it"), a commit in the repo, a passing test run, the stop and the end.
fn session(root: &Path, harness: &str, s: &str, later: &[&str]) {
    let cwd = root.join("harbor_orion");
    let env = env(harness);
    borg_as(root, harness, "prompt", &json!({"session_id": s, "cwd": cwd, "prompt": "Tidy the budget handling of the Orion net"}), &env);
    for p in later {
        borg_as(root, harness, "prompt", &json!({"session_id": s, "cwd": cwd, "prompt": p}), &env);
    }
    std::fs::write(cwd.join("net.txt"), format!("budget: drained by {s}\n")).unwrap();
    git(&cwd, &["commit", "-qam", "drain the budget place"]);
    let (event, result) = passing_run(harness);
    let mut input = json!({"session_id": s, "cwd": cwd, "tool_name": "Bash", "tool_input": {"command": "cargo test -p orion"}});
    input.as_object_mut().unwrap().extend(result.as_object().unwrap().clone());
    borg_as(root, harness, event, &input, &env);
    let stop = json!({"session_id": s, "cwd": cwd, "last_assistant_message": "Done: the Orion budget place is drained.", "stop_hook_active": false});
    borg_as(root, harness, "stop", &stop, &env);
    borg_as(root, harness, "end", &json!({"session_id": s, "cwd": cwd}), &env);
}

fn outcomes(root: &Path, kind: &str, session: &str) -> usize {
    let text = std::fs::read_to_string(store(root).join("outcomes.jsonl")).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|o| o["kind"] == kind && o["session"] == session).count()
}

/// Waits (at most 20 s) until the end's `borg learn` settled the session's ask: it ran to its end.
fn settled(root: &Path, s: &str) {
    let until = Instant::now() + Duration::from_secs(20);
    while outcomes(root, "asked_not_corrected", s) == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(outcomes(root, "asked_not_corrected", s) > 0, "{s}: learning settled the session");
}

fn reflections(root: &Path) -> Vec<String> {
    prompts(root).into_iter().filter(|p| p.starts_with(borg::playbook::OPENING)).collect()
}

/// `borg <args>` as the engineer, in the repo, on the store `home`: its stdout.
fn run(root: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
        .env("HOME", root.join("home"))
        .env("BORG_HOME", home)
        .env("BORG_STATE", store(root).join("state"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_VECTORS", "off")
        .env("BORG_LEARN_MODEL", "claude:fake")
        .current_dir(root.join("harbor_orion"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "borg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into()
}

#[test]
fn a_clean_plain_session_proposes_one_playbook_edit_and_once_promoted_a_prompt_of_its_shape_gets_it() {
    for harness in HARNESSES {
        let root = setup_git(&format!("plain-pb-{harness}"));
        session(&root, harness, "clean1", &["Commit it"]);
        settled(&root, "clean1");
        let asked = reflections(&root);
        assert_eq!(asked.len(), 1, "{harness}: one reflection");
        assert!(
            asked[0].contains("## The task\nTidy the budget handling of the Orion net\n")
                && !asked[0].contains("Commit it")
                && asked[0].contains("The session landed 1 commit(s)")
                && asked[0].contains("(none)"),
            "{harness}: {}",
            asked[0]
        );
        let queued: Vec<_> = borg::queen::open(&store(&root)).into_iter().filter(|e| e.kind == borg::queen::Kind::Playbook).collect();
        assert_eq!(queued.len(), 1, "{harness}: one edit waits for the engineer");
        assert!(queued[0].question.contains(STEP), "{}", queued[0].question);
        assert!(!store(&root).join(borg::playbook::FILE).exists(), "{harness}: learning never applies an edit");
        // `borg learn` once more, as the end hook runs it, on the collective's store.
        run(&root, &store(&root), &["learn", "--session", "clean1"]);
        assert_eq!(reflections(&root).len(), 1, "{harness}: once per session");

        // Promoted, the playbook reaches a later prompt naming three of its four shape words, not one
        // naming two.
        let out = run(&root, &root.join("store"), &["queen", "answer", &queued[0].id, "b", "--dir", root.join("harbor_orion").to_str().unwrap()]);
        assert!(out.contains("the edit is promoted"), "{out}");
        let cwd = root.join("harbor_orion");
        let prompt = |s: &str, text: &str| context(&borg_as(&root, harness, "prompt", &json!({"session_id": s, "cwd": cwd, "prompt": text}), &env(harness)));
        assert!(!prompt("later1", "Look at the Orion budget").contains("playbook"), "{harness}: half the shape");
        let shown = prompt("later2", "Tidy up the Orion budget code");
        assert!(shown.contains("the team's playbook") && shown.contains(STEP), "{harness}: {shown}");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn a_plain_session_with_a_correction_asks_no_reflection() {
    for harness in HARNESSES {
        let root = setup_git(&format!("plain-pb-corrected-{harness}"));
        session(&root, harness, "fixed1", &["No, don't reset the budget place; drain it with an explicit consumer instead."]);
        settled(&root, "fixed1");
        assert!(prompts(&root).iter().any(|p| p.starts_with("An engineer corrected a coding agent")), "{harness}: the correction was learned");
        assert!(reflections(&root).is_empty(), "{harness}: a corrected session reflects nothing");
        assert!(borg::queen::open(&store(&root)).iter().all(|e| e.kind != borg::queen::Kind::Playbook), "{harness}");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn a_reflection_the_model_did_not_answer_waits_for_a_later_learn() {
    for harness in HARNESSES {
        let root = setup_git(&format!("plain-pb-retry-{harness}"));
        std::fs::write(root.join("fail-once"), "").unwrap();
        session(&root, harness, "late1", &[]);
        settled(&root, "late1");
        assert_eq!(reflections(&root).len(), 1, "{harness}: asked once, no answer");
        assert!(borg::queen::open(&store(&root)).iter().all(|e| e.kind != borg::queen::Kind::Playbook), "{harness}");
        let waiting = std::fs::read_to_string(store(&root).join(borg::playbook::PLAIN)).unwrap_or_default();
        assert_eq!(waiting, "late1\n", "{harness}: the session waits, though its asks settled");
        // A later `borg learn` of no session (another session's end, or the engineer's) reflects it.
        run(&root, &store(&root), &["learn"]);
        assert_eq!(reflections(&root).len(), 2, "{harness}: asked again");
        let queued = borg::queen::open(&store(&root)).into_iter().filter(|e| e.kind == borg::queen::Kind::Playbook).count();
        assert_eq!(queued, 1, "{harness}");
        assert!(!store(&root).join(borg::playbook::PLAIN).exists(), "{harness}: nothing waits");
        run(&root, &store(&root), &["learn"]);
        assert_eq!(reflections(&root).len(), 2, "{harness}: once per session");
        let _ = std::fs::remove_dir_all(root);
    }
}
