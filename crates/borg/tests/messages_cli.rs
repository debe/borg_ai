//! Messages between sessions through the binary (msg.rs): `borg tell` and `borg ask` from a session on
//! one harness reach a session on another at its next prompt or tool call, once, even across hooks
//! running at the same moment; `local` reaches every live session on this machine but the sender,
//! and `collective` and `all` are written `all` (every session of the collective); an ask carries the
//! command that answers it, `--re` threads the answer, and `borg inbox` lists both sides. A sender may
//! have a window of unread messages to one recipient, which its recipient's seeing them (or replying)
//! opens; a message longer than one event shows its head and `borg inbox --show`. The recorded hook
//! payloads of tests/fixtures/<harness> drive Claude Code, Codex and the simulated harness; omp and
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
fn local_reaches_every_live_session_here_but_the_sender() {
    let (root, work) = setup("local");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "codex", "prompt", &fixture("codex", "prompt", "s-codex", &work));
    hook(&root, "sim", "prompt", &fixture("sim", "prompt", "s-sim", &work));
    hook(&root, "omp", "prompt", &plain("s-omp", &work, json!({"prompt": "docs", "pid": 1})));
    hook(&root, "opencode", "prompt", &prompt("s-oc", &work));
    let id = send(&root, &work, "tell", &["local", "freeze", "merges"], &[("CODEX_THREAD_ID", "s-codex")], "local");
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
    assert!(r.out.lines().any(|l| l.contains(&id) && l.contains("-> local [")), "{}", r.out);

    // `local` is written for this machine's node; `collective` and its synonym `all` as `all`.
    let collective = send(&root, &work, "tell", &["collective", "staging", "is", "down"], &[], "collective");
    let all = send(&root, &work, "tell", &["all", "staging", "is", "back"], &[("CODEX_THREAD_ID", "s-codex")], "collective");
    let v: Value = serde_json::from_str(&borg(&root, &work, &["inbox", "--json"], &[], "").out).unwrap();
    let to = |id: &str| v.as_array().unwrap().iter().find(|m| m["id"] == id).map(|m| m["to"].as_str().unwrap_or_default().to_string()).unwrap();
    assert!(to(&id).starts_with("local@") && to(&id).len() > 6, "{v}");
    assert_eq!((to(&collective), to(&all)), ("all".into(), "all".into()), "{v}");
    let c = context(&hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work)));
    assert!(
        c.contains(&format!("[{collective}]: staging is down")) && c.contains(&format!("[{all}]: staging is back")),
        "the collective includes this machine: {c}"
    );
    let r = borg(&root, &work, &["inbox"], &[], "");
    assert!(r.out.lines().any(|l| l.contains(&collective) && l.contains("-> collective [")), "{}", r.out);
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
    let answer = hook(&root, "claude", "post_tool", &fixture("claude", "edit", "s-claude", &work));
    assert!(answer.get("shown_ids").is_none(), "Claude Code gets only the keys it defines: {answer}");
    let c = context(&answer);
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

/// The collective's store (`BORG_HOME`/collectives/<name>).
fn store(root: &Path) -> PathBuf {
    root.join("base/collectives/corp")
}

/// The lines of the messages log whose kind is `kind`.
fn logged(root: &Path, kind: &str) -> usize {
    let text = std::fs::read_to_string(store(root).join("messages.jsonl")).unwrap_or_default();
    text.lines().filter(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v["kind"] == kind)).count()
}

/// The unread count `borg top` gives `session`.
fn unread(root: &Path, work: &Path, session: &str) -> u64 {
    let v: Value = serde_json::from_str(&borg(root, work, &["top", "--json"], &[], "").out).unwrap();
    let drones = v.as_array().into_iter().flatten().flat_map(|c| c["drones"].as_array().cloned().unwrap_or_default());
    drones.into_iter().find(|d| d["session"] == session).and_then(|d| d["unread"].as_u64()).unwrap_or_else(|| panic!("{session}: {v}"))
}

/// A sender may have one unread message to a recipient at first: the next is refused, saying how many
/// are unread. Being shown it opens the window wider, a reply acknowledges what it answers, and the
/// engineer's terminal inbox acknowledges what it lists for the engineer. The hook's answer names what
/// it showed; an acknowledgement is no message and changes no count.
#[test]
fn the_window_closes_on_unread_messages_and_an_acknowledgement_or_a_reply_opens_it() {
    let (root, work) = setup("window");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "opencode", "prompt", &prompt("s-oc", &work));
    let me = [("CLAUDE_CODE_SESSION_ID", "s-claude")];
    let refused = |text: &str, want: &str| {
        let r = borg(&root, &work, &["tell", "s-oc", text], &me, "");
        assert_eq!(r.code, Some(1), "{}", r.out);
        assert!(r.err.starts_with(&format!("borg tell: {want} unread by s-oc:")), "{}", r.err);
    };
    let one = send(&root, &work, "tell", &["s-oc", "one"], &me, "s-oc");
    refused("two", "1");
    let shown = hook(&root, "opencode", "prompt", &prompt("s-oc", &work));
    assert!(context(&shown).contains(&format!("[{one}]: one")), "{shown}");
    assert_eq!(shown["shown_ids"], json!([one]), "an adapter learns which, to drop the wake that names it: {shown}");
    assert_eq!(logged(&root, "seen"), 1, "the delivery acknowledged it");
    let two = send(&root, &work, "tell", &["s-oc", "two"], &me, "s-oc");
    send(&root, &work, "tell", &["s-oc", "three"], &me, "s-oc");
    refused("four", "2");
    // A reply acknowledges what it answers, unshown as it is.
    send(&root, &work, "tell", &["s-claude", "--re", &two, "--from", "s-oc", "got it"], &[], "s-claude");
    send(&root, &work, "tell", &["s-oc", "four"], &me, "s-oc");

    // Acknowledgements are no messages: every count and list is of messages alone.
    let v: Value = serde_json::from_str(&borg(&root, &work, &["inbox", "--json"], &[], "").out).unwrap();
    let kinds: Vec<&str> = v.as_array().unwrap().iter().map(|m| m["kind"].as_str().unwrap_or_default()).collect();
    assert_eq!(kinds, ["tell"; 5], "{v}");
    assert_eq!((unread(&root, &work, "s-oc"), unread(&root, &work, "s-claude")), (3, 1), "two, three and four wait; the reply too");
    let r = borg(&root, &work, &["inbox"], &[("CLAUDE_CODE_SESSION_ID", "s-claude")], "");
    assert_eq!(r.out.lines().count(), 1, "{}", r.out);
    assert_eq!((unread(&root, &work, "s-claude"), logged(&root, "seen")), (0, 2), "the session's inbox claimed and acknowledged the reply");

    // To the engineer: listing it in the terminal is the engineer being shown it.
    send(&root, &work, "tell", &["engineer", "the build is green"], &me, "engineer");
    let r = borg(&root, &work, &["tell", "engineer", "and the docs"], &me, "");
    assert!(r.err.starts_with("borg tell: 1 unread by engineer:"), "{}", r.err);
    borg(&root, &work, &["inbox"], &[], "");
    send(&root, &work, "tell", &["engineer", "and the docs"], &me, "engineer");
    let _ = std::fs::remove_dir_all(&root);
}

/// A drone's `borg top`, piped or `--once`, runs in the engineer's repo and prints the message to the
/// engineer, but acknowledges nothing as them, as `borg inbox` in its shell would not (top.rs
/// `engineer_looks`): its sender's window to the engineer stays closed. Claude Code's and Codex's
/// shells name their session, opencode's its pid. The engineer's own `borg top --once` acknowledges it
/// and the window opens.
#[test]
fn a_drones_borg_top_acknowledges_nothing_as_the_engineer() {
    let (root, work) = setup("top-ack");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "opencode", "prompt", &plain("s-oc", &work, json!({"prompt": "docs", "pid": std::process::id()})));
    let me = [("CLAUDE_CODE_SESSION_ID", "s-claude")];
    send(&root, &work, "tell", &["engineer", "the build is green"], &me, "engineer");
    let closed = || borg(&root, &work, &["tell", "engineer", "and the docs"], &me, "").err.starts_with("borg tell: 1 unread by engineer:");
    assert!(closed(), "one unread message to the engineer closes its sender's window");
    let pid = std::process::id().to_string();
    let drones: [(&[&str], (&str, &str)); 3] = [
        (&["top", "--once"], ("CLAUDE_CODE_SESSION_ID", "s-claude")),
        (&["top"], ("CODEX_THREAD_ID", "s-codex")),
        (&["top", "--once"], ("OPENCODE_PID", &pid)),
    ];
    for (args, var) in drones {
        let r = borg(&root, &work, args, &[var], "");
        assert!(r.out.contains("the build is green"), "{args:?} {var:?}: {}", r.out);
        assert_eq!(logged(&root, "seen"), 0, "{args:?} {var:?}: nothing acknowledged as the engineer");
    }
    assert!(closed(), "the window stays closed");
    let r = borg(&root, &work, &["top", "--once"], &[], "");
    assert!(r.out.contains("the build is green"), "{}", r.out);
    assert_eq!(logged(&root, "seen"), 1, "the engineer's terminal acknowledges it");
    send(&root, &work, "tell", &["engineer", "and the docs"], &me, "engineer");
    let _ = std::fs::remove_dir_all(&root);
}

/// A message longer than what one event takes shows its head and `borg inbox --show <id>`, which prints
/// it whole (and, in its recipient's session, marks it shown, as the hook already did here).
#[test]
fn a_long_message_shows_its_head_and_inbox_show_prints_it_whole() {
    let (root, work) = setup("show");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "opencode", "prompt", &prompt("s-oc", &work));
    let budget = borg::harness::by_name("opencode").unwrap().caps().context_chars();
    assert!(budget < borg::msg::WIRE_TEXT, "a whole text must not fit one event for this test");
    let words: Vec<String> = (0..borg::msg::WIRE_TEXT / 7).map(|i| format!("w{i:05}")).collect();
    let text = words.join(" ");
    let last = words.last().unwrap().clone();
    let id = send(&root, &work, "ask", &["s-oc", &text], &[("CLAUDE_CODE_SESSION_ID", "s-claude")], "s-oc");

    let shown = hook(&root, "opencode", "prompt", &prompt("s-oc", &work));
    let c = context(&shown);
    assert!(c.chars().count() <= budget && c.contains(&format!("[{id}]: w00000 w00001")) && !c.contains(&last), "the head only: {c}");
    assert!(c.contains(&format!("The whole message: borg inbox --show {id} --from s-oc")), "{c}");
    assert_eq!(shown["shown_ids"], json!([id]), "{shown}");

    let r = borg(&root, &work, &["inbox", "--show", &id, "--from", "s-oc"], &[], "");
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.out.contains(&format!("{id} ask from")) && r.out.contains(&text), "whole: {}", r.out.chars().take(200).collect::<String>());
    assert!(r.out.contains(&format!("They asked for a reply: borg tell s-claude --re {id} --from s-oc")), "{}", r.out);
    let v: Value = serde_json::from_str(&borg(&root, &work, &["inbox", "--show", &id, "--json"], &[], "").out).unwrap();
    assert_eq!((v["id"].as_str(), v["text"].as_str(), v["v"].as_u64()), (Some(id.as_str()), Some(text.as_str()), Some(2)));
    assert_eq!(logged(&root, "seen"), 1, "shown once, acknowledged once");
    let r = borg(&root, &work, &["inbox", "--show", "mffffffffffff"], &[], "");
    assert_eq!(r.code, Some(1));
    assert!(r.err.contains("no recent message mffffffffffff"), "{}", r.err);
    let _ = std::fs::remove_dir_all(&root);
}

/// `borg top` in `work` on a pseudo-terminal, as a harness that runs its model's commands on one runs
/// it, in `borg`'s bare test environment with `env` on top: util-linux `script` gives it the terminal,
/// so it is the live screen (top_live.rs). Returns its output up to its first frame's end (a frame
/// clears the rest of the screen, and acknowledges before it is drawn), after which it quits on `q`, as
/// the engineer quits it.
#[cfg(target_os = "linux")]
fn live_top(root: &Path, work: &Path, env: &[(&str, &str)]) -> String {
    use std::io::Read;
    let mut c = Command::new("script");
    c.args(["-qec", &format!("'{}' top", env!("CARGO_BIN_EXE_borg")), "/dev/null"])
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
        .env("NO_COLOR", "1")
        .envs(env.iter().copied())
        .current_dir(work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    let (mut stdin, mut stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
    let (mut out, mut buf) = (Vec::new(), [0u8; 4096]);
    while !out.windows(3).any(|w| w == b"\x1b[J") {
        let n = stdout.read(&mut buf).unwrap();
        assert!(n > 0, "borg top ended before its first frame: {}", String::from_utf8_lossy(&out));
        out.extend_from_slice(&buf[..n]);
    }
    stdin.write_all(b"q").unwrap();
    stdin.flush().unwrap();
    stdout.read_to_end(&mut Vec::new()).unwrap();
    let done = child.wait_with_output().unwrap();
    assert!(done.status.success(), "{}: {}", done.status, String::from_utf8_lossy(&done.stderr));
    String::from_utf8_lossy(&out).into_owned()
}

/// `borg top` on a terminal is the live screen (top_live.rs), on a harness's pseudo-terminal too: a
/// drone's live screen shows the message to the engineer and acknowledges nothing as them, as its
/// printed screen would not (top.rs `engineer_looks`), so its sender's window stays closed. Claude
/// Code's and Codex's shells name their session, opencode's its pid. The engineer's own live screen
/// acknowledges it and the window opens. Linux only: BSD `script` takes other arguments.
#[cfg(target_os = "linux")]
#[test]
fn a_drones_live_borg_top_acknowledges_nothing_as_the_engineer() {
    let (root, work) = setup("top-live-ack");
    hook(&root, "claude", "prompt", &fixture("claude", "prompt", "s-claude", &work));
    hook(&root, "opencode", "prompt", &plain("s-oc", &work, json!({"prompt": "docs", "pid": std::process::id()})));
    let me = [("CLAUDE_CODE_SESSION_ID", "s-claude")];
    send(&root, &work, "tell", &["engineer", "the build is green"], &me, "engineer");
    let closed = || borg(&root, &work, &["tell", "engineer", "and the docs"], &me, "").err.starts_with("borg tell: 1 unread by engineer:");
    assert!(closed(), "one unread message to the engineer closes its sender's window");
    let pid = std::process::id().to_string();
    for var in [("CLAUDE_CODE_SESSION_ID", "s-claude"), ("CODEX_THREAD_ID", "s-codex"), ("OPENCODE_PID", pid.as_str())] {
        let frame = live_top(&root, &work, &[var]);
        assert!(frame.contains("\x1b[?1049h") && frame.contains("the build is green"), "{var:?}: the live screen shows it: {frame:?}");
        assert_eq!(logged(&root, "seen"), 0, "{var:?}: nothing acknowledged as the engineer");
    }
    assert!(closed(), "the window stays closed");
    let frame = live_top(&root, &work, &[]);
    assert!(frame.contains("the build is green"), "{frame:?}");
    assert_eq!(logged(&root, "seen"), 1, "the engineer's live screen acknowledges it");
    send(&root, &work, "tell", &["engineer", "and the docs"], &me, "engineer");
    let _ = std::fs::remove_dir_all(&root);
}
