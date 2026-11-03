//! `borg listen` through the binary (listen.rs): a listening idle session is woken when a message for it
//! arrives, a busy one only once its turn ends, and a plain one never. The hooks run as the harnesses
//! call them (`borg hook <harness> <event>`, the recorded payloads of tests/fixtures for Claude Code and
//! codex, canonical input for opencode); the waiters run as the real processes, `codex` is a fake on
//! PATH; the hooks name a stand-in harness process tied to the test's life (`host`). Every wait is on an event (an exit, a line, a file the waiter writes) under a generous guard,
//! never a sleep as the assertion; every waiter ends on a signal a user sends, or by itself. No model call.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long any awaited event may take before the test fails: a bound on a hang, not a margin.
const GUARD: Duration = Duration::from_secs(60);

struct Run {
    out: String,
    err: String,
    code: Option<i32>,
}

fn setup(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("borg-listen-cli-{name}-{}", std::process::id()));
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

/// The collective's listen files (listen.rs `DIR`).
fn listen_dir(root: &Path) -> PathBuf {
    root.join("base/collectives/corp/listen")
}

/// `borg args…` in `cwd`, with only `env` on top of a bare test environment (no outer session id).
fn command(root: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    command_of(Path::new(env!("CARGO_BIN_EXE_borg")), root, cwd, args, env)
}

/// `command`, run from the binary at `exe`.
fn command_of(exe: &Path, root: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(exe);
    c.args(args)
        .env_clear()
        // The stop's own check and its block (check_async.rs `on`), as these tests pin.
        .env("BORG_ASYNC", "off")
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        // No fake model here: a model call fails open instead of reaching a real CLI on PATH.
        .env("BORG_PREDICT_MODEL", "off")
        .env("BORG_LEARN_MODEL", "off")
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_MATCH", "bm25")
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_TEST_HARNESS", "1")
        .envs(env.iter().copied())
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn borg(root: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Run {
    let mut child = command(root, cwd, args, env).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    Run { out: String::from_utf8_lossy(&out.stdout).into(), err: String::from_utf8_lossy(&out.stderr).into(), code: out.status.code() }
}

/// The hook of `harness` for canonical `event`: its answer (Null: none). Always exits 0.
/// The stand-in for the harness process the hooks name (their `pid`): a `cat` reading a pipe only this
/// test process holds. A waiter ends once the harness process it names is gone (listen.rs `look`), so
/// one this test leaves behind, as a test killed before its guards ran would, ends with the test: the
/// pipe closes, `cat` exits. Never an ancestor of a command the test runs, so a `borg tell` from the
/// test stays the engineer's.
fn host() -> u32 {
    static HOST: std::sync::OnceLock<(Child, std::process::ChildStdin)> = std::sync::OnceLock::new();
    let (child, _) = HOST.get_or_init(|| {
        let mut c = Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let pipe = c.stdin.take().unwrap();
        (c, pipe)
    });
    child.id()
}

/// `borg hook <harness> <event>` with `input`, naming `host` as its harness process unless it names one.
fn hook(root: &Path, harness: &str, event: &str, input: &Value, env: &[(&str, &str)]) -> Value {
    let mut input = input.clone();
    if let Some(o) = input.as_object_mut() {
        o.entry("pid").or_insert_with(|| json!(host()));
    }
    let r = borg(root, root, &["hook", harness, event], env, &input.to_string());
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

/// Claude Code's hook for the recorded `step` (a fixture name) as canonical `event`.
fn claude(root: &Path, work: &Path, event: &str, step: &str, session: &str) -> Value {
    hook(root, "claude", event, &fixture("claude", step, session, work), &[])
}

/// Canonical input, as the omp and opencode adapters send it.
fn plain(session: &str, cwd: &Path, extra: &Value) -> Value {
    let mut v = json!({"session_id": session, "cwd": cwd.display().to_string()});
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    v
}

fn context(v: &Value) -> String {
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

/// `borg listen` in `session`: the opt-in, and what it tells the session.
fn opt_in(root: &Path, work: &Path, session: &str) -> String {
    let r = borg(root, work, &["listen", "--session", session], &[], "");
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(listen_dir(root).join(format!("{session}.json")).exists());
    r.out
}

/// Sends with `cmd` (tell, ask) from the engineer's terminal; returns the message id.
fn send(root: &Path, work: &Path, cmd: &str, to: &str, text: &str) -> String {
    send_as(root, work, &[], cmd, to, text)
}

/// Sends from the session `env` names (a sender's window to one recipient, msg.rs `window_for`).
fn send_as(root: &Path, work: &Path, env: &[(&str, &str)], cmd: &str, to: &str, text: &str) -> String {
    let r = borg(root, work, &[cmd, to, text], env, "");
    assert_eq!(r.code, Some(0), "{cmd} {to}: {}", r.err);
    let mut w = r.out.split_whitespace();
    assert_eq!(w.next(), Some("sent"), "{}", r.out);
    w.next().unwrap().to_string()
}

/// Polls `cond` until it holds; fails after `GUARD` with `what`.
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < GUARD, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The pid in `session`'s lock file: written by the waiter once it holds the lock, after its signals
/// are trapped. Read, never locked, so the probe cannot make the waiter think another one runs.
fn lock_pid(root: &Path, session: &str) -> Option<i32> {
    std::fs::read_to_string(listen_dir(root).join(format!("{session}.lock"))).ok()?.trim().parse().ok()
}

fn signal(pid: i32, sig: i32) {
    // SAFETY: kill(2) on a waiter this test started.
    assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "kill {pid}");
}

/// A waiter this test started (`borg listen --wait|--stream`): its stdout lines as they come, and its
/// stdin when the test answers its wakes as an adapter does. Dropped while it runs (a failed
/// assertion), it gets SIGINT, as an engineer stops it.
struct Waiter {
    child: Option<Child>,
    pid: i32,
    lines: Receiver<String>,
    stdin: Option<std::process::ChildStdin>,
}

impl Waiter {
    fn start(root: &Path, work: &Path, flag: &str, session: &str) -> Waiter {
        Waiter::spawn(root, work, flag, session, Stdio::null())
    }

    /// A stream whose wakes the test answers on its stdin (`answer`), as an adapter does.
    fn answering(root: &Path, work: &Path, session: &str) -> Waiter {
        Waiter::spawn(root, work, "--stream", session, Stdio::piped())
    }

    fn spawn(root: &Path, work: &Path, flag: &str, session: &str, stdin: Stdio) -> Waiter {
        Waiter::spawn_from(Path::new(env!("CARGO_BIN_EXE_borg")), root, work, flag, session, stdin)
    }

    fn spawn_from(exe: &Path, root: &Path, work: &Path, flag: &str, session: &str, stdin: Stdio) -> Waiter {
        let dir = work.display().to_string();
        let mut child = command_of(exe, root, work, &["listen", flag, "--session", session, "--dir", &dir], &[]).stdin(stdin).spawn().unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let input = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        until("the waiter holds its lock", || lock_pid(root, session) == Some(pid));
        Waiter { child: Some(child), pid, lines, stdin: input }
    }

    fn line(&self) -> String {
        self.lines.recv_timeout(GUARD).unwrap_or_else(|e| panic!("no line from the waiter: {e}"))
    }

    /// Its next wake line, notes skipped (into `notes`).
    fn wake(&self, notes: &mut Vec<String>) -> Value {
        loop {
            let v: Value = serde_json::from_str(&self.line()).unwrap();
            match v.get("note").and_then(Value::as_str) {
                Some(n) => notes.push(n.to_string()),
                None => return v,
            }
        }
    }

    /// An adapter's answer to a wake, one JSON line on the waiter's stdin.
    fn answer(&mut self, v: &Value) {
        let stdin = self.stdin.as_mut().expect("a waiter started with `answering`");
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    /// Whether it still runs.
    fn runs(&mut self) -> bool {
        self.child.as_mut().unwrap().try_wait().unwrap().is_none()
    }

    /// Its exit code and every line it printed after those read, once it ends by itself.
    fn exit(mut self) -> (Option<i32>, Vec<String>) {
        let mut child = self.child.take().unwrap();
        let t0 = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if t0.elapsed() > GUARD {
                signal(self.pid, libc::SIGINT);
                panic!("the waiter did not end");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Its stdout is closed: the reader's channel ends.
        let rest = self.lines.iter().collect();
        (status.code(), rest)
    }

    fn stop(self, sig: i32) -> (Option<i32>, Vec<String>) {
        signal(self.pid, sig);
        self.exit()
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            signal(self.pid, libc::SIGINT);
            let _ = c.wait();
        }
    }
}

#[test]
fn an_idle_claude_session_is_asked_once_for_the_wait_which_ends_with_the_message() {
    let (root, work) = setup("claude");
    claude(&root, &work, "prompt", "prompt", "s-claude");
    claude(&root, &work, "prompt", "prompt", "s-plain");
    let told = opt_in(&root, &work, "s-claude");
    assert!(told.contains("borg listen --wait --session 's-claude'") && told.contains("run_in_background now"), "{told}");

    let wait = format!("borg listen --wait --session 's-claude' --dir '{}'", work.display());
    let asked = claude(&root, &work, "stop", "stop", "s-claude");
    assert!(context(&asked).contains(&wait) && context(&asked).contains("run_in_background"), "{asked}");
    assert_eq!(asked["hookSpecificOutput"]["hookEventName"], "Stop", "Stop feedback, not a hook error: {asked}");
    assert!(asked.get("decision").is_none(), "{asked}");
    assert!(!context(&claude(&root, &work, "stop", "stop_again", "s-claude")).contains("borg listen"), "asked once");
    for _ in 0..2 {
        assert!(!claude(&root, &work, "stop", "stop", "s-plain").to_string().contains("borg listen"), "a plain session is never asked");
    }
    assert!(!listen_dir(&root).join("s-plain.json").exists());

    // The model runs what it was asked: it ends with the summary once a message arrives.
    let w = Waiter::start(&root, &work, "--wait", "s-claude");
    let id = send(&root, &work, "tell", "s-claude", "the schema froze");
    let (code, out) = w.exit();
    assert_eq!(code, Some(0), "{out:?}");
    let out = out.join("\n");
    assert!(out.contains("[borg listen] 1 message from other sessions waits for you (alice@x tells)"), "{out}");
    assert!(out.contains("run `borg inbox`") && !out.contains("schema") && !out.contains(&id), "never the text: {out}");

    // The hook shows it once, labelled; a started wait answered the ask, so the next stop may ask again.
    let shown = context(&claude(&root, &work, "prompt", "prompt", "s-claude"));
    assert!(shown.contains(&format!("[{id}]: the schema froze")), "{shown}");
    assert!(context(&claude(&root, &work, "stop", "stop", "s-claude")).contains(&wait), "asked again after a wait ran");
    assert!(!claude(&root, &work, "stop", "stop", "s-plain").to_string().contains("borg listen"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_busy_session_is_not_woken_until_its_turn_ends() {
    let (root, work) = setup("busy");
    claude(&root, &work, "prompt", "prompt", "s-busy");
    claude(&root, &work, "prompt", "prompt", "s-peer");
    opt_in(&root, &work, "s-busy");
    // A turn runs (its prompt and a tool call) with the wait in the background.
    claude(&root, &work, "prompt", "prompt", "s-busy");
    claude(&root, &work, "pre_tool", "pre_tool", "s-busy");
    let mut w = Waiter::start(&root, &work, "--wait", "s-busy");

    // An ask arrives mid-turn: the turn's own next hook shows it, and the waiter never wakes for it.
    let during = send(&root, &work, "ask", "s-busy", "which migration runs first?");
    let shown = context(&claude(&root, &work, "post_tool", "edit", "s-busy"));
    assert!(shown.contains(&format!("[{during}]: which migration runs first?")), "{shown}");
    let stop = claude(&root, &work, "stop", "stop", "s-busy");
    assert!(w.runs(), "no wake for a message the turn showed: {:?}", w.lines.try_iter().collect::<Vec<_>>());
    assert!(!context(&stop).contains("borg listen"), "its wait runs: nothing to ask for: {stop}");

    // A tell after the stop wakes it, for that one alone (a wake for the ask would name alice's ask).
    send_as(&root, &work, &[("CLAUDE_CODE_SESSION_ID", "s-peer")], "tell", "s-busy", "ship it");
    let (code, out) = w.exit();
    let out = out.join("\n");
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("[borg listen] 1 message from other sessions waits for you (") && out.contains(" tells)"), "{out}");
    assert!(!out.contains("asks") && !out.contains("alice@x"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_turn_halted_mid_tool_call_does_not_leave_the_session_deaf() {
    let (root, work) = setup("halted");
    let oc = |event: &str, extra: Value| hook(&root, "opencode", event, &plain("s-oc", &work, &extra), &[]);
    oc("prompt", json!({"prompt": "carry on"}));
    opt_in(&root, &work, "s-oc");
    assert_eq!(oc("stop", json!({})), json!({"listen": true}), "the adapter is told to start its stream");
    let w = Waiter::start(&root, &work, "--stream", "s-oc");

    // A turn whose tool call never ends (the engineer aborts it): a message waits meanwhile.
    oc("prompt", json!({"prompt": "carry on"}));
    oc("pre_tool", json!({"tool_name": "bash", "tool_input": {"command": "cargo build"}}));
    send(&root, &work, "tell", "s-oc", "the build host is back");
    // The adapter reports the halted turn: the mark goes at once, and the waiting message wakes it.
    assert_eq!(oc("halted", json!({})), Value::Null);
    assert!(!listen_dir(&root).join("s-oc.busy").exists());
    let wake: Value = serde_json::from_str(&w.line()).unwrap();
    assert!(wake["wake"].as_str().is_some_and(|t| t.contains("1 message") && !t.contains("build host")), "{wake}");
    let (code, rest) = w.stop(libc::SIGHUP);
    assert_eq!((code, rest), (Some(128 + libc::SIGHUP), vec![r#"{"end":"stopped"}"#.to_string()]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_stream_says_each_message_once_on_one_json_line() {
    let (root, work) = setup("stream");
    let oc = |event: &str, extra: Value| hook(&root, "opencode", event, &plain("s-oc", &work, &extra), &[]);
    oc("prompt", json!({"prompt": "carry on"}));
    claude(&root, &work, "prompt", "prompt", "s-peer");
    opt_in(&root, &work, "s-oc");
    let prior = json!({"listen": true});
    assert_eq!(oc("stop", json!({})), prior);
    let w = Waiter::start(&root, &work, "--stream", "s-oc");
    assert!(oc("stop", json!({})).get("listen").is_none(), "its stream runs: not started twice");

    let mut wakes = Vec::new();
    let peer = [("CLAUDE_CODE_SESSION_ID", "s-peer")];
    for (from, text) in [(&[][..], "first note"), (&peer[..], "second note")] {
        send_as(&root, &work, from, "tell", "s-oc", text);
        // A note (a rebuilt borg followed in place) is no wake.
        let line = std::iter::repeat_with(|| serde_json::from_str::<Value>(&w.line()).unwrap()).find(|l| l.get("note").is_none()).unwrap();
        let t = line["wake"].as_str().unwrap_or_else(|| panic!("{line}")).to_string();
        assert!(t.starts_with("[borg listen] 1 message from other sessions waits for you (") && !t.contains("note"), "1 each, never the text: {t}");
        wakes.push(t);
        // The wake's turn ends: until its stop, it holds the next wake off.
        oc("stop", json!({}));
    }
    let (code, rest) = w.stop(libc::SIGINT);
    assert_eq!(code, Some(128 + libc::SIGINT));
    assert_eq!(rest, vec![r#"{"end":"stopped"}"#.to_string()], "no wake twice, the end last");
    assert!(wakes[0].contains("(alice@x tells)") && !wakes[1].contains("alice@x"), "each wake for its own message: {wakes:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The adapter could not send a wake in (`{"failed": ids}` on the stream's stdin): the waiter backs off,
/// then wakes the session once more for the same message; once the adapter says it went in, never
/// again. The second wake comes only after the backoff (10 s), awaited as a line, not slept.
#[test]
fn a_wake_the_adapter_could_not_send_in_wakes_once_more_after_the_backoff() {
    let (root, work) = setup("refused");
    let oc = |event: &str, extra: Value| hook(&root, "opencode", event, &plain("s-oc", &work, &extra), &[]);
    oc("prompt", json!({"prompt": "carry on"}));
    opt_in(&root, &work, "s-oc");
    assert_eq!(oc("stop", json!({})), json!({"listen": true}));
    let mut w = Waiter::answering(&root, &work, "s-oc");

    let id = send(&root, &work, "tell", "s-oc", "the build host is back");
    let mut notes = Vec::new();
    let first = w.wake(&mut notes);
    assert_eq!((&first["n"], &first["ids"]), (&json!(1), &json!([id])), "the wake names its message: {first}");
    w.answer(&json!({"failed": [id]}));
    let again = w.wake(&mut notes);
    assert_eq!((&again["n"], &again["ids"], &again["wake"]), (&json!(1), &json!([id]), &first["wake"]), "the same message, once more");
    assert!(notes.iter().any(|n| n.contains("could not send the wake in: tried again in 10 s")), "{notes:?}");
    w.answer(&json!({"sent": [id]}));
    let (code, rest) = w.stop(libc::SIGINT);
    assert_eq!(code, Some(128 + libc::SIGINT));
    assert_eq!(rest, vec![r#"{"end":"stopped"}"#.to_string()], "it went in: no third wake");
    let _ = std::fs::remove_dir_all(&root);
}

/// A turn that went quiet with no stop (an Esc the adapter never reported) stops holding wakes off once
/// it has been quiet past the floor (`TURN_IDLE_MS`), so the session is not left deaf. Its session's own
/// pauses (a history of 50 ms gaps, injected) never shorten that hold (listen.rs's unit tests, on an
/// injected clock); the turn's mark is injected as quiet since just past the floor.
#[test]
fn a_quiet_turn_releases_its_wake_once_quiet_past_the_floor() {
    let (root, work) = setup("learned");
    let oc = |event: &str, extra: Value| hook(&root, "opencode", event, &plain("s-oc", &work, &extra), &[]);
    oc("prompt", json!({"prompt": "carry on"}));
    opt_in(&root, &work, "s-oc");
    assert_eq!(oc("stop", json!({})), json!({"listen": true}));
    let w = Waiter::start(&root, &work, "--stream", "s-oc");
    // A turn runs and goes quiet; the session pauses 50 ms at most.
    oc("prompt", json!({"prompt": "and the retries"}));
    let busy = listen_dir(&root).join("s-oc.busy");
    assert!(busy.exists(), "the turn holds wakes off");
    let history: String = (0..40).map(|_| "idle 50\ntool 50\n").collect();
    std::fs::write(listen_dir(&root).join("s-oc.gaps"), history).unwrap();
    std::fs::write(&busy, format!("0 {}", borg::learn::now_ms() - borg::listen::TURN_IDLE_MS - 1)).unwrap();
    send(&root, &work, "tell", "s-oc", "the schema froze");
    let mut notes = Vec::new();
    let wake = w.wake(&mut notes);
    assert!(wake["wake"].as_str().is_some_and(|t| t.contains("1 message")), "{wake}");
    let _ = w.stop(libc::SIGINT);
    let _ = std::fs::remove_dir_all(&root);
}

/// codex's stop starts its waiter detached; it is no child of the test, so it is synchronised on its
/// files: its lock's pid, the fake `codex`'s record, its log.
#[test]
fn codexs_stop_starts_a_queue_waiter_that_wakes_through_codex_queue() {
    let (root, work) = setup("codex");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = root.join("codex-calls");
    let fake = bin.join("codex");
    std::fs::write(&fake, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" --end >> '{}'\n", calls.display())).unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let env = [("PATH", path.as_str())];

    hook(&root, "codex", "prompt", &fixture("codex", "prompt", "s-codex", &work), &env);
    opt_in(&root, &work, "s-codex");
    assert!(!hook(&root, "codex", "stop", &fixture("codex", "stop", "s-codex", &work), &env).to_string().contains("listen"));
    until("codex's stop starts its waiter", || lock_pid(&root, "s-codex").is_some());
    let pid = lock_pid(&root, "s-codex").unwrap();
    struct Stop(i32);
    impl Drop for Stop {
        fn drop(&mut self) {
            // SAFETY: kill(2) SIGINT on the waiter codex's stop started for this test.
            unsafe { libc::kill(self.0, libc::SIGINT) };
        }
    }
    let _stop = Stop(pid);

    send(&root, &work, "tell", "s-codex", "rebase on main");
    until("the waiter runs codex queue", || std::fs::read_to_string(&calls).is_ok_and(|t| t.contains("--end")));
    let args: Vec<String> = std::fs::read_to_string(&calls).unwrap().lines().map(String::from).collect();
    assert_eq!(&args[..4], ["queue", "--thread", "s-codex", "--message"], "{args:?}");
    assert!(args[4].starts_with("[borg listen] 1 message from other sessions waits for you (alice@x tells)") && !args[4].contains("rebase"), "{args:?}");
    assert_eq!(args.len(), 6, "once: {args:?}");

    let log = listen_dir(&root).join("s-codex.log");
    signal(pid, libc::SIGHUP);
    until("the waiter ends on SIGHUP", || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("[borg listen] ends: stopped")));
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains(" queued: [borg listen] 1 message"), "{log}");
    assert_eq!(std::fs::read_to_string(&calls).unwrap().matches("--end").count(), 1, "one message, one codex queue");
    let _ = std::fs::remove_dir_all(&root);
}

/// A `codex queue` that fails leaves the message unread: the waiter logs why and backs off.
#[test]
fn a_failed_codex_queue_is_logged_and_backed_off() {
    let (root, work) = setup("codex-fails");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("codex");
    std::fs::write(&fake, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let env = [("PATH", path.as_str())];

    hook(&root, "codex", "prompt", &fixture("codex", "prompt", "s-codex", &work), &env);
    opt_in(&root, &work, "s-codex");
    hook(&root, "codex", "stop", &fixture("codex", "stop", "s-codex", &work), &env);
    until("codex's stop starts its waiter", || lock_pid(&root, "s-codex").is_some());
    let pid = lock_pid(&root, "s-codex").unwrap();
    struct Stop(i32);
    impl Drop for Stop {
        fn drop(&mut self) {
            // SAFETY: kill(2) SIGINT on the waiter codex's stop started for this test (a no-op once ended).
            unsafe { libc::kill(self.0, libc::SIGINT) };
        }
    }
    let _stop = Stop(pid);

    send(&root, &work, "tell", "s-codex", "rebase on main");
    let log = listen_dir(&root).join("s-codex.log");
    let failed = "the wake did not reach the session (`codex queue` exited 3): tried again in 10 s";
    until("the waiter logs the failed wake", || std::fs::read_to_string(&log).is_ok_and(|t| t.contains(failed)));
    signal(pid, libc::SIGINT);
    until("the waiter ends on SIGINT", || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("[borg listen] ends: stopped")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_waiter_ends_cleanly_on_sigint_and_refuses_a_session_that_does_not_listen() {
    let (root, work) = setup("sigint");
    claude(&root, &work, "prompt", "prompt", "s-claude");
    let dir = work.display().to_string();
    let r = borg(&root, &work, &["listen", "--wait", "--session", "s-claude", "--dir", &dir], &[], "");
    assert_eq!(r.code, Some(1), "not listening: {}", r.out);
    let r = borg(&root, &work, &["listen", "--wait", "--off"], &[], "");
    assert_eq!(r.code, Some(2), "{}", r.err);

    opt_in(&root, &work, "s-claude");
    let w = Waiter::start(&root, &work, "--wait", "s-claude");
    let (code, out) = w.stop(libc::SIGINT);
    assert_eq!(code, Some(128 + libc::SIGINT));
    assert_eq!(out, vec!["[borg listen] the wait ends: stopped".to_string()]);
    // Its lock is free again: the next wait takes it.
    let w = Waiter::start(&root, &work, "--wait", "s-claude");
    let r = borg(&root, &work, &["listen", "--off", "--session", "s-claude"], &[], "");
    assert!(r.out.contains("listens no more"), "{}", r.out);
    let (code, out) = w.exit();
    assert!(code.is_some() && out.len() == 1 && out[0].starts_with("[borg listen] the wait ends: "), "{code:?} {out:?}");
    assert!(!listen_dir(&root).join("s-claude.json").exists(), "the opt-in is gone");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn under_a_binding_plugin_the_shells_listen_binds_no_session_by_pid() {
    let (root, work) = setup("binder");
    // Two root sessions of one opencode process: both drones name its pid; s-b's hook touched last.
    let pid = std::process::id().to_string();
    for s in ["s-a", "s-b"] {
        hook(&root, "opencode", "prompt", &plain(s, &work, &json!({"pid": std::process::id(), "prompt": "hi"})), &[]);
    }
    let listens = |s: &str| listen_dir(&root).join(format!("{s}.json")).exists();
    let shell = [("OPENCODE_PID", pid.as_str()), ("BORG_LISTEN_BINDER", "opencode")];

    // s-a's model runs `borg listen` in its shell: the plugin binds, borg picks no session by the pid.
    let r = borg(&root, &work, &["listen"], &shell, "");
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.out.contains("the opencode plugin binds") && !r.out.contains("This session listens"), "{}", r.out);
    assert!(!listens("s-a") && !listens("s-b"), "nobody opted in by the pid");
    // The plugin's own bind, with the session it saw run the command: only s-a listens.
    let r = borg(&root, &work, &["listen", "--session", "s-a"], &shell, "");
    assert!(r.out.contains("This session listens"), "{}", r.out);
    assert!(listens("s-a") && !listens("s-b"));
    let r = borg(&root, &work, &["listen", "--off"], &shell, "");
    assert!(r.out.contains("the opencode plugin binds") && listens("s-a") && !listens("s-b"), "{}", r.out);

    // Without the plugin's mark the pid picks the drone touched last: s-b, which never asked.
    let r = borg(&root, &work, &["listen"], &shell[..1], "");
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(listens("s-b"), "the case the binder prevents: {}", r.out);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------- a detached check's asks

/// A store lesson about the work, and a fake `claude` on PATH (`root/bin`) that reads its prompt from
/// stdin and answers it applies at grade 3: a detached check (check_async.rs) of the stop asks it. The
/// hooks' environment for it: the check detached, nothing between the stop and the model.
fn async_store(root: &Path) -> Vec<(&'static str, String)> {
    let store = root.join("base/collectives/corp");
    std::fs::create_dir_all(&store).unwrap();
    let lesson = json!({"id": "Mdrain", "origin_ms": 1, "question": "Does the net drain the budget place with an explicit consumer instead of a reset arc?", "consequence": "a reset arc drops the budget token"});
    std::fs::write(store.join("lessons.jsonl"), format!("{lesson}\n")).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("claude");
    std::fs::write(&fake, "#!/bin/sh\ncat > /dev/null\necho '{\"structured_output\":{\"moves\":[],\"applies\":[{\"id\":\"Mdrain\",\"why\":\"w\",\"grade\":3}]},\"total_cost_usd\":0.01}'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    vec![
        ("BORG_ASYNC", "on".into()),
        ("BORG_MATCH", String::new()),
        ("BORG_PREFILTER", "off".into()),
        ("BORG_EARLY", "off".into()),
        ("BORG_SELECT", "fixed".into()),
        ("BORG_PREDICT_MODEL", "claude:".into()),
        ("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default())),
    ]
}

fn state_of(root: &Path, session: &str, ext: &str) -> String {
    std::fs::read_to_string(root.join("base/collectives/corp/state").join(format!("{session}.{ext}"))).unwrap_or_default()
}

/// An opencode turn on the lesson's point, opted in, and its stop: the detached check's ask waits in
/// the queue (the check ended by itself). Its check's id.
fn asked_at_stop(root: &Path, work: &Path, env: &[(&str, &str)], session: &str) -> String {
    let oc = |event: &str, extra: Value| hook(root, "opencode", event, &plain(session, work, &extra), env);
    oc("prompt", json!({"prompt": "drain the budget place of the net"}));
    opt_in(root, work, session);
    let out = oc("stop", json!({"last_assistant_message": "I drop the budget token with a reset arc."}));
    assert_eq!(out, json!({"listen": true}), "no block, the adapter starts its stream");
    let check = state_of(root, session, "ctx").lines().find_map(|l| l.strip_prefix("#borg-checking @").map(String::from)).expect("a detached check");
    until("the check's result", || state_of(root, session, "checked").contains(&check));
    assert_eq!(state_of(root, session, "queued"), format!("live Mdrain @{check}\n"));
    check
}

/// A detached check's ask wakes an idle listening session, and the wake carries its question about the
/// turn it judged (a wake's turn may fire no prompt hook). Reserved while the wake is out; once the
/// adapter says it went in, it is marked asked (via the wake) and never shown or woken for again.
#[test]
fn an_async_ask_wakes_an_idle_stream_session_with_its_text() {
    let (root, work) = setup("async-ask");
    let env = async_store(&root);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let check = asked_at_stop(&root, &work, &env, "s-oc");
    let mut w = Waiter::answering(&root, &work, "s-oc");
    let mut notes = Vec::new();
    let wake = w.wake(&mut notes);
    let id = format!("L@{check}:Mdrain");
    assert_eq!((&wake["n"], &wake["ids"]), (&json!(1), &json!([id])), "{wake}");
    let text = wake["wake"].as_str().unwrap_or_default();
    assert!(text.contains("[Mdrain]") && text.contains("I drop the budget token with a reset arc."), "the question and its turn: {text}");
    assert!(state_of(&root, "s-oc", "queued").is_empty(), "reserved: off the queue");
    assert_eq!(state_of(&root, "s-oc", "queued.wake"), format!("live Mdrain @{check}\n"));
    w.answer(&json!({"sent": [id]}));
    until("the wake's ask marked", || state_of(&root, "s-oc", "ctx").contains("via=wake"));
    let ctx = state_of(&root, "s-oc", "ctx");
    assert!(ctx.contains(&format!("#borg-asked Mdrain @{check}\n#borg-live Mdrain ms=")), "{ctx}");
    assert!(state_of(&root, "s-oc", "queued.wake").is_empty(), "committed");
    let edits = state_of(&root, "s-oc", "edits");
    assert!(edits.contains("I drop the budget token with a reset arc.\n#borg-asked Mdrain"), "the judged turn before the question: {edits}");
    // Its turn: the prompt shows it no more.
    let next = hook(&root, "opencode", "prompt", &plain("s-oc", &work, &json!({"prompt": "and the retries"})), &env);
    assert!(!context(&next).contains("[Mdrain]"), "{next}");
    let (code, rest) = w.stop(libc::SIGINT);
    assert_eq!(code, Some(128 + libc::SIGINT));
    assert_eq!(rest, vec![r#"{"end":"stopped"}"#.to_string()], "woken once");
    let _ = std::fs::remove_dir_all(&root);
}

/// An ask a wake reserved is not the prompt's to show; a wake the adapter could not send in puts it back
/// on the queue and leaves the engineer's turn mark (set meanwhile) as it is; the next prompt shows it
/// and names it in `shown_ids`, so an adapter drops any wake it holds for it.
#[test]
fn a_failed_lesson_wake_returns_its_asks_to_the_queue_and_keeps_busy() {
    let (root, work) = setup("async-failed");
    let env = async_store(&root);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let check = asked_at_stop(&root, &work, &env, "s-oc");
    let mut w = Waiter::answering(&root, &work, "s-oc");
    let mut notes = Vec::new();
    let wake = w.wake(&mut notes);
    let id = format!("L@{check}:Mdrain");
    assert_eq!(wake["ids"], json!([id]), "{wake}");
    // The engineer types before the adapter answers: the reserved ask is not this prompt's.
    let typed = hook(&root, "opencode", "prompt", &plain("s-oc", &work, &json!({"prompt": "now the retries"})), &env);
    assert!(!context(&typed).contains("[Mdrain]") && typed.get("shown_ids").is_none(), "a_reserved_ask_is_not_delivered_by_the_prompt: {typed}");
    let busy = std::fs::read_to_string(listen_dir(&root).join("s-oc.busy")).unwrap();
    assert!(busy.starts_with("0 "), "the engineer's turn: {busy}");
    w.answer(&json!({"failed": [id]}));
    until("the ask back on the queue", || state_of(&root, "s-oc", "queued") == format!("live Mdrain @{check}\n"));
    assert!(state_of(&root, "s-oc", "queued.wake").is_empty());
    assert_eq!(std::fs::read_to_string(listen_dir(&root).join("s-oc.busy")).unwrap(), busy, "a failed wake never clears the engineer's mark");
    assert!(!state_of(&root, "s-oc", "ctx").contains("#borg-asked"), "nothing marked asked");
    let shown = hook(&root, "opencode", "post_tool", &plain("s-oc", &work, &json!({"tool_name": "Read", "tool_input": {"file_path": "/x"}})), &env);
    assert!(context(&shown).contains("[Mdrain]"), "{shown}");
    assert_eq!(shown["shown_ids"], json!([id]), "{shown}");
    let (code, rest) = w.stop(libc::SIGINT);
    assert_eq!(code, Some(128 + libc::SIGINT));
    assert!(rest.iter().all(|l| !l.contains("wake\"")), "the shown ask wakes nothing: {rest:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A waiter that ends with an ask reserved (its adapter never said the wake went in) gives it back to
/// the queue as it ends, so the next prompt, or the next waiter, delivers it: a waiting ask wakes once
/// per waiter, and one the session was shown never.
#[test]
fn a_waiting_ask_wakes_once_and_a_delivered_one_never() {
    let (root, work) = setup("async-once");
    let env = async_store(&root);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let check = asked_at_stop(&root, &work, &env, "s-oc");
    let id = format!("L@{check}:Mdrain");
    let w = Waiter::answering(&root, &work, "s-oc");
    let mut notes = Vec::new();
    assert_eq!(w.wake(&mut notes)["ids"], json!([id]));
    let (code, rest) = w.stop(libc::SIGINT);
    assert_eq!((code, rest), (Some(128 + libc::SIGINT), vec![r#"{"end":"stopped"}"#.to_string()]), "one wake for it");
    assert!(state_of(&root, "s-oc", "queued.wake").is_empty(), "no answer came: given back as the waiter ended");
    assert_eq!(state_of(&root, "s-oc", "queued"), format!("live Mdrain @{check}\n"));
    // The wake's turn ends: its stop's check grades the waiting ask again and asks it in its place.
    let out = hook(&root, "opencode", "stop", &plain("s-oc", &work, &json!({"last_assistant_message": "Looked at it."})), &env);
    assert_eq!(out, json!({"listen": true}));
    let newer = state_of(&root, "s-oc", "ctx").lines().rev().find_map(|l| l.strip_prefix("#borg-checking @")).unwrap_or_default().to_string();
    assert_ne!(newer, check);
    until("the stop's own check", || state_of(&root, "s-oc", "checked").contains(&newer));
    assert_eq!(state_of(&root, "s-oc", "queued"), format!("live Mdrain @{newer}\n"), "superseded: one ask waits");
    // The next waiter wakes for it once; this time it goes in.
    let id = format!("L@{newer}:Mdrain");
    let mut w = Waiter::answering(&root, &work, "s-oc");
    let again = w.wake(&mut notes);
    assert_eq!(again["ids"], json!([id]), "{again}");
    w.answer(&json!({"sent": [id]}));
    until("marked asked", || state_of(&root, "s-oc", "ctx").contains("via=wake"));
    let (code, rest) = w.stop(libc::SIGINT);
    assert_eq!((code, rest), (Some(128 + libc::SIGINT), vec![r#"{"end":"stopped"}"#.to_string()]), "delivered: never again");
    assert!(state_of(&root, "s-oc", "queued").is_empty() && state_of(&root, "s-oc", "queued.wake").is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// The waiter's lines within `window`, none of them a wake: notes only (none of its passes woke).
fn no_wake(w: &Waiter, window: Duration) -> Vec<String> {
    let end = Instant::now() + window;
    let mut seen = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match w.lines.recv_timeout(left) {
            Ok(l) => {
                assert!(!l.contains("\"wake\""), "no wake: {l}");
                seen.push(l);
            }
            Err(_) => break,
        }
    }
    seen
}

/// While a detached check of the session runs (a job claimed by a live process), a waiting ask wakes
/// nothing: that check may grade it again against newer work. Once it writes its result the waiter looks
/// again and the ask wakes the session.
#[test]
fn a_waiting_ask_wakes_nothing_while_a_check_of_the_session_runs() {
    let (root, work) = setup("async-running");
    let env = async_store(&root);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let check = asked_at_stop(&root, &work, &env, "s-oc");
    let state = root.join("base/collectives/corp/state");
    let claimed = state.join(format!("s-oc.job.0000000000ff.{}", std::process::id()));
    std::fs::write(&claimed, "{}").unwrap();
    let mut w = Waiter::answering(&root, &work, "s-oc");
    // Its first pass looks at once; the window spans more than one look (listen.rs `LOOK`).
    no_wake(&w, Duration::from_secs(3));
    assert_eq!(state_of(&root, "s-oc", "queued"), format!("live Mdrain @{check}\n"), "not reserved");
    // The check ends: its job file goes, its result is written.
    std::fs::remove_file(&claimed).unwrap();
    let done = state_of(&root, "s-oc", "checked");
    std::fs::write(state.join("s-oc.checked"), done).unwrap();
    let mut notes = Vec::new();
    let id = format!("L@{check}:Mdrain");
    assert_eq!(w.wake(&mut notes)["ids"], json!([id]));
    w.answer(&json!({"sent": [id]}));
    until("marked asked", || state_of(&root, "s-oc", "ctx").contains("via=wake"));
    let (code, _) = w.stop(libc::SIGINT);
    assert_eq!(code, Some(128 + libc::SIGINT));
    let _ = std::fs::remove_dir_all(&root);
}

/// The running binary of `pid` was replaced on disk and this process still runs it (Linux: `/proc`).
#[cfg(target_os = "linux")]
fn runs_deleted(pid: i32) -> bool {
    std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|p| p.to_string_lossy().ends_with(" (deleted)"))
}

/// A reinstalled borg restarts a waiter (reexec.rs) only once no wake's asks wait reserved for the
/// adapter's answer: an answer the old image read into its buffer would be lost by the exec, and
/// nothing would deliver those asks. Once the answer came, the waiter restarts on the new binary.
#[cfg(target_os = "linux")]
#[test]
fn a_waiter_restarts_on_a_reinstalled_borg_only_once_its_wake_is_answered() {
    let (root, work) = setup("async-reexec");
    let env = async_store(&root);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let check = asked_at_stop(&root, &work, &env, "s-oc");
    let exe = root.join("exe/borg");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_borg"), &exe).unwrap();
    let mut w = Waiter::spawn_from(&exe, &root, &work, "--stream", "s-oc", Stdio::piped());
    let mut notes = Vec::new();
    let id = format!("L@{check}:Mdrain");
    assert_eq!(w.wake(&mut notes)["ids"], json!([id]));
    assert!(!state_of(&root, "s-oc", "queued.wake").is_empty(), "reserved");
    // The reinstall: another file at the path the waiter was started by.
    let new = root.join("exe/borg.new");
    if std::fs::hard_link(env!("CARGO_BIN_EXE_borg"), &new).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_borg"), &new).unwrap();
    }
    std::fs::rename(&new, &exe).unwrap();
    assert!(runs_deleted(w.pid));
    // More than two looks (listen.rs `LOOK`): each would restart it, were it not for the reserved ask.
    let end = Instant::now() + Duration::from_millis(4_500);
    while Instant::now() < end {
        assert!(runs_deleted(w.pid), "no restart while the wake's ask waits for its answer");
        std::thread::sleep(Duration::from_millis(50));
    }
    w.answer(&json!({"sent": [id]}));
    until("marked asked", || state_of(&root, "s-oc", "ctx").contains("via=wake"));
    until("restarted on the new binary", || !runs_deleted(w.pid));
    assert!(state_of(&root, "s-oc", "queued.wake").is_empty());
    let (code, rest) = w.stop(libc::SIGINT);
    assert_eq!(code, Some(128 + libc::SIGINT), "{rest:?}");
    let _ = std::fs::remove_dir_all(&root);
}
