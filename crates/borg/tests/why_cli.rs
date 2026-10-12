//! `borg why` end to end: an off repo (or a broken config) prints nothing and exits 0; a directory
//! without git is a clear error; a note shows with its linked lesson, as text and as JSON.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::json;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("borg-why-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // Resolved, as borg routes it (the temp dir may be a symlink).
    std::fs::canonicalize(&d).unwrap()
}

fn borg(args: &[&str], root: &Path) -> Output {
    borg_in(args, root, root)
}

fn borg_in(args: &[&str], root: &Path, cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("BORG_MATCH", "bm25") // word matching only: no model call from a test
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Ann", "-c", "user.email=ann@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn why_from_the_command_line() {
    let root = tmp("e2e");
    let (repo, off, plain) = (root.join("corp/app"), root.join("secret/app"), root.join("corp/plain"));
    for d in [&repo, &off, &plain] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("a.rs"), "fn a() {}\n").unwrap();
    }
    std::fs::write(
        root.join("collectives.json"),
        json!({"default": "off", "off": [root.join("secret")], "collectives": [{"name": "corp", "paths": [root.join("corp")], "share": "team"}]}).to_string(),
    )
    .unwrap();
    for d in [&repo, &off] {
        git(d, &["init", "-q"]);
        git(d, &["add", "."]);
        git(d, &["commit", "-q", "-m", "add a"]);
    }
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let note = "borg reasoning (session s-1, collective corp)\n\nTask:\nadd a\n\nLast proposal:\nA keeps the API small.\n\nCorrections (episodes): s-1-7\n";
    git(&repo, &["notes", "--ref=refs/notes/borg", "add", "-m", note, &head]);
    git(&off, &["notes", "--ref=refs/notes/borg", "add", "-m", note, "HEAD"]);
    let store = borg::collective::store(&root.join("store"), "corp");
    std::fs::create_dir_all(&store).unwrap();
    let lesson = json!({"id": "L7", "origin_ms": 1, "repo": "app", "question": "Is the API still small?", "situations": [],
        "consequence": "callers break", "support": 1, "episodes": ["s-1-7"], "history": [], "version": 1});
    std::fs::write(store.join("lessons.jsonl"), format!("{lesson}\n")).unwrap();

    let out = borg(&["why", "a.rs:1", "--dir", repo.to_str().unwrap()], &root);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("Ann") && text.contains("task: add a") && text.contains("A keeps the API small."), "{text}");
    assert!(text.contains("lesson L7: Is the API still small? -> callers break"), "{text}");

    let out = borg(&["why", "HEAD", "--json", "--dir", repo.to_str().unwrap()], &root);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["commits"][0]["sha"], head.as_str());
    assert_eq!(v["commits"][0]["reasoning"]["episodes"][0], "s-1-7");

    // Off: nothing at all, exit 0, even though the repo has a note.
    let out = borg(&["why", "a.rs:1", "--dir", off.to_str().unwrap()], &root);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");

    // Relative: `--dir .` (and no --dir) in the off repo routes the resolved directory: nothing.
    for args in [&["why", "a.rs:1", "--dir", "."][..], &["why", "a.rs"], &["why", "HEAD", "--dir", "."]] {
        let out = borg_in(args, &root, &off);
        assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty(), "{args:?}: {out:?}");
    }
    let out = borg_in(&["why", "a.rs:1", "--dir", "."], &root, &repo);
    assert!(String::from_utf8_lossy(&out.stdout).contains("task: add a"), "a relative --dir in a team repo still works: {out:?}");

    // A symlink inside the team repo into the off repo: nothing, as a file and as a directory.
    std::os::unix::fs::symlink(&off, repo.join("link")).unwrap();
    for (args, cwd) in [(&["why", "link/a.rs"][..], &repo), (&["why", "a.rs:1"], &repo.join("link")), (&["why", "HEAD", "--dir", "link"], &repo)] {
        let out = borg_in(args, &root, cwd);
        assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty(), "{args:?} in {cwd:?}: {out:?}");
    }

    // No git: a clear error.
    let out = borg(&["why", "a.rs:1", "--dir", plain.to_str().unwrap()], &root);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not in a git repo"), "{out:?}");

    // A broken config routes everything off: nothing is shown.
    std::fs::write(root.join("collectives.json"), "{not json").unwrap();
    let out = borg(&["why", "a.rs:1", "--dir", repo.to_str().unwrap()], &root);
    assert!(out.status.success() && out.stdout.is_empty(), "{out:?}");

    assert_eq!(borg(&["why"], &root).status.code(), Some(2), "usage");
    let _ = std::fs::remove_dir_all(&root);
}
