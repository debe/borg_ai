//! `borg-eval dogfood [--run NAME] [--model sonnet] [--setup-only]`: Borg end to end as a product, in a
//! sandbox HOME of its own (reports/dogfood-e2e.md).
//!
//! Everything runs inside the eval sandbox (`sandbox_with` + `Extra::sandbox_home`): the real HOME is
//! hidden, the sandbox HOME (`target/dogfood/<run>/home`) is bound read-write and is the only place
//! anything is written. The Claude credentials and a sanitised `~/.claude.json` are copied to a
//! private temp dir *outside* that HOME (`Secrets`) and bound into it only while each sandbox runs;
//! the guard deletes them when the run ends or panics, so no token is left on disk. The HOME holds a
//! user `settings.json` with a foreign hook, the collectives config and three tiny git repos: `~/work/relay` (work, team), `~/private/notes` (private,
//! local) and `~/diary` (off). Claude Code reads *user* settings there, so the hooks `borg install`
//! writes are the ones that fire.
//!
//! Steps: install + doctor; session A plans a lock-based relay and is corrected on resume, its
//! SessionEnd runs `borg learn`; session B (similar task, another file) must be asked that lesson at
//! Stop; session C's private correction stays private; session D (work) is never asked it;
//! session E (off) leaves no file under BORG_HOME; status, then uninstall restores settings.json
//! byte for byte; then a control arm (B's prompt with borg uninstalled) and a blind judge that
//! decides, for B's plan before the question, B's final plan and the control's plan, whether each
//! gives a concrete atomic remedy. `--setup-only` runs install/doctor/status/uninstall without any
//! model call. Results: `target/dogfood/<run>/results.json` (git revision, check definitions,
//! checks, evidence, timings, cost).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{Extra, sandbox_with};

/// The user settings.json the sandbox HOME starts with: someone's own Stop hook and permissions,
/// which install must keep and uninstall must restore byte for byte.
pub(crate) const ORIGINAL_SETTINGS: &str = r#"{
  "cleanupPeriodDays": 30,
  "permissions": {
    "allow": [
      "Read"
    ]
  },
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "true"
          }
        ]
      }
    ]
  }
}
"#;

/// Work (team), private (local), off; unlisted repos route off.
pub(crate) const COLLECTIVES: &str = r#"{
  "default": "off",
  "off": ["~/diary"],
  "collectives": [
    { "name": "work", "paths": ["~/work"], "share": "team" },
    { "name": "private", "paths": ["~/private"], "share": "local" }
  ]
}
"#;

const SYSTEM: &str = "You are a coding agent working in this repository with read-only tools. When asked for a plan, \
investigate the code you need, then reply with the complete plan as your final message. Do not write code files and do \
not ask questions.";

const RELAY: &str = r#"package relay;

import java.util.ArrayDeque;
import java.util.Queue;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.Consumer;

/** Forwards messages from many producer threads to one consumer thread. Not yet thread-safe. */
public final class Relay {
    private final AtomicBoolean closed = new AtomicBoolean();
    private final Queue<String> pending = new ArrayDeque<>();
    private final Consumer<String> sink;

    public Relay(Consumer<String> sink) {
        this.sink = sink;
    }

    /** Called by producer threads. */
    public void deliver(String msg) {
        if (closed.get()) {
            throw new IllegalStateException("relay closed");
        }
        pending.add(msg);
    }

    /** Called by the consumer thread. */
    public void drain() {
        String m;
        while ((m = pending.poll()) != null) {
            sink.accept(m);
        }
    }

    /** Called by any thread; afterwards no message may be accepted, and none accepted may be lost. */
    public void close() {
        closed.set(true);
        drain();
    }
}
"#;

const FAST_RELAY: &str = r#"package relay;

import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.function.Consumer;

/** The sibling path for high-volume topics: a lock-free handoff to the consumer. */
public final class FastRelay {
    private final ConcurrentLinkedQueue<String> handoff = new ConcurrentLinkedQueue<>();
    private final Consumer<String> sink;

    public FastRelay(Consumer<String> sink) {
        this.sink = sink;
    }

    public void deliver(String msg) {
        handoff.offer(msg);
    }

    public void drain() {
        String m;
        while ((m = handoff.poll()) != null) {
            sink.accept(m);
        }
    }
}
"#;

const BROADCASTER: &str = r#"package relay.fanout;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.Consumer;

/** Fans each published event out to every subscriber. Not yet thread-safe. */
public final class Broadcaster {
    private final AtomicBoolean stopped = new AtomicBoolean();
    private final List<Consumer<String>> subscribers = new ArrayList<>();

    public void subscribe(Consumer<String> s) {
        subscribers.add(s);
    }

    /** Called by many publisher threads. */
    public void publish(String event) {
        if (stopped.get()) {
            return;
        }
        for (Consumer<String> s : subscribers) {
            s.accept(event);
        }
    }

    /** Called by any thread; afterwards no subscriber may receive an event. */
    public void stop() {
        stopped.set(true);
        subscribers.clear();
    }
}
"#;

const NOTES: &str = r#"import json
import sys

PATH = "notes.json"


def load():
    try:
        with open(PATH) as f:
            return json.load(f)
    except FileNotFoundError:
        return []


def save(notes):
    with open(PATH, "w") as f:
        json.dump(notes, f, indent=2)


def add(text):
    notes = load()
    notes.append({"text": text, "tags": []})
    save(notes)


if __name__ == "__main__":
    add(" ".join(sys.argv[1:]))
"#;

pub(crate) const PROMPT_A: &str = "Plan how to make Relay (src/main/java/relay/Relay.java) safe when deliver(), drain() and close() \
are called from different threads. I want a lock-based design: one ReentrantLock guarding deliver, drain and close. Plan only; \
reply with the complete plan.";
pub(crate) const CORRECTION_A: &str = "no — carefully check TOCTOU on the closed flag; the sibling path is lock-free, use a \
ConcurrentLinkedQueue handoff. Revise the plan.";
const PROMPT_B: &str = "Plan how to make Broadcaster (src/main/java/relay/fanout/Broadcaster.java) safe when publish(), \
subscribe() and stop() are called from different threads. Plan only; reply with the complete plan.";
const PROMPT_C: &str = "Plan adding a `tag` command to notes.py that adds a tag to an existing note (by index) and saves \
the notes. Plan only; reply with the complete plan.";
const CORRECTION_C: &str = "no — never rewrite notes.json in place: write the JSON to a temporary file in the same directory, \
fsync it, then atomically rename it over notes.json, or a crash mid-write loses every note. Revise the plan.";
/// Tempts the private lesson (writing a JSON file on disk): a leak would show up here.
const PROMPT_D: &str = "Plan adding a persistent journal to Relay: messages still pending must survive a process restart, \
so save them to a journal.json file on disk and load it again at startup. Plan only; reply with the complete plan.";
/// The blind judge of B's plans: told neither the arm nor that a question was asked. Decides the
/// remedy, not the vocabulary.
pub(crate) const JUDGE: &str = "You review a plan for making the Java class below thread-safe. Decide one thing only: does \
the plan give a concrete remedy that makes publish()'s `stopped` check atomic with the delivery it guards, so that once \
stop() has returned no subscriber can receive an event? PASS examples: publish() holds a lock (or the read side of a \
read-write lock) across both the stopped check and the delivery loop, and stop() takes the same lock (or the write side); a \
CAS or state machine with the same effect; delivery from a snapshot where every delivery re-checks the stop state under a \
guard that stop() also takes. FAIL: the flag and the list are only made individually thread-safe (AtomicBoolean, volatile, \
CopyOnWriteArrayList, a concurrent map) while publish() still checks and then delivers with nothing stop() must wait for; \
the race is named but no such remedy is given; the remedy is vague or left as an option. Judge the remedy, not the \
vocabulary: words such as TOCTOU, race or check-then-act neither pass nor fail a plan.";
const JUDGE_SCHEMA: &str = r#"{"type":"object","properties":{"remedy":{"type":"string","description":"the plan's remedy for the stopped check, in one sentence, or none"},"pass":{"type":"boolean"}},"required":["remedy","pass"]}"#;
/// The keyword check on B's final plan (it answered the question, which itself says check-then-act).
const B_KEYWORDS: [&str; 2] = ["toctou", "check-then-act"];
const PROMPT_E: &str = "In two sentences, what does this repository contain?";
const CORRECTION_E: &str = "no — keep it to one sentence.";

pub(crate) struct Ctx {
    dir: PathBuf,
    pub(crate) home: PathBuf,
    /// The credentials and `.claude.json` copies, outside `home` (see `Secrets`).
    secrets: PathBuf,
    borg: PathBuf,
    model: String,
    checks: Vec<Value>,
    steps: serde_json::Map<String, Value>,
    cost: f64,
    judge_cost: f64,
}

fn uuid() -> String {
    fs::read_to_string("/proc/sys/kernel/random/uuid").expect("uuid").trim().to_string()
}

fn now_s() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

/// The copied credentials and `~/.claude.json`, in a private (0700) temp dir outside the persistent
/// sandbox HOME. They are bound into the HOME only while a sandbox runs (`Ctx::extra`). Dropping the
/// guard (end of run, or a panic unwinding) deletes the dir and the empty mount points bwrap left in
/// the HOME, so no refresh token outlives the run and no later `claude` in that HOME can log in.
#[derive(Debug)]
pub(crate) struct Secrets {
    pub(crate) dir: PathBuf,
    home: PathBuf,
}

impl Secrets {
    const FILES: [(&'static str, &'static str); 2] = [("credentials.json", ".claude/.credentials.json"), ("claude.json", ".claude.json")];

    /// (copy, where it appears inside the HOME) for each secret file.
    pub(crate) fn binds(dir: &Path, home: &Path) -> Vec<(PathBuf, PathBuf)> {
        Self::FILES.iter().map(|(src, dst)| (dir.join(src), home.join(dst))).collect()
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        for (_, dst) in Self::binds(&self.dir, &self.home) {
            let _ = fs::remove_file(dst);
        }
    }
}

/// The sandbox HOME: the original settings.json, the collectives config, `bin/borg`; and, outside
/// it, the `Secrets`: the credentials (only the Claude.ai OAuth entry, and only while it stays valid
/// for an hour, so the sandbox never refreshes it and the real file is never written) and a
/// sanitised `~/.claude.json`.
pub(crate) fn prepare_home(home: &Path, real_home: &Path, borg: &Path) -> Result<Secrets, String> {
    prepare_home_with(home, real_home, borg, ORIGINAL_SETTINGS, COLLECTIVES, 3600.0)
}

/// `prepare_home` with its own user `settings`, `collectives` config, and the time the token must
/// stay valid for (`min_valid_s`: at least the sandbox's whole run, so it never refreshes it).
pub(crate) fn prepare_home_with(home: &Path, real_home: &Path, borg: &Path, settings: &str, collectives: &str, min_valid_s: f64) -> Result<Secrets, String> {
    fs::create_dir_all(home.join(".claude")).map_err(|e| e.to_string())?;
    let creds: Value = serde_json::from_str(&fs::read_to_string(real_home.join(".claude/.credentials.json")).map_err(|e| format!("credentials: {e}"))?)
        .map_err(|e| format!("credentials: {e}"))?;
    let oauth = creds.get("claudeAiOauth").cloned().ok_or("credentials: no claudeAiOauth entry")?;
    let left_s = oauth["expiresAt"].as_f64().unwrap_or(0.0) / 1000.0 - now_s();
    if left_s < min_valid_s {
        return Err(format!(
            "the Claude token expires in {:.0} min (needed: {:.0}); a sandbox refresh could invalidate the real one. Run any claude command outside first.",
            left_s / 60.0,
            min_valid_s / 60.0
        ));
    }
    let dir = std::env::temp_dir().join(format!("borg-dogfood-secrets-{}-{}", std::process::id(), uuid()));
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let secrets = Secrets { dir, home: home.to_path_buf() };
    write_private(&secrets.dir.join("credentials.json"), &json!({"claudeAiOauth": oauth}).to_string())?;
    let mut cfg: Value = fs::read_to_string(real_home.join(".claude.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    if let Some(o) = cfg.as_object_mut() {
        o.remove("projects");
        o.remove("mcpServers");
    }
    write_private(&secrets.dir.join("claude.json"), &cfg.to_string())?;
    prepare_home_bare(home, borg, settings, collectives)?;
    Ok(secrets)
}

/// The sandbox HOME without any credentials (a fake or simulated harness): the user `settings`, the
/// `collectives` config and `bin/borg`.
pub(crate) fn prepare_home_bare(home: &Path, borg: &Path, settings: &str, collectives: &str) -> Result<(), String> {
    fs::create_dir_all(home.join(".claude")).map_err(|e| e.to_string())?;
    fs::write(home.join(".claude/settings.json"), settings).map_err(|e| e.to_string())?;
    fs::create_dir_all(home.join(".config/borg")).map_err(|e| e.to_string())?;
    fs::write(home.join(".config/borg/collectives.json"), collectives).map_err(|e| e.to_string())?;
    fs::create_dir_all(home.join("bin")).map_err(|e| e.to_string())?;
    // Idempotent: a HOME prepared again (a hive chain's, goal after goal) gets the link anew.
    let link = home.join("bin/borg");
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(borg, link).map_err(|e| e.to_string())
}

/// `git rev-parse HEAD` of the harness, and whether the harness sources differ from it.
fn git_revision(root: &Path) -> Value {
    let (_, rev) = crate::sh(root, "git rev-parse HEAD");
    let (_, dirty) = crate::sh(root, "git status --porcelain -- crates eval");
    json!({"commit": rev.trim(), "dirty": !dirty.trim().is_empty()})
}

/// What each model-dependent check means, recorded in results.json so runs of different harness
/// versions are never pooled under one name.
fn check_definitions() -> Value {
    json!({
        "B: the conscience asked the TOCTOU lesson at Stop": "B's state file has `#borg-asked <id>` for a work lesson learned from A, and the stream contains the Stop block 'Before you finish'",
        "B: the drone answered the question (names check-then-act/TOCTOU)": format!("B's final result text contains any of {B_KEYWORDS:?} (case-insensitive). The question itself contains these words, so this shows the question was answered, not that the bug was fixed"),
        "B: a blind judge finds a concrete atomic remedy in the final plan": format!("judge (sonnet, no tools, told neither the arm nor the question) returns pass=true on B's final result text. Prompt: {JUDGE}"),
    })
}

fn write_private(p: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

fn git_repo(dir: &Path, files: &[(&str, &str)], email: &str) {
    for (rel, text) in files {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    let (ok, out) =
        crate::sh(dir, &format!("git init -q && git config user.email {email} && git config user.name dogfood && git add -A && git commit -qm init"));
    assert!(ok, "git init {}: {out}", dir.display());
}

/// Every file under `dir`: (relative path, size, mtime ns), sorted.
pub(crate) fn tree(dir: &Path) -> Vec<(String, u64, u128)> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<(String, u64, u128)>) {
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            let Ok(m) = fs::symlink_metadata(&p) else { continue };
            if m.is_dir() {
                walk(base, &p, out);
            } else {
                let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
                out.push((p.strip_prefix(base).unwrap().display().to_string(), m.len(), mtime));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn jsonl(p: &Path) -> Vec<Value> {
    fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Latest version of each lesson id in a store's lessons.jsonl.
fn lesson_ids(p: &Path) -> Vec<String> {
    let mut ids: Vec<String> = jsonl(p).iter().filter_map(|l| l["id"].as_str().map(str::to_string)).collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The ids the conscience asked in a session (`#borg-asked a,b` lines of its state file).
pub(crate) fn asked_ids(state: &str) -> Vec<String> {
    state
        .lines()
        .filter_map(|l| l.strip_prefix("#borg-asked "))
        .flat_map(|ids| ids.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>())
        .collect()
}

/// Hook lifecycle events from a stream (`--include-hook-events`), compacted.
fn hook_events(stream: &str) -> Vec<Value> {
    stream
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "system" && v["subtype"].as_str().is_some_and(|s| s.starts_with("hook")))
        .map(|mut v| {
            for k in ["output", "stdout", "stderr"] {
                if let Some(s) = v[k].as_str() {
                    v[k] = json!(s.chars().take(400).collect::<String>());
                }
            }
            v
        })
        .collect()
}

/// The drone's last text before a hook first blocked it (its plan before the conscience asked);
/// empty if no hook blocked.
pub(crate) fn text_before_block(stream: &str) -> String {
    let mut last = String::new();
    for v in stream.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if v["type"] == "assistant" {
            let t: Vec<&str> =
                v["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
            if !t.join("").trim().is_empty() {
                last = t.join("\n");
            }
        }
        if v["subtype"] == "hook_response" && v["output"].as_str().is_some_and(|o| o.contains("\"block\"")) {
            return last;
        }
    }
    String::new()
}

/// Milliseconds from each hook's `hook_started` to its `hook_response`, by event, from timed lines.
/// Stop runs two hooks (the foreign `true` and borg's); both are listed, borg's is the slower.
pub(crate) fn hook_latencies(timed: &[(f64, String)]) -> Value {
    let mut started: std::collections::HashMap<String, f64> = Default::default();
    let mut by: serde_json::Map<String, Value> = Default::default();
    for (t, line) in timed {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let (Some(id), Some(ev)) = (v["hook_id"].as_str(), v["hook_event"].as_str()) else { continue };
        match v["subtype"].as_str() {
            Some("hook_started") => {
                started.insert(id.to_string(), *t);
            }
            Some("hook_response") => {
                if let Some(t0) = started.get(id) {
                    let list = by.entry(ev.to_string()).or_insert_with(|| json!([]));
                    list.as_array_mut().unwrap().push(json!(((t - t0) * 1000.0).round()));
                }
            }
            _ => {}
        }
    }
    Value::Object(by)
}

pub(crate) struct Turn {
    pub(crate) text: String,
    pub(crate) stream: String,
    pub(crate) ended_at: Instant,
}

impl Ctx {
    fn extra(&self) -> Extra {
        Extra { sandbox_home: Some(self.home.clone()), file_rw: Secrets::binds(&self.secrets, &self.home), ..Extra::default() }
    }

    fn store(&self) -> PathBuf {
        self.home.join(".local/share/borg")
    }

    fn collective(&self, name: &str) -> PathBuf {
        self.store().join("collectives").join(name)
    }

    /// A command inside the sandbox HOME: (exit code, stdout, stderr).
    pub(crate) fn exec(&self, cwd: &Path, argv: &[&str], timeout_s: u32) -> (i32, String, String) {
        let (mut cmd, private) = sandbox_with(cwd, &[&self.home], &[&self.borg], &self.extra(), timeout_s);
        let out = cmd.args(argv).stdin(Stdio::null()).output().expect("spawn sandbox");
        let _ = fs::remove_dir_all(private);
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn check(&mut self, name: &str, ok: bool, evidence: Value) {
        eprintln!("  [{}] {name}", if ok { "PASS" } else { "FAIL" });
        self.checks.push(json!({"check": name, "pass": ok, "evidence": evidence}));
    }

    /// One `claude -p` turn in `cwd`: a new session `id`, or `--resume id`. User settings load (no
    /// `--setting-sources`), so the installed hooks fire; read-only tools; hook events in the stream.
    fn claude(&mut self, label: &str, cwd: &Path, prompt: &str, id: &str, resume: bool, budget: &str) -> Turn {
        let stream_path = self.dir.join(format!("{label}.stream.jsonl"));
        let mut args: Vec<String> = ["-p", prompt, "--model", &self.model, "--max-budget-usd", budget, "--tools", "Read,Grep,Glob"].map(String::from).to_vec();
        args.extend(
            ["--append-system-prompt", SYSTEM, "--output-format", "stream-json", "--verbose", "--include-hook-events", "--strict-mcp-config"].map(String::from),
        );
        args.extend([if resume { "--resume" } else { "--session-id" }.to_string(), id.to_string()]);
        let (mut cmd, private) = sandbox_with(cwd, &[&self.home], &[&self.borg], &self.extra(), 900);
        eprintln!("  {label}: claude -p in {} …", cwd.display());
        let started = Instant::now();
        let mut child = cmd.arg("claude").args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("spawn claude");
        // Each line's arrival time: a hook's latency is hook_response minus hook_started.
        let mut timed: Vec<(f64, String)> = Vec::new();
        for line in std::io::BufRead::lines(std::io::BufReader::new(child.stdout.take().unwrap())).map_while(Result::ok) {
            timed.push((started.elapsed().as_secs_f64(), line));
        }
        let status = child.wait().expect("wait claude");
        let secs = started.elapsed().as_secs_f64();
        let _ = fs::remove_dir_all(private);
        let stream: String = timed.iter().map(|(_, l)| format!("{l}\n")).collect();
        fs::write(&stream_path, &stream).unwrap();
        let hook_ms = hook_latencies(&timed);
        let result = stream.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).rfind(|v| v["type"] == "result").unwrap_or(Value::Null);
        let cost = result["total_cost_usd"].as_f64().unwrap_or(0.0);
        self.cost += cost;
        let text = result["result"].as_str().unwrap_or_default().to_string();
        eprintln!("    exit {status} · ${cost:.3} · {secs:.0}s");
        self.steps.insert(
            label.into(),
            json!({"session": id, "resume": resume, "exit": status.code(), "cost_usd": cost, "wall_s": secs, "turns": result["num_turns"],
                   "result_excerpt": text.chars().take(1500).collect::<String>(), "hooks": hook_events(&stream), "hook_ms": hook_ms,
                   "stream": stream_path.display().to_string()}),
        );
        Turn { text, stream, ended_at: Instant::now() }
    }

    /// Waits for the `borg learn --session id` that SessionEnd spawned. Returns (seen, seconds from
    /// the claude process's exit until learn finished, learn's own run time).
    fn wait_learn(&self, id: &str, ended_at: Instant) -> (bool, f64, f64) {
        let pattern = format!("borg learn --session {id}");
        let mut first: Option<(Instant, f64)> = None;
        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            let out = Command::new("pgrep").args(["-f", &pattern]).output().expect("pgrep");
            let pids: Vec<String> = String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_string).collect();
            match (pids.first(), first) {
                (Some(pid), None) => {
                    let et = Command::new("ps").args(["-o", "etimes=", "-p", pid]).output().ok();
                    let et: f64 = et.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok()).unwrap_or(0.0);
                    first = Some((Instant::now(), et));
                }
                (None, Some((seen, et))) => return (true, ended_at.elapsed().as_secs_f64(), et + seen.elapsed().as_secs_f64()),
                (None, None) if ended_at.elapsed() > Duration::from_secs(15) => return (false, ended_at.elapsed().as_secs_f64(), 0.0),
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        (first.is_some(), ended_at.elapsed().as_secs_f64(), -1.0)
    }

    fn state(&self, collective: &str, id: &str) -> String {
        fs::read_to_string(self.collective(collective).join("state").join(format!("{id}.ctx"))).unwrap_or_default()
    }

    /// The blind judge on one plan: (pass, the remedy it found, cost). No tools, a fresh session that
    /// is not persisted, run from the run dir after borg is uninstalled.
    fn judge(&mut self, plan: &str) -> (bool, String, f64) {
        let prompt = format!("{JUDGE}\n\nThe class:\n```java\n{BROADCASTER}```\n\nThe plan:\n<plan>\n{plan}\n</plan>");
        let argv = [
            "claude",
            "-p",
            &prompt,
            "--model",
            "sonnet",
            "--tools",
            "",
            "--output-format",
            "json",
            "--json-schema",
            JUDGE_SCHEMA,
            "--no-session-persistence",
            "--strict-mcp-config",
            "--setting-sources",
            "project",
            "--max-budget-usd",
            "0.3",
        ];
        let (_, out, err) = self.exec(&self.dir.clone(), &argv, 300);
        let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
        let cost = v["total_cost_usd"].as_f64().unwrap_or(0.0);
        self.judge_cost += cost;
        let verdict = v
            .get("structured_output")
            .cloned()
            .filter(|s| s.is_object())
            .or_else(|| v["result"].as_str().and_then(|t| serde_json::from_str(t.trim()).ok()))
            .unwrap_or(Value::Null);
        if verdict.is_null() {
            eprintln!("    judge: no verdict: {} {}", out.chars().take(300).collect::<String>(), err.chars().take(300).collect::<String>());
        }
        (verdict["pass"] == true, verdict["remedy"].as_str().unwrap_or_default().to_string(), cost)
    }

    fn save(&self, harness: &Value) {
        let v = json!({"harness": harness, "check_definitions": check_definitions(),
                       "home": self.home, "borg": self.borg, "model": self.model, "drone_cost_usd": self.cost, "judge_cost_usd": self.judge_cost,
                       "checks": self.checks, "steps": self.steps,
                       "passed": self.checks.iter().filter(|c| c["pass"] == true).count(), "total": self.checks.len()});
        fs::write(self.dir.join("results.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }
}

pub fn main(raw: &[String]) {
    let (mut run, mut model, mut setup_only) = (String::new(), "sonnet".to_string(), false);
    let mut it = raw.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--run" => run = it.next().expect("--run NAME").clone(),
            "--model" => model = it.next().expect("--model M").clone(),
            "--setup-only" => setup_only = true,
            other => panic!("unknown flag {other}"),
        }
    }
    if run.is_empty() {
        run = format!("run-{}", now_s() as u64);
    }
    let root = fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    let borg = root.join("target/release/borg");
    assert!(borg.exists(), "cargo build --release -p borg-ai first");
    let dir = root.join("target/dogfood").join(&run);
    assert!(!dir.exists(), "{} exists; pick another --run", dir.display());
    fs::create_dir_all(dir.join("home")).unwrap();
    let dir = fs::canonicalize(dir).unwrap();
    let home = dir.join("home");
    let real_home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let harness = git_revision(&root);
    let secrets = match prepare_home(&home, &real_home, &borg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("dogfood: {e}");
            std::process::exit(1);
        }
    };
    let mut cx = Ctx {
        dir: dir.clone(),
        home: home.clone(),
        secrets: secrets.dir.clone(),
        borg,
        model,
        checks: Vec::new(),
        steps: Default::default(),
        cost: 0.0,
        judge_cost: 0.0,
    };
    eprintln!("dogfood {run}: HOME={} · harness {harness}", home.display());
    let code = steps(&mut cx, setup_only);
    // Before anything else (and on a panic, by unwinding): no token may outlive the run.
    drop(secrets);
    cx.save(&harness);
    let passed = cx.checks.iter().filter(|c| c["pass"] == true).count();
    eprintln!("{passed}/{} checks passed · drone ${:.2} · judge ${:.2} · {}", cx.checks.len(), cx.cost, cx.judge_cost, dir.join("results.json").display());
    std::process::exit(code.max(i32::from(passed != cx.checks.len())));
}

fn steps(cx: &mut Ctx, setup_only: bool) -> i32 {
    let home = cx.home.clone();
    let real_creds = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/.credentials.json");
    let copy = cx.secrets.join("credentials.json");
    let creds_before = (fs::read(&real_creds).ok(), fs::read(&copy).ok());
    // Repos: work (Java relay service), private (notes), off (diary).
    let (corp, private, diary) = (home.join("work/relay"), home.join("private/notes"), home.join("diary"));
    git_repo(
        &corp,
        &[
            ("src/main/java/relay/Relay.java", RELAY),
            ("src/main/java/relay/FastRelay.java", FAST_RELAY),
            ("src/main/java/relay/fanout/Broadcaster.java", BROADCASTER),
        ],
        "ann@corp.example",
    );
    git_repo(&private, &[("notes.py", NOTES), ("notes.json", "[]\n")], "me@home.example");
    git_repo(&diary, &[("2026-10-09.md", "# 9 October\n\nWalked by the river. Thought about the relay design.\n")], "me@home.example");

    // Step 2: install for real (no --home: HOME is the sandbox's), then doctor.
    let (code, out, err) = cx.exec(&home, &["borg", "install", "claude", "--apply"], 60);
    let settings = fs::read_to_string(home.join(".claude/settings.json")).unwrap_or_default();
    let v: Value = serde_json::from_str(&settings).unwrap_or(Value::Null);
    let wired = ["UserPromptSubmit", "PostToolUse", "PostToolUseFailure", "Stop", "SessionEnd"]
        .iter()
        .filter(|e| v["hooks"][**e].to_string().contains("hook claude"))
        .count();
    cx.check(
        "install: exit 0, 5 events wired, the foreign Stop hook kept first",
        code == 0 && wired == 5 && v["hooks"]["Stop"][0]["hooks"][0]["command"] == "true",
        json!({"stdout": out, "stderr": err, "wired_events": wired}),
    );
    let (code, out, err) = cx.exec(&home, &["borg", "doctor"], 60);
    cx.check("doctor: exit 0, no FAIL", code == 0 && !out.contains("FAIL"), json!({"stdout": out, "stderr": err}));
    for (dir, want) in [(&corp, "work"), (&private, "private"), (&diary, "off")] {
        let (_, out, _) = cx.exec(dir, &["borg", "collective"], 30);
        cx.check(&format!("collective of {}: {want}", dir.strip_prefix(&home).unwrap().display()), out.starts_with(want), json!(out.trim()));
    }

    if !setup_only {
        sessions(cx, &corp, &private, &diary);
    }

    // Step 6: status, then uninstall restores settings.json exactly.
    let (code, out, _) = cx.exec(&home, &["borg", "status"], 60);
    cx.steps.insert("status".into(), json!(out));
    cx.check(
        "status: exit 0, both collectives listed",
        code == 0 && out.contains("collective work") && out.contains("collective private"),
        json!(out.lines().take(40).collect::<Vec<_>>()),
    );
    let (code, out, err) = cx.exec(&home, &["borg", "uninstall", "claude", "--apply"], 60);
    let after = fs::read_to_string(home.join(".claude/settings.json")).unwrap_or_default();
    cx.check(
        "uninstall: settings.json restored byte for byte",
        code == 0 && after == ORIGINAL_SETTINGS,
        json!({"stdout": out, "stderr": err, "equal": after == ORIGINAL_SETTINGS}),
    );
    if !setup_only {
        control_and_judge(cx, &corp);
    }
    // The sandbox never refreshed its copied token, and the real credentials were never written.
    let creds_after = (fs::read(&real_creds).ok(), fs::read(&copy).ok());
    cx.check(
        "credentials: sandbox copy never refreshed, real file unchanged",
        creds_after == creds_before,
        json!({"real_unchanged": creds_after.0 == creds_before.0, "copy_unchanged": creds_after.1 == creds_before.1}),
    );
    0
}

/// The B keywords a text contains.
fn named(t: &str) -> Vec<&'static str> {
    B_KEYWORDS.into_iter().filter(|k| t.to_lowercase().contains(k)).collect()
}

/// After uninstall: the control arm (B's prompt, borg's hooks gone), then the blind judge on B's
/// plan before the question, B's final plan and the control's plan, each in its own call.
fn control_and_judge(cx: &mut Ctx, corp: &Path) {
    // Claude Code's auto-memory would be a channel from A to the control; it must be empty.
    let memory: Vec<String> = tree(&cx.home.join(".claude/projects")).into_iter().map(|(p, _, _)| p).filter(|p| p.contains("/memory/")).collect();
    let control = cx.claude("B-control", corp, PROMPT_B, &uuid(), false, "0.5");
    let hooked = control.stream.contains("Before you finish") || control.stream.contains("hook claude");
    let texts = cx.steps.get("B-texts").cloned().unwrap_or(Value::Null);
    let mut verdicts = serde_json::Map::new();
    for (arm, plan) in [
        ("B-before-question", texts["before_the_question"].as_str().unwrap_or_default().to_string()),
        ("B-final", texts["final"].as_str().unwrap_or_default().to_string()),
        ("control", control.text.clone()),
    ] {
        if plan.trim().is_empty() {
            verdicts.insert(arm.into(), json!({"pass": null, "note": "no text"}));
            continue;
        }
        let (pass, remedy, cost) = cx.judge(&plan);
        eprintln!("    judge {arm}: {} · {remedy}", if pass { "remedy" } else { "no remedy" });
        verdicts.insert(arm.into(), json!({"pass": pass, "remedy": remedy, "cost_usd": cost, "keywords": named(&plan)}));
    }
    cx.steps.insert("B-judge".into(), json!({"verdicts": verdicts, "control_saw_borg": hooked, "auto_memory_files_before_control": memory}));
    cx.check("B: a blind judge finds a concrete atomic remedy in the final plan", verdicts["B-final"]["pass"] == true, json!(verdicts["B-final"]));
}

fn sessions(cx: &mut Ctx, corp: &Path, private: &Path, diary: &Path) {
    let store = cx.store();
    let corp_lessons = cx.collective("work").join("lessons.jsonl");
    let private_lessons = cx.collective("private").join("lessons.jsonl");

    // Step 3: session A, corrected on resume; SessionEnd runs `borg learn`.
    let a = uuid();
    let a1 = cx.claude("A1-plan", corp, PROMPT_A, &a, false, "0.5");
    let (seen1, _, _) = cx.wait_learn(&a, a1.ended_at);
    let a2 = cx.claude("A2-correction", corp, CORRECTION_A, &a, true, "0.5");
    let (seen, after_end_s, learn_s) = cx.wait_learn(&a, a2.ended_at);
    let episodes = jsonl(&cx.collective("work").join("episodes.jsonl"));
    let lessons = jsonl(&corp_lessons);
    let elsewhere: Vec<String> =
        tree(&store).into_iter().map(|(p, _, _)| p).filter(|p| p.ends_with("lessons.jsonl") && !p.starts_with("collectives/work/")).collect();
    cx.steps.insert(
        "A-learn".into(),
        json!({"learn_seen_after_A1": seen1, "learn_seen": seen, "after_session_end_s": after_end_s, "learn_run_s": learn_s,
        "episodes": episodes, "lessons": lessons}),
    );
    cx.check(
        "A: the correction became an episode in the work store",
        episodes.iter().any(|e| e["session"] == a.as_str() && e["correction"].as_str().is_some_and(|c| c.contains("TOCTOU"))),
        json!(episodes.len()),
    );
    let repos: Vec<&str> = episodes.iter().chain(&lessons).filter_map(|x| x["repo"].as_str()).collect();
    cx.check("A: episode and lesson name the repo they came from (relay)", !repos.is_empty() && repos.iter().all(|r| *r == "relay"), json!(repos));
    cx.check("A: SessionEnd ran borg learn", seen, json!({"after_session_end_s": after_end_s, "learn_run_s": learn_s}));
    cx.check(
        "A: a lesson in the work store, none elsewhere",
        !lessons.is_empty() && elsewhere.is_empty(),
        json!({"work": lesson_ids(&corp_lessons), "elsewhere": elsewhere}),
    );
    let toctou: Vec<String> = lesson_ids(&corp_lessons);

    // Step 4: session B, a similar task in another file: asked at Stop, and the drone answers. The
    // question itself says check-then-act and TOCTOU, so the keyword check only shows it was
    // answered; whether the plan fixes the bug is the blind judge's call (`control_and_judge`).
    let b = uuid();
    let b1 = cx.claude("B-plan", corp, PROMPT_B, &b, false, "0.5");
    let asked = asked_ids(&cx.state("work", &b));
    let before_ask = text_before_block(&b1.stream);
    cx.check(
        "B: the conscience asked the TOCTOU lesson at Stop",
        asked.iter().any(|i| toctou.contains(i)) && b1.stream.contains("Before you finish"),
        json!({"asked": asked, "lessons": toctou}),
    );
    cx.check(
        "B: the drone answered the question (names check-then-act/TOCTOU)",
        !named(&b1.text).is_empty(),
        json!({"final": named(&b1.text), "before_the_question": named(&before_ask)}),
    );
    cx.steps.insert("B-texts".into(), json!({"before_the_question": before_ask, "final": b1.text}));
    let (_, _, _) = cx.wait_learn(&b, b1.ended_at);

    // Step 5: C (private) corrected → lesson only in the private store.
    let corp_before = lesson_ids(&corp_lessons);
    let c = uuid();
    let _c1 = cx.claude("C1-plan", private, PROMPT_C, &c, false, "0.4");
    cx.wait_learn(&c, Instant::now());
    let c2 = cx.claude("C2-correction", private, CORRECTION_C, &c, true, "0.4");
    let (seen_c, after_c, learn_c) = cx.wait_learn(&c, c2.ended_at);
    let private_ids = lesson_ids(&private_lessons);
    cx.steps
        .insert("C-learn".into(), json!({"learn_seen": seen_c, "after_session_end_s": after_c, "learn_run_s": learn_c, "lessons": jsonl(&private_lessons)}));
    cx.check(
        "C: a lesson in the private store only",
        !private_ids.is_empty() && lesson_ids(&corp_lessons) == corp_before,
        json!({"private": private_ids, "work": lesson_ids(&corp_lessons)}),
    );

    // D (work), tempting the private lesson: never asked it, never shown it.
    let d = uuid();
    let d1 = cx.claude("D-plan", corp, PROMPT_D, &d, false, "0.5");
    let asked_d = asked_ids(&cx.state("work", &d));
    let private_questions: Vec<String> = jsonl(&private_lessons).iter().filter_map(|l| l["question"].as_str().map(str::to_string)).collect();
    let shown = private_questions.iter().any(|q| d1.stream.contains(q.as_str()));
    cx.check(
        "D: work drone never asked the private lesson",
        !asked_d.iter().any(|i| private_ids.contains(i)) && !shown,
        json!({"asked": asked_d, "private": private_ids}),
    );
    cx.wait_learn(&d, d1.ended_at);

    // E (off): a task and a correction; nothing anywhere under BORG_HOME changes.
    let before = tree(&store);
    let e = uuid();
    let e1 = cx.claude("E1", diary, PROMPT_E, &e, false, "0.2");
    cx.wait_learn(&e, e1.ended_at);
    let e2 = cx.claude("E2-correction", diary, CORRECTION_E, &e, true, "0.2");
    cx.wait_learn(&e, e2.ended_at);
    let after = tree(&store);
    let (_, grep, _) = cx.exec(&cx.home.clone(), &["grep", "-rl", &e, store.to_str().unwrap()], 30);
    cx.check(
        "E: the off repo left no file under BORG_HOME",
        before == after && grep.trim().is_empty(),
        json!({"files_before": before.len(), "files_after": after.len(), "mentions": grep.trim()}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-dogfood-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn asked_ids_reads_every_asked_line() {
        assert_eq!(asked_ids("task\n#borg-asked La,Lb\nRead x\n#borg-asked \n#borg-asked Lc\n"), ["La", "Lb", "Lc"]);
        assert!(asked_ids("no asks").is_empty());
    }

    #[test]
    fn hook_latency_pairs_start_and_response_by_id() {
        let line = |sub: &str, id: &str, ev: &str| json!({"type": "system", "subtype": sub, "hook_id": id, "hook_event": ev}).to_string();
        let timed = vec![
            (1.0, line("hook_started", "a", "Stop")),
            (1.001, line("hook_started", "b", "Stop")),
            (1.004, line("hook_response", "a", "Stop")),
            (1.250, line("hook_response", "b", "Stop")),
            (2.0, "not json".to_string()),
            (3.0, line("hook_response", "orphan", "PostToolUse")),
        ];
        assert_eq!(hook_latencies(&timed), json!({"Stop": [4.0, 249.0]}));
    }

    #[test]
    fn the_plan_before_the_block_is_found() {
        let a = |t: &str| json!({"type": "assistant", "message": {"content": [{"type": "text", "text": t}]}}).to_string();
        let hook = |out: &str| json!({"type": "system", "subtype": "hook_response", "hook_event": "Stop", "output": out}).to_string();
        let stream = [a("draft"), a("plan v1"), hook(""), hook(r#"{"decision":"block","reason":"Before you finish"}"#), a("plan v2")].join("\n");
        assert_eq!(text_before_block(&stream), "plan v1");
        assert_eq!(text_before_block(&[a("x"), hook("")].join("\n")), "");
    }

    #[test]
    fn the_correction_is_one_borg_captures() {
        // The correction must start the way an engineer's push-back does (learn::is_correction).
        for c in [CORRECTION_A, CORRECTION_C, CORRECTION_E] {
            assert!(c.starts_with("no — "), "{c}");
        }
    }

    #[test]
    fn sandbox_home_is_exported_and_the_environment_cleared() {
        let home = tmp("args");
        let (cmd, private) = sandbox_with(&home, &[&home], &[], &Extra { sandbox_home: Some(home.clone()), ..Extra::default() }, 5);
        let _ = fs::remove_dir_all(private);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let at = |w: &[&str]| args.windows(w.len()).position(|x| x == w);
        let h = home.display().to_string();
        let clear = at(&["--clearenv"]).expect("--clearenv");
        assert!(at(&["--setenv", "HOME", &h]).is_some_and(|i| i > clear), "HOME set after clearing: {args:?}");
        assert!(at(&["--bind", &h, &h]).is_some());
        assert!(!args.iter().any(|a| a.ends_with(".credentials.json")), "the real credentials are not bound");
        assert!(!args.iter().any(|a| a.starts_with("--setting-sources")));
        // The secrets are bound in after the HOME, so they shadow its (empty) mount points.
        let (src, dst) = (PathBuf::from("/s/credentials.json"), home.join(".claude/.credentials.json"));
        let (cmd, private) =
            sandbox_with(&home, &[&home], &[], &Extra { sandbox_home: Some(home.clone()), file_rw: vec![(src.clone(), dst.clone())], ..Extra::default() }, 5);
        let _ = fs::remove_dir_all(private);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let at = |w: &[&str]| args.windows(w.len()).position(|x| x == w);
        let secret = at(&["--bind", "/s/credentials.json", &dst.display().to_string()]).expect("secret bound");
        assert!(secret > at(&["--bind", &h, &h]).unwrap());
    }

    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "uuid() reads /proc")]
    fn prepare_home_copies_only_a_valid_oauth_entry() {
        let (real, home) = (tmp("real"), tmp("home"));
        fs::create_dir_all(real.join(".claude")).unwrap();
        let creds = |left_s: f64| json!({"claudeAiOauth": {"accessToken": "a", "expiresAt": (now_s() + left_s) * 1000.0}, "mcpOAuth": {"x": "secret"}});
        fs::write(real.join(".claude/.credentials.json"), creds(60.0).to_string()).unwrap();
        fs::write(real.join(".claude.json"), json!({"projects": {"/x": {}}, "mcpServers": {"m": {}}, "userID": "u"}).to_string()).unwrap();
        let err = prepare_home(&home, &real, Path::new("/b/borg")).unwrap_err();
        assert!(err.contains("expires"), "{err}");
        let home = tmp("home2");
        fs::write(real.join(".claude/.credentials.json"), creds(7200.0).to_string()).unwrap();
        let before = fs::read(real.join(".claude/.credentials.json")).unwrap();
        let secrets = prepare_home(&home, &real, Path::new("/b/borg")).unwrap();
        assert!(!secrets.dir.starts_with(&home));
        assert!(!home.join(".claude/.credentials.json").exists() && !home.join(".claude.json").exists(), "no secret in the persistent HOME");
        let copied: Value = serde_json::from_str(&fs::read_to_string(secrets.dir.join("credentials.json")).unwrap()).unwrap();
        assert!(copied.get("mcpOAuth").is_none() && copied["claudeAiOauth"]["accessToken"] == "a");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(secrets.dir.join("credentials.json")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&secrets.dir).unwrap().permissions().mode() & 0o777, 0o700);
        let cfg: Value = serde_json::from_str(&fs::read_to_string(secrets.dir.join("claude.json")).unwrap()).unwrap();
        assert!(cfg.get("projects").is_none() && cfg.get("mcpServers").is_none() && cfg["userID"] == "u");
        assert_eq!(fs::read_to_string(home.join(".claude/settings.json")).unwrap(), ORIGINAL_SETTINGS);
        assert_eq!(fs::read(real.join(".claude/.credentials.json")).unwrap(), before, "the real file is untouched");
        // The binds land where Claude Code looks; dropping the guard removes the copies and the
        // empty mount points bwrap leaves behind.
        let binds = Secrets::binds(&secrets.dir, &home);
        assert_eq!(binds[0].1, home.join(".claude/.credentials.json"));
        assert_eq!(binds[1].1, home.join(".claude.json"));
        fs::write(home.join(".claude/.credentials.json"), "").unwrap();
        let dir = secrets.dir.clone();
        drop(secrets);
        assert!(!dir.exists() && !home.join(".claude/.credentials.json").exists());
        let cfg: Value = serde_json::from_str(COLLECTIVES).unwrap();
        assert_eq!(cfg["off"][0], "~/diary");
    }

    /// Install, doctor, routing, status and uninstall inside the sandbox HOME, as a process: no model
    /// calls. Needs bwrap, `cargo build --release -p borg-ai` and a Claude login valid for an hour.
    /// cargo test -p borg-eval dogfood_setup -- --ignored --nocapture
    #[test]
    #[ignore]
    fn dogfood_setup_installs_and_restores() {
        let root = fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        // Not under /tmp: the sandbox mounts a fresh tmpfs there.
        let dir = root.join("target/dogfood-test").join(format!("setup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        let home = dir.join("home");
        let borg = root.join("target/release/borg");
        let secrets = prepare_home(&home, Path::new(&std::env::var("HOME").unwrap()), &borg).unwrap();
        let mut cx = Ctx {
            dir: dir.clone(),
            home: home.clone(),
            secrets: secrets.dir.clone(),
            borg,
            model: "sonnet".into(),
            checks: Vec::new(),
            steps: Default::default(),
            cost: 0.0,
            judge_cost: 0.0,
        };
        steps(&mut cx, true);
        // Before any assert: no token may outlive the test.
        drop(secrets);
        assert!(!cx.secrets.exists() && !home.join(".claude/.credentials.json").exists() && !home.join(".claude.json").exists());
        let failed: Vec<&Value> = cx.checks.iter().filter(|c| c["pass"] != true).collect();
        assert!(failed.is_empty(), "{failed:#?}");
        assert_eq!(cx.checks.len(), 8);
        let _ = fs::remove_dir_all(dir);
    }
}
