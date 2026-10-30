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

/// The finding both plain-agent fixtures report, as a reviewer answers in its text: a fenced JSON block.
fn reviewer_text() -> String {
    let finding =
        json!({"findings": [{"file": "src/net.rs", "severity": "high", "title": "The budget place is reset by an arc instead of drained by a consumer"}]});
    format!("Reviewed the diff.\n```json\n{finding}\n```")
}

/// One Agent-tool sub-agent `id` of the plain Claude Code session `session` in `repo` (under root),
/// started at `ts` with `prompt`, that answered `content`.
fn write_agent(root: &Path, session: &str, repo: &str, (id, description, ts): (&str, &str, &str), prompt: &str, content: Value) {
    let cwd = root.join(repo);
    let dir = root.join("home/.claude/projects").join(format!("-{repo}")).join(session).join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("agent-{id}.meta.json")), json!({"agentType": "general-purpose", "description": description}).to_string()).unwrap();
    let lines = [
        json!({"type": "user", "isSidechain": true, "agentId": id, "sessionId": session, "timestamp": ts, "cwd": cwd, "message": {"role": "user", "content": prompt}}),
        json!({"type": "assistant", "isSidechain": true, "agentId": id, "sessionId": session, "timestamp": ts, "cwd": cwd, "message": {"role": "assistant", "content": [content]}}),
    ];
    std::fs::write(dir.join(format!("agent-{id}.jsonl")), lines.iter().map(|v| format!("{v}\n")).collect::<String>()).unwrap();
}

/// A plain Claude Code session `session` in `repo` (under root) that ran two Agent-tool sub-agents, no
/// workflow: a reviewer answering with a finding in its text, then a fixer editing its file.
fn write_agents(root: &Path, session: &str, repo: &str) {
    let review = json!({"type": "text", "text": reviewer_text()});
    write_agent(root, session, repo, ("a1", "Review the net diff", "2026-09-14T13:15:00.000Z"), "review the diff", review);
    let edit = json!({"type": "tool_use", "name": "Bash", "input": {"command": "sed -i 's/reset/drain/' src/net.rs src/arc.rs"}});
    write_agent(root, session, repo, ("a2", "Fix the budget arc", "2026-09-14T13:16:00.000Z"), "apply the finding", edit);
}

/// A Codex session `id` in `repo` (under root) whose agent spawned two others: a reviewer answering with
/// a finding in its text, then a fixer patching its file through code mode's `exec`.
fn write_codex(root: &Path, id: &str, repo: &str) {
    let cwd = root.join(repo);
    let day = root.join("home/.codex/sessions/2026/09/14");
    std::fs::create_dir_all(&day).unwrap();
    let rollout = |thread: &str, ts: &str, source: Value, items: &[Value]| {
        let meta = json!({"timestamp": ts, "type": "session_meta", "payload": {"id": thread, "session_id": id, "timestamp": ts, "cwd": cwd, "originator": "codex-tui", "source": source}});
        let mut lines = vec![meta];
        lines.extend(items.iter().map(|p| json!({"timestamp": ts, "type": "response_item", "payload": p})));
        let name = format!("rollout-{}-{thread}.jsonl", ts.replace(':', "-"));
        std::fs::write(day.join(name), lines.iter().map(|v| format!("{v}\n")).collect::<String>()).unwrap();
    };
    let msg = |role: &str, kind: &str, text: &str| json!({"type": "message", "role": role, "content": [{"type": kind, "text": text}]});
    let spawned = |path: &str, role: &str| json!({"subagent": {"thread_spawn": {"parent_thread_id": id, "depth": 1, "agent_path": path, "agent_nickname": "Cedar", "agent_role": role}}});
    rollout(id, "2026-09-14T13:14:05.691Z", json!("cli"), &[msg("user", "input_text", "build the net, review it, fix what the review finds")]);
    rollout(
        "01a0a00d-0000-7000-8000-0000000000a1",
        "2026-09-14T13:15:00.000Z",
        spawned("/root/review_net", "explorer"),
        &[
            msg("user", "input_text", "# AGENTS.md instructions\n\n<INSTRUCTIONS>…"),
            msg("user", "input_text", "review the diff"),
            msg("assistant", "output_text", &reviewer_text()),
        ],
    );
    let patch = "*** Begin Patch\n*** Update File: src/net.rs\n-reset\n+drain\n*** End Patch";
    rollout(
        "01a0a00d-0000-7000-8000-0000000000a2",
        "2026-09-14T13:16:00.000Z",
        spawned("/root/fix_net", "worker"),
        &[
            msg("user", "input_text", "apply the finding"),
            json!({"type": "custom_tool_call", "name": "exec", "input": format!("await tools.apply_patch({patch:?})")}),
        ],
    );
}

/// The review episodes in the corp store.
fn review_episodes(root: &Path) -> Vec<Value> {
    episodes(root).lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["id"].as_str().is_some_and(|i| i.starts_with('V'))).collect()
}

fn learn(root: &Path, session: &str, env: &[(&str, &str)]) {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let ok = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["learn", "--session", session])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
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

#[test]
fn a_plain_claude_sessions_agent_tool_review_is_recorded_at_its_end() {
    let root = common::setup("review-fix-agents");
    write_agents(&root, "s1", "harbor_orion");
    learn(&root, "s1", &[("BORG_HARNESS", "claude")]);
    let review = review_episodes(&root);
    assert_eq!(review.len(), 1, "{}", episodes(&root));
    assert_eq!(review[0]["id"], "Vagents-s1#a1.0");
    assert_eq!(review[0]["session"], "s1");
    assert!(review[0]["correction"].as_str().unwrap().contains("drained by a consumer"), "{}", review[0]);
    assert!(common::prompts(&root).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_reviewer_that_started_earlier_but_finished_later_neither_hides_nor_repeats_a_finding() {
    let root = common::setup("review-fix-agents-late");
    write_agents(&root, "s1", "harbor_orion");
    let search = json!({"type": "text", "text": "The arcs are reset in src/net.rs."});
    write_agent(&root, "s1", "harbor_orion", ("a9", "Find the arc code", "2026-09-14T13:13:00.000Z"), "find where arcs are reset", search);
    learn(&root, "s1", &[("BORG_HARNESS", "claude")]);
    // A background reviewer, started before the first, gives its finding after that Stop.
    let late = json!({"findings": [{"file": "src/arc.rs", "severity": "medium", "title": "The arc's weight is read before the place is checked"}]});
    let late = json!({"type": "text", "text": format!("```json\n{late}\n```")});
    write_agent(&root, "s1", "harbor_orion", ("a0", "Review the arcs", "2026-09-14T13:14:00.000Z"), "review the arcs", late);
    learn(&root, "s1", &[("BORG_HARNESS", "claude")]);
    let mut ids: Vec<String> = review_episodes(&root).iter().map(|v| v["id"].as_str().unwrap().to_string()).collect();
    ids.sort();
    assert_eq!(ids, ["Vagents-s1#a0.0", "Vagents-s1#a1.0"], "{}", episodes(&root));
    // The session's own agent built: no sub-agent's prompt is the episode's task.
    assert!(review_episodes(&root).iter().all(|v| v["task"].as_str().unwrap_or_default().is_empty()), "{}", episodes(&root));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_codex_sessions_spawned_review_is_recorded_at_its_end() {
    let root = common::setup("review-fix-codex");
    let id = "01a0a00d-0000-7000-8000-000000000001";
    write_codex(&root, id, "harbor_orion");
    learn(&root, id, &[("BORG_HARNESS", "codex")]);
    let review = review_episodes(&root);
    assert_eq!(review.len(), 1, "{}", episodes(&root));
    assert_eq!(review[0]["id"], format!("V{id}#01a0a00d-0000-7000-8000-0000000000a1.0"));
    assert_eq!(review[0]["session"], id);
    assert!(common::prompts(&root).is_empty());
    // A spawned thread's own id names no session of its own.
    let other = common::setup("review-fix-codex-child");
    write_codex(&other, id, "harbor_orion");
    learn(&other, "01a0a00d-0000-7000-8000-0000000000a1", &[("BORG_HARNESS", "codex")]);
    assert!(review_episodes(&other).is_empty(), "{}", episodes(&other));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(other);
}
