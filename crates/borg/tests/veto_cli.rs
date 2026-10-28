//! `borg no` through the binary: a terminal veto finds the repo's drone (or asks which), credits the
//! lessons it was shown, records the reason as a veto episode and counts it; an off repo is left without
//! a file. In the session, a prompt starting "borg no" is the same veto, and the drone is told to stop.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-veto-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `borg <args>` in `dir`: (stdout, stderr, exit code).
fn borg(root: &Path, dir: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> (String, String, Option<i32>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_MATCH", "bm25") // word matching only: no model call from a test
        .env("BORG_PREDICT_MODEL", "off")
        .env("BORG_LEARN_MODEL", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_MODE", "hook")
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_QUEEN", "off")
        .envs(env.iter().copied())
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code())
}

fn prompt(root: &Path, work: &Path, session: &str, text: &str, env: &[(&str, &str)]) -> String {
    prompt_as(root, work, "claude", session, text, env)
}

/// `prompt` through `harness`'s hook.
fn prompt_as(root: &Path, work: &Path, harness: &str, session: &str, text: &str, env: &[(&str, &str)]) -> String {
    let input = json!({"session_id": session, "cwd": work.display().to_string(), "prompt": text});
    let (out, _, code) = borg(root, root, &["hook", harness, "prompt"], &input.to_string(), env);
    assert_eq!(code, Some(0));
    serde_json::from_str::<Value>(out.trim()).ok().and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)).unwrap_or_default()
}

fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = tmp(name);
    let (work, secret) = (root.join("work/app"), root.join("secret/x"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&secret).unwrap();
    let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&work).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["config", "user.email", "alice@x"]);
    let cfg = json!({"default": "off", "off": [root.join("secret")], "collectives": [{"name": "corp", "paths": [root.join("work")], "share": "local"}]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    (root, work, secret)
}

fn store(root: &Path) -> PathBuf {
    root.join("base/collectives/corp")
}

fn jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn of_kind(path: &Path, kind: &str) -> Vec<Value> {
    jsonl(path).into_iter().filter(|v| v["kind"] == kind).collect()
}

#[test]
fn a_terminal_veto_credits_the_shown_lessons_and_records_the_reason() {
    let (root, work, _) = setup("terminal");
    prompt(&root, &work, "aaaa-1111", "fix the merge lookup", &[]);
    let asked = json!({"kind": "asked", "session": "aaaa-1111", "lessons": ["Lmerge"], "ms": 1, "repo": "app"});
    std::fs::write(store(&root).join("outcomes.jsonl"), format!("{asked}\n")).unwrap();

    let (out, err, code) = borg(&root, &work, &["no", "never", "read", "the", "commit", "log", "for", "merges"], "", &[]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("vetoed One of One (aaaa-111)") && out.contains("Lmerge") && out.contains("borg pause aaaa-111"), "{out}");
    let vetoed = of_kind(&store(&root).join("outcomes.jsonl"), "vetoed");
    assert_eq!((vetoed.len(), vetoed[0]["lessons"].clone(), vetoed[0]["session"].clone()), (1, json!(["Lmerge"]), json!("aaaa-1111")));
    let eps = of_kind(&store(&root).join("episodes.jsonl"), "veto");
    assert_eq!(eps.len(), 1);
    assert_eq!(
        (eps[0]["correction"].as_str(), eps[0]["task"].as_str(), eps[0]["engineer"].as_str()),
        (Some("never read the commit log for merges"), Some("fix the merge lookup"), Some("alice@x"))
    );
    let ev = of_kind(&store(&root).join("stats/events.jsonl"), "veto");
    assert_eq!((ev.len(), ev[0]["why"].as_str(), ev[0]["lessons"].clone()), (1, Some("cli"), json!(["Lmerge"])));
    assert!(of_kind(&store(&root).join("stats/events.jsonl"), "correction").is_empty(), "a veto is no correction");

    // --json for scripts; a second veto credits nothing new.
    let (out, _, code) = borg(&root, &work, &["no", "--json", "and not the reflog either"], "", &[]);
    assert_eq!(code, Some(0));
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!((v["session"].as_str(), v["credited"].clone()), (Some("aaaa-1111"), json!([])));
    assert_eq!(of_kind(&store(&root).join("outcomes.jsonl"), "vetoed").len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_drones_in_the_repo_need_a_session_and_bad_input_records_nothing() {
    let (root, work, _) = setup("ambiguous");
    prompt(&root, &work, "aaaa-1111", "fix the merge lookup", &[]);
    prompt(&root, &work, "bbbb-2222", "write the docs", &[]);
    let (out, err, code) = borg(&root, &work, &["no", "never read the commit log"], "", &[]);
    assert_eq!(code, Some(1));
    assert!(err.contains("2 drones work in app") && err.contains("aaaa-111") && err.contains("bbbb-222"), "{out}{err}");
    assert!(of_kind(&store(&root).join("episodes.jsonl"), "veto").is_empty());
    let (out, err, code) = borg(&root, &work, &["no", "--session", "bbbb", "never read the commit log"], "", &[]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(of_kind(&store(&root).join("episodes.jsonl"), "veto")[0]["session"], "bbbb-2222");
    // --dir from elsewhere names the repo the same way.
    let (_, _, code) = borg(&root, &root, &["no", "--dir", &work.display().to_string(), "--session", "aaaa", "not this way"], "", &[]);
    assert_eq!(code, Some(0));
    // No reason: usage.
    let (_, err, code) = borg(&root, &work, &["no", "--session", "bbbb"], "", &[]);
    assert_eq!(code, Some(2), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_off_repo_records_nothing() {
    let (root, _, secret) = setup("off");
    let (out, err, code) = borg(&root, &secret, &["no", "never read the commit log"], "", &[]);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(err.starts_with("off:"), "{err}");
    assert!(!root.join("base").exists(), "an off repo leaves no file");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_veto_said_in_the_session_tells_the_drone_and_keeps_the_task() {
    let (root, work, _) = setup("prompt");
    prompt(&root, &work, "aaaa-1111", "fix the merge lookup", &[]);
    let ctx = prompt(&root, &work, "aaaa-1111", "borg no don't touch the public schema", &[]);
    assert!(ctx.contains("said no to this work") && ctx.contains("don't touch the public schema"), "{ctx}");
    let eps = of_kind(&store(&root).join("episodes.jsonl"), "veto");
    assert_eq!((eps.len(), eps[0]["correction"].as_str()), (1, Some("don't touch the public schema")));
    assert_eq!(std::fs::read_to_string(store(&root).join("state/aaaa-1111.task")).unwrap(), "fix the merge lookup");
    assert_eq!(of_kind(&store(&root).join("stats/events.jsonl"), "veto")[0]["why"], "prompt");

    // BORG_VETO=off: an ordinary prompt.
    let ctx = prompt(&root, &work, "bbbb-2222", "borg no don't touch the public schema", &[("BORG_VETO", "off")]);
    assert!(!ctx.contains("said no"), "{ctx}");
    assert!(of_kind(&store(&root).join("episodes.jsonl"), "veto").iter().all(|e| e["session"] != "bbbb-2222"));
    let _ = std::fs::remove_dir_all(&root);
}

/// A constraint question as `borg learn` queues it from a veto (veto.rs `propose`).
fn proposal(id: &str, lesson: &str, invariant: &str, ms: i64) -> Value {
    json!({"id": id, "decision_key": format!("{id}0000000000"), "kind": "constraint",
        "question": format!("Make this a constraint of the collective? \"{invariant}\""),
        "options": ["keep it a lesson only", "ratify: make it a constraint (only an engineer can)"], "default": "keep it a lesson only",
        "evidence": [format!("invariant: {invariant}"), format!("lesson {lesson}"), "veto: no"],
        "session": "s0", "cwd": "", "repo": "app", "engineer": "alice@x", "ms": ms})
}

#[test]
fn a_ratified_constraint_is_a_standing_lesson_of_the_next_prompt_and_a_proposal_is_not() {
    let (root, work, _) = setup("ratified");
    let dir = store(&root);
    std::fs::create_dir_all(dir.join("queen")).unwrap();
    let lesson = |id: &str, scope: &[&str]| json!({"id": id, "origin_ms": 1, "question": "Is the lookup cached?", "repo": "app", "scope": scope});
    std::fs::write(dir.join("lessons.jsonl"), format!("{}\n{}\n", lesson("Lone", &[]), lesson("Lorion", &["Orion"]))).unwrap();
    let (anywhere, orion, pending) =
        ("merge lookups go through the review API", "the Orion net never resets its budget place", "the public schema changes only additively");
    let queue = [proposal("q1", "Lone", anywhere, 2), proposal("q2", "Lorion", orion, 3), proposal("q3", "Lone", pending, 4)];
    std::fs::write(dir.join("queen/queue.jsonl"), queue.iter().map(|q| format!("{q}\n")).collect::<String>()).unwrap();
    // Before any answer: nothing is a constraint, and no prompt hears of one as a lesson (the queen may
    // ask the proposal itself).
    let standing = |ctx: &str, text: &str| ctx.contains(&format!("- {text} (a constraint this collective ratified)"));
    let ctx = prompt(&root, &work, "aaaa-1111", "write the release notes", &[]);
    assert!(!ctx.contains("(a constraint this collective ratified)"), "{ctx}");
    for key in ["q1", "q2"] {
        let (out, err, code) = borg(&root, &work, &["queen", "answer", key, "b"], "", &[]);
        assert_eq!(code, Some(0), "{out}{err}");
        assert!(out.contains("constraint added"), "{out}");
    }
    // The next session's first prompt, on unrelated work: the unscoped constraint comes, matched or not;
    // the scoped one waits for its scope; the open proposal never comes.
    let ctx = prompt(&root, &work, "bbbb-2222", "write the release notes", &[]);
    assert!(standing(&ctx, anywhere) && !standing(&ctx, orion) && !standing(&ctx, pending), "{ctx}");
    let ctx = prompt(&root, &work, "bbbb-2222", "now tidy the Orion net", &[]);
    assert!(standing(&ctx, orion) && !standing(&ctx, anywhere) && !standing(&ctx, pending), "once per session, in its scope: {ctx}");
    let line: Value = serde_json::from_str(std::fs::read_to_string(dir.join("constraints.jsonl")).unwrap().lines().next().unwrap()).unwrap();
    assert!(line.get("veto").is_none(), "the synced line holds no raw veto words: {line}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The ratified constraints `lines` (`id`, invariant, the lesson it came from) in the store, as `borg
/// queen answer` writes them (veto.rs `ratify`).
fn ratified(root: &Path, lines: &[(&str, &str, &str)]) {
    let text: String = lines
        .iter()
        .map(|(id, text, lesson)| format!("{}\n", json!({"id": id, "text": text, "lesson": lesson, "repo": "app", "by": "lead@x", "ms": 2, "source": "veto"})))
        .collect();
    std::fs::write(store(root).join("constraints.jsonl"), text).unwrap();
}

const RATIFIED: &str = "(a constraint this collective ratified)";

/// An unscoped constraint makes the first prompt its topic beside a named scope, but only for itself:
/// a consolidated generic lesson (which comes at a first prompt that names no scope) stays out, and the
/// named scope's lessons come as they did without the constraint.
#[test]
fn an_unscoped_constraint_beside_a_named_scope_brings_no_generic_lesson() {
    let (root, work, _) = setup("beside");
    let dir = store(&root);
    std::fs::create_dir_all(&dir).unwrap();
    let lessons = [
        json!({"id": "Lorion", "origin_ms": 1, "question": "Is the Orion lookup cached before the loop?", "repo": "app", "scope": ["Orion"]}),
        json!({"id": "Lgen", "origin_ms": 1, "question": "Is the lookup cached before the loop?", "repo": "app", "engineers": ["alice@x", "bob@x"]}),
    ];
    std::fs::write(dir.join("lessons.jsonl"), lessons.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    // Changed in three sessions by two engineers: consolidated (memory.rs).
    let outcomes: String = (0..3)
        .map(|i| format!("{}\n", json!({"kind": "asked_then_changed", "session": format!("old{i}"), "lessons": ["Lgen"], "ms": i, "repo": "app"})))
        .collect();
    std::fs::write(dir.join("outcomes.jsonl"), outcomes).unwrap();
    // The control: with no scope named, the consolidated lesson comes at the first prompt.
    let ctx = prompt(&root, &work, "cccc-0000", "cache the lookup before the loop", &[]);
    assert!(ctx.contains("Is the lookup cached before the loop?"), "{ctx}");
    let ctx = prompt(&root, &work, "dddd-0000", "cache the Orion lookup before the loop", &[]);
    assert!(ctx.contains("Is the Orion lookup cached") && !ctx.contains("- Is the lookup cached"), "without a constraint: {ctx}");
    ratified(&root, &[("c1", "merge lookups go through the review API", "Lgen")]);
    let ctx = prompt(&root, &work, "eeee-0000", "cache the Orion lookup before the loop", &[]);
    assert!(ctx.contains(&format!("- merge lookups go through the review API {RATIFIED}")), "{ctx}");
    assert!(ctx.contains("Is the Orion lookup cached") && !ctx.contains("- Is the lookup cached"), "the generic lesson stays out: {ctx}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Every constraint comes once its scope is in play, past the early caps: more unscoped ones than one
/// delivery holds come at the next prompts, and a scoped one comes after the session's last plain
/// early delivery.
#[test]
fn every_constraint_comes_past_the_early_caps() {
    let (root, work, _) = setup("caps");
    let dir = store(&root);
    std::fs::create_dir_all(&dir).unwrap();
    let lesson = |id: &str, scope: &[&str]| json!({"id": id, "origin_ms": 1, "question": "Is the lookup cached?", "repo": "app", "scope": scope});
    std::fs::write(dir.join("lessons.jsonl"), format!("{}\n{}\n", lesson("Lone", &[]), lesson("Lorion", &["Orion"]))).unwrap();
    let unscoped: Vec<String> = (0..7).map(|i| format!("unscoped rule number {i} holds")).collect();
    let mut lines: Vec<(String, String, &str)> = unscoped.iter().enumerate().map(|(i, t)| (format!("u{i}"), t.clone(), "Lone")).collect();
    lines.push(("o1".into(), "the Orion net never resets its budget place".into(), "Lorion"));
    ratified(&root, &lines.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), *c)).collect::<Vec<_>>());
    let mut heard = String::new();
    for p in ["write the release notes", "and the changelog", "and the summary"] {
        heard.push_str(&prompt(&root, &work, "ffff-0000", p, &[]));
    }
    assert!(unscoped.iter().all(|t| heard.contains(&format!("- {t} {RATIFIED}"))), "{heard}");
    assert_eq!(heard.matches(RATIFIED).count(), 7, "each once: {heard}");
    // The session's plain early deliveries are used up: the scoped constraint still comes on its scope.
    let state = dir.join("state/ffff-0000.ctx");
    let mut f = std::fs::OpenOptions::new().append(true).open(&state).unwrap();
    writeln!(f, "#borg-early x1\n#borg-early x2\n#borg-early x3").unwrap();
    let ctx = prompt(&root, &work, "ffff-0000", "now tidy the Orion net", &[]);
    assert!(ctx.contains(&format!("- the Orion net never resets its budget place {RATIFIED}")), "{ctx}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Off Claude Code (no early delivery of plain lessons there), a constraint still comes with the prompt
/// on every harness that takes a prompt's context, matched or not; plain lessons stay with the Stop.
#[test]
fn a_constraint_comes_on_every_harness() {
    for harness in ["omp", "opencode", "codex"] {
        let (root, work, _) = setup(&format!("harness-{harness}"));
        let dir = store(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let lesson = json!({"id": "Lorion", "origin_ms": 1, "question": "Is the Orion lookup cached?", "repo": "app", "scope": ["Orion"]});
        std::fs::write(dir.join("lessons.jsonl"), format!("{lesson}\n")).unwrap();
        let session = format!("{harness}-0000");
        assert!(!prompt_as(&root, &work, harness, &session, "cache the Orion lookup", &[]).contains("Is the Orion lookup cached"), "{harness}");
        ratified(&root, &[("o1", "the Orion net never resets its budget place", "Lorion")]);
        let ctx = prompt_as(&root, &work, harness, &format!("{harness}-1111"), "cache the Orion lookup", &[]);
        assert!(ctx.contains(&format!("- the Orion net never resets its budget place {RATIFIED}")), "{harness}: {ctx}");
        assert!(!ctx.contains("Is the Orion lookup cached"), "{harness}: plain lessons wait for the Stop: {ctx}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
