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
//! dir: claude without tools or user settings, codex read-only, omp without tools and extensions,
//! opencode without plugins and with every permission denied. No adapter loads, so a call never
//! recurses into borg, and transcript text in the prompt cannot make the model act. Each child runs
//! in a process group of its own that is killed when it ends or outlives the deadline.

use serde_json::Value;
use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
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

    fn of(name: &str) -> Option<Runner> {
        [Runner::Claude, Runner::Omp, Runner::Opencode, Runner::Codex].into_iter().find(|r| r.name() == name)
    }
}

/// A runner and the model it is asked for (None: the runner's own default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub runner: Runner,
    pub model: Option<String>,
}

/// The order borg tries runners in after the drone's own harness.
const FALLBACK: [Runner; 3] = [Runner::Claude, Runner::Omp, Runner::Opencode];

/// Claude's model when none is configured.
const CLAUDE_DEFAULT: &str = "sonnet";

/// The runners to try, in order: `spec` is the configured model (`BORG_LEARN_MODEL`,
/// `BORG_PREDICT_MODEL`), `harness` the drone's, `on_path` whether a CLI is installed. Empty: no
/// runner is installed.
pub fn pick(spec: Option<&str>, harness: Option<&str>, on_path: &dyn Fn(&str) -> bool) -> Vec<Choice> {
    let spec = spec.map(str::trim).filter(|s| !s.is_empty());
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

/// `pick` for this process: the harness from `BORG_HARNESS`, CLIs looked up on PATH.
pub fn choose(spec: Option<&str>) -> Vec<Choice> {
    let harness = std::env::var("BORG_HARNESS").ok();
    pick(spec, harness.as_deref(), &on_path)
}

fn on_path(cmd: &str) -> bool {
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
        Runner::Omp | Runner::Opencode => {
            let out = run_capped(command(c, &with_schema(prompt, schema)), None, timeout)?;
            let (text, cost) = match c.runner {
                Runner::Omp => omp_answer(&out)?,
                _ => opencode_answer(&out)?,
            };
            Some((last_json(&text)?, cost))
        }
    }
}

/// The session title opencode stores borg's calls under (it has no ephemeral mode), so they can be
/// told apart from the engineer's own sessions.
pub const OPENCODE_TITLE: &str = "borg (model call)";

const SYSTEM: &str = "You are a careful analyst. You never use tools. You answer with exactly one JSON object and nothing else.";

/// The headless command for omp or opencode.
fn command(c: &Choice, prompt: &str) -> Command {
    let mut cmd = Command::new(c.runner.name());
    match c.runner {
        Runner::Omp => {
            cmd.args(["-p", "--mode", "json", "--no-session", "--no-tools", "--no-extensions", "--no-skills", "--no-rules"]).args([
                "--no-title",
                "--no-lsp",
                "--system-prompt",
                SYSTEM,
            ]);
            if let Some(m) = &c.model {
                cmd.args(["--model", m]);
            }
        }
        _ => {
            // --pure drops plugins (borg's adapter among them), not tools: every permission is denied
            // instead, and the skill and CLAUDE.md scans are skipped (they only add prompt tokens).
            cmd.args(["run", "--pure", "--format", "json", "--title", OPENCODE_TITLE])
                .env("OPENCODE_PERMISSION", r#"{"*":"deny"}"#)
                .env("OPENCODE_DISABLE_CLAUDE_CODE", "1")
                .env("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1")
                .env("OPENCODE_DISABLE_PROJECT_CONFIG", "1")
                .env("OPENCODE_DISABLE_AUTOUPDATE", "1");
            if let Some(m) = &c.model {
                cmd.args(["-m", m]);
            }
        }
    }
    cmd.arg(prompt).current_dir(std::env::temp_dir());
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
fn omp_answer(out: &[u8]) -> Option<(String, Option<f64>)> {
    let m = events(out).filter(|e| e["type"] == "message_end" && e["message"]["role"] == "assistant").last()?;
    let m = &m["message"];
    if m["stopReason"] == "error" {
        return None;
    }
    Some((text_of(&m["content"]), m["usage"]["cost"]["total"].as_f64()))
}

/// opencode `run --format json`: the text of the last step and the summed `step_finish` costs.
/// An `error` event (an unavailable model, a failed request) is no answer.
fn opencode_answer(out: &[u8]) -> Option<(String, Option<f64>)> {
    let (mut text, mut cost, mut priced) = (String::new(), 0.0, false);
    for e in events(out) {
        match e["type"].as_str() {
            Some("error") => return None,
            Some("step_start") => text.clear(),
            Some("text") => text.push_str(e["part"]["text"].as_str().unwrap_or_default()),
            Some("step_finish") => {
                if let Some(c) = e["part"]["cost"].as_f64() {
                    (cost, priced) = (cost + c, true);
                }
            }
            _ => {}
        }
    }
    Some((text, Some(cost).filter(|_| priced)))
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

/// A scratch dir of one codex call, removed when the call ends on any path. Calls run in parallel
/// (facets, reach), so each gets its own.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Option<Scratch> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("borg-codex-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// codex reports tokens, not dollars: the "cost" is input plus output tokens.
fn codex(prompt: &str, model: &str, schema: &Value, timeout: Duration) -> Option<(Value, Option<f64>)> {
    let dir = Scratch::new()?;
    std::fs::write(dir.0.join("schema.json"), schema.to_string()).ok()?;
    let mut cmd = Command::new("codex");
    cmd.args(["exec", "-m", model, "--ignore-user-config", "--ephemeral", "--skip-git-repo-check", "-s", "read-only", "--json"])
        .arg("--output-schema")
        .arg(dir.0.join("schema.json"))
        .arg("-o")
        .arg(dir.0.join("out.json"))
        .arg("-")
        .current_dir(&dir.0);
    let out = run_capped(cmd, Some(prompt.as_bytes()), timeout)?;
    let tokens: f64 = events(&out)
        .filter(|e| e["type"] == "turn.completed")
        .map(|e| e["usage"]["input_tokens"].as_f64().unwrap_or(0.0) + e["usage"]["output_tokens"].as_f64().unwrap_or(0.0))
        .sum();
    let v = serde_json::from_str(&std::fs::read_to_string(dir.0.join("out.json")).ok()?).ok()?;
    Some((v, Some(tokens)))
}

/// Stdout of `cmd` (fed `input` on stdin), or None when it cannot start or fails to finish within
/// `timeout` (then killed).
///
/// `cmd` runs in a process group of its own, and the whole group is killed when the cap passes or
/// `cmd` exits (nothing it started, such as an MCP server, outlives it). On Linux it also dies with
/// this process: a harness that kills `borg` (its own hook timeout) never leaves a model call running
/// and spending.
pub fn run_capped(mut cmd: Command, input: Option<&[u8]>, timeout: Duration) -> Option<Vec<u8>> {
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
    // SAFETY: kill(2) on the child's own process group (a negative pid), which this process created.
    let kill_group = || unsafe { libc::kill(-group, libc::SIGKILL) };
    if let (Some(data), Some(mut pipe)) = (input, child.stdin.take()) {
        // Written on a thread: a child that reads slowly must not block the deadline below.
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                kill_group(); // a straggler holding stdout open would block the reader forever
                return reader.join().ok();
            }
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let mut cmd = command(&c[0], "p");
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

    #[test]
    fn omp_and_opencode_run_headless_without_borgs_adapter_or_tools() {
        let args = |c: &Choice| command(c, "p").get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let omp = args(&Choice { runner: Runner::Omp, model: None });
        for f in ["-p", "--no-extensions", "--no-tools", "--no-session"] {
            assert!(omp.contains(&f.to_string()), "{f}");
        }
        assert!(!omp.contains(&"--model".to_string()));
        let c = Choice { runner: Runner::Opencode, model: Some("p/m".into()) };
        let oc = args(&c);
        assert!(oc.contains(&"--pure".to_string()) && oc.windows(2).any(|w| w == ["-m", "p/m"]));
        assert_eq!(oc.last().unwrap(), "p");
        let cmd = command(&c, "p");
        let env: Vec<_> = cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
        assert!(env.contains(&("OPENCODE_PERMISSION".into(), Some(r#"{"*":"deny"}"#.into()))));
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
}
