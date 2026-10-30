//! An absorbed lesson (absorb.rs) is delivered on no path and no harness, and `borg memory` exports,
//! absorbs and releases through the binary. Every harness of `harness::ALL` (the simulated one with early
//! delivery on) runs the same scenario through `borg hook <harness> <event>`: a control session before
//! the absorb shows each path delivering the lesson (early at the prompt where the harness takes it, the
//! stop's block, a live check's queued ask), then the same paths after `borg memory absorb` deliver it
//! nowhere, though the fake model still grades it as applying, while the constraint ratified from it
//! stays standing. `borg debug recall` and `borg debug prefilter` read the lessons as a drone gets them.
//! The model is the fake `claude` of tests/common: 0 tokens.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use borg::harness::{self, sim};
use common::{borg_as, context, prompts, setup, state};
use serde_json::{Value, json};

/// Words of the absorbed lesson's question (tests/common `Mdrain`), and of the constraint ratified from it.
const DRAIN: &str = "drain the budget place with an explicit consumer";
const STANDING: &str = "The Orion budget place is never reset";

/// `borg <args>` in `root` with `stdin`: (stdout, stderr, exit code).
fn cli(root: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().unwrap_or(-1))
}

fn repo(root: &Path) -> String {
    root.join("harbor_orion").display().to_string()
}

fn store(root: &Path) -> std::path::PathBuf {
    root.join("store/collectives/corp")
}

/// The constraint an engineer ratified from `Mdrain` (veto.rs `ratify`): a standing item of its own.
fn ratify(root: &Path) {
    let line = json!({"id": "cdrain", "text": STANDING, "lesson": "Mdrain", "scope": [], "repo": "harbor_orion", "reach": "", "by": "lead@x", "ms": 5, "source": "veto", "check": ""});
    std::fs::write(store(root).join("constraints.jsonl"), format!("{line}\n")).unwrap();
}

/// One drone of `harness` under `env`, in the Orion repo of tests/common.
struct Drone<'a> {
    root: &'a Path,
    harness: &'static str,
    session: String,
    env: Vec<(&'static str, &'static str)>,
}

impl Drone<'_> {
    fn hook(&self, event: &str, input: Value) -> String {
        borg_as(self.root, self.harness, event, &input, &self.env)
    }

    fn prompt(&self) -> String {
        context(&self.hook("prompt", json!({"session_id": self.session, "cwd": repo(self.root), "prompt": "Tidy the budget handling of the Orion net"})))
    }

    fn edit(&self) -> String {
        let input = json!({"session_id": self.session, "cwd": repo(self.root), "tool_name": "Edit",
            "tool_input": {"file_path": format!("{}/src/net.rs", repo(self.root)), "old_string": "drain the budget", "new_string": "use a reset arc on the budget place"}});
        context(&self.hook("post_tool", input))
    }

    fn stop(&self) -> String {
        self.hook("stop", json!({"session_id": self.session, "cwd": repo(self.root), "last_assistant_message": "Done: the budget place uses a reset arc.", "stop_hook_active": false}))
    }

    /// The live check's queue once the check the edit started finished (its mark is written last).
    fn live_queue(&self) -> String {
        let until = Instant::now() + Duration::from_secs(20);
        while state(self.root, &self.session, "live.mark").is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        state(self.root, &self.session, "queued")
    }
}

/// The env of a drone of `h`: early delivery where asked, the model pinned to the fake `claude`.
fn env(h: &str, early: bool, mode: &'static str) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        (harness::TEST_ENV, "1"),
        ("BORG_PREFILTER", "off"),
        ("BORG_PREDICT_MODEL", "claude:sonnet"),
        ("BORG_EARLY", if early { "on" } else { "off" }),
        ("BORG_MODE", mode),
        ("BORG_LIVE_DEBOUNCE_S", "0"),
    ];
    if h == "sim" {
        env.push((sim::CAPS_ENV, "prompt,tool,early,deny"));
    }
    env
}

#[test]
fn an_absorbed_lesson_is_delivered_on_no_harness_path() {
    for &h in harness::ALL {
        let name = h.name();
        let root = setup(&format!("absorb-{name}"));
        ratify(&root);
        let caps = if name == "sim" { sim::caps_of("prompt,tool,early,deny") } else { h.caps() };
        let drone = |session: &str, early: bool, mode: &'static str| Drone { root: &root, harness: name, session: session.into(), env: env(name, early, mode) };

        // Before: each path delivers it.
        let early = drone("c-early", true, "hook").prompt();
        assert_eq!(early.contains(DRAIN), caps.early_delivery, "{name}: early where the harness takes it: {early}");
        let d = drone("c-stop", false, "hook");
        d.prompt();
        d.edit();
        assert!(d.stop().contains("[Mdrain]"), "{name}: the stop asks it");
        let d = drone("c-live", false, "live");
        d.prompt();
        d.edit();
        assert!(d.live_queue().contains("Mdrain"), "{name}: a live check queues it");

        let (out, err, code) = cli(&root, &["memory", "absorb", "Mdrain", "--note", "skills/review@2026-10-30", "--dir", &repo(&root)], "");
        assert_eq!(code, 0, "{name}: {out} {err}");
        let asked = prompts(&root).len();

        // After: no path delivers it, though the fake model still grades it as applying.
        let early = drone("a-early", true, "hook").prompt();
        assert!(!early.contains(DRAIN), "{name}: not early: {early}");
        if caps.prompt_context {
            assert!(early.contains(STANDING), "{name}: the constraint ratified from it stays standing: {early}");
        }
        let d = drone("a-stop", false, "hook");
        d.prompt();
        assert!(!d.edit().contains(DRAIN), "{name}");
        let stop = d.stop();
        assert!(!stop.contains("[Mdrain]") && !stop.contains(DRAIN), "{name}: the stop never asks it: {stop}");
        let d = drone("a-live", false, "live");
        d.prompt();
        d.edit();
        assert!(!d.live_queue().contains("Mdrain"), "{name}: no live check queues it");
        assert!(!d.prompt().contains(DRAIN) && !d.edit().contains(DRAIN), "{name}: nor hands it over");
        let after = prompts(&root);
        assert!(after.len() > asked, "{name}: the model was still asked about the rest");
        assert!(after[asked..].iter().all(|p| !p.contains(DRAIN)), "{name}: never shown to the model either");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn borg_memory_exports_absorbs_and_releases() {
    let root = setup("memory-cli");
    let dir = repo(&root);
    let lessons = store(&root).join("lessons.jsonl").display().to_string();
    let recall = |root: &Path| cli(root, &["debug", "recall", &lessons], "drain the Orion budget place, no reset arc").0;
    assert!(recall(&root).contains("Mdrain"), "{}", recall(&root));

    // A directory of no collective: nothing, exit 0; usage errors exit 2.
    assert_eq!(cli(&root, &["memory", "export", "--dir", "/elsewhere/off"], ""), (String::new(), String::new(), 0));
    assert_eq!(cli(&root, &["memory"], "").2, 2);
    assert_eq!(cli(&root, &["memory", "absorb", "--dir", &dir], "").2, 2, "an id is required");
    assert_eq!(cli(&root, &["memory", "export", "Mdrain", "--dir", &dir], "").2, 2);

    let (out, _, code) = cli(&root, &["memory", "export", "--json", "--dir", &dir], "");
    assert_eq!(code, 0);
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((doc["schema"].as_u64(), doc["collective"].as_str()), (Some(1), Some("corp")));
    let m = doc["lessons"].as_array().unwrap().iter().find(|l| l["id"] == "Mdrain").unwrap().clone();
    assert_eq!((m["absorbed"].clone(), m["memory"]["alpha"].as_f64(), m["breadth"]["sessions"].as_u64()), (Value::Null, Some(1.0), Some(0)));
    assert_eq!(doc["lessons"].as_array().unwrap().iter().find(|l| l["id"] == "Old").unwrap()["superseded_by"], json!(["New"]));

    // A note longer than 1000 characters is refused, and nothing is written.
    let long = "x".repeat(1001);
    let (_, err, code) = cli(&root, &["memory", "absorb", "Mdrain", "--note", &long, "--dir", &dir], "");
    assert!(code == 2 && err.contains("at most 1000"), "{err}");
    assert!(!store(&root).join("absorbed.jsonl").exists());

    // Absorbed, with an id the store does not hold yet (accepted, with a warning).
    let (out, err, code) = cli(&root, &["memory", "absorb", "Mdrain", "Lnotyet", "--note", "skills/review@2026-10-30", "--dir", &dir, "--json"], "");
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Lnotyet is not a lesson of this collective (yet)"), "{err}");
    let written: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((written[0]["id"].as_str(), written[0]["known"].as_bool(), written[1]["known"].as_bool()), (Some("Mdrain"), Some(true), Some(false)));
    let log = std::fs::read_to_string(store(&root).join("absorbed.jsonl")).unwrap();
    let line: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    let mut keys: Vec<&str> = line.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    // The line the mesh checks; the store's line names no version, so neither does the absorb.
    assert_eq!(keys, ["by", "id", "kind", "ms", "note"], "{line}");
    assert_eq!((line["kind"].as_str(), line["note"].as_str()), (Some("absorb"), Some("skills/review@2026-10-30")));

    let (out, _, _) = cli(&root, &["memory", "export", "--absorbed", "--dir", &dir], "");
    assert!(out.contains("1 lessons, 1 absorbed") && out.contains("Mdrain") && out.contains("absorbed: skills/review@2026-10-30"), "{out}");
    assert!(!out.contains("G0"), "--absorbed lists absorbed lessons only: {out}");
    let (out, _, _) = cli(&root, &["memory", "export", "--json", "--since", "2999-01-01", "--dir", &dir], "");
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["lessons"], json!([]));
    assert_eq!(cli(&root, &["memory", "export", "--since", "yesterday", "--dir", &dir], "").2, 2);
    let (out, _, _) = cli(&root, &["status"], "");
    assert!(out.contains("absorbed     1, recurred 0"), "{out}");

    // Withheld from what a drone gets; the memory view still shows it.
    assert!(!recall(&root).contains("Mdrain"), "{}", recall(&root));
    let (prefilter, _, _) = cli(&root, &["debug", "prefilter", &lessons], "drain the Orion budget place, no reset arc");
    assert!(!prefilter.contains("Mdrain"), "{prefilter}");
    assert!(cli(&root, &["debug", "memory", &lessons], "").0.contains("\"Mdrain\""));

    // Released: delivered again.
    let (out, _, code) = cli(&root, &["memory", "release", "Mdrain", "--dir", &dir], "");
    assert!(code == 0 && out.contains("released Mdrain"), "{out}");
    assert!(recall(&root).contains("Mdrain"));
    let (out, _, _) = cli(&root, &["status"], "");
    assert!(out.contains("absorbed     0, recurred 0"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}
