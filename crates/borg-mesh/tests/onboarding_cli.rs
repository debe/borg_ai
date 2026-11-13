//! Onboarding through the binary, as an engineer does it: the admin runs `serve` and `invite`, the
//! new member runs `join <ticket>` (a dry run that writes nothing and dials nobody), then
//! `join <ticket> --apply`, and holds the admin's lesson. Relays are off: the test stays on
//! localhost and never touches the network.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-mesh-onboard-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("work/app")).unwrap();
    d
}

fn cmd(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg-mesh"));
    c.args(args)
        .args(["--dir", &root.join("work/app").display().to_string()])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("base"))
        .env("BORG_CONFIG", root.join("collectives.json"));
    c
}

fn run(root: &Path, args: &[&str]) -> Output {
    cmd(root, args).output().unwrap()
}

fn ok(root: &Path, args: &[&str]) -> String {
    let out = run(root, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into()
}

/// Liveness only: bounds a hung `serve`; every wait on it ends on what it does.
const LIVENESS: Duration = Duration::from_secs(60);

/// A running `borg-mesh serve`, stopped as an engineer stops it: Ctrl-C.
struct Serve(Child);

impl Serve {
    /// Sends SIGINT and waits for the exit; `None` if `serve` is still running after `LIVENESS`.
    fn interrupt(&mut self) -> Option<ExitStatus> {
        let _ = Command::new("kill").args(["-INT", &self.0.id().to_string()]).status();
        let start = Instant::now();
        while start.elapsed() < LIVENESS {
            if let Ok(Some(status)) = self.0.try_wait() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) && self.interrupt().is_none() && !std::thread::panicking() {
            panic!("serve did not stop on Ctrl-C");
        }
    }
}

/// `mesh/addr.json` as `invite` reads it.
#[derive(Deserialize)]
struct Reach<'a> {
    #[serde(borrow)]
    hints: Cow<'a, str>,
}

/// `serve` wrote its address in full: the file is created before its content is written.
fn reach_written(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| serde_json::from_str::<Reach>(&t).is_ok_and(|r| !r.hints.is_empty()))
}

#[test]
fn an_admin_invites_and_a_new_member_joins_with_one_ticket() {
    let admin = tmp("admin");
    let cfg = json!({"default": "off", "collectives": [{"name": "corp", "paths": [admin.join("work")], "share": "team", "relay": "disabled"}]});
    std::fs::write(admin.join("collectives.json"), cfg.to_string()).unwrap();
    ok(&admin, &["init"]);
    let lesson = json!({"id": "L1", "origin_ms": 1, "question": "who owns the lock?", "version": 1}).to_string();
    std::fs::write(admin.join("base/collectives/corp/lessons.jsonl"), format!("{lesson}\n")).unwrap();

    let mut serve = Serve(cmd(&admin, &["serve", "--bind", "127.0.0.1:0", "--interval", "1"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let reach = admin.join("base/collectives/corp/mesh/addr.json");
    let start = Instant::now();
    // A ready `serve` is one whose address `invite` can read.
    while !reach_written(&reach) {
        assert!(start.elapsed() < LIVENESS, "serve never wrote its address");
        std::thread::sleep(Duration::from_millis(50));
    }

    let out = ok(&admin, &["invite", "--role", "steer", "--expires", "1h"]);
    let ticket = out.lines().find(|l| l.starts_with("BORG1")).expect("a ticket line").to_string();
    assert!(out.contains('█'), "a QR code is printed");
    assert!(ok(&admin, &["invites"]).contains("steer 1 use(s) left"));

    // The new member: no config yet. The dry run shows the diff and changes nothing.
    let joiner = tmp("joiner");
    let cfg_path = joiner.join("collectives.json");
    let dry = ok(&joiner, &["join", &ticket]);
    assert!(dry.contains("dry run") && dry.contains("CHANGE-ME") && dry.contains("\"relay\": \"disabled\""), "{dry}");
    assert!(!cfg_path.exists(), "a dry run writes nothing");
    assert!(!joiner.join("base/collectives/corp/mesh/membership.json").exists(), "and dials nobody");

    let done = ok(&joiner, &["join", &ticket, "--apply"]);
    assert!(done.contains("joined corp") && done.contains("as steer"), "{done}");
    let store = joiner.join("base/collectives/corp");
    assert!(std::fs::read_to_string(store.join("lessons.jsonl")).unwrap().contains("who owns the lock?"), "synced at once");
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
    assert_eq!(written["collectives"][0]["paths"][0], "~/CHANGE-ME/repos-of-corp", "no repo is routed by the join");

    // The ticket is spent: a third machine is refused.
    let late = tmp("late");
    let refused = run(&late, &["join", &ticket, "--apply"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("already used"), "{}", String::from_utf8_lossy(&refused.stderr));
    assert!(ok(&admin, &["invites"]).contains("no open invites"));
    let members = ok(&admin, &["members"]);
    assert_eq!(members.lines().filter(|l| l.ends_with(" steer")).count(), 1, "{members}");
    let stopped = serve.interrupt().expect("serve stops on Ctrl-C");
    assert!(stopped.success(), "serve exits cleanly on Ctrl-C: {stopped}");
}

#[test]
fn version_prints_one_line() {
    for spelling in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_borg-mesh")).arg(spelling).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "borg-mesh {spelling}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("borg-mesh {}\n", env!("CARGO_PKG_VERSION")));
    }
}
