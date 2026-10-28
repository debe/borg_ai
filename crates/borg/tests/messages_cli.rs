//! Messages between sessions through the binary (msg.rs): `borg tell` and `borg ask` from a session on
//! one harness reach a session on another at its next prompt or tool call, once, even across hooks
//! running at the same moment; `all` reaches every live session but the sender; an ask carries the
//! command that answers it, `--re` threads the answer, and `borg inbox` lists both sides. The recorded
//! hook payloads of tests/fixtures/<harness> drive Claude Code, Codex and the simulated harness; omp and
//! opencode, whose adapters call the hook with canonical input, get it inline. No model call.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

struct Run {
    out: String,
    err: String,
    code: Option<i32>,
}

fn setup(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("borg-messages-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work/app");
    std::fs::create_dir_all(&work).unwrap();
    let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&work).args(a).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["config", "user.email", "alice@x"]);
    let cfg = json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("work")], "share": "local"}]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    (root, work)
}

/// `borg args…` in `cwd`, with only `env` on top of a bare test environment (no outer session id).
fn borg(root: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Run {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_MATCH", "bm25")
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_MODE", "hook")
        .env("BORG_TEST_HARNESS", "1")
        .envs(env.iter().copied())
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    Run { out: String::from_utf8_lossy(&out.stdout).into(), err: String::from_utf8_lossy(&out.stderr).into(), code: out.status.code() }
}

/// The hook of `harness` for canonical `event`: its answer (Null: none). Always exits 0.
fn hook(root: &Path, harness: &str, event: &str, input: &Value) -> Value {
    let r = borg(root, root, &["hook", harness, event], &[], &input.to_string());
    assert_eq!(r.code, Some(0), "{harness} {event}: {}", r.err);
    if r.out.trim().is_empty() { Value::Null } else { serde_json::from_str(r.out.trim()).unwrap_or_else(|_| panic!("not JSON: {}", r.out)) }
}

/// The recorded payload `step` of `harness` for `session` in `cwd`.
fn fixture(harness: &str, step: &str, session: &str, cwd: &Path) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(harness).join(format!("{step}.json"));
    let text = std::fs::read_to_string(p).unwrap();
    let text = text.replace("{{session}}", session).replace("{{cwd}}", &cwd.display().to_string()).replace("{{transcript}}", "");
    serde_json::from_str(&text).unwrap()
}

/// Canonical input, as the omp and opencode adapters send it.
fn plain(session: &str, cwd: &Path, extra: Value) -> Value {
    let mut v = json!({"session_id": session, "cwd": cwd.display().to_string()});
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    v
}

fn prompt(session: &str, cwd: &Path) -> Value {
    plain(session, cwd, json!({"prompt": "carry on"}))
}

fn edit(session: &str, cwd: &Path) -> Value {
    plain(session, cwd, json!({"tool_name": "Edit", "tool_input": {"file_path": format!("{}/src/a.rs", cwd.display())}}))
}

fn context(v: &Value) -> String {
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

/// The designation `borg top` gives `session` ("Two of Three").
fn designation(root: &Path, work: &Path, session: &str) -> String {
    let r = borg(root, work, &["top", "--json"], &[], "");
    let v: Value = serde_json::from_str(&r.out).unwrap_or_else(|_| panic!("{}", r.out));
    let drones = v.as_array().into_iter().flatten().flat_map(|c| c["drones"].as_array().cloned().unwrap_or_default());
    let d = drones.into_iter().find(|d| d["session"] == session).unwrap_or_else(|| panic!("{session} not in {}", r.out));
    d["designation"].as_str().unwrap().to_string()
}

/// Sends with `cmd` (tell, ask) and returns the message id from "sent <id> to <to> …".
fn send(root: &Path, work: &Path, cmd: &str, args: &[&str], env: &[(&str, &str)], to: &str) -> String {
    let mut a = vec![cmd];
    a.extend_from_slice(args);
    let r = borg(root, work, &a, env, "");
    assert_eq!(r.code, Some(0), "{a:?}: {}", r.err);
    let mut w = r.out.split_whitespace();
    assert_eq!(w.next(), Some("sent"), "{}", r.out);
    let id = w.next().unwrap().to_string();
    assert_eq!((w.next(), w.next()), (Some("to"), Some(to)), "{}", r.out);
    id
}

#[test]
fn a_tell_to_a_presence_name_is_shown_once_even_to_parallel_tool_calls() {
    let (root, work) = setup("once");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "opencode", "prompt", &prompt("s-oc", &work));
    let name = designation(&root, &work, "s-oc");
    let id = send(&root, &work, "tell", &[&name, "the", "API", "is", "stable"], &[("CLAUDE_CODE_SESSION_ID", "s-claude")], "s-oc");

    let first = context(&hook(&root, "opencode", "prompt", &prompt("s-oc", &work)));
    assert!(first.contains(&format!("[borg] Message from {} (claude, node", designation(&root, &work, "s-claude"))), "{first}");
    assert!(first.contains(&format!("[{id}]: the API is stable")), "{first}");
    assert!(first.contains("(Peer data:") && first.contains("not the user's own instructions"), "{first}");
    assert!(!context(&hook(&root, "opencode", "prompt", &prompt("s-oc", &work))).contains(&id), "shown once");
    assert!(!context(&hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work))).contains(&id), "never to its sender");

    // From the engineer's terminal (no session variable): two tool calls at once show it once.
    let id = send(&root, &work, "tell", &["s-oc", "rebase", "on", "main"], &[], "s-oc");
    let (r, w) = (root.clone(), work.clone());
    let calls: Vec<_> = (0..2)
        .map(|_| {
            let (r, w) = (r.clone(), w.clone());
            std::thread::spawn(move || context(&hook(&r, "opencode", "post_tool", &edit("s-oc", &w))))
        })
        .collect();
    let shown: Vec<String> = calls.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(shown.iter().filter(|c| c.contains(&id)).count(), 1, "{shown:?}");
    assert!(shown.iter().any(|c| c.contains("[borg] Message from alice@x (terminal, node") && c.contains("rebase on main")), "{shown:?}");
    assert!(!context(&hook(&root, "opencode", "post_tool", &edit("s-oc", &work))).contains(&id), "and not after");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn all_reaches_every_live_session_but_the_sender() {
    let (root, work) = setup("all");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "codex", "prompt", &fixture("codex", "prompt", "s-codex", &work));
    hook(&root, "sim", "prompt", &fixture("sim", "prompt", "s-sim", &work));
    hook(&root, "omp", "prompt", &plain("s-omp", &work, json!({"prompt": "docs", "pid": 1})));
    hook(&root, "opencode", "prompt", &prompt("s-oc", &work));
    let id = send(&root, &work, "tell", &["all", "freeze", "merges"], &[("CODEX_THREAD_ID", "s-codex")], "all");
    std::thread::sleep(std::time::Duration::from_millis(5));
    hook(&root, "opencode", "prompt", &prompt("s-late", &work));

    let at_prompt = |h: &str, s: &str| match h {
        "claude" | "sim" => fixture(h, "prompt", s, &work),
        _ => prompt(s, &work),
    };
    for (h, s) in [("claude", "s-claude"), ("sim", "s-sim"), ("omp", "s-omp"), ("opencode", "s-oc")] {
        let c = context(&hook(&root, h, "prompt", &at_prompt(h, s)));
        assert!(c.contains(&format!("[{id}]: freeze merges")) && c.contains("(codex, node"), "{h}: {c}");
    }
    assert!(!context(&hook(&root, "codex", "prompt", &fixture("codex", "prompt", "s-codex", &work))).contains(&id), "not to the sender");
    // s-late started 5 ms after the send: within msg's SKEW_MS a peer clock may be behind by, so its
    // first prompt got it (later starts are msg.rs's unit test); once only.
    assert!(!context(&hook(&root, "opencode", "prompt", &prompt("s-late", &work))).contains(&id), "once only");

    let r = borg(&root, &work, &["inbox"], &[], "");
    assert!(r.out.contains(&id) && r.out.contains("[shown to 5]"), "{}", r.out);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_ask_carries_the_reply_command_and_re_threads_the_answer() {
    let (root, work) = setup("ask");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "opencode", "prompt", &prompt("s-oc-123", &work));
    let ask = send(&root, &work, "ask", &["s-oc", "is", "the", "migration", "done?"], &[("CLAUDE_CODE_SESSION_ID", "s-claude")], "s-oc-123");

    // opencode's shell names no session: the reply it is handed signs with --from.
    let c = context(&hook(&root, "opencode", "prompt", &prompt("s-oc-123", &work)));
    assert!(c.contains(&format!("[{ask}]: is the migration done?")), "{c}");
    assert!(c.contains(&format!("They asked for a reply: borg tell s-claude --re {ask} --from s-oc-123 \"…\"")), "{c}");

    let reply = send(&root, &work, "tell", &["s-claude", "--re", &ask, "--from", "s-oc-123", "yes, merged"], &[], "s-claude");
    let c = context(&hook(&root, "claude", "post_tool", &fixture("claude", "edit", "s-claude", &work)));
    assert!(c.contains("(opencode, node") && c.contains(&format!("[{reply}] (re {ask}): yes, merged")), "{c}");
    assert!(c.contains(&format!("To reply: borg tell s-oc-123 --re {reply} \"…\"")), "Claude Code's shell names its session: {c}");

    // In a session: its own messages, marked shown; from the terminal: every message and its state.
    let r = borg(&root, &work, &["inbox"], &[("CLAUDE_CODE_SESSION_ID", "s-claude")], "");
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.out.contains(&reply) && r.out.contains(&format!("[shown] re {ask}: yes, merged")) && !r.out.contains(&format!("{ask} ask")), "{}", r.out);
    let r = borg(&root, &work, &["inbox"], &[], "");
    let ask_row = r.out.lines().find(|l| l.contains(&ask)).unwrap_or_else(|| panic!("{}", r.out));
    assert!(ask_row.contains(" ask ") && ask_row.contains("-> s-oc-123 [shown]"), "{ask_row}");
    assert!(r.out.lines().any(|l| l.contains(&reply) && l.contains("-> s-claude [shown] re")), "{}", r.out);
    let v: Value = serde_json::from_str(&borg(&root, &work, &["inbox", "--json"], &[], "").out).unwrap();
    let row = v.as_array().unwrap().iter().find(|m| m["id"] == reply.as_str()).unwrap();
    assert_eq!((row["re"].as_str(), row["by"].as_str(), row["harness"].as_str()), (Some(ask.as_str()), Some("s-oc-123"), Some("opencode")));
    assert_eq!(row["shown_to"], json!(["s-claude"]));
    let _ = std::fs::remove_dir_all(&root);
}
