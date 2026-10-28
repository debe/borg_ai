//! The queen end to end through the binary: the Stop hook asks in the routed collective only, `borg queen`
//! lists and answers there, and the answer reaches the next drone through the conscience.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-queen-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap() // macOS: /var -> /private/var
}

/// borg with a clean environment: only the given store, config and a temp HOME (git reads no user config).
fn borg(root: &Path, base: &Path, args: &[&str], stdin: &str, dir: &Path) -> (String, String, Option<i32>) {
    borg_queen(root, base, args, stdin, dir, Some("on")) // off by default; these tests exercise it
}

/// `borg` with BORG_QUEEN set to `queen`, or unset.
fn borg_queen(root: &Path, base: &Path, args: &[&str], stdin: &str, dir: &Path, queen: Option<&str>) -> (String, String, Option<i32>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", base)
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_MATCH", "bm25") // word matching only: no model call from a test
        .env("BORG_NOTIFY", "off") // never a desktop notification from a test
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(q) = queen {
        c.env("BORG_QUEEN", q);
    }
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into(), out.status.code())
}

fn repo(path: &Path, email: &str) {
    std::fs::create_dir_all(path).unwrap();
    let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(path).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["config", "user.email", email]);
}

fn stop(session: &str, cwd: &Path, plan: &str) -> String {
    serde_json::json!({"session_id": session, "cwd": cwd, "last_assistant_message": plan, "stop_hook_active": false}).to_string()
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            out.extend(files(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

const PLAN: &str = "Plan: rewrite the commit messages, then force-push the rewritten history to origin main.";

#[test]
fn queen_asks_in_the_routed_store_and_the_answer_reaches_the_conscience() {
    let root = tmp("routed");
    let base = root.join("store");
    let (corp, private, secret) = (root.join("corp/app"), root.join("priv/app"), root.join("secret/app"));
    for (p, e) in [(&corp, "c@x"), (&private, "p@x"), (&secret, "s@x")] {
        repo(p, e);
    }
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        serde_json::json!({"default": "off", "off": [root.join("secret")],
            "collectives": [{"name": "corp", "paths": [root.join("corp")]}, {"name": "mine", "paths": [root.join("priv")], "share": "local"}]})
        .to_string(),
    )
    .unwrap();
    let hook = ["hook", "claude", "stop"];

    // An off repo: nothing asked, nothing written.
    let (out, _, code) = borg(&root, &base, &hook, &stop("s-off", &secret, PLAN), &secret);
    assert_eq!((out.as_str(), code), ("", Some(0)));
    assert!(files(&base).is_empty(), "{:?}", files(&base));

    // A private repo: the queen is asked, once, in the private store only.
    let (out, _, _) = borg(&root, &base, &hook, &stop("s1", &private, PLAN), &private);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["decision"], "block");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("(p@x)") && reason.contains("proceed with default: proceed as planned"), "{reason}");
    assert!(base.join("collectives/mine/queen/queue.jsonl").exists());
    assert!(!base.join("collectives/corp/queen").exists());

    // `borg queen` lists it for the private collective only, numbered; `show` has the rest.
    let (list, _, _) = borg(&root, &base, &["queen"], "", &private);
    assert!(list.starts_with("app (1)\n  1  ") && list.contains("force-push"), "{list}");
    let (shown, _, _) = borg(&root, &base, &["queen", "show", "1"], "", &private);
    assert!(shown.starts_with("#1  q") && shown.contains("p@x") && shown.contains("status:   open"), "{shown}");
    let (corp_list, _, _) = borg(&root, &base, &["queen", "--dir", corp.to_str().unwrap()], "", &private);
    assert!(corp_list.contains("no open queen questions"), "{corp_list}");
    let (off_list, _, _) = borg(&root, &base, &["queen"], "", &secret);
    assert!(off_list.starts_with("off:"), "{off_list}");
    let id = shown.split_whitespace().nth(1).unwrap().to_string();

    // The engineer answers; the answer is a lesson of the private collective.
    let (said, err, code) = borg(&root, &base, &["queen", "answer", &id, "Never force-push main; push a new branch."], "", &private);
    assert_eq!(code, Some(0), "{said}{err}");
    let lessons = std::fs::read_to_string(base.join("collectives/mine/lessons.jsonl")).unwrap();
    assert!(lessons.contains("\"author\":\"p@x\"") && lessons.contains("decision_key"), "{lessons}");
    let (list, _, _) = borg(&root, &base, &["queen"], "", &private);
    assert!(list.contains("no open queen questions"), "{list}");

    // A later drone with the same plan gets the answer from the conscience, not the queen.
    let (out, _, _) = borg(&root, &base, &hook, &stop("s9", &private, PLAN), &private);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.starts_with("Before you finish") && reason.contains("Never force-push main"), "{reason}");
    assert_eq!(std::fs::read_to_string(base.join("collectives/mine/queen/queue.jsonl")).unwrap().lines().count(), 2, "asked + answered");
}

#[test]
fn the_hook_fails_open_when_the_store_is_unusable() {
    let root = tmp("failopen");
    let project = root.join("p");
    repo(&project, "e@x");
    std::fs::create_dir_all(root.join("home")).unwrap();
    // No collectives config: the default collective lives at BORG_HOME, which here is a file.
    let base = root.join("store-is-a-file");
    std::fs::write(&base, "x").unwrap();
    let (out, _, code) = borg(&root, &base, &["hook", "claude", "stop"], &stop("s1", &project, PLAN), &project);
    assert_eq!((out.as_str(), code), ("", Some(0)));
}

#[test]
fn contradicting_lessons_the_conscience_asks_also_go_to_the_queen() {
    let root = tmp("conflict");
    let project = root.join("p");
    repo(&project, "e@x");
    std::fs::create_dir_all(root.join("home")).unwrap();
    // No collectives config: the default collective lives at BORG_HOME.
    let base = root.join("store");
    std::fs::create_dir_all(&base).unwrap();
    let lesson = |id: &str, situation: &str, consequence: &str| {
        serde_json::json!({"id": id, "origin_ms": 1, "question": format!("Before you force-push the rewritten history to origin main: {situation}?"),
            "situations": [situation], "consequence": consequence})
        .to_string()
    };
    std::fs::write(
        base.join("lessons.jsonl"),
        format!(
            "{}\n{}\n",
            lesson("a", "force-push the rewritten history to origin main", "the team loses commits; never do it"),
            lesson("b", "force-push rewritten history to main", "fine after a squash; CI rebuilds cleanly")
        ),
    )
    .unwrap();
    let (out, err, _) = borg(&root, &base, &["hook", "claude", "stop"], &stop("s1", &project, PLAN), &project);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("{out}{err}"));
    let reason = v["reason"].as_str().unwrap();
    // One block: the conscience's questions first, then the queen's.
    assert!(reason.starts_with("Before you finish") && reason.contains("Borg asked the engineer (e@x)"), "{reason}");
    let queue = std::fs::read_to_string(base.join("queen/queue.jsonl")).unwrap();
    assert!(queue.contains("\"disagreeing\":[\"a\",\"b\"]"), "{queue}");
    // The conscience's ask is still tracked for outcomes.
    assert!(std::fs::read_to_string(base.join("outcomes.jsonl")).unwrap().contains("\"a\""));
}

#[test]
fn the_engineers_prompts_state_a_move_but_the_drones_own_tool_calls_do_not() {
    let root = tmp("stated");
    let project = root.join("p");
    repo(&project, "e@x");
    std::fs::create_dir_all(root.join("home")).unwrap();
    // No collectives config: the default collective lives at BORG_HOME.
    let base = root.join("store");
    std::fs::create_dir_all(&base).unwrap();
    let pid = std::process::id();
    let event = |name: &str, v: serde_json::Value| borg(&root, &base, &["hook", "claude", name], &v.to_string(), &project);

    // The engineer asked for the force-push: the inputs state it, the queen stays quiet.
    let asked = format!("stated-prompt-{pid}");
    event("prompt", serde_json::json!({"session_id": asked, "cwd": project, "prompt": "Squash the branch and force-push it to origin main, I approve."}));
    let (out, _, _) = event("stop", serde_json::from_str(&stop(&asked, &project, PLAN)).unwrap());
    assert_eq!(out, "", "the engineer stated it");

    // The drone ran a force-push itself: its own tool call does not state the decision.
    let ran = format!("stated-touch-{pid}");
    event("prompt", serde_json::json!({"session_id": ran, "cwd": project, "prompt": "Clean up the commit messages on this branch."}));
    event("post_tool", serde_json::json!({"session_id": ran, "cwd": project, "tool_name": "Bash", "tool_input": {"command": "git push --force origin main"}}));
    let (out, _, _) = event("stop", serde_json::from_str(&stop(&ran, &project, PLAN)).unwrap());
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("{out}"));
    assert!(v["reason"].as_str().unwrap().contains("force-push"), "{out}");
}

#[test]
fn a_lesson_is_asked_once_per_session() {
    let root = tmp("once");
    let base = root.join("store");
    let corp = root.join("corp/app");
    repo(&corp, "c@x");
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        serde_json::json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("corp")]}]}).to_string(),
    )
    .unwrap();
    // A routine plan (nothing irreversible), so the queen stays out of it and only the conscience speaks.
    let plan = "Plan: the relay reads a shared closed flag from two threads before delivering to the socket.";
    let lesson = serde_json::json!({"id": "Lflag", "origin_ms": 1, "question": "Is the shared closed flag read and written atomically across threads?",
        "situations": ["read a shared flag from two threads"], "consequence": "a check-then-act race delivers after close"});
    std::fs::create_dir_all(base.join("collectives/corp")).unwrap();
    std::fs::write(base.join("collectives/corp/lessons.jsonl"), format!("{lesson}\n")).unwrap();
    let hook = ["hook", "claude", "stop"];
    let (first, _, _) = borg(&root, &base, &hook, &stop("s1", &corp, plan), &corp);
    assert!(first.contains("closed flag read and written atomically"), "asked at the first turn end: {first}");
    let (second, _, _) = borg(&root, &base, &hook, &stop("s1", &corp, plan), &corp);
    assert!(!second.contains("closed flag read and written atomically"), "not again in the same session: {second}");
    let (other, _, _) = borg(&root, &base, &hook, &stop("s2", &corp, plan), &corp);
    assert!(other.contains("closed flag read and written atomically"), "a new session is asked again: {other}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_queen_is_off_unless_borg_queen_is_on() {
    let root = tmp("default-off");
    let project = root.join("p");
    repo(&project, "e@x");
    std::fs::create_dir_all(root.join("home")).unwrap();
    let base = root.join("store");
    std::fs::create_dir_all(&base).unwrap();
    let hook = ["hook", "claude", "stop"];
    // The plan that makes the queen ask when it is on (`queen_asks_in_the_routed_store_...`).
    for (session, queen) in [("s-unset", None), ("s-off", Some("off")), ("s-yes", Some("yes"))] {
        let (out, _, code) = borg_queen(&root, &base, &hook, &stop(session, &project, PLAN), &project, queen);
        assert_eq!((out.as_str(), code), ("", Some(0)), "BORG_QUEEN={queen:?}");
    }
    assert!(!base.join("queen/queue.jsonl").exists(), "nothing queued");
    let (out, _, _) = borg_queen(&root, &base, &hook, &stop("s-on", &project, PLAN), &project, Some("on"));
    assert!(out.contains("Borg asked the engineer (e@x)"), "{out}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_queue_answers_by_number_dismisses_and_speaks_json() {
    let root = tmp("json");
    let project = root.join("app");
    repo(&project, "e@x");
    std::fs::create_dir_all(root.join("home")).unwrap();
    // No collectives config: the default collective lives at BORG_HOME.
    let base = root.join("store");
    std::fs::create_dir_all(base.join("queen")).unwrap();
    let entry = |id: &str, q: &str| {
        serde_json::json!({"id": id, "decision_key": format!("k{id}"), "kind": "open_decision", "question": q, "options": ["monthly", "quarterly"],
            "default": "monthly", "evidence": ["docs/a.md:1: open"], "session": "import", "cwd": project, "engineer": "e@x", "ms": 1})
        .to_string()
    };
    let questions = [entry("qa", "Do service tokens rotate monthly or quarterly?"), entry("qb", "Which retry budget does the webhook consumer get?")];
    std::fs::write(base.join("queen/queue.jsonl"), questions.join("\n") + "\n").unwrap();

    let (out, _, code) = borg(&root, &base, &["queen", "--json"], "", &project);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (v[0]["n"].as_u64(), v[1]["id"].as_str(), v[1]["answer_cmd"].as_str(), code),
        (Some(1), Some("qb"), Some("borg queen answer 'qb' '<answer>'"), Some(0))
    );
    let (out, _, _) = borg(&root, &base, &["queen", "--json", "--repo", "app"], "", &project);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap()[0]["id"], "qa");

    // An option's letter answers with its text; a number names the question.
    let (_, err, code) = borg(&root, &base, &["queen", "answer", "1", "b"], "", &project);
    assert_eq!(code, Some(0), "{err}");
    assert!(std::fs::read_to_string(base.join("lessons.jsonl")).unwrap().contains("\"consequence\":\"quarterly\""));
    // Numbers follow the open list: qb is now 1.
    let (out, _, code) = borg(&root, &base, &["queen", "dismiss", "1", "not", "ours"], "", &project);
    assert_eq!((out.trim(), code), ("qb dismissed (not ours); no lesson written", Some(0)));
    let (out, _, _) = borg(&root, &base, &["queen", "--json"], "", &project);
    assert_eq!(out.trim(), "[]");
    let (out, _, _) = borg(&root, &base, &["queen"], "", &project);
    assert!(out.starts_with("no open queen questions"), "{out}");
    // `ask` wants a terminal.
    let (_, err, code) = borg(&root, &base, &["queen", "ask"], "", &project);
    assert_eq!(code, Some(2), "{err}");
    let _ = std::fs::remove_dir_all(root);
}
