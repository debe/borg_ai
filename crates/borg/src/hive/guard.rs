//! The guard: what a hive drone does to tests, CI and the oracle's files, flagged as it happens
//! (docs/design/hive-learning.md "Guardrails against reward hacking"). Flag only: measure, don't block.
//!
//! At every tool call of a session in a hive goal ([`pre_tool`]), the paths the call writes (an edit
//! tool's files; the targets of a shell command's redirects, `sed -i`, `rm`, `mv`, `cp`, `tee`, ...)
//! are classed ([`class_of`]): `test`, `ci` or `oracle`; the paths it only reads count when they are
//! the oracle's (the hidden checks, oracle.jsonl, the scratch checkouts). Each flag is one line of
//! `<goal>/flags.jsonl`, once per (session, claim, class, path) (`.ctx` marker `#borg-hive-flag`: each
//! claim's verdict sees its own flags), and the
//! verify task its done opens lists them for the verifier. A session in no hive goal costs one failed
//! file open; nothing is read or created.
//!
//! Best-effort by design: `python -c` writes, generated paths and `cd`-relative paths escape it. The
//! verdict's diff of the claim's snapshots (verdict.rs) is the ground truth on every harness; a flag
//! is an early sign, and only an `oracle` flag decides a verdict.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Goal, board, goal};
use crate::{hook, prefilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Test,
    Ci,
    Oracle,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Test => "test",
            Class::Ci => "ci",
            Class::Oracle => "oracle",
        }
    }
}

/// One line of flags.jsonl. `scoped`: the path is in the task's own `files`, which the plan made
/// editable (a task to fix a test may edit it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flag {
    pub v: u8,
    pub ms: i64,
    pub session: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub drone: String,
    /// The claim held then; empty for none.
    #[serde(default)]
    pub task: String,
    pub class: Class,
    pub path: String,
    pub tool: String,
    #[serde(default)]
    pub scoped: bool,
}

const MARKER: &str = "#borg-hive-flag ";

pub fn flags_path(home: &Path, goal: &str) -> PathBuf {
    super::dir(home, goal).join("flags.jsonl")
}

/// The oracle's own files, which no drone has a reason to open: the hidden checks' directory, the
/// scores (oracle.jsonl) and the scratch checkouts they run in; with a hidden oracle, goal.json too,
/// which names its commands.
pub fn guarded(home: &Path, g: &Goal) -> Vec<PathBuf> {
    let dir = super::dir(home, &g.id);
    let mut out = vec![super::hidden::scores_path(home, &g.id), dir.join(super::hidden::SCRATCH)];
    if let Some(h) = g.hidden.as_ref() {
        out.extend([PathBuf::from(&h.dir), super::goal_path(home, &g.id)]);
    }
    out
}

/// A test runner's config, wherever it lies: it decides how the existing tests run (a `conftest.py`
/// hook that deselects them, `addopts = -k ...`), so adding one is changing the tests. `pyproject.toml`
/// and `package.json` are not counted: most of their changes are not about tests.
pub fn runner_config(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    ["conftest.py", "pytest.ini", "tox.ini", "noxfile.py", "karma.conf.js", "phpunit.xml", "phpunit.xml.dist", ".rspec", "spec_helper.rb", "rails_helper.rb"]
        .contains(&name)
        || ["jest.config.", "jest.setup.", "vitest.config.", "vitest.workspace.", "setupTests.", ".mocharc"].iter().any(|p| name.starts_with(p))
}

/// The repo files the goal's visible checks run (a `grade.sh`): a command word that is a file under
/// `root`, repo-relative. Editing one is editing CI.
pub fn oracle_files(g: &Goal, root: &Path) -> Vec<String> {
    let mut out: Vec<String> = g
        .oracle
        .iter()
        .flat_map(|c| c.cmd.split(|ch: char| ch.is_whitespace() || ";&|()<>'\"`".contains(ch)))
        .map(|w| w.trim_start_matches("./"))
        .filter(|w| !w.is_empty() && !w.starts_with('-') && !Path::new(w).is_absolute() && !w.contains(".."))
        .filter(|w| root.join(w).is_file())
        .map(String::from)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The lexical absolute form of `p` (no filesystem access).
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    out.extend(crate::collective::normal(p));
    out
}

/// The class of a path: `oracle` when `abs` lies under one of `guard`; `ci` for CI config (GitHub,
/// GitLab, CircleCI, Buildkite, Jenkins, Azure pipelines) and the visible checks' own files
/// (`oracle_files`); `test` for a file under a test|tests|spec|specs|__tests__ directory, named
/// test_*.*, *_test.*, *.test.*, *.spec.*, *_spec.rb, or a test runner's config (`runner_config`).
/// `rel` is the path as the repo names it.
pub fn class_of(rel: &str, abs: &Path, guard: &[PathBuf], oracle_files: &[String]) -> Option<Class> {
    let abs = lexical(abs);
    if guard.iter().any(|g| abs.starts_with(lexical(g))) {
        return Some(Class::Oracle);
    }
    let rel = rel.trim_start_matches("./");
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let (name, dirs) = parts.split_last()?;
    let ci_dir = dirs.windows(2).any(|w| w == [".github", "workflows"]) || dirs.iter().any(|d| *d == ".circleci" || *d == ".buildkite");
    if ci_dir || [".gitlab-ci.yml", "Jenkinsfile", "azure-pipelines.yml"].contains(name) || oracle_files.iter().any(|f| f == rel) {
        return Some(Class::Ci);
    }
    let test_dir = dirs.iter().any(|d| ["test", "tests", "spec", "specs", "__tests__"].contains(d));
    let stem_ext = name.rsplit_once('.');
    let test_name = match stem_ext {
        Some((stem, ext)) => {
            (stem.starts_with("test_") && stem.len() > 5)
                || stem.ends_with("_test")
                || stem.ends_with(".test")
                || stem.ends_with(".spec")
                || (ext == "rb" && stem.ends_with("_spec"))
        }
        None => false,
    };
    (test_dir || test_name || runner_config(rel)).then_some(Class::Test)
}

/// The words of a shell command that may be paths, in order: split at whitespace and shell
/// punctuation, an `--opt=value` read as its value; URLs left out.
fn words(cmd: &str) -> Vec<String> {
    cmd.split(|c: char| c.is_whitespace() || ";&|()<>'\"`".contains(c))
        .map(|w| w.rsplit_once('=').map_or(w, |(_, v)| v))
        .filter(|w| !w.is_empty() && !w.contains("://"))
        .map(String::from)
        .collect()
}

/// The paths a shell command writes, best-effort: redirect targets, and the operands of programs that
/// write their arguments (`rm`, `mv`, `cp`'s target, `tee`, `truncate`, `touch`, `ln`, `install`,
/// `chmod`, `patch`), of `sed -i` / `perl -i`, and of `git checkout|restore|rm|mv`.
pub fn bash_writes(cmd: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Redirects: the word after `>` or `>>` (not a `>&2` duplication).
    let mut rest = cmd;
    while let Some(i) = rest.find('>') {
        let after = rest[i + 1..].trim_start_matches('>');
        if !after.starts_with('&') {
            let target: String = after.trim_start().chars().take_while(|c| !c.is_whitespace() && !";&|()<>'\"`".contains(*c)).collect();
            if !target.is_empty() && target != "/dev/null" {
                out.push(target);
            }
        }
        rest = rest[i + 1..].trim_start_matches('>');
    }
    for part in cmd.split(['\n', ';', '|', '&', '(', ')', '`']) {
        let w: Vec<String> = words(part).into_iter().skip_while(|w| w.contains('=')).collect();
        let Some(prog) = w.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()) else { continue };
        let operands = || w[1..].iter().filter(|a| !a.starts_with('-')).cloned().collect::<Vec<_>>();
        match prog.as_str() {
            "rm" | "mv" | "tee" | "truncate" | "touch" | "ln" | "install" | "unlink" | "shred" | "patch" => out.extend(operands()),
            "chmod" | "chown" => out.extend(operands().into_iter().skip(1)),
            "cp" => out.extend(operands().pop()),
            "sed" | "perl" if w.iter().any(|a| a.starts_with("-i") || a == "--in-place" || a.starts_with("-pi")) => {
                // The script is the first operand, or the word after each `-e`; the rest are files.
                let mut files = Vec::new();
                let (mut scripted, mut after_e) = (false, false);
                for a in &w[1..] {
                    if after_e {
                        after_e = false;
                    } else if a == "-e" || a == "--expression" {
                        (scripted, after_e) = (true, true);
                    } else if !a.starts_with('-') {
                        files.push(a.clone());
                    }
                }
                out.extend(files.into_iter().skip(usize::from(!scripted)));
            }
            "git" => {
                let ops = operands();
                if ops.first().is_some_and(|s| ["checkout", "restore", "rm", "mv"].contains(&s.as_str())) {
                    out.extend(ops.into_iter().skip(1).filter(|a| a.contains('/') || a.contains('.')));
                }
            }
            _ => {}
        }
    }
    out
}

/// A tool call's paths: (written, read). An edit tool writes its files (`file_path`, `notebook_path`,
/// each `edits[].file_path`); a shell command writes `bash_writes` and reads every word that may be a
/// path; Read, Grep and Glob read their path or pattern.
pub fn paths(tool: &str, input: &Value) -> (Vec<String>, Vec<String>) {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let lower = tool.to_ascii_lowercase();
    if lower == "bash" || lower == "shell" {
        let cmd = s("command");
        let writes = if prefilter::changes("bash", input) { bash_writes(&cmd) } else { Vec::new() };
        let reads = words(&cmd).into_iter().filter(|w| w.contains('/')).collect();
        return (writes, reads);
    }
    if prefilter::changes(tool, input) {
        // An omp patch of several files names them all in one `file_path`, space-separated (its adapter).
        let split = |p: String| -> Vec<String> {
            match p.contains(' ') && !Path::new(&p).exists() {
                true => p.split_whitespace().map(String::from).collect(),
                false => vec![p],
            }
        };
        let mut w: Vec<String> = [s("file_path"), s("notebook_path")].into_iter().filter(|p| !p.is_empty()).flat_map(split).collect();
        w.extend(hook::further_files(input).map(String::from));
        return (w, Vec::new());
    }
    let reads = [s("file_path"), s("path"), s("pattern"), s("notebook_path")].into_iter().filter(|p| p.contains('/')).collect();
    (Vec::new(), reads)
}

/// The flags of a tool call by a drone in `cwd`: each written path of any class, each read path of
/// class `oracle`, once per (class, path), in order.
pub fn flags_for(tool: &str, input: &Value, cwd: &Path, guard: &[PathBuf], oracle_files: &[String]) -> Vec<(Class, String)> {
    let (writes, reads) = paths(tool, input);
    let mut out: Vec<(Class, String)> = Vec::new();
    let classed = |p: &str| {
        let abs = if Path::new(p).is_absolute() { lexical(Path::new(p)) } else { lexical(&cwd.join(p)) };
        let rel = abs.strip_prefix(lexical(cwd)).map_or_else(|_| abs.display().to_string(), |r| r.display().to_string());
        class_of(&rel, &abs, guard, oracle_files).map(|c| (c, rel))
    };
    for p in &writes {
        out.extend(classed(p));
    }
    for p in &reads {
        out.extend(classed(p).filter(|(c, _)| *c == Class::Oracle));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|f| seen.insert(f.clone()));
    out
}

/// The hook's pre_tool step: flags what this tool call does to tests, CI or the oracle's files. Never
/// a decision. A session in no hive goal returns after one failed file open.
pub fn pre_tool(home: &Path, input: &Value, now: i64) {
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("unknown");
    let Some(hs) = super::bound(session) else { return };
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or_default();
    let tool_input = input.get("tool_input").unwrap_or(&Value::Null);
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or_default();
    if cwd.is_empty() {
        return;
    }
    let Ok(g) = goal::load(home, &hs.goal) else { return };
    let root = Path::new(cwd);
    let hits = flags_for(tool, tool_input, root, &guarded(home, &g), &oracle_files(&g, root));
    if hits.is_empty() {
        return;
    }
    let b = board::fold(&board::read(&super::tasks_path(home, &g.id)));
    let held = board::holding(&b, session, now);
    let globs: Vec<regex::Regex> = held.map(|s| s.task.files.iter().map(|f| crate::assimilate_dir::glob(f)).collect()).unwrap_or_default();
    let state = std::fs::read_to_string(hook::state_file(session)).unwrap_or_default();
    // The claim the flags fall in (`task@since`, `-` for none): a re-claim or the next task flags again.
    let claim = held.map_or_else(|| "-".to_string(), |s| format!("{}@{}", s.task.id, s.live(now).map_or(0, |l| l.2)));
    let mut marks = Vec::new();
    for (class, path) in hits {
        let mark = format!("{MARKER}{claim} {} {path}", class.name());
        if state.lines().any(|l| l == mark) {
            continue;
        }
        let f = Flag {
            v: 1,
            ms: now,
            session: session.into(),
            drone: hs.drone.clone(),
            task: held.map(|s| s.task.id.clone()).unwrap_or_default(),
            class,
            path: path.clone(),
            tool: tool.into(),
            scoped: globs.iter().any(|g| g.is_match(&path)),
        };
        if super::append_line(&flags_path(home, &g.id), &f).is_ok() {
            marks.push(mark);
        }
    }
    if !marks.is_empty() {
        hook::append(session, &marks.join("\n"));
    }
}

/// The flags of `session` at `from_ms..=to_ms` in `goal` (one pass over flags.jsonl).
pub fn flags_of(home: &Path, goal: &str, session: &str, from_ms: i64, to_ms: i64) -> Vec<Flag> {
    super::read_lines::<Flag>(&flags_path(home, goal)).into_iter().filter(|f| f.session == session && (from_ms..=to_ms).contains(&f.ms)).collect()
}

/// A flag as the verifier reads it.
pub fn render(f: &Flag) -> String {
    format!("{} {} ({}{})", f.class.name(), f.path, f.tool, if f.scoped { ", in the task's scope" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn class_of_tells_tests_ci_and_the_oracle_apart() {
        let guard = [PathBuf::from("/held/out"), PathBuf::from("/s/hive/g/oracle.jsonl")];
        let of = |rel: &str| class_of(rel, &Path::new("/w/app").join(rel), &guard, &["grade.sh".to_string()]);
        for t in [
            "tests/a.py",
            "src/test/Main.java",
            "web/__tests__/x.js",
            "spec/models/user_spec.rb",
            "pkg/test_util.py",
            "x/foo_test.go",
            "a.test.ts",
            "b.spec.js",
            "./tests/a.py",
        ] {
            assert_eq!(of(t), Some(Class::Test), "{t}");
        }
        for c in
            [".github/workflows/ci.yml", ".gitlab-ci.yml", ".circleci/config.yml", ".buildkite/pipeline.yml", "Jenkinsfile", "azure-pipelines.yml", "grade.sh"]
        {
            assert_eq!(of(c), Some(Class::Ci), "{c}");
        }
        for n in ["src/main.rs", "testing/x.rs", "src/contest.rs", "test_", "README.md", "src/latest.rs", "lib/spec.rb", ".github/ISSUE.md", "scripts/grade.sh"]
        {
            assert_eq!(of(n), None, "{n}");
        }
        assert_eq!(class_of("../../held/out/t.py", Path::new("/held/out/t.py"), &guard, &[]), Some(Class::Oracle));
        assert_eq!(class_of("x", Path::new("/held/out/../out/x"), &guard, &[]), Some(Class::Oracle), "lexically normal");
        assert_eq!(class_of("x", Path::new("/held/outside/x"), &guard, &[]), None, "whole components only");
        assert_eq!(class_of("o", Path::new("/s/hive/g/oracle.jsonl"), &guard, &[]), Some(Class::Oracle));
    }

    #[test]
    fn a_test_runners_config_is_a_test_and_goal_json_is_the_oracles_with_a_hidden_one() {
        for t in ["tests/conftest.py", "conftest.py", "pytest.ini", "web/jest.config.ts", "vitest.config.mjs", ".mocharc.yml", "spec/spec_helper.rb"] {
            assert_eq!(class_of(t, &Path::new("/w/app").join(t), &[], &[]), Some(Class::Test), "{t}");
        }
        for n in ["pyproject.toml", "package.json", "src/jest.rs", "setup.cfg"] {
            assert_eq!(class_of(n, &Path::new("/w/app").join(n), &[], &[]), None, "{n}");
        }
        let home = Path::new("/s");
        let mut g = Goal {
            v: 1,
            id: "g".into(),
            spec: "s".into(),
            oracle: vec![],
            budget_usd: 1.0,
            repos: vec![],
            author: String::new(),
            created_ms: 0,
            sig: String::new(),
            hidden: None,
        };
        let goal_json = super::super::goal_path(home, "g");
        assert!(!guarded(home, &g).contains(&goal_json), "without a hidden oracle goal.json names nothing held out");
        g.hidden = Some(goal::Hidden { dir: "/held".into(), commands: vec!["true".into()], timeout_s: 60 });
        let guard = guarded(home, &g);
        let cat = json!({"command": format!("cat {}", goal_json.display())});
        assert_eq!(flags_for("Bash", &cat, Path::new("/w/app"), &guard, &[]), [(Class::Oracle, goal_json.display().to_string())]);
    }

    #[test]
    fn a_shell_command_writes_its_targets_and_reads_the_rest() {
        assert_eq!(bash_writes("sed -i 's/a/b/' tests/a.py"), ["tests/a.py"]);
        assert_eq!(bash_writes("sed -i -e 's/a/b/' tests/a.py tests/b.py"), ["tests/a.py", "tests/b.py"]);
        assert_eq!(bash_writes("echo x > tests/a.py; cat y >> out.txt 2>&1"), ["tests/a.py", "out.txt"]);
        assert_eq!(bash_writes("rm -f tests/a.py && cp src/x.py tests/b.py"), ["tests/a.py", "tests/b.py"]);
        assert_eq!(bash_writes("git checkout -- tests/a.py"), ["tests/a.py"]);
        assert!(bash_writes("cat tests/a.py; pytest tests/test_a.py; echo hi >&2; ls > /dev/null").is_empty());
        let (w, r) = paths("Bash", &json!({"command": "cat tests/a.py /held/out/t.py"}));
        assert!(w.is_empty() && r == ["tests/a.py", "/held/out/t.py"]);
        let (w, _) = paths("Edit", &json!({"file_path": "/w/app/tests/a.py", "edits": [{"file_path": "/w/app/ci.yml"}]}));
        assert_eq!(w, ["/w/app/tests/a.py", "/w/app/ci.yml"]);
        let (w, _) = paths("Edit", &json!({"file_path": "src/a.rs tests/b.py", "new_string": "*** Update File: src/a.rs"}));
        assert_eq!(w, ["src/a.rs", "tests/b.py"], "an omp patch of several files");
        let (w, r) = paths("Grep", &json!({"pattern": "fn", "path": "/held/out"}));
        assert!(w.is_empty() && r == ["/held/out"]);
    }

    #[test]
    fn flags_for_counts_reads_of_the_oracle_only() {
        let guard = [PathBuf::from("/held/out")];
        let cwd = Path::new("/w/app");
        let f = |tool: &str, input: Value| flags_for(tool, &input, cwd, &guard, &[]);
        assert_eq!(f("Bash", json!({"command": "sed -i s/a/b/ tests/a.py"})), [(Class::Test, "tests/a.py".to_string())]);
        assert!(f("Bash", json!({"command": "cat tests/a.py"})).is_empty(), "reading a test is not a flag");
        assert!(f("Read", json!({"file_path": "/w/app/tests/a.py"})).is_empty());
        assert_eq!(f("Bash", json!({"command": "cat /held/out/t.py"})), [(Class::Oracle, "/held/out/t.py".to_string())]);
        assert!(f("Grep", json!({"pattern": "x", "path": "/held"})).is_empty(), "a search rooted above the oracle is not a peek into it");
        assert_eq!(f("Write", json!({"file_path": "/w/app/tests/a.py"})), [(Class::Test, "tests/a.py".to_string())]);
    }
}
