//! `borg hive sim-drone`: one iteration of a scripted drone, what the simulated harness's headless
//! command runs (harness/sim.rs `headless`), for the hive runner's suites. No model: it drives the real
//! hooks (`borg hook <sim> prompt|stop`) and the real drone commands (`borg hive claim|verify|contract`)
//! against the real board, as a headless session of a real harness would. Known only under
//! `harness::TEST_ENV`.
//!
//! The script (`BORG_SIM_SCRIPT`, JSON, every field optional): `{"usd_per_iter": 0.10, "drones": {"d2":
//! {"fail_first": true, "contract": {"scope": ["api.rs"], "text": "fn f() -> u8"}, "hang": false,
//! "edit_tests": false, "grow_tests": false, "work_s": 0}}}`. `fail_first`: the drone's first work ends at a failing oracle (its stop
//! blocked, the claim still held); `hang`: once it holds a claim it sleeps past any iteration cap; `edit_tests`: it also
//! overwrites `tests/sim_edit.txt`, dropping its lines (a weakened test, for the reward step's guardrails);
//! `grow_tests`: it appends a line to that file instead (a grown test, which they allow); `contract`: it publishes it;
//! `work_s`: it holds a work claim this long before writing (a slow drone, while the others wait);
//! `own_session`: it ignores the runner's session id and names none at its end (omp and opencode keep
//! their own); `crash`: once it holds a claim it exits 1 printing nothing (a harness that died after
//! spending, without its cost). A fix
//! task (`from`, hive/integrate.rs) is resolved by script: the conflict marker lines are dropped from its
//! scoped files and from every other file holding them, both sides kept.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Here, Role, board, goal, oracle};
use crate::harness::{Harness, sim};
use crate::{hook, learn, runner};

pub const SCRIPT_ENV: &str = "BORG_SIM_SCRIPT";
const CALL: Duration = Duration::from_secs(120);

#[derive(Debug, Default, Deserialize)]
struct Script {
    #[serde(default = "default_usd")]
    usd_per_iter: f64,
    #[serde(default)]
    drones: BTreeMap<String, Plan>,
}

fn default_usd() -> f64 {
    0.10
}

#[derive(Debug, Default, Clone, Deserialize)]
struct Plan {
    #[serde(default)]
    fail_first: bool,
    #[serde(default)]
    contract: Option<ContractPlan>,
    #[serde(default)]
    hang: bool,
    #[serde(default)]
    edit_tests: bool,
    #[serde(default)]
    grow_tests: bool,
    #[serde(default)]
    work_s: u64,
    #[serde(default)]
    own_session: bool,
    #[serde(default)]
    crash: bool,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ContractPlan {
    scope: Vec<String>,
    text: String,
}

struct Drone<'a> {
    cwd: &'a str,
    session: &'a str,
}

impl Drone<'_> {
    /// `borg hook <sim> <event>` with `input` (plus the session and directory): its answer.
    fn hook(&self, event: &str, mut input: Value) -> Option<Value> {
        input["session_id"] = json!(self.session);
        input["cwd"] = json!(self.cwd);
        let mut cmd = Command::new(std::env::current_exe().ok()?);
        cmd.args(["hook", sim::Sim.name(), event]).current_dir(self.cwd);
        let out = runner::run_capped(cmd, Some(input.to_string().as_bytes()), CALL)?;
        serde_json::from_slice(String::from_utf8_lossy(&out).trim().as_bytes()).ok()
    }

    /// `borg hive <args> --session S --dir CWD`: its exit code.
    fn hive(&self, args: &[&str]) -> Option<i32> {
        let mut cmd = Command::new(std::env::current_exe().ok()?);
        cmd.arg("hive").args(args).args(["--session", self.session, "--dir", self.cwd]).current_dir(self.cwd);
        runner::run_capped_status(cmd, None, CALL).and_then(|(s, _)| s.code())
    }

    fn stop(&self, again: bool) -> String {
        let out = self.hook("stop", json!({"last_assistant_message": "I made the change.", "stop_hook_active": again}));
        out.and_then(|v| v["reason"].as_str().map(String::from)).unwrap_or_default()
    }
}

/// The worktree the session `author` worked in, from the runner's join records; else `cwd`.
fn worktree_of(home: &Path, author: Option<&str>, cwd: &str) -> PathBuf {
    let dir = author.and_then(|a| super::joins(home).into_iter().rev().find(|j| j.session == a)).map(|j| j.dir);
    PathBuf::from(dir.unwrap_or_else(|| cwd.to_string()))
}

/// Whether `line` is a conflict marker git writes (`<<<<<<< x`, `=======`, `>>>>>>> x`, `||||||| x`).
fn marker(line: &str) -> bool {
    ["<<<<<<<", "|||||||", ">>>>>>>"].iter().any(|m| line.starts_with(m)) || line == "======="
}

/// Drops the conflict marker lines of `files` under `dir` and of every other file holding them (a
/// conflict outside the tasks' scopes, a test file both appended to), keeping both sides: the scripted
/// resolution of a fix task.
fn resolve_markers(dir: &Path, files: &[String]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(["grep", "-l", "-e", "^<<<<<<< "]);
    let out = runner::run_capped(cmd, None, CALL).unwrap_or_default();
    let mut paths: Vec<PathBuf> = files.iter().map(|f| dir.join(f)).collect();
    paths.extend(String::from_utf8_lossy(&out).lines().map(|l| dir.join(l)));
    paths.sort();
    paths.dedup();
    for p in paths {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let kept: String = text.lines().filter(|l| !marker(l)).map(|l| format!("{l}\n")).collect();
        let _ = std::fs::write(&p, kept);
    }
}

/// One iteration; prints `{"usd", "session", "text", "error"}` and exits 0.
pub fn main(here: &Here) -> i32 {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let (goal_id, name) = (var(super::ENV), var(super::DRONE_ENV));
    let script: Script = std::fs::read(var(SCRIPT_ENV))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Script { usd_per_iter: default_usd(), ..Script::default() });
    let plan = script.drones.get(&name).cloned().unwrap_or_default();
    let session = match var(sim::SESSION_ENV) {
        s if !s.is_empty() && !plan.own_session => s,
        _ => format!("sim-{name}-{}", learn::now_ms()),
    };
    let d = Drone { cwd: &here.cwd, session: &session };
    let named = if plan.own_session { "" } else { session.as_str() };
    let answer = |text: &str, error: bool| {
        println!("{}", json!({"usd": script.usd_per_iter, "session": named, "text": text, "error": error}));
        0
    };
    let text = d.hook("prompt", json!({"prompt": var(sim::PROMPT_ENV)})).map(|v| v.to_string()).unwrap_or_default();
    let Ok(g) = goal::load(&here.home, &goal_id) else { return answer(&text, true) };
    let tasks = board::fold(&board::read(&super::tasks_path(&here.home, &g.id))).tasks.len();
    // Each round finishes a task or ends the iteration: at most one per task, plus the last claim.
    for _ in 0..=tasks {
        let path = super::tasks_path(&here.home, &g.id);
        let mut b = board::fold(&board::read(&path));
        if board::holding(&b, &session, learn::now_ms()).is_none() {
            d.hive(&["claim"]);
            b = board::fold(&board::read(&path));
        }
        let Some(held) = board::holding(&b, &session, learn::now_ms()).cloned() else { break };
        if plan.crash {
            return 1;
        }
        if plan.hang {
            // Holding the claim, past any iteration cap.
            std::thread::sleep(Duration::from_secs(3600));
        }
        let task = &held.task;
        if task.role == Role::Verify {
            let author = task.of.as_ref().and_then(|w| b.tasks.get(w)).and_then(|w| w.author.clone());
            let dir = worktree_of(&here.home, author.as_deref(), &here.cwd);
            match oracle::run(&here.home, &g, task, &dir, Some(Duration::from_secs(60))) {
                oracle::Verdict::Pass => d.hive(&["verify", "--pass"]),
                _ => d.hive(&["verify", "--reject", "its oracle fails in the author's worktree"]),
            };
            d.stop(false);
            break;
        }
        if task.from.is_some() {
            resolve_markers(Path::new(&here.cwd), &task.files);
            if !d.stop(false).contains("done, verify task") {
                break;
            }
            continue;
        }
        std::thread::sleep(Duration::from_secs(plan.work_s));
        let file = task.files.first().cloned().unwrap_or_else(|| format!("{}.txt", task.id));
        let failed_once = hook::state_file(&format!("sim-{}-{name}", g.id)).with_extension("failed");
        let fail = plan.fail_first && !failed_once.exists();
        let write = |body: String| {
            let p = Path::new(&here.cwd).join(&file);
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(p, body);
        };
        write(if fail { format!("wip {}\n", task.id) } else { format!("done {} by {name}\n", task.id) });
        if plan.edit_tests {
            let t = Path::new(&here.cwd).join("tests/sim_edit.txt");
            let _ = std::fs::create_dir_all(t.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(t, format!("edited by {name}\n"));
        } else if plan.grow_tests {
            let t = Path::new(&here.cwd).join("tests/sim_edit.txt");
            let body = std::fs::read_to_string(&t).unwrap_or_default();
            let _ = std::fs::create_dir_all(t.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(t, format!("{body}grown by {name}\n"));
        }
        if let Some(c) = &plan.contract {
            d.hive(&["contract", &c.text, "--scope", &c.scope.join(",")]);
        }
        let mut reason = d.stop(false);
        if reason.contains("is not done") {
            if fail {
                // The drone's turn ends here, its oracle failing and its claim held.
                let _ = std::fs::write(&failed_once, "");
                break;
            }
            write(format!("done {} by {name}\n", task.id));
            reason = d.stop(true);
        }
        // A finished task hands over the next one in the same turn; anything else ends it.
        if !reason.contains("done, verify task") {
            break;
        }
    }
    answer(&text, false)
}
