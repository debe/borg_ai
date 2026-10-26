//! Codex CLI: native command hooks in `~/.codex/hooks.json` (`$CODEX_HOME`), each running
//! `<borg> hook codex <event>`. The file has Claude Code's shape, and so do the events and the answers
//! Codex reads back (`hookSpecificOutput.additionalContext`, `{"decision":"block","reason"}`), except
//! the edit tool: `apply_patch` sends a patch, which [`Codex::normalise`] turns into a canonical `Edit`.
//!
//! Codex runs a new or changed hook only once the engineer trusted it (`/hooks` in codex), and skips an
//! untrusted one without a word: `borg install` says so, and `borg doctor` asks Codex itself which of
//! borg's hooks are not trusted (`codex app-server`, `hooks/list`). borg never writes that trust.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use super::wiring::Hooks;
use super::{Caps, Harness, Headless, HeadlessEnd, Level, Probe, Run, SubAgent, Transcript, Wiring};

/// Codex hook events and the canonical borg events they become. Codex has no failure event (a failed
/// tool call fires no PostToolUse at all), so denials are not captured.
pub const EVENTS: [(&str, &str); 5] =
    [("UserPromptSubmit", "prompt"), ("PreToolUse", "pre_tool"), ("PostToolUse", "post_tool"), ("Stop", "stop"), ("SessionEnd", "end")];

/// Seconds Codex gives each hook, written explicitly (its own default is 600 s, and 1 s for SessionEnd,
/// which it clamps to 3 s). The trust Codex keeps covers the timeout too: changing one here makes every
/// engineer trust borg's hooks again.
pub const TIMEOUTS: [(&str, u64); 5] = [("prompt", 30), ("pre_tool", 10), ("post_tool", 30), ("stop", STOP_TIMEOUT_S), ("end", 3)];

/// The stop's timeout, and so its budget: the live wait, the stop's model call and the expert drone keep
/// inside it.
pub const STOP_TIMEOUT_S: u64 = 90;

/// How long `borg doctor` waits for Codex to list its hooks.
const LIST_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Codex;

impl Harness for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn caps(&self) -> Caps {
        Caps {
            // Both reach the next model request as a developer message (in code mode, after the whole
            // `exec` script ran).
            prompt_context: true,
            tool_context: true,
            early_delivery: false,
            steer_in_context: false,
            question_dialog: false,
            // `transcript_path` is a Codex rollout, not a Claude transcript (`normalise` drops it).
            transcript: Transcript::None,
            deny_capture: false,
            sends_pid: false,
            stop_budget: Some(Duration::from_secs(STOP_TIMEOUT_S)),
            headless_budget: false,
        }
    }

    /// `codex exec --json` in the drone's worktree, sandboxed to it (`-s workspace-write`), with the
    /// engineer's config and hooks on (unlike borg's own model calls, runner.rs `codex_command`), checked
    /// against `codex exec --help` of 0.153.2. Codex takes no budget and reports tokens, not dollars.
    fn headless(&self, run: &Headless) -> Option<Command> {
        let mut cmd = Command::new("codex");
        cmd.args(["exec", "--json", "-s", "workspace-write"]).arg("-C").arg(run.cwd);
        if let Some(m) = run.model {
            cmd.args(["-m", m]);
        }
        cmd.arg(run.prompt).current_dir(run.cwd).envs(run.env.iter().map(|(k, v)| (*k, v.as_str())));
        Some(cmd)
    }

    /// The thread id, the last agent message and whether the turn failed; never a cost (tokens only).
    fn headless_end(&self, out: &[u8]) -> HeadlessEnd {
        let mut end = HeadlessEnd::default();
        for e in out.split(|b| *b == b'\n').filter_map(|l| serde_json::from_slice::<Value>(l).ok()) {
            match e["type"].as_str() {
                Some("thread.started") => end.session = e["thread_id"].as_str().unwrap_or_default().into(),
                Some("item.completed") if e["item"]["type"] == "agent_message" => end.text = e["item"]["text"].as_str().unwrap_or_default().into(),
                Some("turn.failed" | "error") => end.error = true,
                _ => {}
            }
        }
        end
    }

    fn wiring(&self) -> Wiring {
        Wiring::CommandHooks { events: &EVENTS, timeouts: &TIMEOUTS, through_links: true }
    }

    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")).join("hooks.json")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "codex", tested: " (hooks tested with 0.153.2)" }
    }

    /// `apply_patch` becomes a canonical `Edit` (see [`patch_edit`]); the rollout path is dropped.
    fn normalise(&self, mut raw: Value) -> Value {
        let Some(m) = raw.as_object_mut() else { return raw };
        m.shift_remove("transcript_path");
        if m.get("tool_name").and_then(Value::as_str) == Some("apply_patch") {
            let cwd = m.get("cwd").and_then(Value::as_str).unwrap_or_default().to_string();
            let patch = match m.get("tool_input") {
                Some(Value::String(p)) => p.clone(),
                Some(v) => v.get("command").and_then(Value::as_str).unwrap_or_default().to_string(),
                None => String::new(),
            };
            if let Some(edit) = patch_edit(&patch, Path::new(&cwd)) {
                m.insert("tool_name".into(), json!("Edit"));
                m.insert("tool_input".into(), edit);
            }
        }
        raw
    }

    /// Each session's spawned agents ([`spawn_runs`]) under `$CODEX_HOME/sessions`, else
    /// `home/.codex/sessions`.
    fn sub_runs(&self, home: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
        let codex = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()).map_or_else(|| home.join(".codex"), PathBuf::from);
        spawn_runs(&codex.join("sessions"), session, since)
    }

    fn install_notes(&self) -> Vec<String> {
        vec!["open codex and run /hooks to trust borg's hooks (codex skips untrusted hooks silently)".into()]
    }

    /// Which of borg's hooks Codex does not trust, as Codex itself reports it. Only once borg's hooks are
    /// wired; doctor asks only when codex is installed and answers `--version`. A failed call is a note,
    /// never a failure.
    fn checks(&self, target: &Path) -> Vec<(Level, String)> {
        let Some(hooks) = Hooks::of(self) else { return Vec::new() };
        if hooks.wired(std::fs::read_to_string(target).ok().as_deref()).is_empty() {
            return Vec::new();
        }
        let home = target.parent().unwrap_or(Path::new("."));
        let cwd = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
        match hooks_list(home, &cwd, LIST_TIMEOUT) {
            Ok(list) => trust_findings(&list, &hooks),
            Err(why) => vec![(Level::Ok, format!("codex: could not ask codex whether it trusts borg's hooks ({why}); run /hooks in codex to check"))],
        }
    }
}

/// One `apply_patch` as the canonical edit: the first file the patch names (`*** Add|Update|Delete
/// File:`) as `file_path` with its added lines as `new_string`, each further file in `edits` the same
/// way. A rename (`*** Move to:` after an Update) names its destination as a further file, which gets
/// the hunk's added lines. Headers are matched on the trimmed line, as Codex does. Relative paths are
/// taken from `cwd`. None for a patch naming no file.
pub fn patch_edit(patch: &str, cwd: &Path) -> Option<Value> {
    let mut files: Vec<(String, Vec<&str>)> = Vec::new();
    for line in patch.lines() {
        let header = ["*** Add File: ", "*** Update File: ", "*** Delete File: ", "*** Move to: "].iter().find_map(|h| line.trim().strip_prefix(h));
        if let Some(path) = header {
            let p = Path::new(path.trim());
            files.push((if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }.display().to_string(), Vec::new()));
        } else if let (Some((_, added)), Some(text)) = (files.last_mut(), line.strip_prefix('+')) {
            added.push(text);
        }
    }
    let mut edits = files.into_iter().map(|(file_path, added)| json!({"file_path": file_path, "new_string": added.join("\n")}));
    let mut edit = edits.next()?;
    let rest: Vec<Value> = edits.collect();
    if !rest.is_empty() {
        edit["edits"] = json!(rest);
    }
    Some(edit)
}

// ---------------------------------------------------------------- spawned agents

/// One run per session whose agent spawned others (`spawn_agent`), oldest first: of `session` only when
/// given, from sessions that started at or after `since`. Every thread keeps its own rollout
/// (`<sessions>/YYYY/MM/DD/rollout-*.jsonl`), whose first line, `session_meta`, names a spawned
/// thread's parent (`source.subagent.thread_spawn.parent_thread_id`, with its `agent_path`, nickname
/// and role; checked against the serde names in codex 0.153.2). An agent spawned by a spawned agent
/// belongs to its root session's run, which is [`Run::loose`]. Codex has no structured output, so
/// findings come from an agent's text. This reader is built from codex's serde names and hand-made
/// rollouts, not checked against a real spawned one yet.
pub fn spawn_runs(sessions: &Path, session: Option<&str>, since: Option<i64>) -> Vec<Run> {
    let metas: Vec<(PathBuf, Value)> = rollouts(sessions).into_iter().filter_map(|f| Some((f.clone(), first_meta(&f)?))).collect();
    let file: std::collections::HashMap<&str, &Path> = metas.iter().filter_map(|(f, m)| Some((m["id"].as_str()?, f.as_path()))).collect();
    let parent = |m: &Value| m["source"]["subagent"]["thread_spawn"]["parent_thread_id"].as_str().map(String::from);
    let parents: std::collections::HashMap<String, String> = metas.iter().filter_map(|(_, m)| Some((m["id"].as_str()?.to_string(), parent(m)?))).collect();
    let mut runs: std::collections::BTreeMap<String, Run> = std::collections::BTreeMap::new();
    for (f, m) in metas.iter().filter(|(_, m)| parent(m).is_some()) {
        let id = root_of(&parents, m["id"].as_str().unwrap_or_default());
        if session.is_some_and(|s| s != id) {
            continue;
        }
        let from = parent(m).unwrap_or_default();
        let mut a = spawned(m, &id, &from);
        // A forked agent's rollout may open with the history it inherited (`fork_turns`): its parent's.
        let inherited = m["forked_from_id"].as_str().unwrap_or(&from);
        read_rollout(f, file.get(inherited).copied(), &mut a);
        runs.entry(id.clone()).or_insert_with(|| Run { id: id.clone(), session: id, loose: true, ..Default::default() }).agents.push(a);
    }
    let started: std::collections::HashMap<&str, i64> = metas.iter().filter_map(|(_, m)| Some((m["id"].as_str()?, ts(&m["timestamp"])))).collect();
    let mut out: Vec<Run> = runs
        .into_values()
        .map(|mut r| {
            r.agents.sort_by_key(|a| a.started);
            r.started = started.get(r.id.as_str()).copied().filter(|t| *t > 0).unwrap_or(r.agents[0].started);
            r
        })
        .filter(|r| since.is_none_or(|s| r.started >= s))
        .collect();
    out.sort_by_key(|r| r.started);
    out
}

/// The root of spawned thread `id`: its `parents` followed up to a thread nobody spawned.
fn root_of(parents: &std::collections::HashMap<String, String>, id: &str) -> String {
    let mut id = id.to_string();
    for _ in 0..parents.len() {
        match parents.get(&id) {
            Some(p) => id = p.clone(),
            None => break,
        }
    }
    id
}

/// The spawned agent of `session_meta` payload `m`, in the run of root thread `root`, spawned by
/// thread `from` (no parent agent when the root's own agent spawned it); its work not read yet.
fn spawned(m: &Value, root: &str, from: &str) -> SubAgent {
    let spawn = &m["source"]["subagent"]["thread_spawn"];
    let s = |k: &str| spawn[k].as_str().or_else(|| m[k].as_str()).unwrap_or_default().to_string();
    let path = s("agent_path");
    let label = path.rsplit('/').next().filter(|l| !l.is_empty()).map_or_else(|| s("agent_nickname"), String::from);
    SubAgent {
        id: m["id"].as_str().unwrap_or_default().to_string(),
        parent: if from == root { String::new() } else { from.to_string() },
        label,
        kind: s("agent_role"),
        started: ts(&m["timestamp"]),
        cwd: s("cwd"),
        ..Default::default()
    }
}

/// The `rollout-*.jsonl` files under `dir`, at any depth.
fn rollouts(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    let mut out = Vec::new();
    for p in entries {
        if p.is_dir() {
            out.extend(rollouts(&p));
        } else if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rollout-")) && p.extension().is_some_and(|e| e == "jsonl") {
            out.push(p);
        }
    }
    out
}

/// A rollout's `session_meta` payload, from its first line only.
fn first_meta(f: &Path) -> Option<Value> {
    let line = std::io::BufReader::new(std::fs::File::open(f).ok()?).lines().next()?.ok()?;
    let v: Value = serde_json::from_str(&line).ok()?;
    (v["type"] == "session_meta").then(|| v["payload"].clone())
}

fn ts(v: &Value) -> i64 {
    v.as_str().map_or(0, borg_trace::parse_ts)
}

/// A rollout's response items, in order.
fn items(f: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(f).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["type"] == "response_item").map(|v| v["payload"].clone()).collect()
}

/// Read one spawned agent's rollout into `a`: its task (its first user message that Codex did not
/// inject: AGENTS.md, environment and other tagged context), every tool input and text it produced.
/// The items it shares, from the start, with its parent's rollout (`parent`) are history it inherited,
/// never its own task or work.
fn read_rollout(f: &Path, parent: Option<&Path>, a: &mut SubAgent) {
    let own = items(f);
    let inherited = parent.map(items).unwrap_or_default();
    let skip = own.iter().zip(&inherited).take_while(|(o, i)| o == i).count();
    for p in &own[skip..] {
        let said = || p["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
        match (p["type"].as_str(), p["role"].as_str()) {
            (Some("message"), Some("user")) if a.prompt.is_empty() => {
                let t = said();
                let t = t.trim_start();
                if !t.starts_with('<') && !t.starts_with("# AGENTS.md") {
                    a.prompt = t.to_string();
                }
            }
            (Some("message"), Some("assistant")) => {
                let t = said();
                if !t.trim().is_empty() {
                    a.said(&t);
                }
            }
            (Some("function_call"), _) => {
                let args = p["arguments"].as_str().unwrap_or_default();
                a.called(&serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.into())));
            }
            (Some("custom_tool_call"), _) => a.called(&p["input"]),
            (Some("local_shell_call"), _) => a.called(&p["action"]),
            _ => {}
        }
    }
}

/// Codex's own list of its hooks for `cwd` (`codex app-server`, JSON-RPC over stdio: `initialize`,
/// `initialized`, `hooks/list`), with `CODEX_HOME` at `home`; the whole process group is killed after
/// the answer or `timeout`.
fn hooks_list(home: &Path, cwd: &Path, timeout: Duration) -> Result<Value, String> {
    use std::os::unix::process::CommandExt as _;
    let mut child = Command::new("codex")
        .arg("app-server")
        .env("CODEX_HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("codex app-server: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let msgs = [
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "borg-doctor", "title": null, "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"method": "initialized"}),
        json!({"id": 2, "method": "hooks/list", "params": {"cwds": [cwd]}}),
    ];
    let sent = msgs.iter().try_for_each(|m| writeln!(stdin, "{m}")).and_then(|_| stdin.flush());
    let answers = answer_to(2, stdout);
    let answer = sent.map_err(|e| e.to_string()).and_then(|_| answers.recv_timeout(timeout).map_err(|_| format!("no answer within {} s", timeout.as_secs())));
    // SAFETY: a plain kill(2) of the child's own process group (the npm launcher and the native binary).
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.wait();
    drop(stdin);
    let answer = answer?;
    match answer.get("error") {
        Some(e) => Err(format!("hooks/list: {e}")),
        None => Ok(answer),
    }
}

/// The JSON-RPC answer to request `id` among the lines `out` prints, read on a thread of its own.
fn answer_to(id: u64, out: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let answer = std::io::BufReader::new(out).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<Value>(&l).ok()).find(|v| v["id"] == id);
        if let Some(v) = answer {
            let _ = tx.send(v);
        }
    });
    rx
}

/// `borg doctor`'s findings from a `hooks/list` answer: borg's hooks (`hooks` recognises their commands)
/// that Codex would skip (untrusted, changed since trusted, disabled), and Codex's load warnings.
pub fn trust_findings(list: &Value, hooks: &Hooks) -> Vec<(Level, String)> {
    let data = list["result"]["data"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<(Level, String)> = Vec::new();
    let (mut trusted, mut skipped) = (0, Vec::new());
    for h in data.iter().flat_map(|d| d["hooks"].as_array().cloned().unwrap_or_default()) {
        let Some((_, ev)) = h["command"].as_str().and_then(|c| hooks.recognise(c)) else { continue };
        let status = h["trustStatus"].as_str().unwrap_or("unknown");
        match (h["enabled"].as_bool() != Some(false), status) {
            (true, "trusted" | "managed") => trusted += 1,
            (false, _) => skipped.push(format!("{ev} disabled")),
            (true, "modified") => skipped.push(format!("{ev} changed since trusted")),
            (true, s) => skipped.push(format!("{ev} {s}")),
        }
    }
    for w in data.iter().flat_map(|d| d["warnings"].as_array().cloned().unwrap_or_default()) {
        out.push((Level::Warn, format!("codex: {}", w.as_str().map_or_else(|| w.to_string(), str::to_string))));
    }
    if !skipped.is_empty() {
        out.push((Level::Warn, format!("codex: skips borg's hooks silently ({}): open codex and run /hooks to trust borg's hooks", skipped.join(", "))));
    } else if trusted == 0 {
        out.push((Level::Warn, "codex: lists none of borg's hooks (are hooks off in codex's config, features.hooks = false?)".into()));
    } else {
        out.push((Level::Ok, format!("codex: trusts borg's {trusted} hooks")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex").join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn a_forked_agents_inherited_history_is_neither_its_task_nor_its_work() {
        let sessions = std::env::temp_dir().join(format!("borg-codex-fork-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sessions);
        let day = sessions.join("2026/09/14");
        std::fs::create_dir_all(&day).unwrap();
        let rollout = |thread: &str, ts: &str, meta: Value, items: &[Value]| {
            let mut lines = vec![json!({"timestamp": ts, "type": "session_meta", "payload": meta})];
            lines.extend(items.iter().map(|p| json!({"timestamp": ts, "type": "response_item", "payload": p})));
            std::fs::write(day.join(format!("rollout-{ts}-{thread}.jsonl")), lines.iter().map(|v| format!("{v}\n")).collect::<String>()).unwrap();
        };
        let msg = |role: &str, text: &str| json!({"type": "message", "role": role, "content": [{"type": "input_text", "text": text}]});
        let edit = json!({"type": "custom_tool_call", "name": "apply_patch", "input": "*** Update File: src/parent.rs"});
        let parent = [msg("user", "build the net"), edit.clone(), msg("assistant", "Built.")];
        rollout("p", "2026-09-14T13-14-00Z", json!({"id": "p", "timestamp": "2026-09-14T13:14:00Z", "source": "cli"}), &parent);
        let source = json!({"subagent": {"thread_spawn": {"parent_thread_id": "p", "agent_path": "/root/fix_net", "agent_role": "worker"}}});
        let meta = json!({"id": "c", "timestamp": "2026-09-14T13:15:00Z", "forked_from_id": "p", "source": source});
        let mut own = parent.to_vec();
        own.extend([msg("user", "fix the arc"), json!({"type": "custom_tool_call", "name": "apply_patch", "input": "*** Update File: src/arc.rs"})]);
        rollout("c", "2026-09-14T13-15-00Z", meta, &own);
        let runs = spawn_runs(&sessions, None, None);
        assert_eq!(runs.len(), 1);
        let a = &runs[0].agents[0];
        assert_eq!((runs[0].id.as_str(), runs[0].loose, a.id.as_str(), a.prompt.as_str()), ("p", true, "c", "fix the arc"));
        assert!(a.work.contains("src/arc.rs") && !a.work.contains("src/parent.rs"), "{}", a.work);
        let _ = std::fs::remove_dir_all(sessions);
    }

    #[test]
    fn a_one_file_patch_is_an_edit_of_that_file_and_the_rollout_is_dropped() {
        let raw = json!({"session_id": "s", "cwd": "/w", "transcript_path": "/x/rollout.jsonl", "tool_name": "apply_patch",
            "tool_input": {"command": "*** Begin Patch\n*** Update File: src/net.rs\n@@\n-drain\n+reset arc\n+on the place\n*** End Patch\n"},
            "tool_response": "Exit code: 0\nOutput:\nSuccess. Updated the following files:\nM src/net.rs\n"});
        let ev = Codex.normalise(raw);
        assert_eq!(ev["tool_name"], "Edit");
        assert_eq!(ev["tool_input"], json!({"file_path": "/w/src/net.rs", "new_string": "reset arc\non the place"}));
        assert!(ev.get("transcript_path").is_none(), "a Codex rollout is no Claude transcript: {ev}");
        assert!(ev["tool_response"].is_string(), "the tool's output stays as Codex sent it");
    }

    #[test]
    fn a_patch_of_several_files_names_every_file() {
        let raw = fixture("multi_patch.json");
        let ev = Codex.normalise(raw);
        assert_eq!(ev["tool_name"], "Edit");
        assert_eq!(ev["tool_input"]["file_path"], "/work/b.txt");
        assert_eq!(ev["tool_input"]["new_string"], "new file line");
        assert_eq!(ev["tool_input"]["edits"], json!([{"file_path": "/work/a.txt", "new_string": "hello world"}]));
        let gone = patch_edit("*** Begin Patch\n*** Delete File: /abs/old.rs\n*** End Patch\n", Path::new("/w")).unwrap();
        assert_eq!(gone, json!({"file_path": "/abs/old.rs", "new_string": ""}), "an absolute path is kept, a deletion adds nothing");
        assert_eq!(patch_edit("not a patch", Path::new("/w")), None);
        let moved = patch_edit("*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n@@\n-old\n+new\n*** End Patch\n", Path::new("/w")).unwrap();
        assert_eq!(
            moved,
            json!({"file_path": "/w/a.rs", "new_string": "", "edits": [{"file_path": "/w/b.rs", "new_string": "new"}]}),
            "a rename names its destination"
        );
        let indented = patch_edit("*** Begin Patch\n  *** Update File: x.rs\n+y\n*** End Patch\n", Path::new("/w")).unwrap();
        assert_eq!(indented, json!({"file_path": "/w/x.rs", "new_string": "y"}), "a header is matched trimmed, as Codex does");
    }

    #[test]
    fn a_shell_call_passes_through_and_a_stop_keeps_its_round_flag() {
        let bash = fixture("bash.json");
        let ev = Codex.normalise(bash.clone());
        assert_eq!((&ev["tool_name"], &ev["tool_input"], &ev["tool_response"]), (&bash["tool_name"], &bash["tool_input"], &bash["tool_response"]));
        assert!(ev.get("transcript_path").is_none());
        let stop = Codex.normalise(fixture("stop_again.json"));
        assert_eq!(stop["stop_hook_active"], true);
        assert_eq!(Codex.normalise(json!("not an object")), json!("not an object"));
    }

    #[test]
    fn every_hook_gets_its_timeout_and_the_file_is_codex_home_hooks_json() {
        let k = Hooks::of(&Codex).unwrap();
        let v: Value = serde_json::from_str(&k.install(None, Path::new("/b/borg")).unwrap().unwrap()).unwrap();
        let timeout = |native: &str| v["hooks"][native][0]["hooks"][0]["timeout"].as_u64();
        let got: Vec<Option<u64>> = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop", "SessionEnd"].iter().map(|n| timeout(n)).collect();
        assert_eq!(got, [Some(30), Some(10), Some(30), Some(90), Some(3)]);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "/b/borg hook codex stop");
        assert_eq!(Codex.caps().stop_budget, Some(Duration::from_secs(90)), "the stop's budget is its timeout");
        let none = |_: &str| None;
        assert_eq!(Codex.target(Path::new("/h"), &none), Path::new("/h/.codex/hooks.json"));
        let set = |k: &str| (k == "CODEX_HOME").then(|| PathBuf::from("/c"));
        assert_eq!(Codex.target(Path::new("/h"), &set), Path::new("/c/hooks.json"));
    }

    #[test]
    fn doctor_reports_borgs_untrusted_hooks_from_codexs_own_list() {
        let k = Hooks::of(&Codex).unwrap();
        let found = trust_findings(&fixture("hooks-list.json"), &k);
        let warn: Vec<&String> = found.iter().filter(|(l, _)| *l == Level::Warn).map(|(_, m)| m).collect();
        assert_eq!(warn.len(), 1, "{found:?}");
        assert!(warn[0].contains("prompt untrusted") && warn[0].contains("stop changed since trusted") && warn[0].contains("/hooks"), "{}", warn[0]);
        assert!(!warn[0].contains("post_tool"), "a trusted hook is fine: {}", warn[0]);
        assert!(!warn[0].contains("notify"), "someone else's hook is not borg's: {}", warn[0]);
        let mut all = fixture("hooks-list.json");
        for h in all["result"]["data"][0]["hooks"].as_array_mut().unwrap() {
            h["trustStatus"] = json!("trusted");
        }
        assert!(matches!(trust_findings(&all, &k).as_slice(), [(Level::Ok, m)] if m.contains("trusts borg's")), "{:?}", trust_findings(&all, &k));
        assert_eq!(trust_findings(&json!({"result": {"data": [{"hooks": []}]}}), &k)[0].0, Level::Warn, "wired, but codex lists none");
    }
}
