//! Decision → fix links end to end, on a scripted repo: `borg notes attach` (the end-of-session
//! job) links a later fix from another session to the decision whose lines it changed, in both notes
//! and, for a team collective, in reasoning.jsonl; a reflow, a move, the same session, a young
//! origin, an origin without reasoning and an off repo give none; `borg regress` is a dry run.
//! With `BORG_REGRESS=learn` the session's end turns a strong link into a regret episode and `borg
//! learn` (chained after the notes job) into a lesson asked at the decision point, against a fake
//! model; `borg why` shows both directions; the hook records test runs for the tests signal.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

const T0: i64 = 1_700_000_000;
const DAY: i64 = 86_400;

struct Fx {
    root: PathBuf,
    repo: PathBuf,
    store: PathBuf,
    state: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git_at(dir: &Path, secs: i64, args: &[&str]) -> String {
    let date = format!("@{secs} +0000");
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Ann", "-c", "user.email=ann@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_AUTHOR_DATE", &date)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_at(dir, T0, args)
}

/// `<root>/<under>/app` with `src/lib.rs` (lines 1-3) committed at T0 as the decision `D`; the
/// collectives: `work` (team), `private` (local), `secret` (off).
fn fixture(tag: &str, under: &str) -> (Fx, String) {
    let root = std::env::temp_dir().join(format!("borg-regress-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let repo = root.join(under).join("app");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "off": [root.join("secret")], "collectives": [
            {"name": "work", "paths": [root.join("work")], "share": "team"},
            {"name": "private", "paths": [root.join("private")], "share": "local"}]})
        .to_string(),
    )
    .unwrap();
    let name = if under == "private" { "private" } else { "work" };
    let store = borg::collective::store(&root.join("store"), name);
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    fake_model(&root);
    git(&repo, &["init", "-q"]);
    // borg's own `git notes add` needs an identity too (CI runners have none).
    git(&repo, &["config", "user.name", "Ann"]);
    git(&repo, &["config", "user.email", "ann@example.com"]);
    std::fs::write(repo.join("src/lib.rs"), "fn a() {}\nfn b() { read_the_flag(); }\nfn c() {}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "add a b c"]);
    let d = git(&repo, &["rev-parse", "HEAD"]);
    (Fx { root, repo, store, state }, d)
}

/// The question the fake model words a regret as (and a correction as "Q …"): which prompt it was
/// sent shows in the lesson.
const BEFORE: &str = "Before reading a shared flag without a lock, has a concurrent writer been considered?";

/// A fake `claude` on `<root>/bin`: answers a regret prompt with `BEFORE`, any other with "Q other",
/// and keeps each prompt in `<root>/prompts.txt`.
fn fake_model(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let answer = |q: &str| {
        json!({"structured_output": {"is_lesson": true, "question": q, "situations": ["reading a shared flag across threads"],
            "consequence": "a race", "same_as": "", "area": "concurrency", "supersedes": [], "reach": "team"}, "total_cost_usd": 0})
        .to_string()
    };
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n---\\n' \"$2\" >> '{log}'\ncase \"$2\" in\n  'A coding agent made a decision'*) echo '{regret}' ;;\n  *) echo '{other}' ;;\nesac\n",
        log = root.join("prompts.txt").display(),
        regret = answer(BEFORE),
        other = answer("Q other"),
    );
    let fake = root.join("bin/claude");
    std::fs::write(&fake, script).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fx {
    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
        c.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", format!("{}:{}", self.root.join("bin").display(), std::env::var("PATH").unwrap_or_default()))
            // The fake `claude` alone answers: a failed answer would otherwise fall through to the next model
            // CLI on PATH (runner.rs `pick`), a real call.
            .env("BORG_PREDICT_MODEL", "claude:")
            .env("HOME", self.root.join("home"))
            .env("BORG_HOME", self.root.join("store"))
            .env("BORG_STATE", &self.state)
            .env("BORG_CONFIG", self.root.join("collectives.json"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("BORG_LEARN_MODEL", "claude:fake")
            .env("BORG_QUEEN", "off")
            .env("BORG_VECTORS", "off")
            .env("BORG_MODE", "hook")
            .envs(env.iter().copied());
        c
    }

    fn borg(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.command(args, env).output().unwrap()
    }

    /// One hook event as Claude Code sends it.
    fn hook(&self, event: &str, input: &Value, env: &[(&str, &str)]) {
        let mut child = self.command(&["hook", "claude", event], env).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        child.wait().unwrap();
    }

    fn episodes(&self) -> Vec<Value> {
        std::fs::read_to_string(self.store.join("episodes.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// The lessons once the background `borg learn` wrote one (at most 30 s).
    fn lessons(&self) -> Vec<Value> {
        for _ in 0..300 {
            let text = std::fs::read_to_string(self.store.join("lessons.jsonl")).unwrap_or_default();
            if !text.is_empty() {
                return text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Vec::new()
    }

    fn why(&self, target: &str, json: bool) -> String {
        let args = ["why", target, "--json"];
        let out = self.borg(&args[..if json { 3 } else { 2 }], &[]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The borg note of `commit` from `session`.
    fn note(&self, commit: &str, session: &str, collective: &str) {
        let note = format!("borg reasoning (session {session}, collective {collective})\n\nTask:\nread the flag in b\n\nLast proposal:\nNo lock needed.\n");
        git(&self.repo, &["notes", "--ref=refs/notes/borg", "add", "-f", "-m", &note, commit]);
    }

    fn show(&self, commit: &str) -> String {
        Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["notes", "--ref=refs/notes/borg", "show", commit])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap()
    }

    /// Session `s-7` starts at `at - 100` s (HEAD then) and commits `file`'s new text at `at` as `msg`.
    fn fix(&self, at: i64, file: &str, text: &str, msg: &str) -> String {
        let start = json!({"repo": self.repo, "head": git(&self.repo, &["rev-parse", "HEAD"]), "ms": (at - 100) * 1000});
        std::fs::write(self.state.join("s-7.head"), start.to_string()).unwrap();
        std::fs::write(self.state.join("s-7.active"), format!("prompt {}\n", (at - 100) * 1000)).unwrap();
        std::fs::write(self.state.join("s-7.task"), "make the flag safe").unwrap();
        if let Some(dir) = self.repo.join(file).parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(self.repo.join(file), text).unwrap();
        git_at(&self.repo, at, &["add", "-A"]);
        git_at(&self.repo, at, &["commit", "-q", "-m", msg]);
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn attach(&self, env: &[(&str, &str)]) -> Output {
        self.borg(&["notes", "attach", "--session", "s-7"], env)
    }

    /// An `asked` outcome of `lesson` in `session` at `secs`.
    fn asked(&self, session: &str, lesson: &str, secs: i64) {
        let o = json!({"kind": "asked", "session": session, "lessons": [lesson], "ms": secs * 1000, "repo": "app"});
        std::fs::create_dir_all(&self.store).unwrap();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.store.join("outcomes.jsonl")).unwrap();
        writeln!(f, "{o}").unwrap();
    }

    /// The `regretted` outcomes, once at least `want` are written (at most 30 s).
    fn regretted(&self, want: usize) -> Vec<Value> {
        for i in 0..300 {
            let text = std::fs::read_to_string(self.store.join("outcomes.jsonl")).unwrap_or_default();
            let found: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["kind"] == "regretted").collect();
            if found.len() >= want || i == 299 {
                return found;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Vec::new()
    }

    fn records(&self) -> Vec<Value> {
        std::fs::read_to_string(self.store.join("reasoning.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["kind"] == "regress")
            .collect()
    }

    fn explain(&self, range: &str, extra: &[&str]) -> String {
        let mut args = vec!["regress", "--replay", range, "--explain"];
        args.extend(extra);
        let out = self.borg(&args, &[]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

const FIXED: &str = "fn a() {}\nfn b() { lock(); read_the_flag(); }\nfn c() {}\n";

#[test]
fn a_later_fix_links_to_the_decision_in_both_notes_and_the_store() {
    let (fx, d) = fixture("strong", "work");
    fx.note(&d, "s-1", "work");
    std::fs::write(fx.state.join("s-7.tests"), format!("fail {}\npass {}\n", (T0 + 3 * DAY - 50) * 1000, (T0 + 3 * DAY - 20) * 1000)).unwrap();
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock before the flag");
    let out = fx.attach(&[]);
    let msg = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(msg.contains("1 link(s) to earlier decisions (1 fix)"), "{msg}");

    let decision = fx.show(&d);
    assert!(decision.contains("borg fixed in:\n- ") && decision.contains(&f) && decision.contains("fix session s-7: make the flag safe"), "{decision}");
    assert!(decision.contains("No lock needed."), "the decision's reasoning is kept");
    let fixed = fx.show(&f);
    assert!(fixed.contains("borg fixes:\n- ") && fixed.contains(&d) && fixed.contains("session s-1: read the flag in b"), "{fixed}");

    let recs = fx.records();
    assert_eq!(recs.len(), 1, "{recs:?}");
    let r = &recs[0];
    assert_eq!((r["commit"].as_str(), r["fix"].as_str(), r["strength"].as_str()), (Some(d.as_str()), Some(f.as_str()), Some("fix")));
    let signals: Vec<&str> = r["signals"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(signals.contains(&"subject") && signals.contains(&"tests"), "{signals:?}");
    assert_eq!(r["files"], json!([{"path": "src/lib.rs", "lines": [[2, 2]]}]));
    assert_eq!((r["age_days"].as_u64(), r["collective"].as_str(), r["decision"]["session"].as_str()), (Some(3), Some("work"), Some("s-1")));

    // Another `end`: nothing twice.
    assert!(fx.attach(&[]).status.success());
    assert_eq!(fx.records().len(), 1);
    assert_eq!(fx.show(&d).matches(&f).count(), 1);

    // The dry run lists what is already linked as a duplicate and writes nothing.
    let before = (git(&fx.repo, &["rev-parse", "refs/notes/borg"]), std::fs::read_to_string(fx.store.join("reasoning.jsonl")).unwrap());
    let text = fx.explain(&format!("{d}..{f}"), &[]);
    assert!(text.contains("duplicate"), "{text}");
    let after = (git(&fx.repo, &["rev-parse", "refs/notes/borg"]), std::fs::read_to_string(fx.store.join("reasoning.jsonl")).unwrap());
    assert_eq!(before, after, "replay writes nothing");
}

#[test]
fn a_change_for_no_stated_reason_is_a_weak_link_and_a_dry_run_shows_it_first() {
    let (fx, d) = fixture("weak", "work");
    fx.note(&d, "s-1", "work");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "rename flag");
    let dry = fx.borg(&["regress", "--json"], &[]);
    let v: Value = serde_json::from_slice(&dry.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&dry.stderr)));
    assert_eq!((v["links"][0]["commit"].as_str(), v["links"][0]["strength"].as_str(), v["scanned"].as_u64()), (Some(d.as_str()), Some("revisited"), Some(1)));
    assert!(fx.records().is_empty() && !fx.show(&d).contains("borg fixed in"), "the dry run wrote nothing");
    assert!(fx.attach(&[]).status.success());
    assert_eq!(fx.records()[0]["strength"], "revisited");
    assert!(fx.show(&d).contains(&format!("- {f} ")) && fx.show(&d).contains(" revisited session s-7"), "{}", fx.show(&d));
}

#[test]
fn a_reflow_or_a_move_blames_nobody() {
    let (fx, d) = fixture("reflow", "work");
    fx.note(&d, "s-1", "work");
    let reflowed = fx.fix(T0 + 3 * DAY, "src/lib.rs", "fn a() {}\nfn b() {\n    read_the_flag();\n}\nfn c() {}\n", "fix: format");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.records().is_empty() && !fx.show(&d).contains("borg fixed in"), "{}", fx.show(&d));
    let text = fx.explain(&format!("{d}..{reflowed}"), &[]);
    assert!(text.contains(": format") && text.contains("0 link(s)"), "{text}");

    // b moves to another file unchanged: removed here, added verbatim there.
    let base = git(&fx.repo, &["rev-parse", "HEAD"]);
    std::fs::write(fx.repo.join("src/lib.rs"), "fn a() {}\nfn c() {}\n").unwrap();
    let moved = fx.fix(T0 + 4 * DAY, "src/b.rs", "fn b() {\n    read_the_flag();\n}\n", "fix: move b");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.records().is_empty(), "{:?}", fx.records());
    let text = fx.explain(&format!("{base}..{moved}"), &[]);
    assert!(text.contains(": move"), "{text}");
}

#[test]
fn the_same_session_a_young_origin_or_one_without_reasoning_gives_none() {
    let (fx, d) = fixture("filters", "work");
    fx.note(&d, "s-7", "work");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.records().is_empty(), "the session fixing its own decision is no regret");
    assert!(fx.explain(&format!("{d}..{f}"), &[]).contains("same-session"));

    // Another session's decision a day old: too young, unless the minimum age says otherwise.
    fx.note(&d, "s-1", "work");
    git(&fx.repo, &["reset", "-q", "--hard", &d]);
    let f = fx.fix(T0 + DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.records().is_empty());
    assert!(fx.explain(&format!("{d}..{f}"), &[]).contains("too-young (1d < 2d)"));
    assert!(fx.attach(&[("BORG_REGRESS_MIN_AGE", "12h")]).status.success());
    assert_eq!(fx.records().len(), 1);

    // No reasoning behind the origin: nothing, but --any-origin measures the detector anyway.
    let (fx, d) = fixture("noreason", "work");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.records().is_empty() && !fx.show(&d).contains("borg fixed in"));
    assert!(fx.explain(&format!("{d}..{f}"), &[]).contains("no-reasoning"));
    let any = fx.explain(&format!("{d}..{f}"), &["--any-origin"]);
    assert!(any.contains(&format!("fixes {}", &d[..7])) && any.contains("1 link(s), 1 fix"), "{any}");
}

#[test]
fn off_repos_get_nothing_local_ones_keep_it_in_their_notes() {
    let (fx, d) = fixture("off", "secret");
    fx.note(&d, "s-1", "work");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    let out = fx.attach(&[]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("off"));
    assert!(!fx.show(&d).contains("borg fixed in") && fx.show(&f).is_empty());
    let dry = fx.borg(&["regress", "--replay", &format!("{d}..{f}"), "--explain"], &[]);
    assert!(dry.status.success() && dry.stdout.is_empty(), "an off repo prints nothing");

    let (fx, d) = fixture("local", "private");
    fx.note(&d, "s-1", "private");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.show(&d).contains(&format!("- {f} ")), "{}", fx.show(&d));
    assert!(!fx.store.join("reasoning.jsonl").exists(), "nothing queued for any peer");

    // A note of another collective is invisible here: no link to it.
    let (fx, d) = fixture("foreign", "work");
    fx.note(&d, "s-1", "private");
    fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.records().is_empty() && !fx.show(&d).contains("borg fixed in"));
}

#[test]
fn off_switch_and_timeouts_fail_open() {
    let (fx, d) = fixture("gate", "work");
    fx.note(&d, "s-1", "work");
    fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.attach(&[("BORG_REGRESS", "off")]).status.success());
    assert!(fx.records().is_empty());
    let out = fx.attach(&[("BORG_REGRESS_TIMEOUT_S", "0")]);
    assert!(out.status.success(), "a timed-out git call is no error");
    assert!(fx.records().is_empty() && !fx.show(&d).contains("borg fixed in"));
    assert!(fx.attach(&[]).status.success());
    assert_eq!(fx.records().len(), 1, "the next run makes the link");
}

#[test]
fn a_fix_touching_too_many_files_is_skipped_whole() {
    let (fx, d) = fixture("large", "work");
    fx.note(&d, "s-1", "work");
    for i in 0..41 {
        std::fs::write(fx.repo.join(format!("src/f{i}.rs")), "fn x() {}\n").unwrap();
    }
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: everything");
    assert!(fx.attach(&[]).status.success());
    assert!(fx.records().is_empty());
    assert!(fx.explain(&format!("{d}..{f}"), &[]).contains("too-large (42 files"));
}

#[test]
fn a_shallow_clone_gives_no_link_and_no_error() {
    let (fx, d) = fixture("shallow", "work");
    fx.note(&d, "s-1", "work");
    fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    let clone = fx.root.join("work/clone");
    git(&fx.root, &["clone", "-q", "--depth", "1", &format!("file://{}", fx.repo.display()), &clone.to_string_lossy()]);
    let out = Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(["regress", "--replay", "HEAD", "--explain", "--any-origin", "--dir", &clone.to_string_lossy()])
        .env("BORG_HOME", fx.root.join("store"))
        .env("BORG_CONFIG", fx.root.join("collectives.json"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("no-parent") && text.contains("0 link(s)") && text.contains("shallow"), "{text}");
}

#[test]
fn with_learning_on_the_sessions_end_turns_a_strong_link_into_a_lesson_asked_before_the_decision() {
    let (fx, d) = fixture("learn", "work");
    fx.note(&d, "s-1", "work");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock before the flag");
    // The deciding session asked La before the decision commit (at T0) and Lb after it.
    fx.asked("s-1", "La", T0 - 60);
    fx.asked("s-1", "Lb", T0 + 60);
    // The drone's session ends: the hook starts the notes job, which links, writes the regret
    // episode and then starts `borg learn`, all in the background.
    let end = json!({"session_id": "s-7", "cwd": fx.repo, "last_assistant_message": "Took the lock."});
    fx.hook("end", &end, &[("BORG_REGRESS", "learn")]);
    let lessons = fx.lessons();
    let id = format!("R{}-{}", &d[..10], &f[..10]);
    // The regret reaches back: La, asked before the decision, shares its blame alone (one ground, the
    // fix's subject: 0.5); Lb, asked after it, is not blamed.
    let blamed = fx.regretted(1);
    assert_eq!(blamed.len(), 1, "{blamed:?}");
    let b = &blamed[0];
    assert_eq!(
        (b["lessons"][0].as_str(), b["session"].as_str(), b["cause"].as_str(), b["credit"].as_f64()),
        (Some("La"), Some("s-1"), Some(id.as_str()), Some(0.5))
    );
    assert_eq!(fx.records()[0]["decision"]["secs"], T0, "the decision's committer time is kept");
    assert_eq!(lessons.len(), 1, "{lessons:?}");
    assert_eq!((lessons[0]["id"].as_str(), lessons[0]["question"].as_str()), (Some(format!("L{id}").as_str()), Some(BEFORE)));
    let prompts = std::fs::read_to_string(fx.root.join("prompts.txt")).unwrap();
    assert!(prompts.starts_with("A coding agent made a decision") && prompts.contains("## The decision (task)\nread the flag in b"), "{prompts}");
    assert!(
        prompts.contains(
            "## What the fix changed in the decision's files (excerpt)\nsrc/lib.rs\n- fn b() { read_the_flag(); }\n+ fn b() { lock(); read_the_flag(); }"
        ),
        "{prompts}"
    );

    let eps = fx.episodes();
    assert_eq!(eps.len(), 1, "{eps:?}");
    let e = &eps[0];
    assert_eq!(
        (e["id"].as_str(), e["kind"].as_str(), e["session"].as_str(), e["engineer"].as_str()),
        (Some(id.as_str()), Some("regret"), Some("s-7"), Some("ann@example.com"))
    );
    assert_eq!((e["regret"]["decision"].as_str(), e["regret"]["decision_proposal"].as_str()), (Some(d.as_str()), Some("No lock needed.")));
    assert!(fx.show(&f).contains(&format!("Corrections (episodes): {id}")), "the fix's note lists it: {}", fx.show(&f));
    // `borg why` on the fixed line: the decision behind the fix and the lesson learned from it.
    let why = fx.why("src/lib.rs:2", false);
    assert!(why.contains(&format!("← decided in {} ", &d[..7])) && why.contains("(task: read the flag in b) → fixed here"), "{why}");
    assert!(why.contains(&format!("lesson L{id}: {BEFORE}")), "{why}");

    // The next end of this session writes the episode only once, and the regret blames La once.
    assert!(fx.borg(&["notes", "attach", "--session", "s-7"], &[("BORG_REGRESS", "learn")]).status.success());
    assert_eq!(fx.episodes().len(), 1);
    assert!(fx.borg(&["learn"], &[("BORG_HOME", &fx.store.to_string_lossy())]).status.success());
    assert_eq!(fx.regretted(0).len(), 1);
}

#[test]
fn a_regret_learned_with_credit_off_blames_nothing_until_it_is_back_on() {
    let (fx, d) = fixture("creditoff", "work");
    fx.note(&d, "s-1", "work");
    fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock before the flag");
    fx.asked("s-1", "La", T0 - 60);
    assert!(fx.borg(&["notes", "attach", "--session", "s-7"], &[("BORG_REGRESS", "learn")]).status.success());
    assert_eq!(fx.episodes().len(), 1);
    // `borg learn` on the collective's store, as the session's end runs it.
    let store = fx.store.to_string_lossy().into_owned();
    assert!(fx.borg(&["learn"], &[("BORG_HOME", &store), ("BORG_REGRET_CREDIT", "off")]).status.success());
    assert_eq!(fx.lessons().len(), 1, "learned as a lesson");
    assert!(fx.regretted(0).is_empty(), "BORG_REGRET_CREDIT=off");
    // Back on, the next run credits the regret learned before: once.
    assert!(fx.borg(&["learn"], &[("BORG_HOME", &store)]).status.success());
    assert!(fx.borg(&["learn"], &[("BORG_HOME", &store)]).status.success());
    assert_eq!(fx.regretted(0).len(), 1);
}

#[test]
fn a_weak_link_gives_no_episode_links_alone_learn_nothing_and_a_local_collective_learns_too() {
    let (fx, d) = fixture("noepisode", "work");
    fx.note(&d, "s-1", "work");
    fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "rename flag");
    assert!(fx.borg(&["notes", "attach", "--session", "s-7"], &[("BORG_REGRESS", "learn")]).status.success());
    assert_eq!(fx.records().len(), 1);
    assert!(fx.episodes().is_empty(), "a revisit is no regret");

    let (fx, d) = fixture("default", "work");
    fx.note(&d, "s-1", "work");
    fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.attach(&[]).status.success());
    assert_eq!(fx.records().len(), 1);
    assert!(fx.episodes().is_empty(), "BORG_REGRESS defaults to links: nothing is learned yet");

    let (fx, d) = fixture("localepisode", "private");
    fx.note(&d, "s-1", "private");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock");
    assert!(fx.borg(&["notes", "attach", "--session", "s-7"], &[("BORG_REGRESS", "learn")]).status.success());
    assert!(fx.show(&d).contains(&format!("- {f} ")), "the link is in the local notes");
    assert_eq!(fx.episodes().len(), 1, "a local collective learns from its own links too");
}

#[test]
fn why_shows_a_link_from_both_sides_from_notes_or_the_store() {
    let (fx, d) = fixture("why", "work");
    fx.note(&d, "s-1", "work");
    let f = fx.fix(T0 + 3 * DAY, "src/lib.rs", FIXED, "fix: take the lock before the flag");
    assert!(fx.attach(&[]).status.success());
    let decision = fx.why(&d, false);
    assert!(decision.contains(&format!("→ fixed in {} 2023-11-17 (task: make the flag safe)", &f[..7])), "{decision}");
    let fixed = fx.why("src/lib.rs:2", false);
    assert!(fixed.contains(&format!("← decided in {} 2023-11-14 (task: read the flag in b) → fixed here", &d[..7])), "{fixed}");
    let v: Value = serde_json::from_str(&fx.why(&f, true)).unwrap();
    assert_eq!((v["commits"][0]["fixes"][0]["sha"].as_str(), v["commits"][0]["fixes"][0]["strength"].as_str()), (Some(d.as_str()), Some("fix")));
    assert!(v["commits"][0].get("fixed_in").is_none(), "an empty direction stays off the line");

    // A link only the store holds (fetched from the mesh, the notes lack it) shows too; one of
    // another collective does not.
    let (fx, d) = fixture("whystore", "work");
    fx.note(&d, "s-1", "work");
    let side = |task: &str| json!({"session": "s-9", "task": task, "date": "2026-10-12"});
    for (fix, collective) in [("a".repeat(40), "work"), ("b".repeat(40), "private")] {
        let rec = json!({"kind": "regress", "commit": d, "fix": fix, "repo": "app", "collective": collective, "strength": "revisited",
            "signals": [], "decision": side("read the flag in b"), "fixed": side(&format!("rename in {collective}")), "files": [], "lines": 1, "age_days": 3, "ms": 1});
        std::fs::create_dir_all(&fx.store).unwrap();
        std::fs::OpenOptions::new().create(true).append(true).open(fx.store.join("reasoning.jsonl")).unwrap().write_all(format!("{rec}\n").as_bytes()).unwrap();
    }
    let text = fx.why(&d, false);
    assert!(text.contains("→ revisited in aaaaaaa 2026-10-12 (task: rename in work)") && !text.contains("bbbbbbb"), "{text}");
}

#[test]
fn the_hook_records_test_runs_for_the_tests_signal() {
    let (fx, _) = fixture("capture", "work");
    let bash = |command: &str, response: Value| json!({"session_id": "s-9", "cwd": fx.repo, "tool_name": "Bash", "tool_input": {"command": command}, "tool_response": response});
    fx.hook("post_tool", &bash("cargo test -p app", json!({"stdout": "test result: FAILED. 0 passed; 1 failed", "stderr": ""})), &[]);
    let mut failure = bash("pytest -q", Value::Null);
    failure["error"] = json!("Exit code 1");
    fx.hook("post_tool_failure", &failure, &[]);
    fx.hook("post_tool", &bash("cargo test -p app", json!({"stdout": "test result: ok. 1 passed; 0 failed", "stderr": ""})), &[]);
    fx.hook("post_tool", &bash("ls -la", json!("FAILED")), &[]);
    fx.hook("post_tool", &bash("cargo test", json!("test result: FAILED")), &[("BORG_LEARN", "off")]);
    let tests = std::fs::read_to_string(fx.state.join("s-9.tests")).unwrap_or_default();
    let kinds: Vec<&str> = tests.lines().filter_map(|l| l.split(' ').next()).collect();
    assert_eq!(kinds, ["fail", "fail", "pass"], "{tests}");
    assert!(tests.lines().all(|l| l.split(' ').nth(1).is_some_and(|ms| ms.parse::<i64>().is_ok())), "{tests}");
}
