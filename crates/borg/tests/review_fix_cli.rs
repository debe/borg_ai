//! Review → fix at session end: `borg learn --session S` (as the end hook starts it, with the
//! collective's store as BORG_HOME) records the session's strong review → fix findings as episodes,
//! unless `BORG_REVIEW_FIX=off`, from the runs of the drone's harness (`BORG_HARNESS`). No test makes a model call (BORG_LEARN_MODEL=off, fake `claude` on PATH).

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// A workflow run `run` of `session` in `repo` (under root): a reviewer's finding, then a fix step
/// naming its file.
fn write_run(root: &Path, session: &str, run: &str, repo: &str) {
    let cwd = root.join(repo);
    let dir = root.join("home/.claude/projects").join(format!("-{repo}")).join(session);
    let agents = dir.join("subagents/workflows").join(run);
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    let line = |v: Value| format!("{v}\n");
    let user = |p: &str| line(json!({"type": "user", "cwd": cwd, "message": {"role": "user", "content": p}}));
    let tool = |name: &str, input: Value| line(json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": name, "input": input}]}}));
    let finding =
        json!({"findings": [{"file": "src/net.rs", "severity": "high", "title": "The budget place is reset by an arc instead of drained by a consumer"}]});
    std::fs::write(agents.join("agent-a1.jsonl"), user("review the diff") + &tool("StructuredOutput", finding)).unwrap();
    std::fs::write(agents.join("agent-a2.jsonl"), user("apply the findings") + &tool("Bash", json!({"command": "sed -i 's/reset/drain/' src/net.rs"})))
        .unwrap();
    let progress = json!([
        {"type": "workflow_agent", "label": "review:nets", "phaseTitle": "Review", "agentId": "a1", "startedAt": "10"},
        {"type": "workflow_agent", "label": "fix", "phaseTitle": "Fix", "agentId": "a2", "startedAt": "20"}
    ]);
    let r = json!({"runId": run, "workflowName": "build", "startTime": 10, "workflowProgress": progress});
    std::fs::write(dir.join("workflows").join(format!("{run}.json")), r.to_string()).unwrap();
}

/// An omp session `id` in `repo` (under root) whose task agents review and fix: a reviewer yields
/// one finding, a `…Fix` task edits its file.
fn write_omp(root: &Path, id: &str, repo: &str) {
    let cwd = root.join(repo);
    let dir = root.join("home/.omp/agent/sessions").join(format!("-{repo}")).join(format!("2026-09-14T13-14-05-691Z_{id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let lines = |vs: &[Value]| vs.iter().map(|v| format!("{v}\n")).collect::<String>();
    std::fs::write(dir.with_extension("jsonl"), lines(&[json!({"type": "session", "id": id, "timestamp": "2026-09-14T13:14:05.691Z", "cwd": cwd})])).unwrap();
    let agent = |kind: &str, ts: &str, task: &str, calls: &[(&str, Value)]| {
        let mut v = vec![
            json!({"type": "session", "id": "x", "timestamp": ts, "cwd": cwd}),
            json!({"type": "session_init", "timestamp": ts, "task": task, "agent": kind}),
        ];
        for (name, args) in calls {
            v.push(json!({"type": "message", "message": {"role": "assistant", "content": [{"type": "toolCall", "name": name, "arguments": args}]}}));
        }
        lines(&v)
    };
    let finding = json!({"title": "The budget place is reset by an arc instead of drained by a consumer", "body": "src/net.rs resets it.", "priority": 1, "file_path": "src/net.rs"});
    std::fs::write(
        dir.join("NetReview.jsonl"),
        agent("reviewer", "2026-09-14T13:15:00.000Z", "review the net", &[("yield", json!({"type": ["findings"], "data": finding}))]),
    )
    .unwrap();
    std::fs::write(
        dir.join("NetFix.jsonl"),
        agent("task", "2026-09-14T13:16:00.000Z", "fix it", &[("bash", json!({"command": "sed -i 's/reset/drain/' src/net.rs"}))]),
    )
    .unwrap();
}

fn learn(root: &Path, session: &str, env: &[(&str, &str)]) {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let ok = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["learn", "--session", session])
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store/collectives/corp"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN_MODEL", "off")
        .env("BORG_VECTORS", "off")
        .envs(env.iter().copied())
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(ok.success());
}

fn episodes(root: &Path) -> String {
    episodes_of(root, "corp")
}

fn episodes_of(root: &Path, collective: &str) -> String {
    std::fs::read_to_string(root.join("store/collectives").join(collective).join("episodes.jsonl")).unwrap_or_default()
}

#[test]
fn off_records_no_review_episode() {
    let root = common::setup("review-fix-off");
    write_run(&root, "s1", "wf_1", "harbor_orion");
    learn(&root, "s1", &[("BORG_REVIEW_FIX", "off")]);
    assert!(!episodes(&root).contains("Vwf_1#"), "{}", episodes(&root));
    // On by default: the same run, unset, records it.
    learn(&root, "s1", &[]);
    assert!(episodes(&root).contains("Vwf_1#"), "{}", episodes(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn learn_records_the_ended_sessions_findings_once_with_no_model_call() {
    let root = common::setup("review-fix-on");
    write_run(&root, "s1", "wf_1", "harbor_orion");
    write_run(&root, "s2", "wf_2", "harbor_orion");
    learn(&root, "s1", &[("BORG_REVIEW_FIX", "learn")]);
    learn(&root, "s1", &[("BORG_REVIEW_FIX", "learn")]);
    let e = episodes(&root);
    let review: Vec<Value> =
        e.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["id"].as_str().is_some_and(|i| i.starts_with('V'))).collect();
    assert_eq!(review.len(), 1, "{e}");
    assert!(review[0]["id"].as_str().unwrap().starts_with("Vwf_1#"));
    assert_eq!(review[0]["session"], "s1");
    assert_eq!(review[0]["kind"], "regret");
    assert!(common::prompts(&root).is_empty());
    // BORG_LEARN=off (eval drones) records nothing either.
    let root2 = common::setup("review-fix-learn-off");
    write_run(&root2, "s1", "wf_1", "harbor_orion");
    learn(&root2, "s1", &[("BORG_REVIEW_FIX", "learn"), ("BORG_LEARN", "off")]);
    assert!(!episodes(&root2).contains("Vwf_1#"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

#[test]
fn each_finding_goes_to_its_repos_store_and_an_off_repo_to_none() {
    let root = common::setup("review-fix-route");
    std::fs::create_dir_all(root.join("side")).unwrap();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "collectives": [
            {"name": "corp", "paths": [root.join("harbor_orion")], "share": "local"},
            {"name": "priv", "paths": [root.join("side")], "share": "local"}
        ]})
        .to_string(),
    )
    .unwrap();
    write_run(&root, "s1", "wf_1", "harbor_orion");
    write_run(&root, "s1", "wf_2", "side");
    write_run(&root, "s1", "wf_3", "elsewhere");
    learn(&root, "s1", &[("BORG_REVIEW_FIX", "learn")]);
    let (corp, private) = (episodes(&root), episodes_of(&root, "priv"));
    assert!(corp.contains("Vwf_1#") && !corp.contains("wf_2") && !corp.contains("wf_3"), "{corp}");
    assert!(private.contains("Vwf_2#") && !private.contains("wf_1") && !private.contains("wf_3"), "{private}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_omp_sessions_finding_is_recorded_at_its_end() {
    let root = common::setup("review-fix-omp");
    let id = "01a0a00d-0000-7000-8000-000000000001";
    write_omp(&root, id, "harbor_orion");
    // A Claude Code run of the same session id is not the omp drone's: only its harness is read.
    write_run(&root, id, "wf_1", "harbor_orion");
    learn(&root, id, &[("BORG_HARNESS", "omp")]);
    let e = episodes(&root);
    let review: Vec<Value> =
        e.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["id"].as_str().is_some_and(|i| i.starts_with('V'))).collect();
    assert_eq!(review.len(), 1, "{e}");
    assert_eq!(review[0]["id"], format!("V2026-09-14T13-14-05-691Z_{id}#0"));
    assert_eq!(review[0]["session"], id);
    assert!(common::prompts(&root).is_empty());
    let _ = std::fs::remove_dir_all(root);
}
