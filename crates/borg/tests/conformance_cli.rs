//! The harness conformance suite for command-hook harnesses (docs/harnesses.md): every harness in
//! `harness::ALL` wired by command hooks (Claude Code, the simulated one, any added later) is installed
//! into a temp HOME, and the *installed* command string runs its recorded native payloads
//! (tests/fixtures/<harness>/). One scenario, the same for every harness: install, the hooks, live mode,
//! and what `borg top --json` (the web page's drones) shows. An assertion that does not apply is skipped
//! by a capability flag (`Caps`), never by the harness's name, and a flag that is off has its other side
//! asserted where a profile has it off. The model is the fake `claude` of
//! tests/common: 0 tokens. The adapter harnesses (omp, opencode) run the same scenario in
//! adapters/conformance/conformance.test.ts.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use borg::harness::{self, Caps, Harness, Transcript, Wiring, sim};
use common::{answer, prompts, setup, state};
use serde_json::{Value, json};

/// One harness under test, and for the simulated one the capabilities it runs with (`BORG_SIM_CAPS`).
struct Profile {
    h: &'static dyn Harness,
    sim_caps: Option<&'static str>,
}

impl Profile {
    fn caps(&self) -> Caps {
        self.sim_caps.map_or_else(|| self.h.caps(), sim::caps_of)
    }

    fn tag(&self) -> String {
        let caps = match self.sim_caps {
            None => "own",
            Some("") => "none",
            Some(c) => c,
        };
        format!("{}-{}", self.h.name(), caps.replace(',', "_"))
    }
}

/// Every command-hook harness; the simulated one with no extras, without tool context, with no flag at all
/// (no context anywhere, no denials, no pid), and with every flag: each flag both on and off.
fn profiles() -> Vec<Profile> {
    let mut out = Vec::new();
    for &h in harness::ALL {
        if !matches!(h.wiring(), Wiring::CommandHooks { .. }) {
            continue;
        }
        if h.hidden() {
            for caps in ["prompt,tool,deny", "prompt,deny", "", "prompt,tool,early,steer,dialog,transcript,deny,pid"] {
                out.push(Profile { h, sim_caps: Some(caps) });
            }
        } else {
            out.push(Profile { h, sim_caps: None });
        }
    }
    out
}

/// A drone of `p` in the Orion repo of tests/common, with borg installed into its HOME.
struct Drone<'a> {
    p: &'a Profile,
    root: PathBuf,
    session: String,
    env: Vec<(String, String)>,
}

fn borg_cli(root: &Path, args: &[&str], env: &[(String, String)]) -> (String, i32) {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_BIN", env!("CARGO_BIN_EXE_borg"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().unwrap_or(-1))
}

impl<'a> Drone<'a> {
    fn new(p: &'a Profile, tag: &str, mode: &str) -> Drone<'a> {
        let root = setup(&format!("conf-{tag}-{}", p.tag()));
        std::fs::write(root.join("transcript.jsonl"), std::fs::read(fixture_dir("claude").join("live-turn.jsonl")).unwrap()).unwrap();
        let mut env: Vec<(String, String)> = [
            ("BORG_QUEEN", "off"),
            ("BORG_LEARN", "off"),
            ("BORG_VECTORS", "off"),
            ("BORG_PREFILTER", "off"),
            ("BORG_EARLY", "off"),
            ("BORG_NOTIFY", "off"),
            ("BORG_LIVE_DEBOUNCE_S", "0"),
            // Pinned to the fake `claude`: a harness's own CLI would otherwise answer.
            ("BORG_PREDICT_MODEL", "claude:sonnet"),
            ("BORG_MODE", mode),
            // The simulated harness is known by name only here.
            (harness::TEST_ENV, "1"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        if let Some(c) = p.sim_caps {
            env.push((sim::CAPS_ENV.into(), c.into()));
        }
        let d = Drone { p, root, session: format!("s-{tag}"), env };
        let (out, code) = d.cli(&["install", p.h.name(), "--apply", "--home", &d.home().display().to_string()]);
        assert_eq!(code, 0, "{}: {out}", p.tag());
        d
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn cwd(&self) -> PathBuf {
        self.root.join("harbor_orion")
    }

    fn cli(&self, args: &[&str]) -> (String, i32) {
        borg_cli(&self.root, args, &self.env)
    }

    fn with(&mut self, k: &str, v: &str) {
        self.env.retain(|(x, _)| x != k);
        self.env.push((k.into(), v.into()));
    }

    /// The installed command for canonical `event`, read back from the harness's config.
    fn command(&self, event: &str) -> String {
        let Wiring::CommandHooks { events, .. } = self.p.h.wiring() else { unreachable!() };
        let native = events.iter().find(|(_, e)| *e == event).unwrap().0;
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(self.p.h.target(&self.home(), &|_| None)).unwrap()).unwrap();
        let cmds: Vec<String> = cfg["hooks"][native]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(String::from))
            .collect();
        assert_eq!(cmds.len(), 1, "{native}: {cmds:?}");
        cmds[0].clone()
    }

    /// The recorded native payload `step` of this harness, for this drone.
    fn payload(&self, step: &str, cwd: &Path) -> String {
        let text = std::fs::read_to_string(fixture_dir(self.p.h.name()).join(format!("{step}.json"))).unwrap();
        let text = text
            .replace("{{session}}", &self.session)
            .replace("{{cwd}}", &cwd.display().to_string())
            .replace("{{transcript}}", &self.root.join("transcript.jsonl").display().to_string());
        let mut v: Value = serde_json::from_str(&text).unwrap();
        if self.p.caps().sends_pid {
            v["pid"] = json!(std::process::id());
        }
        v.to_string()
    }

    /// Runs the installed hook for canonical `event` with `stdin`, as the harness would: (stdout, exit code).
    fn run(&self, event: &str, stdin: &str) -> (String, i32) {
        let path = format!("{}:{}", self.root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
        let mut c = Command::new("sh");
        c.args(["-c", &self.command(event)])
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("BORG_HOME", self.root.join("store"))
            .env("BORG_CONFIG", self.root.join("collectives.json"))
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().unwrap_or(-1))
    }

    /// One step of the scenario in the routed repo: the hook's answer as JSON (Null: none).
    fn step(&self, event: &str, step: &str) -> Value {
        let (out, code) = self.run(event, &self.payload(step, &self.cwd()));
        assert_eq!(code, 0, "{} {step}: fails open, always exit 0", self.p.tag());
        if out.trim().is_empty() { Value::Null } else { serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("{}: not JSON: {out}", self.p.tag())) }
    }

    fn state(&self, ext: &str) -> String {
        state(&self.root, &self.session, ext)
    }

    /// This drone in `borg top --json` (what the web page's drones panel shows).
    fn top(&self) -> Value {
        let (out, _) = self.cli(&["top", "--json"]);
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|_| panic!("{out}"));
        let drones = v.as_array().into_iter().flatten().flat_map(|c| c["drones"].as_array().cloned().unwrap_or_default());
        drones.into_iter().find(|d| d["session"] == self.session.as_str()).unwrap_or(Value::Null)
    }

    fn wait_for(&self, ext: &str, done: impl Fn(&str) -> bool) -> String {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let text = self.state(ext);
            if done(&text) || Instant::now() > until {
                return text;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Drone<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture_dir(harness: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(harness)
}

fn context(v: &Value) -> String {
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().is_dir() { out.extend(files(&e.path())) } else { out.push(e.path()) }
    }
    out.sort();
    out
}

#[test]
fn every_command_hook_harness_has_fixtures_for_the_scenario() {
    for p in profiles() {
        for step in ["prompt", "pre_tool", "read", "edit", "read_again", "bash", "deny", "correction", "stop", "stop_again", "end"] {
            assert!(fixture_dir(p.h.name()).join(format!("{step}.json")).is_file(), "{}: tests/fixtures/{}/{step}.json", p.tag(), p.h.name());
        }
    }
    assert!(profiles().len() >= 2, "claude and the simulated harness at least");
}

#[test]
fn install_is_a_dry_run_first_then_idempotent_kept_apart_and_reversible() {
    for p in profiles().into_iter().filter(|p| p.sim_caps.is_none_or(|c| c == "prompt,tool,deny")) {
        let root = setup(&format!("conf-install-{}", p.tag()));
        let home = root.join("home");
        let h = home.display().to_string();
        let target = p.h.target(&home, &|_| None);
        // Someone else's config, with a hook of their own.
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let theirs = r#"{"model": "mine", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]}}"#;
        std::fs::write(&target, theirs).unwrap();
        // A real HOME and every harness's config-dir variable point outside --home: none of them counts.
        let outside = root.join("outside");
        let mut env: Vec<(String, String)> = vec![(harness::TEST_ENV.into(), "1".into()), ("HOME".into(), outside.join("home").display().to_string())];
        for k in ["CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR", "OPENCODE_CONFIG_DIR", "XDG_CONFIG_HOME"] {
            env.push((k.into(), outside.join(k).display().to_string()));
        }

        let (out, code) = borg_cli(&root, &["install", p.h.name(), "--home", &h], &env);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains(&format!("hook {} stop", p.h.name())) && out.contains("dry run"), "{}: a diff, nothing written: {out}", p.tag());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), theirs);

        let (out, _) = borg_cli(&root, &["install", p.h.name(), "--apply", "--home", &h], &env);
        assert!(out.contains("wrote") && out.contains("backup"), "{out}");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(v["model"], "mine", "{}: foreign keys kept", p.tag());
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "notify-send done", "{}: their hook kept", p.tag());
        let backups = files(target.parent().unwrap()).into_iter().filter(|f| f.to_string_lossy().contains(".borg-backup-")).count();
        assert_eq!(backups, 1, "{}", p.tag());

        let (out, _) = borg_cli(&root, &["install", p.h.name(), "--home", &h], &env);
        assert!(out.contains("nothing to change"), "{}: idempotent: {out}", p.tag());
        let (out, _) = borg_cli(&root, &["status", "--home", &h], &env);
        assert!(out.lines().any(|l| l.trim_start().starts_with(p.h.name()) && l.contains("wired: ")), "{}: {out}", p.tag());
        let (out, _) = borg_cli(&root, &["doctor", "--home", &h], &env);
        assert!(out.contains(&format!("ok   {}: hooks wired to", p.h.name())), "{}: {out}", p.tag());

        let (out, _) = borg_cli(&root, &["uninstall", p.h.name(), "--apply", "--home", &h], &env);
        assert!(out.contains("wrote"), "{out}");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(v, serde_json::from_str::<Value>(theirs).unwrap(), "{}: only borg's entries removed", p.tag());
        // --home isolates: the target and its backup under it, nothing at the real HOME or config dirs.
        assert!(files(&root).iter().all(|f| f.starts_with(&home) || !f.to_string_lossy().contains("borg-backup")));
        assert_eq!(files(&outside), Vec::<PathBuf>::new(), "{}: --home ignores HOME and the config-dir variables", p.tag());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn hooks_record_the_turn_block_once_and_end_the_drone() {
    for p in profiles() {
        let caps = p.caps();
        let d = Drone::new(&p, "hooks", "hook");
        let tag = p.tag();

        // Off-repo: no state, no presence.
        let (out, code) = d.run("prompt", &d.payload("prompt", Path::new("/elsewhere/off")));
        assert_eq!((out.as_str(), code), ("", 0), "{tag}");
        assert!(files(&d.root.join("store/collectives/corp")).iter().all(|f| f.ends_with("lessons.jsonl")), "{tag}: off-repo writes nothing");

        // The prompt reaches the state; the drone shows as working.
        d.step("prompt", "prompt");
        assert!(d.state("prompts").contains("Tidy the budget handling"), "{tag}");
        assert_eq!(d.state("lastprompt"), "Tidy the budget handling of the Orion net", "{tag}");
        let top = d.top();
        assert_eq!((top["harness"].as_str(), top["state"].as_str()), (Some(p.h.name()), Some("working")), "{tag}: {top}");
        assert_eq!(top["prompt"], "Tidy the budget handling of the Orion net", "{tag}");

        // pre_tool says nothing while nobody steers.
        assert_eq!(d.step("pre_tool", "pre_tool"), Value::Null, "{tag}");

        // Tool calls: touches in the state, the edit's path and text in .edits.
        d.step("post_tool", "read");
        d.step("post_tool", "edit");
        d.step("post_tool", "bash");
        let net = d.cwd().join("src/net.rs").display().to_string();
        assert!(d.state("ctx").contains(&net), "{tag}");
        let edits = d.state("edits");
        assert!(edits.contains(&net) && edits.contains("reset arc on the budget place"), "{tag}: edits land in .edits with path: {edits}");
        // Where the turn's work comes from: the transcript, or the logged tool calls.
        let turn = d.state("turn");
        match caps.transcript {
            Transcript::Claude => assert_eq!(turn, "", "{tag}: a transcript harness logs no .turn"),
            Transcript::None => assert!(turn.contains(&format!("[Edit] {net}")) && turn.contains("[Bash] src/budget.rs"), "{tag}: {turn}"),
        }

        // The stop: one block with the lesson, inside the budget; the second round passes.
        answer(&d.root, &[("Mdrain", 3)]);
        let started = Instant::now();
        let stop = d.step("stop", "stop");
        assert!(started.elapsed() < caps.stop_budget.unwrap_or(harness::DEFAULT_STOP_BUDGET), "{tag}");
        assert_eq!(stop["decision"], "block", "{tag}: {stop}");
        assert!(stop["reason"].as_str().unwrap().contains("[Mdrain]"), "{tag}: {stop}");
        assert_eq!(d.top()["state"], "waiting", "{tag}");
        assert_eq!(d.step("stop", "stop_again"), Value::Null, "{tag}: one round per stop");

        // The end: the drone is ended.
        d.step("end", "end");
        assert_eq!(d.top()["state"], "ended", "{tag}");

        // Fail open: garbage on stdin.
        assert_eq!(d.run("stop", "not json"), (String::new(), 0), "{tag}");
    }
}

#[test]
fn a_paused_drone_has_its_tools_refused() {
    for p in profiles() {
        let d = Drone::new(&p, "pause", "hook");
        d.step("prompt", "prompt");
        let inbox = d.root.join("store/collectives/corp/steer/inbox.jsonl");
        std::fs::create_dir_all(inbox.parent().unwrap()).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let directive = |id: &str, kind: &str, text: &str| {
            json!({"id": id, "collective": "corp", "key": "ab".repeat(32), "engineer": "bob@x", "target": {"node": "", "session": d.session},
                "kind": kind, "text": text, "ms": now, "sig": "00", "role": "steer"})
            .to_string()
                + "\n"
        };
        // Steering: at the next tool call where the harness shows it there, else at the stop.
        std::fs::write(&inbox, directive("d1", "say", "use the mock db")).unwrap();
        let at_tool = context(&d.step("post_tool", "read"));
        let at_stop = d.step("stop", "stop");
        if p.caps().steer_in_context {
            assert!(at_tool.contains("use the mock db"), "{}: {at_tool}", p.tag());
        } else {
            assert!(at_tool.is_empty() && at_stop["reason"].as_str().unwrap_or_default().contains("use the mock db"), "{}: {at_stop}", p.tag());
        }
        std::fs::write(&inbox, std::fs::read_to_string(&inbox).unwrap() + &directive("p1", "pause", "")).unwrap();
        let deny = d.step("pre_tool", "pre_tool");
        assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny", "{}: {deny}", p.tag());
        assert!(deny["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("paused"), "{}", p.tag());
    }
}

#[test]
fn a_change_is_checked_live_and_its_ask_arrives_on_the_next_call_the_harness_reads() {
    for p in profiles() {
        let caps = p.caps();
        let tag = p.tag();
        let d = Drone::new(&p, "live", "live");
        d.step("prompt", "prompt");
        answer(&d.root, &[("Mdrain", 3)]);
        d.step("post_tool", "read");
        assert_eq!(d.state("live.n"), "", "{tag}: a read changes nothing");
        // An edit starts a detached check; the hook returns at once with nothing to say yet.
        let started = Instant::now();
        let out = d.step("post_tool", "edit");
        assert!(started.elapsed() < Duration::from_secs(5) && context(&out).is_empty(), "{tag}: {out}");
        assert_eq!(d.wait_for("queued", |q| q.contains("live ")), "live Mdrain\n", "{tag}: the ask is queued");
        d.wait_for("live.mark", |m| !m.is_empty());
        // The check read the turn so far: the transcript's prose, or the logged tool calls.
        let checked = &prompts(&d.root)[0];
        match caps.transcript {
            Transcript::Claude => assert!(checked.contains("I will replace the reset arc."), "{tag}: {checked}"),
            Transcript::None => assert!(checked.contains("[Edit] ") && checked.contains("reset arc on the budget place"), "{tag}: {checked}"),
        }
        // Delivered with the next tool call where the harness reads tool context.
        let next = context(&d.step("post_tool", "read_again"));
        if caps.tool_context {
            assert!(next.contains("borg check of your work so far") && next.contains("[Mdrain]"), "{tag}: {next}");
            // The stop: the check covered the turn, so no model call, and no block: delivered already.
            let stop = d.step("stop", "stop");
            assert_eq!(prompts(&d.root).len(), 1, "{tag}: a covered stop costs no model call");
            assert_eq!(stop, Value::Null, "{tag}: delivered already: {stop}");
            assert!(d.state("ctx").contains("#borg-skip live_covered"), "{tag}: {}", d.state("ctx"));
            continue;
        }
        assert_eq!(next, "", "{tag}: no tool context");
        // Else with the next prompt, where the harness reads a prompt's context.
        let at_prompt = context(&d.step("prompt", "correction"));
        if caps.prompt_context {
            assert!(at_prompt.contains("[Mdrain]"), "{tag}: delivered with the next prompt: {at_prompt}");
            assert_eq!(d.state("queued"), "", "{tag}: handed over");
            continue;
        }
        assert_eq!(at_prompt, "", "{tag}: no prompt context");
        assert_eq!(d.state("queued"), "live Mdrain\n", "{tag}: still waiting");
        // Read nowhere before: the stop asks it, never lost.
        let stop = d.step("stop", "stop");
        assert!(stop["reason"].as_str().unwrap_or_default().contains("[Mdrain]"), "{tag}: late context is not lost: {stop}");
        assert_eq!(d.state("queued"), "", "{tag}: asked at the stop");
    }
}

#[test]
fn early_delivery_only_where_the_harness_takes_it() {
    for p in profiles() {
        let mut d = Drone::new(&p, "early", "hook");
        d.with("BORG_EARLY", "on");
        let out = context(&d.step("prompt", "prompt"));
        if p.caps().early_delivery {
            assert!(out.contains("drain the budget place with an explicit consumer"), "{}: the prompt names Orion: {out}", p.tag());
        } else {
            assert!(out.is_empty(), "{}: {out}", p.tag());
        }
    }
}

#[test]
fn a_wordless_denial_and_the_next_prompt_become_an_episode() {
    for p in profiles() {
        let mut d = Drone::new(&p, "deny", "hook");
        d.with("BORG_LEARN", "on");
        d.with("BORG_PREDICT_MODEL", "off");
        d.step("prompt", "prompt");
        d.step("post_tool_failure", "deny");
        d.step("prompt", "correction");
        let episodes = std::fs::read_to_string(d.root.join("store/collectives/corp/episodes.jsonl")).unwrap_or_default();
        if p.caps().deny_capture {
            assert!(episodes.contains("drain it with an explicit consumer") && episodes.contains("rm -rf target"), "{}: {episodes}", p.tag());
        } else {
            // A harness whose failures carry no denials: the event is ignored.
            assert!(!episodes.contains("rm -rf target"), "{}: {episodes}", p.tag());
        }
    }
}

#[test]
fn a_gone_harness_process_reads_as_ended() {
    for p in profiles() {
        let d = Drone::new(&p, "pid", "hook");
        let mut v: Value = serde_json::from_str(&d.payload("prompt", &d.cwd())).unwrap();
        v["pid"] = json!(4_194_303); // above Linux's default pid_max: never running
        d.run("prompt", &v.to_string());
        // Only a harness that sends its own pid has it read; any other's is dropped.
        let want = if p.caps().sends_pid { "ended" } else { "working" };
        assert_eq!(d.top()["state"], want, "{}", p.tag());
    }
}

#[test]
fn a_slow_model_never_holds_the_stop_past_its_budget() {
    for p in profiles().into_iter().filter(|p| p.sim_caps.is_none_or(|c| c == "prompt,tool,deny")) {
        let mut d = Drone::new(&p, "slow", "hook");
        // A model that would take 30 s, inside a 6 s stop.
        let called = d.root.join("called");
        std::fs::write(d.root.join("bin/claude"), format!("#!/bin/sh\ntouch {}\nsleep 30\n", called.display())).unwrap();
        d.with("BORG_STOP_BUDGET_MS", "6000");
        d.step("prompt", "prompt");
        d.step("post_tool", "edit");
        let started = Instant::now();
        let stop = d.step("stop", "stop");
        assert!(started.elapsed() < Duration::from_secs(6), "{}: {:?}", p.tag(), started.elapsed());
        assert!(called.exists(), "{}: the stop asked the model", p.tag());
        assert_eq!(stop, Value::Null, "{}: the failed call fails open", p.tag());
    }
}

#[test]
fn the_simulated_harness_stays_hidden() {
    let root = setup("conf-hidden");
    let (help, _) = borg_cli(&root, &["help"], &[]);
    let (bash, _) = borg_cli(&root, &["completions", "bash"], &[]);
    for &h in harness::ALL {
        assert_eq!(help.contains(&format!("|{}", h.name())) || help.contains(&format!("<{}|", h.name())), !h.hidden(), "{}: {help}", h.name());
        let install = bash.lines().find(|l| l.contains("install)")).unwrap().replace('"', " ");
        assert_eq!(install.split_whitespace().any(|w| w == h.name()), !h.hidden(), "{}: {install}", h.name());
    }
    let home = root.join("home").display().to_string();
    let (out, _) = borg_cli(&root, &["install", "all", "--home", &home], &[]);
    assert!(!out.contains(".sim/"), "{out}");
    // Known by name only to the suites: for everyone else `sim` is no harness.
    let (out, code) = borg_cli(&root, &["install", "sim", "--home", &home], &[]);
    assert!(code == 2 && !out.contains(".sim/"), "{code}: {out}");
    let (out, code) = borg_cli(&root, &["hook", "sim", "prompt"], &[]);
    assert_eq!((out.as_str(), code), ("", 0));
    let (out, code) = borg_cli(&root, &["install", "sim", "--home", &home], &[(harness::TEST_ENV.into(), "1".into())]);
    assert!(code == 0 && out.contains(".sim/hooks.json"), "{code}: {out}");
    // An unknown harness fails open: no output, exit 0, nothing written.
    let (out, code) = borg_cli(&root, &["hook", "nope", "prompt"], &[]);
    assert_eq!((out.as_str(), code), ("", 0));
    let _ = std::fs::remove_dir_all(root);
}
