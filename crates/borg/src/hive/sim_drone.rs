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
//! `work_s`: it holds a work claim this long before writing (a slow drone, while the others wait), firing
//! the post_tool hook every `poll_s` (default 2; 0: never) meanwhile, as a real drone's tool calls do, and
//! drops the task at once when the hook says the claim was lost;
//! `one_task`: its turn ends after one finished task (one task per iteration, each costing `usd_per_iter`);
//! `node` (top level): a label the drones sign their files with (`by d1@<node>`);
//! `reject_first` (top level): the goal's first verdict is a reject whoever gives it, and a reworked task's
//! file then also names its check (a rework, for hive/rework.rs);
//! `own_session`: it ignores the runner's session id and names none at its end (omp and opencode keep
//! their own); `crash`: once it holds a claim it exits 1 printing nothing (a harness that died after
//! spending, without its cost). A fix
//! task (`from`, hive/integrate.rs) is resolved by script: the conflict marker lines are dropped from its
//! scoped files and from every other file holding them, both sides kept. A check slower than the stop
//! hook's time (a real suite) is run as a real drone is told to, with `borg hive done`; a verifier runs
//! the checks without a hook limit, each within its own timeout.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Here, Role, board, goal, oracle, verdict};
use crate::harness::{Harness, sim};
use crate::{hook, learn, runner};

pub const SCRIPT_ENV: &str = "BORG_SIM_SCRIPT";
const CALL: Duration = Duration::from_secs(120);
/// The cap on `borg hive done`, which runs the checks within their own timeouts.
const DONE_CALL: Duration = Duration::from_secs(3600);

#[derive(Debug, Default, Deserialize)]
struct Script {
    #[serde(default = "default_usd")]
    usd_per_iter: f64,
    #[serde(default)]
    drones: BTreeMap<String, Plan>,
    /// The goal's first verdict, whichever drone gives it, is a reject (`REJECT_NOTE`): a rework.
    #[serde(default)]
    reject_first: bool,
    /// A label the drones sign their work with (`done t1 by d1@<node>`): two stores' drones of one name
    /// (borg-eval's arm E) then write different files, so their snapshots are different commits.
    #[serde(default)]
    node: String,
}

/// The note of the scripted first reject.
const REJECT_NOTE: &str = "the file does not say which check passed it";

fn default_usd() -> f64 {
    0.10
}

fn default_poll() -> u64 {
    2
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
    #[serde(default = "default_poll")]
    poll_s: u64,
    #[serde(default)]
    one_task: bool,
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
        self.hive_within(args, CALL)
    }

    /// `hive`, capped at `cap`.
    fn hive_within(&self, args: &[&str], cap: Duration) -> Option<i32> {
        let mut cmd = Command::new(std::env::current_exe().ok()?);
        cmd.arg("hive").args(args).args(["--session", self.session, "--dir", self.cwd]).current_dir(self.cwd);
        runner::run_capped_status(cmd, None, cap).and_then(|(s, _)| s.code())
    }

    /// Works `plan.work_s` on a claim, firing the post_tool hook every `plan.poll_s` (a tool call):
    /// false as soon as a hook says the claim on `task` was lost.
    fn work(&self, plan: &Plan, task: &str) -> bool {
        let end = std::time::Instant::now() + Duration::from_secs(plan.work_s);
        let every = Duration::from_secs(if plan.poll_s == 0 { plan.work_s } else { plan.poll_s });
        loop {
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(every));
            if plan.poll_s == 0 {
                continue;
            }
            let said = self.hook("post_tool", json!({"tool_name": "Bash", "tool_input": {"command": "true"}})).map(|v| v.to_string()).unwrap_or_default();
            if said.contains(&format!("Your claim on {task} did not take effect")) {
                return false;
            }
        }
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

/// The verify task `task`'s verdict: on a checkout of its attempt's `head` apart from `cwd`'s tree,
/// as a drone is told (mod.rs `review_of`: the author's worktree may be another node's); on
/// `author_dir` only when the attempt has no snapshots (no git). A checkout that fails is an error,
/// never the author's worktree: a sim run must not pass on it once verifying from the commits broke.
fn verdict_of(here: &Here, g: &goal::Goal, task: &super::Task, head: Option<&str>, author_dir: impl FnOnce() -> PathBuf) -> Result<oracle::Verdict, String> {
    match head {
        Some(h) => {
            let key = format!("{}-{}", task.id, std::process::id());
            super::hidden::with_checkout(&std::env::temp_dir().join("borg-sim-verify"), Path::new(&here.cwd), h, &key, |dir| {
                oracle::run(&here.home, g, task, dir, None)
            })
        }
        None => Ok(oracle::run(&here.home, g, task, &author_dir(), None)),
    }
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
    let by = if script.node.is_empty() { name.clone() } else { format!("{name}@{}", script.node) };
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
            let events = board::read(&super::tasks_path(&here.home, &g.id));
            let head = verdict::attempt(&events, &board::fold(&events), &task.id).map(|a| a.head).filter(|h| !h.is_empty());
            let author = task.of.as_ref().and_then(|w| b.tasks.get(w)).and_then(|w| w.author.clone());
            let verdict = verdict_of(here, &g, task, head.as_deref(), || worktree_of(&here.home, author.as_deref(), &here.cwd));
            let rejected_once = hook::state_file(&format!("sim-{}", g.id)).with_extension("rejected");
            match verdict {
                Err(why) => d.hive(&["verify", "--reject", &format!("its commits could not be checked out: {why}")]),
                Ok(oracle::Verdict::Pass) if script.reject_first && !rejected_once.exists() => {
                    let _ = std::fs::write(&rejected_once, "");
                    d.hive(&["verify", "--reject", REJECT_NOTE])
                }
                Ok(oracle::Verdict::Pass) => d.hive(&["verify", "--pass"]),
                _ => d.hive(&["verify", "--reject", "its oracle fails on its commits"]),
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
        if !d.work(&plan, &task.id) {
            continue;
        }
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
        // A rework says which check passed it: what the scripted reject asked for.
        let checked = if held.rework > 0 { "checked by: ok\n" } else { "" };
        write(if fail { format!("wip {}\n", task.id) } else { format!("done {} by {by}\n{checked}", task.id) });
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
        if reason.contains(super::HOOK_TIMED_OUT) && !fail {
            // A suite slower than the hook's time: `borg hive done`, as the hook tells a drone.
            if d.hive_within(&["done"], DONE_CALL) == Some(0) {
                continue;
            }
            break;
        }
        if reason.contains("is not done") {
            if fail {
                // The drone's turn ends here, its oracle failing and its claim held.
                let _ = std::fs::write(&failed_once, "");
                break;
            }
            write(format!("done {} by {by}\n{checked}", task.id));
            reason = d.stop(true);
        }
        // A finished task hands over the next one in the same turn; anything else ends it.
        if plan.one_task || !reason.contains("done, verify task") {
            break;
        }
    }
    answer(&text, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn a_sim_verifier_judges_the_commits_and_never_falls_back_once_they_fail_to_check_out() {
        let root = std::env::temp_dir().join(format!("borg-sim-verify-of-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, author) = (root.join("app"), root.join("author"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&author).unwrap();
        std::fs::write(author.join("parser.rs"), "").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("parser.rs"), "fn parse() {}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "work"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["reset", "-q", "--hard", &base]);
        let home = root.join("home");
        let g = goal::Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: vec![goal::Check::parse("has=test -f parser.rs", 60).unwrap()],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
            held_out: false,
        };
        goal::create(&home, &g).unwrap();
        let task = super::super::new_task("t1.v1", Role::Verify, "verify t1", "", vec![], vec![], vec![]).unwrap();
        let here = Here { home: home.clone(), cwd: repo.display().to_string(), now: 0 };
        let mine = || repo.clone();
        assert_eq!(verdict_of(&here, &g, &task, Some(&head), mine).unwrap(), oracle::Verdict::Pass, "on the head, not this tree");
        assert!(!repo.join(".git/worktrees").read_dir().is_ok_and(|mut d| d.next().is_some()), "the checkout is removed");
        let gone = "0123456789012345678901234567890123456789";
        assert!(verdict_of(&here, &g, &task, Some(gone), || author.clone()).is_err(), "no fallback to the author's worktree");
        assert_eq!(verdict_of(&here, &g, &task, None, || author.clone()).unwrap(), oracle::Verdict::Pass, "no snapshots: its worktree");
        let _ = std::fs::remove_dir_all(root);
    }
}
