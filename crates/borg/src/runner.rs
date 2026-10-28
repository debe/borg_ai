//! Who answers borg's own model calls (learn, assimilate, reach, facets, the turn-end check): the
//! harness the drone runs in, so an engineer on omp or opencode needs no `claude` CLI.
//!
//! `pick` orders the candidates. A prefixed model (`claude:sonnet`, `omp:`, `opencode:provider/model`,
//! `codex:gpt-5`) is the only one. Otherwise the hook's harness (`BORG_HARNESS`, inherited by the
//! `borg learn` a hook spawns) comes first, then every other installed CLI of claude, omp, opencode:
//! a harness that cannot answer (a configured model the account no longer offers) does not leave
//! the engineer without lessons when another runner works. An unprefixed model name is a Claude
//! model, as before; the other runners use their own configured model.
//!
//! Claude and codex enforce the JSON schema themselves; omp and opencode get it in the prompt and
//! the answer is the last JSON object in their final text. Every child runs headless in the temp
//! dir: claude without tools or user settings, codex read-only and without hooks, omp without tools and extensions,
//! opencode without plugins and with every permission denied. No adapter loads, so a call never
//! recurses into borg, and transcript text in the prompt cannot make the model act. Each child runs
//! in a process group of its own that is killed when it ends or outlives the deadline.
//!
//! opencode 1.x and 2.x take a headless call differently (`Opencode`, told by capability from
//! `opencode run --help`, once per process): 1.x drops plugins with `--pure` and reads the deny-all
//! from `OPENCODE_PERMISSION`; 2.x knows neither, and without `--standalone` its call goes to the
//! engineer's background service, which loads borg's own plugin and ignores the call's environment.
//! Both store every call as a session (there is no ephemeral mode), filed under `PWD` rather than
//! the process cwd: `PWD` is the temp dir, so the calls stay out of the engineer's projects, and
//! their title (`OPENCODE_TITLE`) keeps them out of `borg assimilate`.

use serde_json::Value;
use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    Claude,
    Omp,
    Opencode,
    Codex,
}

impl Runner {
    pub fn name(self) -> &'static str {
        match self {
            Runner::Claude => "claude",
            Runner::Omp => "omp",
            Runner::Opencode => "opencode",
            Runner::Codex => "codex",
        }
    }

    /// Every runner borg knows.
    pub const ALL: [Runner; 4] = [Runner::Claude, Runner::Omp, Runner::Opencode, Runner::Codex];

    fn of(name: &str) -> Option<Runner> {
        Runner::ALL.into_iter().find(|r| r.name() == name)
    }
}

/// A runner and the model it is asked for (None: the runner's own default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub runner: Runner,
    pub model: Option<String>,
}

/// The order borg tries runners in after the drone's own harness.
pub const FALLBACK: [Runner; 3] = [Runner::Claude, Runner::Omp, Runner::Opencode];

/// Claude's model when none is configured.
const CLAUDE_DEFAULT: &str = "sonnet";

/// The runners to try, in order: `spec` is the configured model (`BORG_LEARN_MODEL`,
/// `BORG_PREDICT_MODEL`), `harness` the drone's, `on_path` whether a CLI is installed. Empty: no
/// runner is installed, or the model is `off` (tests that record episodes but must never spend).
pub fn pick(spec: Option<&str>, harness: Option<&str>, on_path: &dyn Fn(&str) -> bool) -> Vec<Choice> {
    let spec = spec.map(str::trim).filter(|s| !s.is_empty());
    if spec == Some("off") {
        return Vec::new();
    }
    if let Some((r, m)) = spec.and_then(|s| s.split_once(':')).and_then(|(r, m)| Some((Runner::of(r)?, m))) {
        return vec![Choice { runner: r, model: Some(m.to_string()).filter(|m| !m.is_empty()) }];
    }
    let first = harness.and_then(Runner::of).filter(|r| FALLBACK.contains(r));
    first
        .into_iter()
        .chain(FALLBACK.into_iter().filter(|r| Some(*r) != first))
        .filter(|r| on_path(r.name()))
        .map(|runner| Choice { runner, model: (runner == Runner::Claude).then(|| spec.unwrap_or(CLAUDE_DEFAULT).to_string()) })
        .collect()
}

/// `borg doctor`'s word on whether borg's model steps have a runner: any installed CLI of [`FALLBACK`],
/// else the installed one `spec` pins (Err: none). A CLI that answers only when pinned (codex) is named
/// with the settings that pin it.
pub fn availability(spec: Option<&str>, on_path: &dyn Fn(&str) -> bool) -> Result<String, String> {
    let installed = |spec| pick(spec, None, on_path).iter().map(|c| c.runner.name()).filter(|r| on_path(r)).collect::<Vec<_>>();
    let mut runners = installed(None);
    if runners.is_empty() {
        runners = installed(spec);
    }
    if !runners.is_empty() {
        return Ok(format!("model CLI available ({}): borg learn and the consequence model at Stop can run", runners.join(", ")));
    }
    let [tried @ .., last] = FALLBACK.map(Runner::name);
    let mut msg =
        format!("no model CLI on PATH ({} or {last}): borg learn cannot turn episodes into lessons, and the conscience stays silent at Stop", tried.join(", "));
    for r in Runner::ALL.into_iter().filter(|r| !FALLBACK.contains(r) && on_path(r.name())) {
        let r = r.name();
        msg.push_str(&format!("; {r} answers only when pinned: set BORG_PREDICT_MODEL and BORG_LEARN_MODEL to {r}:<model>"));
    }
    Err(msg)
}

/// `pick` for this process: the harness from `BORG_HARNESS`, CLIs looked up on PATH.
pub fn choose(spec: Option<&str>) -> Vec<Choice> {
    let harness = std::env::var("BORG_HARNESS").ok();
    pick(spec, harness.as_deref(), &on_path)
}

/// Whether a CLI of this name is on PATH.
pub fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// One structured call: the first of `candidates` to answer, all within `timeout`. The answer
/// matches `schema`; the cost is in USD (codex: tokens; None when the runner reported none). None
/// when every runner fails, times out or answers no JSON.
pub fn structured(candidates: &[Choice], prompt: &str, schema: &Value, timeout: Duration) -> Option<(Value, Option<f64>)> {
    let deadline = Instant::now() + timeout;
    candidates.iter().find_map(|c| {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| *d >= Duration::from_secs(1))?;
        ask(c, prompt, schema, left)
    })
}

fn ask(c: &Choice, prompt: &str, schema: &Value, timeout: Duration) -> Option<(Value, Option<f64>)> {
    match c.runner {
        Runner::Claude => claude(prompt, c.model.as_deref().unwrap_or(CLAUDE_DEFAULT), schema, timeout),
        Runner::Codex => codex(prompt, c.model.as_deref().unwrap_or("gpt-5"), schema, timeout),
        Runner::Omp => {
            let (text, cost) = omp_answer(&run_capped(omp_command(c.model.as_deref(), &with_schema(prompt, schema)), None, timeout)?)?;
            Some((last_json(&text)?, cost))
        }
        Runner::Opencode => {
            let cli = opencode();
            // 2.x: an empty config dir of this call's own keeps every plugin out (borg's among them).
            let scratch = match cli {
                Opencode::V2 => Some(Scratch::new("opencode")?),
                Opencode::V1 => None,
            };
            let empty = scratch.as_ref().map(|s| s.0.as_path());
            let cmd = opencode_command(c.model.as_deref(), &with_schema(prompt, schema), cli, empty, engineers_config());
            let (text, cost) = opencode_answer(&run_capped(cmd, None, timeout)?)?;
            Some((last_json(&text)?, cost))
        }
    }
}

/// The session title opencode stores borg's calls under (it has no ephemeral mode), so they can be
/// told apart from the engineer's own sessions.
pub const OPENCODE_TITLE: &str = "borg (model call)";

const SYSTEM: &str = "You are a careful analyst. You never use tools. You answer with exactly one JSON object and nothing else.";

/// The headless omp command.
fn omp_command(model: Option<&str>, prompt: &str) -> Command {
    let mut cmd = Command::new("omp");
    cmd.args(["-p", "--mode", "json", "--no-session", "--no-tools", "--no-extensions", "--no-skills", "--no-rules"]).args([
        "--no-title",
        "--no-lsp",
        "--system-prompt",
        SYSTEM,
    ]);
    if let Some(m) = model {
        cmd.args(["--model", m]);
    }
    cmd.arg(prompt).current_dir(std::env::temp_dir());
    cmd
}

/// Which `opencode run` this machine has (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opencode {
    /// `--pure`, `OPENCODE_PERMISSION`.
    V1,
    /// `--standalone`, a private server; config only through `OPENCODE_CONFIG*`.
    V2,
}

impl Opencode {
    /// From the text of `opencode run --help`: a capability, not a version number.
    pub fn of_help(help: &str) -> Opencode {
        if help.contains("--standalone") { Opencode::V2 } else { Opencode::V1 }
    }
}

/// This machine's opencode, asked once per process (V1 when it cannot be asked: nothing runs then).
pub fn opencode() -> Opencode {
    static CLI: OnceLock<Opencode> = OnceLock::new();
    *CLI.get_or_init(|| opencode_on(None))
}

/// The `opencode` found on `path` (None: this process's PATH). 1.x prints its help to stderr, 2.x
/// to stdout, so both are read.
fn opencode_on(path: Option<&OsStr>) -> Opencode {
    let tmp = std::env::temp_dir();
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "exec opencode run --help 2>&1"]).current_dir(&tmp).env("PWD", &tmp);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    Opencode::of_help(&String::from_utf8_lossy(&run_capped(cmd, None, Duration::from_secs(15)).unwrap_or_default()))
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    }
}

/// The engineer's global opencode config file under `config_home`, the first that exists. A 2.x call
/// replaces the config dir (and with it the plugins in it) by an empty one; this file brings back
/// the providers and models the call needs.
fn global_config(config_home: &Path) -> Option<PathBuf> {
    ["opencode.jsonc", "opencode.json", "config.json"].iter().map(|f| config_home.join("opencode").join(f)).find(|p| p.is_file())
}

/// The config file that holds the engineer's providers: the `OPENCODE_CONFIG` they set themselves
/// (`inherited`) when it names a file (a dotfiles setup keeps providers there), else `global_config`.
fn provider_config(inherited: Option<PathBuf>, config_home: &Path) -> Option<PathBuf> {
    inherited.filter(|p| p.is_file()).or_else(|| global_config(config_home))
}

/// `provider_config` of this process.
pub(crate) fn engineers_config() -> Option<PathBuf> {
    provider_config(std::env::var_os("OPENCODE_CONFIG").filter(|v| !v.is_empty()).map(PathBuf::from), &config_home())
}

/// The env a 2.x `--standalone` server gets from borg (a model call, `borg assimilate`'s listing and
/// exports): `--standalone` alone still loads the global plugins dir (borg's adapter among them) and
/// the project's `.opencode`. `empty` is an empty config dir of the call's own, which keeps the
/// plugins out; `config` (`provider_config`) brings back the providers; the project config stays
/// out. Credentials are in opencode's database, which none of this moves.
pub(crate) fn standalone_env(cmd: &mut Command, empty: Option<&Path>, config: Option<PathBuf>) {
    if let Some(d) = empty {
        cmd.env("OPENCODE_CONFIG_DIR", d);
    }
    match config {
        Some(c) => cmd.env("OPENCODE_CONFIG", c),
        None => cmd.env_remove("OPENCODE_CONFIG"),
    };
    cmd.env("OPENCODE_DISABLE_PROJECT_CONFIG", "1").env("OPENCODE_DISABLE_AUTOUPDATE", "1");
}

/// The headless opencode command. `empty` is the call's own empty config dir (2.x), `config` the
/// file with the engineer's providers (`provider_config`).
fn opencode_command(model: Option<&str>, prompt: &str, cli: Opencode, empty: Option<&Path>, config: Option<PathBuf>) -> Command {
    let mut cmd = Command::new("opencode");
    match cli {
        Opencode::V1 => {
            // --pure drops plugins (borg's adapter among them), not tools: every permission is denied
            // instead, and the skill and CLAUDE.md scans are skipped (they only add prompt tokens).
            cmd.args(["run", "--pure", "--format", "json", "--title", OPENCODE_TITLE])
                .env("OPENCODE_PERMISSION", r#"{"*":"deny"}"#)
                .env("OPENCODE_DISABLE_CLAUDE_CODE", "1")
                .env("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1");
        }
        Opencode::V2 => {
            // A private server that reads this env (the background service would not). The deny-all
            // goes in as config: with it the model is offered no tools at all.
            cmd.args(["run", "--standalone", "--format", "json", "--title", OPENCODE_TITLE]).env("OPENCODE_CONFIG_CONTENT", r#"{"permission":{"*":"deny"}}"#);
            standalone_env(&mut cmd, empty, config);
        }
    }
    cmd.env("OPENCODE_DISABLE_PROJECT_CONFIG", "1").env("OPENCODE_DISABLE_AUTOUPDATE", "1");
    if let Some(m) = model {
        cmd.args(["-m", m]);
    }
    // opencode files the session under PWD, which current_dir does not change.
    let tmp = std::env::temp_dir();
    cmd.arg(prompt).env("PWD", &tmp).current_dir(&tmp);
    cmd
}

fn with_schema(prompt: &str, schema: &Value) -> String {
    format!("{prompt}\n\nAnswer with only one JSON object that matches this JSON schema, no prose and no code fence:\n{schema}")
}

fn events(out: &[u8]) -> impl Iterator<Item = Value> + '_ {
    out.split(|b| *b == b'\n').filter_map(|l| serde_json::from_slice::<Value>(l).ok())
}

fn text_of(content: &Value) -> String {
    content.as_array().into_iter().flatten().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("")
}

/// omp `--mode json`: the last assistant `message_end`'s text and `usage.cost.total`.
pub(crate) fn omp_answer(out: &[u8]) -> Option<(String, Option<f64>)> {
    let m = events(out).filter(|e| e["type"] == "message_end" && e["message"]["role"] == "assistant").last()?;
    let m = &m["message"];
    if m["stopReason"] == "error" {
        return None;
    }
    Some((text_of(&m["content"]), m["usage"]["cost"]["total"].as_f64()))
}

/// opencode `run --format json`, 1.x or 2.x: the text of the last step and the summed `step_finish`
/// costs. A step is one assistant message: its text restarts at a `step_start` or at a new
/// `messageID`, since 2.x leaves out the answer step's `step_start` and `step_finish` (a text-only
/// call has neither). The cost is None when no step was priced or the answer's step was never
/// finished (2.x keeps that cost in the session only). An `error` event (1.x `error.name`, 2.x
/// `error.type`: an unavailable model, a failed request) is no answer.
pub(crate) fn opencode_answer(out: &[u8]) -> Option<(String, Option<f64>)> {
    let (mut text, mut step, mut cost, mut priced, mut open) = (String::new(), None::<String>, 0.0, false, false);
    for e in events(out) {
        let part = &e["part"];
        let message = part["messageID"].as_str().map(str::to_string);
        match e["type"].as_str() {
            Some("error") => return None,
            Some("step_start") => {
                text.clear();
                step = message;
            }
            Some("text") => {
                if message.is_some() && message != step {
                    text.clear();
                    step = message;
                }
                text.push_str(part["text"].as_str().unwrap_or_default());
                open = true;
            }
            Some("step_finish") => {
                if let Some(c) = part["cost"].as_f64() {
                    (cost, priced) = (cost + c, true);
                }
                open = false;
            }
            _ => {}
        }
    }
    Some((text, (priced && !open).then_some(cost)))
}

/// The last top-level JSON object in a model's text; fences and prose around it are ignored.
fn last_json(text: &str) -> Option<Value> {
    let (mut at, mut last) = (0, None);
    while let Some(off) = text[at..].find('{') {
        let start = at + off;
        let mut it = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match it.next() {
            Some(Ok(v)) if v.is_object() => {
                last = Some(v);
                at = start + it.byte_offset();
            }
            _ => at = start + 1,
        }
    }
    last
}

fn claude(prompt: &str, model: &str, schema: &Value, timeout: Duration) -> Option<(Value, Option<f64>)> {
    let mut cmd = Command::new("claude");
    cmd.args(["-p", prompt, "--model", model, "--tools", "", "--output-format", "json"])
        .args(["--json-schema", &schema.to_string(), "--setting-sources", "project", "--strict-mcp-config", "--no-session-persistence"])
        .current_dir(std::env::temp_dir());
    let res: Value = serde_json::from_slice(&run_capped(cmd, None, timeout)?).ok()?;
    let v = res.get("structured_output").cloned().or_else(|| serde_json::from_str(res["result"].as_str()?).ok())?;
    Some((v, res["total_cost_usd"].as_f64()))
}

/// A scratch dir of one codex or opencode call (or one output file), removed when the call ends on any path. Calls run in
/// parallel (facets, reach), so each gets its own. It starts empty: a stale dir of the same name is
/// removed first, and one that cannot be (someone else's in the shared temp dir) fails the call. It
/// is private (0700): it can hold a whole session export, and the temp dir is shared.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new(kind: &str) -> Option<Scratch> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("borg-{kind}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
        Some(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One codex call in `dir`: read-only, without the engineer's config and without any hook.
fn codex_command(model: &str, dir: &Path) -> Command {
    let mut cmd = Command::new("codex");
    // `--ignore-user-config` skips config.toml but still loads `$CODEX_HOME/hooks.json`, borg's own hooks:
    // only the feature switch keeps them out of this call (never `--dangerously-bypass-hook-trust`).
    cmd.args(["exec", "-m", model, "--ignore-user-config", "-c", "features.hooks=false", "--ephemeral", "--skip-git-repo-check", "-s", "read-only", "--json"])
        .arg("--output-schema")
        .arg(dir.join("schema.json"))
        .arg("-o")
        .arg(dir.join("out.json"))
        .arg("-")
        .current_dir(dir);
    cmd
}

/// codex reports tokens, not dollars: the "cost" is input plus output tokens.
fn codex(prompt: &str, model: &str, schema: &Value, timeout: Duration) -> Option<(Value, Option<f64>)> {
    let dir = Scratch::new("codex")?;
    std::fs::write(dir.0.join("schema.json"), schema.to_string()).ok()?;
    let out = run_capped(codex_command(model, &dir.0), Some(prompt.as_bytes()), timeout)?;
    let tokens: f64 = events(&out)
        .filter(|e| e["type"] == "turn.completed")
        .map(|e| e["usage"]["input_tokens"].as_f64().unwrap_or(0.0) + e["usage"]["output_tokens"].as_f64().unwrap_or(0.0))
        .sum();
    let v = serde_json::from_str(&std::fs::read_to_string(dir.0.join("out.json")).ok()?).ok()?;
    Some((v, Some(tokens)))
}

/// Stdout of `cmd` (fed `input` on stdin), or None when it cannot start or fails to finish within
/// `timeout` (then killed). `run_capped_status` without the exit status.
pub fn run_capped(cmd: Command, input: Option<&[u8]>, timeout: Duration) -> Option<Vec<u8>> {
    run_capped_status(cmd, input, timeout).map(|(_, out)| out)
}

/// The exit status and stdout of `cmd` (fed `input` on stdin), or None when it cannot start or fails
/// to finish within `timeout` (then killed). Stderr is discarded: a caller that wants it merges it
/// into stdout in the command itself (hive/oracle.rs runs `sh -c` with `exec 2>&1`). `run_bounded`
/// that drops a run cut off at the cap.
pub fn run_capped_status(cmd: Command, input: Option<&[u8]>, timeout: Duration) -> Option<(std::process::ExitStatus, Vec<u8>)> {
    let b = run_bounded(cmd, input, timeout)?;
    Some((b.status?, b.out))
}

/// One bounded run of a command: its stdout (as far as it got), and its exit status (None: cut off at
/// the cap).
#[derive(Debug)]
pub struct Bounded {
    pub out: Vec<u8>,
    pub status: Option<std::process::ExitStatus>,
}

impl Bounded {
    pub fn timed_out(&self) -> bool {
        self.status.is_none()
    }
}

/// `cmd` (fed `input` on stdin) run to its end or to `timeout`, whichever comes first; None only when
/// it cannot start. A run cut off at the cap keeps what it printed so far (a hive drone's iteration,
/// hive/run.rs).
///
/// `cmd` runs in a process group of its own, and the whole group is killed when the cap passes or
/// `cmd` exits (nothing it started, such as an MCP server, outlives it). The group also dies with this
/// process, on every platform ([`Watchdog`]): a harness that kills `borg` (its own hook timeout) never
/// leaves a model call running and spending, and a killed `borg debug hive run` leaves no drone at work.
pub fn run_bounded(mut cmd: Command, input: Option<&[u8]>, timeout: Duration) -> Option<Bounded> {
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);
    #[cfg(target_os = "linux")]
    {
        // SAFETY: only async-signal-safe calls (prctl, getppid, _exit) between fork and exec.
        let parent = unsafe { libc::getpid() };
        unsafe {
            cmd.pre_exec(move || {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::getppid() != parent {
                    libc::_exit(1); // borg already died between fork and prctl
                }
                Ok(())
            });
        }
    }
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    let mut child = cmd.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let group = child.id() as libc::pid_t;
    let watchdog = Watchdog::start(group);
    // SAFETY: kill(2) on the child's own process group (a negative pid), which this process created.
    // The watchdog goes first: it must never outlive the group and later hit a reused id.
    let kill_group = || {
        if let Some(w) = &watchdog {
            w.stop();
        }
        unsafe { libc::kill(-group, libc::SIGKILL) }
    };
    if let (Some(data), Some(mut pipe)) = (input, child.stdin.take()) {
        // Written on a thread: a child that reads slowly must not block the deadline below.
        let data = data.to_vec();
        helper(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    helper(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                kill_group(); // a straggler holding stdout open would block the reader forever
                return Some(Bounded { out: drain(&rx), status: Some(status) });
            }
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return Some(Bounded { out: drain(&rx), status: None });
            }
        }
    }
}

/// A thread of `run_bounded` that may outlive it (a straggler holds the pipe): it starts with SIGINT,
/// SIGHUP and SIGTERM blocked (a new thread inherits its spawner's mask, held so only around the spawn),
/// so a stop always reaches a thread that may trap it, and one sent while that thread holds them blocked
/// (hive/reexec.rs, across an exec) stays pending for the process.
fn helper(f: impl FnOnce() + Send + 'static) {
    // SAFETY: sigemptyset(3)/sigaddset(3) initialise the zeroed sets; pthread_sigmask(3) on this thread,
    // its old mask restored right after the spawn.
    unsafe {
        let (mut set, mut old): (libc::sigset_t, libc::sigset_t) = (std::mem::zeroed(), std::mem::zeroed());
        libc::sigemptyset(&mut set);
        for s in [libc::SIGINT, libc::SIGHUP, libc::SIGTERM] {
            libc::sigaddset(&mut set, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
        std::thread::spawn(f);
        libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
    }
}

/// Kills a process group once this process is gone, however it went (SIGKILL included): a `sh` in a
/// group of its own, deaf to the terminal's signals, blocks reading a pipe whose only writer is this
/// process (close-on-exec, so no other child holds it). The kernel closes it when this process dies,
/// the read returns, and the group is killed. Linux's `PR_SET_PDEATHSIG` reaches only the direct
/// child; this reaches the whole group, and works where that does not exist (macOS).
struct Watchdog {
    pid: libc::pid_t,
    _writer: std::io::PipeWriter,
}

impl Watchdog {
    /// None when the pipe or the `sh` cannot be made: the group then dies with `borg` only on Linux.
    fn start(group: libc::pid_t) -> Option<Watchdog> {
        use std::os::unix::process::CommandExt as _;
        let (reader, writer) = std::io::pipe().ok()?;
        let child = Command::new("/bin/sh")
            .args(["-c", "trap '' HUP INT QUIT TERM; read -r _; kill -9 \"-$1\"", "sh", &group.to_string()])
            .process_group(0)
            .stdin(reader)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        Some(Watchdog { pid: child.id() as libc::pid_t, _writer: writer })
    }

    /// Ends the watchdog without its kill, and reaps it.
    fn stop(&self) {
        // SAFETY: kill(2) and waitpid(2) on the watchdog this process started and has not reaped.
        unsafe {
            if libc::kill(self.pid, libc::SIGKILL) == 0 {
                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
            }
        }
    }
}

/// How long the output of a command that ended is still read: its group is gone, so the pipe closes
/// at once, unless a process that left the group (`setsid`, a daemon) holds it open.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// The reader's chunks until the pipe closes or `DRAIN_GRACE` passes: a process that left the group
/// and holds the pipe costs what it prints later, never the bound.
fn drain(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let deadline = Instant::now() + DRAIN_GRACE;
    let mut out = Vec::new();
    while let Ok(chunk) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        out.extend_from_slice(&chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `sleep` in a process group of its own, and whether it is still running after `wait`.
    fn group_alive_after(stop_first: bool) -> bool {
        use std::os::unix::process::CommandExt as _;
        let mut sleeper = Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
        let w = Watchdog::start(sleeper.id() as libc::pid_t).expect("a watchdog");
        if stop_first {
            w.stop();
        }
        drop(w); // closes the pipe, as the kernel does when borg dies
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if sleeper.try_wait().unwrap().is_some() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = sleeper.kill();
        let _ = sleeper.wait();
        true
    }

    #[test]
    fn the_watchdog_kills_the_group_once_its_writer_is_gone_and_never_after_a_stop() {
        assert!(!group_alive_after(false), "borg gone: the group dies with it");
        assert!(group_alive_after(true), "a stopped watchdog kills nothing");
    }

    /// A helper thread holds the stop signals blocked from its first instruction, its spawner's mask
    /// as it was: a stop reaches only a thread that may trap it (a hive waiter's re-exec loses none).
    #[test]
    #[cfg(target_os = "linux")]
    fn a_helper_thread_never_takes_a_stop() {
        let blocked = || {
            let s = std::fs::read_to_string("/proc/thread-self/status").unwrap();
            let hex = s.lines().find_map(|l| l.strip_prefix("SigBlk:")).unwrap().trim().to_string();
            u64::from_str_radix(&hex, 16).unwrap()
        };
        let stops = [libc::SIGINT, libc::SIGHUP, libc::SIGTERM].iter().fold(0u64, |m, s| m | 1 << (s - 1));
        let before = blocked();
        let (tx, rx) = std::sync::mpsc::channel();
        helper(move || tx.send(blocked()).unwrap());
        assert_eq!(rx.recv().unwrap() & stops, stops, "the helper holds SIGINT, SIGHUP and SIGTERM blocked");
        assert_eq!(blocked(), before, "the spawner's mask is as it was");
    }

    fn only(cli: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |c| cli.contains(&c)
    }

    fn runners(c: &[Choice]) -> Vec<Runner> {
        c.iter().map(|c| c.runner).collect()
    }

    #[test]
    fn a_prefixed_model_is_the_only_candidate() {
        let c = pick(Some("opencode:github-copilot/claude-haiku-5.5"), Some("omp"), &only(&["omp", "claude"]));
        assert_eq!(c, vec![Choice { runner: Runner::Opencode, model: Some("github-copilot/claude-haiku-5.5".into()) }]);
        assert_eq!(pick(Some("omp:"), None, &only(&[])), vec![Choice { runner: Runner::Omp, model: None }]);
        assert_eq!(runners(&pick(Some("codex:gpt-5"), None, &only(&[]))), [Runner::Codex]);
    }

    #[test]
    fn the_drones_harness_comes_first_then_every_other_installed_runner() {
        let all = only(&["claude", "omp", "opencode"]);
        assert_eq!(runners(&pick(None, Some("opencode"), &all)), [Runner::Opencode, Runner::Claude, Runner::Omp]);
        let c = pick(Some("opus"), Some("omp"), &all);
        assert_eq!(c[0], Choice { runner: Runner::Omp, model: None }, "omp keeps its own model");
        assert_eq!(c[1], Choice { runner: Runner::Claude, model: Some("opus".into()) }, "an unprefixed name is a Claude model");
        // The harness's CLI is missing: the installed ones answer.
        assert_eq!(runners(&pick(None, Some("opencode"), &only(&["omp"]))), [Runner::Omp]);
    }

    #[test]
    fn doctor_names_the_settings_a_pinned_only_runner_needs() {
        let m = availability(None, &only(&["codex"])).unwrap_err();
        assert!(m.contains("no model CLI") && m.contains("BORG_PREDICT_MODEL and BORG_LEARN_MODEL to codex:<model>"), "{m}");
        let m = availability(Some("codex:gpt-5"), &only(&["codex"])).unwrap();
        assert!(m.contains("(codex)"), "a pinned codex runs: {m}");
        assert!(availability(Some("codex:gpt-5"), &only(&[])).is_err(), "pinned, but not installed");
        assert_eq!(
            availability(None, &only(&["omp", "codex"])).as_deref(),
            Ok("model CLI available (omp): borg learn and the consequence model at Stop can run"),
            "as before whenever a fallback CLI is installed"
        );
    }

    #[test]
    fn a_codex_call_runs_no_hook_not_even_borgs_own() {
        let cmd = codex_command("gpt-5", Path::new("/tmp/x"));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|w| w == ["-c", "features.hooks=false"]), "{args:?}");
        assert!(args.iter().any(|a| a == "--ignore-user-config") && !args.iter().any(|a| a.contains("bypass-hook-trust")), "{args:?}");
    }

    #[test]
    fn off_picks_no_runner_even_when_all_are_installed() {
        assert!(pick(Some("off"), Some("opencode"), &only(&["claude", "omp", "opencode"])).is_empty());
    }

    #[test]
    fn without_a_harness_path_order_decides_and_nothing_installed_is_empty() {
        let c = pick(None, None, &only(&["claude", "omp"]));
        assert_eq!(c[0], Choice { runner: Runner::Claude, model: Some("sonnet".into()) });
        assert_eq!(runners(&pick(None, None, &only(&["opencode", "omp"]))), [Runner::Omp, Runner::Opencode]);
        assert!(pick(Some("sonnet"), None, &only(&[])).is_empty());
        assert_eq!(runners(&pick(None, Some("codex"), &only(&["opencode", "codex"]))), [Runner::Opencode], "codex only when named");
    }

    #[test]
    fn a_failing_runner_falls_through_to_the_next_and_the_deadline_is_shared() {
        // No CLI of these names exists: every candidate fails fast and the call is None, in time.
        let started = Instant::now();
        let c = [Choice { runner: Runner::Omp, model: Some("x".into()) }, Choice { runner: Runner::Opencode, model: None }];
        let fake = std::env::temp_dir().join(format!("borg-runner-nopath-{}", std::process::id()));
        std::fs::create_dir_all(&fake).unwrap();
        let mut cmd = omp_command(Some("x"), "p");
        cmd.env("PATH", &fake);
        assert!(run_capped(cmd, None, Duration::from_secs(2)).is_none());
        assert!(structured(&[], "p", &json!({}), Duration::from_secs(5)).is_none());
        assert!(structured(&c, "p", &json!({}), Duration::ZERO).is_none(), "no time left: nothing is started");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn omp_answer_is_the_last_assistant_message_and_its_cost() {
        let out = [
            json!({"type": "message_end", "message": {"role": "user", "content": [{"type": "text", "text": "q"}]}}),
            json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "{"}}),
            json!({"type": "message_end", "message": {"role": "assistant", "content": [{"type": "text", "text": "{\"ok\": true}"}],
                "usage": {"cost": {"total": 0.0143}}, "stopReason": "stop"}}),
            json!({"type": "agent_end"}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let (text, cost) = omp_answer(out.as_bytes()).unwrap();
        assert_eq!(last_json(&text), Some(json!({"ok": true})));
        assert_eq!(cost, Some(0.0143));
        let failed = json!({"type": "message_end", "message": {"role": "assistant", "content": [], "stopReason": "error"}}).to_string();
        assert_eq!(omp_answer(failed.as_bytes()), None);
    }

    #[test]
    fn opencode_answer_is_the_last_steps_text_with_summed_cost_and_an_error_is_none() {
        let out = [
            json!({"type": "step_start", "part": {"type": "step-start"}}),
            json!({"type": "text", "part": {"type": "text", "text": "Let me think {"}}),
            json!({"type": "step_finish", "part": {"type": "step-finish", "cost": 0.001}}),
            json!({"type": "step_start", "part": {"type": "step-start"}}),
            json!({"type": "text", "part": {"type": "text", "text": "{\"ok\": "}}),
            json!({"type": "text", "part": {"type": "text", "text": "true}"}}),
            json!({"type": "step_finish", "part": {"type": "step-finish", "cost": 0.0035}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let (text, cost) = opencode_answer(out.as_bytes()).unwrap();
        assert_eq!(text, "{\"ok\": true}");
        assert!((cost.unwrap() - 0.0045).abs() < 1e-12);
        let err = json!({"type": "error", "error": {"name": "APIError"}}).to_string();
        assert_eq!(opencode_answer(err.as_bytes()), None);
    }

    #[test]
    fn json_is_the_last_object_bare_fenced_or_inside_prose_and_never_panics() {
        assert_eq!(last_json("{\"a\":1}"), Some(json!({"a": 1})));
        assert_eq!(last_json("```json\n{\"a\":1}\n```"), Some(json!({"a": 1})));
        assert_eq!(last_json("Here it is: {\"a\": {\"b\": 2}} done"), Some(json!({"a": {"b": 2}})));
        assert_eq!(last_json("e.g. {\"a\":0}, so: {\"a\":1}"), Some(json!({"a": 1})), "an example before the answer");
        assert_eq!(last_json("The key } is {\"a\":1"), None, "a brace before a truncated object");
        assert_eq!(last_json("} { {\"a\":1} {"), Some(json!({"a": 1})));
        assert_eq!(last_json("no json"), None);
        assert_eq!(last_json("[1,2]"), None);
    }

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn env(cmd: &Command) -> Vec<(String, Option<String>)> {
        cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect()
    }

    fn set(cmd: &Command, k: &str, v: &str) -> bool {
        env(cmd).contains(&(k.to_string(), Some(v.to_string())))
    }

    #[test]
    fn omp_and_opencode_run_headless_without_borgs_adapter_or_tools() {
        let omp = args(&omp_command(None, "p"));
        for f in ["-p", "--no-extensions", "--no-tools", "--no-session"] {
            assert!(omp.contains(&f.to_string()), "{f}");
        }
        assert!(!omp.contains(&"--model".to_string()));
        let tmp = std::env::temp_dir();
        let cmd = opencode_command(Some("p/m"), "p", Opencode::V1, None, None);
        let oc = args(&cmd);
        assert!(oc.contains(&"--pure".to_string()) && oc.windows(2).any(|w| w == ["-m", "p/m"]));
        assert!(oc.windows(2).any(|w| w == ["--title", OPENCODE_TITLE]));
        assert_eq!(oc.last().unwrap(), "p");
        assert!(set(&cmd, "OPENCODE_PERMISSION", r#"{"*":"deny"}"#) && set(&cmd, "OPENCODE_DISABLE_PROJECT_CONFIG", "1"));
        // The session opencode stores lands in the temp dir, not in the engineer's project (PWD wins).
        assert!(set(&cmd, "PWD", &tmp.to_string_lossy()) && cmd.get_current_dir() == Some(tmp.as_path()));
    }

    #[test]
    fn opencode_2_runs_standalone_with_an_empty_config_dir_and_the_engineers_providers() {
        let tmp = std::env::temp_dir();
        let (empty, global) = (tmp.join("borg-empty"), tmp.join("cfg/opencode/opencode.json"));
        let cmd = opencode_command(Some("p/m"), "p", Opencode::V2, Some(&empty), Some(global.clone()));
        let oc = args(&cmd);
        assert_eq!(&oc[..6], ["run", "--standalone", "--format", "json", "--title", OPENCODE_TITLE]);
        assert!(!oc.contains(&"--pure".to_string()), "2.x rejects --pure");
        assert!(oc.windows(2).any(|w| w == ["-m", "p/m"]) && oc.last().unwrap() == "p");
        assert!(set(&cmd, "OPENCODE_CONFIG_DIR", &empty.to_string_lossy()));
        assert!(set(&cmd, "OPENCODE_CONFIG", &global.to_string_lossy()));
        assert!(set(&cmd, "OPENCODE_CONFIG_CONTENT", r#"{"permission":{"*":"deny"}}"#));
        assert!(set(&cmd, "OPENCODE_DISABLE_PROJECT_CONFIG", "1") && set(&cmd, "OPENCODE_DISABLE_AUTOUPDATE", "1"));
        assert!(set(&cmd, "PWD", &tmp.to_string_lossy()) && cmd.get_current_dir() == Some(tmp.as_path()));
        // 2.x does not read OPENCODE_PERMISSION; without a global config file none is passed on.
        assert!(!env(&cmd).iter().any(|(k, _)| k == "OPENCODE_PERMISSION"));
        let bare = opencode_command(None, "p", Opencode::V2, Some(&empty), None);
        assert!(env(&bare).contains(&("OPENCODE_CONFIG".into(), None)), "unset, not inherited");
        assert!(!args(&bare).contains(&"-m".to_string()));
    }

    #[test]
    fn opencode_version_is_told_by_capability_from_its_run_help() {
        assert_eq!(Opencode::of_help(include_str!("../tests/fixtures/opencode/v1/run-help.txt")), Opencode::V1);
        assert_eq!(Opencode::of_help(include_str!("../tests/fixtures/opencode/v2/run-help.txt")), Opencode::V2);
        assert_eq!(Opencode::of_help(""), Opencode::V1, "no opencode: nothing runs anyway");
    }

    #[test]
    fn the_global_config_is_the_first_file_that_exists() {
        let home = std::env::temp_dir().join(format!("borg-runner-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("opencode")).unwrap();
        assert_eq!(global_config(&home), None);
        std::fs::write(home.join("opencode/config.json"), "{}").unwrap();
        assert_eq!(global_config(&home), Some(home.join("opencode/config.json")));
        std::fs::write(home.join("opencode/opencode.json"), "{}").unwrap();
        assert_eq!(global_config(&home), Some(home.join("opencode/opencode.json")));
        std::fs::write(home.join("opencode/opencode.jsonc"), "{}").unwrap();
        assert_eq!(global_config(&home), Some(home.join("opencode/opencode.jsonc")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_inherited_opencode_config_keeps_the_engineers_providers() {
        let home = std::env::temp_dir().join(format!("borg-runner-inherited-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("opencode")).unwrap();
        let (custom, global) = (home.join("dotfiles-opencode.json"), home.join("opencode/opencode.json"));
        std::fs::write(&custom, "{}").unwrap();
        std::fs::write(&global, "{}").unwrap();
        assert_eq!(provider_config(Some(custom.clone()), &home), Some(custom.clone()));
        assert_eq!(provider_config(Some(home.join("gone.json")), &home), Some(global.clone()), "a missing file is no config");
        assert_eq!(provider_config(None, &home), Some(global));
        let cmd = opencode_command(None, "p", Opencode::V2, Some(&home), provider_config(Some(custom.clone()), &home));
        assert!(set(&cmd, "OPENCODE_CONFIG", &custom.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// PATH with the fake `opencode` (tests/fixtures/opencode/fake) first.
    fn fake_path() -> std::ffi::OsString {
        let fake = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/opencode/fake"));
        let rest = std::env::var_os("PATH").unwrap_or_default();
        std::env::join_paths(std::iter::once(fake).chain(std::env::split_paths(&rest))).unwrap()
    }

    #[test]
    fn the_version_sniff_reads_help_printed_on_stderr() {
        // The fake prints 2.x's flag on stderr, where 1.x prints its help.
        assert_eq!(opencode_on(Some(&fake_path())), Opencode::V2);
        let nowhere = std::env::temp_dir().join(format!("borg-runner-noopencode-{}", std::process::id()));
        std::fs::create_dir_all(&nowhere).unwrap();
        let bare = std::env::join_paths([nowhere.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
        assert_eq!(opencode_on(Some(&bare)), Opencode::V1, "no opencode");
        let _ = std::fs::remove_dir_all(nowhere);
    }

    #[test]
    fn a_scratch_dir_starts_empty_and_is_removed_after_the_call() {
        use std::os::unix::fs::PermissionsExt as _;
        let s = Scratch::new("test").unwrap();
        let dir = s.0.clone();
        assert!(dir.is_dir() && std::fs::read_dir(&dir).unwrap().next().is_none());
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "other users cannot read an export");
        drop(s);
        assert!(!dir.exists());
    }

    #[test]
    fn opencode_answers_of_both_versions_parse_from_real_run_output() {
        let v1 = |f: &str| opencode_answer(f.as_bytes());
        let (text, cost) = v1(include_str!("../tests/fixtures/opencode/v1/run.jsonl")).unwrap();
        assert_eq!((last_json(&text), cost), (Some(json!({"ok": true})), Some(0.003544025)));
        // Denied tool calls first, then the answer: only the last step's text counts.
        let (text, cost) = v1(include_str!("../tests/fixtures/opencode/v1/run-deny-all-forced-tool-call.jsonl")).unwrap();
        assert_eq!((text.as_str(), cost), ("Plan: I check closed.get() on the AtomicBoolean before deliver() in relay.rs.", Some(0.0)));
        assert_eq!(v1(include_str!("../tests/fixtures/opencode/v1/run-error-unknown-model.jsonl")), None, "1.x error.name");
        // 2.x: a text-only answer has no step events at all, so its cost is unknown.
        let (text, cost) = v1(include_str!("../tests/fixtures/opencode/v2/run.jsonl")).unwrap();
        assert_eq!((last_json(&text), cost), (Some(json!({"ok": true})), None));
        // 2.x: the answer step after a tool step has no step_finish: the answer alone, cost unknown.
        let (text, cost) = v1(include_str!("../tests/fixtures/opencode/v2/run-tools.jsonl")).unwrap();
        assert_eq!((text.as_str(), cost), ("Done: the mock finished this turn.", None));
        assert_eq!(v1(include_str!("../tests/fixtures/opencode/v2/run-error.jsonl")), None, "2.x error.type");
    }

    #[test]
    fn a_new_message_starts_a_new_step_even_without_step_start() {
        let out = [
            json!({"type": "text", "part": {"messageID": "m1", "text": "Let me look {"}}),
            json!({"type": "text", "part": {"messageID": "m2", "text": "{\"ok\": "}}),
            json!({"type": "text", "part": {"messageID": "m2", "text": "true}"}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        assert_eq!(opencode_answer(out.as_bytes()).unwrap(), ("{\"ok\": true}".to_string(), None));
    }

    #[test]
    fn a_runner_that_hangs_is_cut_off_with_everything_it_started() {
        let started = Instant::now();
        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        assert!(run_capped(sleep, None, Duration::from_secs(1)).is_none());
        // A grandchild holding stdout open does not block the reader after its parent exits.
        let mut sh = Command::new("sh");
        sh.args(["-c", "sleep 30 & echo hi"]);
        assert_eq!(run_capped(sh, None, Duration::from_secs(5)).unwrap(), b"hi\n");
        assert!(started.elapsed() < Duration::from_secs(5));
        let mut cat = Command::new("cat");
        cat.arg("-");
        assert_eq!(run_capped(cat, Some(b"{}"), Duration::from_secs(5)).unwrap(), b"{}");
    }

    #[test]
    fn run_capped_status_returns_the_exit_status_and_run_capped_drops_it() {
        let sh = |script: &str| {
            let mut c = Command::new("sh");
            c.args(["-c", script]);
            c
        };
        let (ok, out) = run_capped_status(sh("echo yes"), None, Duration::from_secs(5)).unwrap();
        assert!(ok.success());
        assert_eq!(out, b"yes\n");
        let (failed, out) = run_capped_status(sh("echo no; exit 3"), None, Duration::from_secs(5)).unwrap();
        assert_eq!((failed.code(), out.as_slice()), (Some(3), b"no\n".as_slice()));
        // A failing command still has its stdout through run_capped, as before.
        assert_eq!(run_capped(sh("echo no; exit 3"), None, Duration::from_secs(5)).unwrap(), b"no\n");
        assert!(run_capped_status(sh("sleep 5"), None, Duration::from_millis(200)).is_none());
    }

    #[test]
    fn a_run_cut_off_at_its_cap_keeps_what_it_printed() {
        let mut sh = Command::new("sh");
        sh.args(["-c", "echo started; sleep 30"]);
        let t0 = Instant::now();
        let b = run_bounded(sh, None, Duration::from_millis(500)).unwrap();
        assert!(b.timed_out() && t0.elapsed() < Duration::from_secs(5));
        assert_eq!(b.out, b"started\n");
        let mut ok = Command::new("echo");
        ok.arg("done");
        let b = run_bounded(ok, None, Duration::from_secs(5)).unwrap();
        assert!(!b.timed_out() && b.status.unwrap().success());
        assert!(run_bounded(Command::new("/nonexistent/borg-no-such-cli"), None, Duration::from_secs(1)).is_none());
    }

    #[test]
    fn a_process_that_left_the_group_and_holds_stdout_never_blocks_a_finished_run() {
        if Command::new("setsid").arg("--version").output().is_err() {
            return;
        }
        let mut sh = Command::new("sh");
        sh.args(["-c", "setsid sleep 30 & echo hi"]);
        let t0 = Instant::now();
        let b = run_bounded(sh, None, Duration::from_secs(20)).unwrap();
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert_eq!((b.out.as_slice(), b.status.is_some_and(|s| s.success())), (b"hi\n".as_slice(), true));
    }
}
