//! Hand-off through the binary (handoff/): one machine (or several stores standing in for machines),
//! sessions named by their harness's variable, repos cloned from a bare local remote. The offer's
//! snapshot is the offering working tree (untracked files in, ignored files and changes that add a
//! secret out) and leaves the engineer's branch, index and stash alone, and its brief reaches the taker
//! byte for byte; the remote's `taken` ref decides every race, two takes or a take-back against a take,
//! exactly one winner each time; an operation interrupted after any step of its log is finished or
//! rolled back by the next run; the original's edits in the handed-off repo are refused, its reads pass
//! and its other repos are untouched. No model call, no network.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::io::Write;

use serde_json::{Value, json};

struct Run {
    out: String,
    err: String,
    code: Option<i32>,
}

/// A test root: a bare remote, the clone `work/app` (alice's, with work in progress), a second clone
/// `work/app2` (bob's), a repo `work/other`, and a team collective over `work`.
fn setup(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-handoff-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("work")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    // The paths borg prints are git's, which resolve symlinks: on macOS the temp dir `/var/folders`
    // is `/private/var/folders`, so the test compares against the resolved root.
    let root = std::fs::canonicalize(&root).unwrap();
    git(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
    let app = root.join("work/app");
    git(&root, &["init", "-q", "-b", "main", &app.to_string_lossy()]);
    identity(&app, "alice@x");
    std::fs::write(app.join(".gitignore"), "build/\n").unwrap();
    std::fs::write(app.join("lib.rs"), "fn a() {}\n").unwrap();
    std::fs::write(app.join("old.rs"), "fn old() {}\n").unwrap();
    std::fs::write(app.join("config.toml"), "name = \"app\"\n").unwrap();
    // A binary of a kind the repo keeps: a new one of the kind goes into a hand-off.
    std::fs::write(app.join("icon.png"), [0x89, b'P', b'N', b'G', 0, 1]).unwrap();
    git(&app, &["add", "-A"]);
    git(&app, &["commit", "-q", "-m", "start"]);
    git(&app, &["remote", "add", "origin", &root.join("remote.git").to_string_lossy()]);
    git(&app, &["push", "-q", "-u", "origin", "main"]);
    git(&root.join("work"), &["clone", "-q", &root.join("remote.git").to_string_lossy(), "app2"]);
    identity(&root.join("work/app2"), "bob@y");
    let other = root.join("work/other");
    git(&root, &["init", "-q", "-b", "main", &other.to_string_lossy()]);
    identity(&other, "alice@x");
    let cfg = json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("work")], "share": "team"}]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    root
}

fn identity(repo: &Path, email: &str) {
    git(repo, &["config", "user.email", email]);
    git(repo, &["config", "user.name", email.split('@').next().unwrap()]);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_NOSYSTEM", "1").env("HOME", dir).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The work in progress the hand-off carries: a tracked change, a deletion, untracked files (a lock
/// file full of base64, a binary of a kind the repo tracks, a symlink to a device), an untracked secret,
/// an untracked binary of a kind it tracks none of (a keystore), a tracked change that adds a token, an
/// ignored build output.
fn work_in_progress(app: &Path) {
    std::fs::write(app.join("lib.rs"), "fn a() { b() }\n").unwrap();
    std::fs::remove_file(app.join("old.rs")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(app.join("src/new.rs"), "fn b() {}\nlet key = map.get(k);\n").unwrap();
    let lock = "{\"integrity\": \"sha512-Zx9Qk2LmPw8RtYv3NbHs6JdFgKa1Bc4DZx9Qk2LmPw8RtYv3NbHs6JdFgKa1Bc4D==\"}\n";
    std::fs::write(app.join("package-lock.json"), lock).unwrap();
    std::fs::write(app.join("logo.png"), [0x89, b'P', b'N', b'G', 0, 0, b'A', b'P', b'I', b'_', b'K', b'E', b'Y', b'=', b'x']).unwrap();
    let _ = std::os::unix::fs::symlink("/dev/zero", app.join("zero"));
    std::fs::write(app.join(".env"), "API_KEY=sk-live-0123456789abcdef0123\n").unwrap();
    std::fs::write(app.join("keys.p12"), [0x30, 0x82, 0, 0, 2, 1, 3]).unwrap();
    std::fs::write(app.join("config.toml"), "name = \"app\"\ntoken = \"ghp_0123456789abcdefghijABCD\"\n").unwrap();
    std::fs::create_dir_all(app.join("build")).unwrap();
    std::fs::write(app.join("build/out.bin"), [0u8, 1, 2]).unwrap();
}

/// A brief the skill's outline would give, with nothing in it to redact.
const BRIEF: &str = "# Lib calls b\n\n## Goal\nlib calls b.\n\n## Next step\nnext: tests for b\n\n## How to verify\n`cargo test`  \ttabs, trailing spaces\n";

/// Offers the work at `cwd` as `session` with `note` and the brief `BRIEF` (from a file outside the repo).
fn offer(root: &Path, cwd: &Path, session: &str, note: &str) -> Run {
    offer_in(root, "store", cwd, session, note, &[])
}

fn offer_in(root: &Path, store: &str, cwd: &Path, session: &str, note: &str, env: &[(&str, &str)]) -> Run {
    let brief = root.join(format!("brief-{session}.md"));
    std::fs::write(&brief, BRIEF).unwrap();
    borg_in(root, store, cwd, Some(session), &["handoff", note, "--brief", &brief.to_string_lossy()], env)
}

/// `borg args…` in `cwd` with the store `store` (a machine of its own), as `session` when given.
fn borg_in(root: &Path, store: &str, cwd: &Path, session: Option<&str>, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(args)
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join(store))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_ASYNC", "off")
        .env("BORG_TEST_HARNESS", "1")
        .envs(session.map(|s| ("CLAUDE_CODE_SESSION_ID", s)))
        .envs(env.iter().copied())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c.output().unwrap();
    Run { out: String::from_utf8_lossy(&out.stdout).into(), err: String::from_utf8_lossy(&out.stderr).into(), code: out.status.code() }
}

fn borg(root: &Path, cwd: &Path, session: &str, args: &[&str]) -> Run {
    borg_in(root, "store", cwd, Some(session), args, &[])
}

/// The hook of Claude Code for `event` (store `store`): its answer, Null for none.
fn hook(root: &Path, store: &str, event: &str, input: &Value) -> Value {
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", "claude", event])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join(store))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_ASYNC", "off")
        .env("BORG_TEST_HARNESS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() { Value::Null } else { serde_json::from_str(text.trim()).unwrap() }
}

/// Whether the hook refuses `tool` with `input` for `session` in `cwd`.
fn refused(root: &Path, session: &str, cwd: &Path, tool: &str, input: Value) -> bool {
    let ev = json!({"session_id": session, "cwd": cwd, "tool_name": tool, "tool_input": input});
    hook(root, "store", "pre_tool", &ev)["hookSpecificOutput"]["permissionDecision"] == "deny"
}

fn id_in(out: &str) -> String {
    let at = out.find("hand-off h").unwrap_or_else(|| panic!("no hand-off id in {out:?}")) + "hand-off ".len();
    out[at..at + 13].to_string()
}

fn store_of(root: &Path, store: &str) -> PathBuf {
    root.join(store).join("collectives/corp")
}

/// The hand-off refs the remote holds, as `ls-remote` prints them.
fn remote_refs(root: &Path) -> String {
    git(&root.join("work/app"), &["ls-remote", "origin", "refs/borg/*"])
}

/// Every file under `dir` (`.git` aside) with its bytes (a symlink: its target), `skip` left out.
fn files(dir: &Path, skip: &[&str]) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(d).unwrap().map(Result::unwrap) {
            let p = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            let t = e.file_type().unwrap();
            let key = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if t.is_symlink() {
                out.insert(key, std::fs::read_link(&p).unwrap().to_string_lossy().into_owned().into_bytes());
            } else if t.is_dir() {
                walk(base, &p, out);
            } else {
                out.insert(key, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out.retain(|k, _| !skip.iter().any(|s| k.starts_with(s)));
    out
}

/// The logs of hand-off `id` on `store`, open and closed, in one text.
fn log_text(root: &Path, store: &str, id: &str) -> String {
    let d = store_of(root, store).join("handoff");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&d)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(id) && (n.ends_with(".wal") || n.ends_with(".done"))))
        .collect();
    names.sort_by_key(|p| p.extension().is_some_and(|e| e == "wal"));
    names.iter().map(|p| std::fs::read_to_string(p).unwrap()).collect()
}

/// Forgets every log of hand-off `id` on the store: a machine that lost its log.
fn forget_logs(root: &Path, id: &str) {
    for e in std::fs::read_dir(store_of(root, "store").join("handoff")).unwrap().filter_map(Result::ok) {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with(id) && (n.ends_with(".wal") || n.ends_with(".done")) {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
}

/// The value metric: the taker's tree is the offering working tree (tracked change, deletion and
/// untracked files in, a lock file, a binary of a kind the repo tracks and a symlink too; the ignored
/// output, the untracked secret, a binary of a kind it tracks none of and the tracked change that adds a
/// token out), and offering left the engineer's branch, index and
/// stash as they were. The brief reaches the taker byte for byte, printed and kept beside its work tree.
/// Then the original is fenced: its edits there are refused, its reads pass, its other repo is
/// untouched, and it is told who took the work.
#[test]
fn a_take_gets_the_offering_working_tree_and_fences_the_original() {
    let root = setup("tree");
    let (app, app2, other) = (root.join("work/app"), root.join("work/app2"), root.join("work/other"));
    work_in_progress(&app);
    git(&app, &["add", "lib.rs"]);
    let wip = git(&app, &["stash", "create"]);
    git(&app, &["stash", "store", "-m", "kept", &wip]);
    let before = (
        git(&app, &["rev-parse", "HEAD"]),
        git(&app, &["status", "--porcelain"]),
        git(&app, &["diff", "--cached", "--name-status"]),
        git(&app, &["stash", "list"]),
    );
    // A pre-push hook that runs (and fails) the tests: the hand-off's own refs skip it.
    std::fs::write(app.join(".git/hooks/pre-push"), "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(app.join(".git/hooks/pre-push"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let r = offer(&root, &app, "s1", "done: lib calls b; next: tests for b");
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(r.out.contains("left out") && r.out.contains("\".env\"") && r.out.contains("\"config.toml\"") && !r.out.contains("package-lock"), "{}", r.out);
    assert!(r.out.contains("untracked binary files") && r.out.contains("\"keys.p12\"") && !r.out.contains("logo.png"), "{}", r.out);
    let id = id_in(&r.out);
    let after = (
        git(&app, &["rev-parse", "HEAD"]),
        git(&app, &["status", "--porcelain"]),
        git(&app, &["diff", "--cached", "--name-status"]),
        git(&app, &["stash", "list"]),
    );
    assert_eq!(before, after, "branch, index and stash untouched");
    let refs = remote_refs(&root);
    assert!(
        refs.contains(&format!("refs/borg/handoff/{id}/state")) && refs.contains(&format!("refs/borg/handoff/{id}/brief")) && !refs.contains("probe"),
        "{refs}"
    );
    let listed = borg(&root, &app2, "s2", &["handoff"]);
    assert!(listed.out.contains(&id) && listed.out.contains("open") && listed.out.contains(&format!("borg handoff take {id}")), "{}", listed.out);

    let t = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert!(
        t.out.contains(&format!("took hand-off {id} from alice@x"))
            && t.out.contains(BRIEF.trim_end())
            && t.out.contains("not in the state")
            && t.out.contains("\".env\""),
        "{}",
        t.out
    );
    assert_eq!(git(&app2, &["branch", "--show-current"]), format!("borg/handoff/{id}"), "a clean clone on the offer's branch switches in place");
    assert_eq!(files(&app2, &["config.toml"]), files(&app, &[".env", "build/", "config.toml", "keys.p12"]), "the taker's tree is the offering working tree");
    assert_eq!(std::fs::read_to_string(app2.join("config.toml")).unwrap(), "name = \"app\"\n", "a tracked change that adds a token: as HEAD has it");
    let offered: Value =
        serde_json::from_str(std::fs::read_to_string(store_of(&root, "store").join("handoffs.jsonl")).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(git(&app2, &["rev-parse", "HEAD^{tree}"]), offered["repos"][0]["tree"].as_str().unwrap(), "the tree hash the offer line names");
    assert!(offered["brief_head"].as_str().unwrap().starts_with("# Lib calls b") && !offered.to_string().contains("sk-live"), "{offered}");
    let kept = app2.join(format!(".git/borg-handoff/{id}"));
    assert_eq!(std::fs::read_to_string(kept.join("HANDOFF.md")).unwrap(), BRIEF, "the brief round-trips byte for byte");
    let meta = std::fs::read_to_string(kept.join("state.json")).unwrap();
    assert!(meta.contains("D old.rs") && meta.contains("\"s1\"") && !meta.contains("sk-live") && !meta.contains("ghp_"), "{meta}");

    // The original: edits refused there, reads pass, its other repo untouched; the taker edits freely.
    assert!(refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")})));
    assert!(refused(&root, "s1", &app, "Bash", json!({"command": "echo x >> lib.rs"})));
    assert!(!refused(&root, "s1", &app, "Read", json!({"file_path": app.join("lib.rs")})));
    assert!(!refused(&root, "s1", &app, "Bash", json!({"command": "git status && cat lib.rs"})));
    assert!(!refused(&root, "s1", &other, "Write", json!({"file_path": other.join("x.rs")})));
    assert!(!refused(&root, "s2", &app2, "Edit", json!({"file_path": app2.join("lib.rs")})));
    let told = hook(&root, "store", "prompt", &json!({"session_id": "s1", "cwd": app, "prompt": "go on"}));
    let ctx = told["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default();
    assert!(ctx.contains(&format!("Hand-off {id} was taken by bob@y")), "{told}");
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains("taken") && listed.out.contains("by bob@y"), "{}", listed.out);
    let _ = std::fs::remove_dir_all(&root);
}

/// A re-run from the taking session finds its claim and only redoes the checkout steps; another session
/// of the same engineer is told whose it is and continues it with `--resume`; a lost local log (a lost
/// reply) is no lost claim: the claim names the node and session. A take in a dirty clone gets a work
/// tree; a branch of that name pointing elsewhere gets a suffix.
#[test]
fn a_take_is_idempotent_and_a_dirty_clone_gets_a_work_tree() {
    let root = setup("again");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "next: tests").out);
    git(&app2, &["branch", &format!("borg/handoff/{id}")]);
    std::fs::write(app2.join("scratch.txt"), "bob's own work\n").unwrap();
    let first = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!(first.code, Some(0), "{}{}", first.out, first.err);
    let wt = root.join(format!("work/app2-{id}-2"));
    assert!(first.out.contains(&format!("branch borg/handoff/{id}-2 in {}", wt.display())), "{}", first.out);
    assert!(first.out.contains(&format!("branch borg/handoff/{id} exists and points elsewhere; using borg/handoff/{id}-2")), "{}", first.out);
    assert_eq!(
        std::fs::read_to_string(app2.join(format!(".git/worktrees/app2-{id}-2/borg-handoff/{id}/HANDOFF.md"))).unwrap(),
        BRIEF,
        "kept in the work tree's git dir"
    );
    assert_eq!(std::fs::read_to_string(app2.join("scratch.txt")).unwrap(), "bob's own work\n", "the clone is untouched");
    assert_eq!(git(&app2, &["branch", "--show-current"]), "main");
    let claim = |o: &str| o.split("claim ").nth(1).map(|c| c[..12].to_string());
    let again = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!((again.code, claim(&again.out)), (Some(0), claim(&first.out)), "{}{}", again.out, again.err);
    forget_logs(&root, &id);
    let lost = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!((lost.code, claim(&lost.out)), (Some(0), claim(&first.out)), "{}{}", lost.out, lost.err);
    let other = borg(&root, &app2, "s3", &["handoff", "take", &id]);
    assert!(other.code == Some(1) && other.err.contains("taken by your session s2"), "{}", other.err);
    let resumed = borg(&root, &app2, "s3", &["handoff", "take", &id, "--resume"]);
    assert_eq!((resumed.code, claim(&resumed.out)), (Some(0), claim(&first.out)), "{}{}", resumed.out, resumed.err);
    let stranger = borg_in(
        &root,
        "store",
        &app2,
        Some("s4"),
        &["handoff", "take", &id],
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "user.email"), ("GIT_CONFIG_VALUE_0", "carol@z")],
    );
    assert!(stranger.code == Some(1) && stranger.err.contains("was taken by bob@y"), "{}", stranger.err);
    let _ = std::fs::remove_dir_all(&root);
}

/// The take-back: the offering engineer's take in the very tree it came from checks nothing out. Taken
/// back by the offering session itself, it fences nobody; by another session of the engineer, the
/// offering session is fenced and told, as for any take, and the taking session edits on.
#[test]
fn the_owner_takes_it_back_in_place() {
    let root = setup("back");
    let app = root.join("work/app");
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "leaving").out);
    let status = git(&app, &["status", "--porcelain"]);
    let r = borg(&root, &app, "s5", &["handoff", "take", &id]);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(r.out.contains("took back hand-off") && r.out.contains("nothing was checked out"), "{}", r.out);
    assert_eq!((git(&app, &["branch", "--show-current"]), git(&app, &["status", "--porcelain"])), ("main".into(), status));
    assert!(refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")})), "the offering session is fenced: s5 has the work");
    assert!(!refused(&root, "s5", &app, "Edit", json!({"file_path": app.join("lib.rs")})), "the taking session edits on");
    let told = hook(&root, "store", "prompt", &json!({"session_id": "s1", "cwd": app, "prompt": "go on"}));
    assert!(told["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().contains("was taken back by alice@x"), "{told}");
    assert!(borg(&root, &app, "s1", &["handoff"]).out.contains("taken back"));
    let own = id_in(&offer(&root, &app, "s6", "back soon").out);
    let r = borg(&root, &app, "s6", &["handoff", "take", &own]);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(!refused(&root, "s6", &app, "Edit", json!({"file_path": app.join("lib.rs")})), "taken back by the offering session itself: nobody fenced");
    let _ = std::fs::remove_dir_all(&root);
}

/// The offer is all or nothing: a refused or unreachable remote, a detached HEAD or a rebase in
/// progress, a local collective: no offer and nothing left on the remote.
#[test]
fn an_offer_refuses_with_the_reason_and_leaves_nothing() {
    let root = setup("refuse");
    let app = root.join("work/app");
    work_in_progress(&app);
    // A remote whose hook refuses the hand-off refs (a hosting rule, a fork without rights).
    std::fs::write(root.join("remote.git/hooks/update"), "#!/bin/sh\ncase \"$1\" in refs/borg/*) echo no custom refs >&2; exit 1;; esac\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.join("remote.git/hooks/update"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let r = offer(&root, &app, "s1", "note");
    assert!(
        r.code == Some(1) && r.err.contains("no offer: remote origin does not take and delete the hand-off refs") && r.err.contains("nothing is left on it"),
        "{}",
        r.err
    );
    // Only the brief refused: the atomic push leaves the state out too.
    std::fs::write(root.join("remote.git/hooks/update"), "#!/bin/sh\ncase \"$1\" in */brief) echo no >&2; exit 1;; esac\n").unwrap();
    let r = offer(&root, &app, "s1", "note");
    assert!(r.code == Some(1) && r.err.contains("no offer: remote origin:") && r.err.contains("nothing is left on it"), "{}", r.err);
    assert_eq!(remote_refs(&root), "", "neither the state nor the brief");
    std::fs::remove_file(root.join("remote.git/hooks/update")).unwrap();
    git(&app, &["remote", "set-url", "origin", &root.join("gone.git").to_string_lossy()]);
    let r = offer(&root, &app, "s1", "note");
    assert!(r.code == Some(1) && r.err.contains("no offer") && !r.err.contains("cannot write the hand-off log"), "{}", r.err);
    git(&app, &["remote", "set-url", "origin", &root.join("remote.git").to_string_lossy()]);
    let head = git(&app, &["rev-parse", "HEAD"]);
    git(&app, &["checkout", "-q", &head]);
    let r = offer(&root, &app, "s1", "note");
    assert!(r.code == Some(1) && r.err.contains("detached"), "{}", r.err);
    git(&app, &["checkout", "-q", "main"]);
    std::fs::create_dir_all(app.join(".git/rebase-merge")).unwrap();
    let r = offer(&root, &app, "s1", "note");
    assert!(r.code == Some(1) && r.err.contains("a rebase is in progress"), "{}", r.err);
    std::fs::remove_dir_all(app.join(".git/rebase-merge")).unwrap();
    assert_eq!(remote_refs(&root), "", "nothing left on the remote");
    assert!(borg(&root, &app, "s1", &["handoff"]).out.contains("no hand-offs"));
    // A terminal (no session variable) fences no session: no offer.
    let brief = root.join("brief-s1.md");
    let r = borg_in(&root, "store", &app, None, &["handoff", "note", "--brief", &brief.to_string_lossy()], &[]);
    assert!(r.code == Some(1) && r.err.contains("no session runs this command"), "{}", r.err);
    let local = json!({"default": "off", "collectives": [{"name": "corp", "paths": [root.join("work")], "share": "local"}]});
    std::fs::write(root.join("collectives.json"), local.to_string()).unwrap();
    let r = offer(&root, &app, "s1", "note");
    assert!(r.code == Some(1) && r.err.contains("local collective"), "{}", r.err);
    let _ = std::fs::remove_dir_all(&root);
}

/// A remote that takes refs but refuses deleting them (a hook or a hosting rule; git's own
/// `receive.denyDeletes` guards branches only) is caught by the probe alone: no offer, nothing but the
/// probe left, the command to delete it named, and the operation ended in that one run.
#[test]
fn a_remote_that_refuses_deletes_is_caught_by_the_probe() {
    let root = setup("nodelete");
    let app = root.join("work/app");
    work_in_progress(&app);
    let hook = root.join("remote.git/hooks/update");
    std::fs::write(&hook, "#!/bin/sh\ncase \"$3\" in *[!0]*) exit 0;; esac\necho no deletes >&2; exit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let r = offer(&root, &app, "s1", "note");
    assert!(
        r.code == Some(1) && r.err.contains("refused deleting") && r.err.contains("delete it by hand: git push origin :refs/borg/handoff/h"),
        "{:?} {}{}",
        r.code,
        r.out,
        r.err
    );
    let refs = remote_refs(&root);
    assert!(refs.contains("/probe") && !refs.contains("/state") && !refs.contains("/brief"), "{refs}");
    let again = borg(&root, &app, "s1", &["handoff"]);
    assert!(!again.out.contains("interrupted") && !again.err.contains("interrupted") && again.out.contains("no hand-offs"), "{}{}", again.out, again.err);
    let _ = std::fs::remove_dir_all(&root);
}

/// Only an offer takes the note; a verb another tool names the same act by, a verb without its id,
/// several bare words or a note without a brief is refused before anything is pushed.
#[test]
fn an_unknown_verb_or_a_loose_note_offers_nothing() {
    let root = setup("verbs");
    let app = root.join("work/app");
    work_in_progress(&app);
    for args in [
        &["handoff", "withdraw", "h0123456789ab"][..],
        &["handoff", "done"],
        &["handoff", "abort", "h0123456789ab", "h0123456789ac"],
        &["handoff", "take", "h0123456789ab", "--why", "x"],
        &["handoff", "decline"],
        &["handoff", "take", "h0123456789ab", "--to", "bob@y"],
        &["handoff", "list"],
        &["handoff", "two", "words"],
        &["handoff", "a note but no brief"],
    ] {
        let r = borg(&root, &app, "s1", args);
        assert_eq!(r.code, Some(2), "{args:?}: {}{}", r.out, r.err);
    }
    let brief = root.join("brief.md");
    std::fs::write(&brief, "# x\n").unwrap();
    let r = borg(&root, &app, "s1", &["handoff", "line one\nline two", "--brief", &brief.to_string_lossy()]);
    assert!(r.code == Some(2) && r.err.contains("the note is one line"), "{}", r.err);
    assert_eq!(remote_refs(&root), "", "nothing pushed");
    let _ = std::fs::remove_dir_all(&root);
}

/// The brief is redacted like a message before it leaves the machine, from stdin as from a file.
#[test]
fn the_brief_is_redacted_and_read_from_stdin() {
    let root = setup("brief");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["handoff", "--brief", "-"])
        .env_clear()
        .env("BORG_UPDATE_CHECK", "off")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_LEARN", "off")
        .env("BORG_TEST_HARNESS", "1")
        .env("CLAUDE_CODE_SESSION_ID", "s1")
        .current_dir(&app)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"# Deploy fix\n\n## Context\nthe db password = hunter22 for staging\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}{}", String::from_utf8_lossy(&out.stderr));
    let id = id_in(&text);
    assert!(borg(&root, &app2, "s2", &["handoff"]).out.contains("Deploy fix"), "the note defaults to the brief's title");
    let t = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    let kept = std::fs::read_to_string(app2.join(format!(".git/borg-handoff/{id}/HANDOFF.md"))).unwrap();
    assert!(!kept.contains("hunter22") && kept.contains("[REDACTED]") && !t.out.contains("hunter22"), "{kept}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A sparse checkout hands off its whole tree: the files outside its cone stay in the state as HEAD has
/// them, not deleted.
#[test]
fn a_sparse_checkout_hands_off_its_whole_tree() {
    let root = setup("sparse");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    std::fs::create_dir_all(app.join("docs")).unwrap();
    std::fs::write(app.join("docs/a.md"), "docs\n").unwrap();
    git(&app, &["add", "-A"]);
    git(&app, &["commit", "-q", "-m", "docs"]);
    git(&app, &["push", "-q", "origin", "main"]);
    git(&app2, &["pull", "-q"]);
    git(&app, &["sparse-checkout", "set", "--cone", "src"]);
    assert!(!app.join("docs/a.md").exists(), "outside the cone");
    std::fs::write(app.join("lib.rs"), "fn a() { b() }\n").unwrap();
    let id = id_in(&offer(&root, &app, "s1", "sparse").out);
    let t = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert_eq!(std::fs::read_to_string(app2.join("docs/a.md")).unwrap(), "docs\n");
    assert_eq!(std::fs::read_to_string(app2.join("lib.rs")).unwrap(), "fn a() { b() }\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// A made-up outcome line can lift no fence: one citing a claim of the offering session itself (and an
/// id that sorts first) changes nothing once the remote was seen holding the real claim, which is kept;
/// a machine that never heard the remote finds it contested and stays fenced.
#[test]
fn a_forged_outcome_lifts_no_fence() {
    let root = setup("forged");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    assert_eq!(borg(&root, &app2, "s2", &["handoff", "take", &id]).code, Some(0));
    let log = store_of(&root, "store").join("handoffs.jsonl");
    let offered: Value = serde_json::from_str(std::fs::read_to_string(&log).unwrap().lines().next().unwrap()).unwrap();
    let zero = "0".repeat(40);
    let forged = json!({"v": 1, "id": id, "kind": "back", "node": offered["node"], "session": "s1", "engineer": "alice@x", "claim": zero, "state": zero});
    std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(format!("{forged}\n").as_bytes()).unwrap();
    let edit = || refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")}));
    assert!(edit(), "still fenced before anyone asks the remote");
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(
        listed.out.contains("taken ") && listed.out.contains("by bob@y") && listed.out.contains("the remote holds it"),
        "the remote's claim: {}",
        listed.out
    );
    assert!(edit(), "fenced by the real claim");
    // What the remote said is kept: with it out of reach (from every clone of it here, the taker's too)
    // the real claim still holds.
    for clone in [&app, &app2] {
        git(clone, &["remote", "set-url", "origin", &root.join("gone.git").to_string_lossy()]);
    }
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains("by bob@y"), "{}", listed.out);
    // The remote refuted it: this machine's log keeps it no more.
    assert!(!std::fs::read_to_string(&log).unwrap().contains(&zero), "the refuted line is dropped");
    // A machine that never heard the remote has the lines alone: contested, and fenced.
    std::fs::remove_file(store_of(&root, "store").join("handoff/remote.json")).unwrap();
    std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(format!("{forged}\n").as_bytes()).unwrap();
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains("contested"), "{}", listed.out);
    assert!(edit(), "unconfirmed: fenced");
    let _ = std::fs::remove_dir_all(&root);
}

/// Two runs of one session's take at once on one store: the second waits for the first, finds its
/// claim and continues it; both report the same claim.
#[test]
fn two_runs_of_one_take_agree() {
    let root = setup("same");
    let app = root.join("work/app");
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    let run = || {
        let (root, id) = (root.clone(), id.clone());
        std::thread::spawn(move || borg_in(&root, "store", &root.join("work/app2"), Some("s2"), &["handoff", "take", &id], &[]))
    };
    let (a, b) = (run(), run());
    let (a, b) = (a.join().unwrap(), b.join().unwrap());
    assert_eq!((a.code, b.code), (Some(0), Some(0)), "{}{} / {}{}", a.out, a.err, b.out, b.err);
    let claim = |o: &str| o.split("claim ").nth(1).map(|c| c[..12].to_string());
    assert_eq!(claim(&a.out), claim(&b.out));
    let _ = std::fs::remove_dir_all(&root);
}

/// The offer line, as another machine's store gets it (the mesh in a later slice; copied here).
fn carry(root: &Path, to: &str) {
    let log = std::fs::read_to_string(store_of(root, "store").join("handoffs.jsonl")).unwrap();
    std::fs::create_dir_all(store_of(root, to)).unwrap();
    std::fs::write(store_of(root, to).join("handoffs.jsonl"), log).unwrap();
}

/// The steps a take logged on a store: whether it got as far as its claim.
fn claimed_on(root: &Path, store: &str, id: &str) -> bool {
    log_text(root, store, id).contains("\"step\":\"claim\"")
}

/// Two machines (stores of their own, clones of their own) take one offer at once, 100 times: the
/// remote's `taken` ref lets exactly one win every round; the other is told who did.
#[test]
fn two_concurrent_takes_have_exactly_one_winner() {
    let root = setup("race");
    let app = root.join("work/app");
    git(&root.join("work"), &["clone", "-q", &root.join("remote.git").to_string_lossy(), "app3"]);
    identity(&root.join("work/app3"), "carol@z");
    let (mut both_claimed, rounds) = (0, 100);
    for round in 0..rounds {
        std::fs::write(app.join("lib.rs"), format!("fn a() {{ {round} }}\n")).unwrap();
        let o = offer(&root, &app, "s1", "round");
        assert_eq!(o.code, Some(0), "{}", o.err);
        let id = id_in(&o.out);
        carry(&root, "storeB");
        carry(&root, "storeC");
        let take = |store: &'static str, clone: &'static str, session: &'static str| {
            let (root, id) = (root.clone(), id.clone());
            std::thread::spawn(move || borg_in(&root, store, &root.join(clone), Some(session), &["handoff", "take", &id], &[]))
        };
        let (b, c) = (take("storeB", "work/app2", "b1"), take("storeC", "work/app3", "c1"));
        let (b, c) = (b.join().unwrap(), c.join().unwrap());
        let won: Vec<&Run> = [&b, &c].into_iter().filter(|r| r.code == Some(0)).collect();
        assert_eq!(won.len(), 1, "round {round}: {}{} / {}{}", b.out, b.err, c.out, c.err);
        let lost = if b.code == Some(0) { &c } else { &b };
        assert!(lost.err.contains("was taken by"), "round {round}: {}", lost.err);
        both_claimed += usize::from(claimed_on(&root, "storeB", &id) && claimed_on(&root, "storeC", &id));
        for clone in ["work/app2", "work/app3"] {
            git(&root.join(clone), &["checkout", "-q", "main"]);
        }
    }
    eprintln!("{rounds} rounds, exactly one winner each; in {both_claimed} both takers pushed a claim");
    let _ = std::fs::remove_dir_all(&root);
}

/// The offering engineer's take-back races a colleague's take through the same ref: exactly one wins,
/// and an owner whose take-back lost is fenced at once.
#[test]
fn a_take_back_and_a_take_have_exactly_one_winner() {
    let root = setup("backrace");
    let app = root.join("work/app");
    let (mut backs, mut both_claimed, rounds) = (0, 0, 20);
    for round in 0..rounds {
        std::fs::write(app.join("lib.rs"), format!("fn a() {{ {round} }}\n")).unwrap();
        let id = id_in(&offer(&root, &app, "s1", "round").out);
        carry(&root, "storeB");
        let (r1, id1) = (root.clone(), id.clone());
        let back = std::thread::spawn(move || borg_in(&r1, "store", &r1.join("work/app"), Some("s1"), &["handoff", "take", &id1], &[]));
        let (r2, id2) = (root.clone(), id.clone());
        let take = std::thread::spawn(move || borg_in(&r2, "storeB", &r2.join("work/app2"), Some("b1"), &["handoff", "take", &id2], &[]));
        let (back, take) = (back.join().unwrap(), take.join().unwrap());
        assert_eq!(
            usize::from(back.code == Some(0)) + usize::from(take.code == Some(0)),
            1,
            "round {round}: {}{} / {}{}",
            back.out,
            back.err,
            take.out,
            take.err
        );
        backs += usize::from(back.code == Some(0));
        if take.code == Some(0) {
            // The take-back that lost learned whose claim holds: its session is fenced at once, before
            // any line of the taker reaches its machine.
            let probe = json!({"file_path": app.join("lib.rs"), "content": "x"});
            assert!(refused(&root, "s1", &app, "Write", probe), "round {round}: the losing owner is fenced: {}", back.err);
        }
        both_claimed += usize::from(claimed_on(&root, "store", &id) && claimed_on(&root, "storeB", &id));
        git(&root.join("work/app2"), &["checkout", "-q", "main"]);
    }
    eprintln!("{rounds} rounds, exactly one winner each; the take-back won {backs}; in {both_claimed} both pushed a claim");
    let _ = std::fs::remove_dir_all(&root);
}

/// The steps of the hand-off's logs on the store, by name.
fn steps(root: &Path, id: &str) -> Vec<String> {
    log_text(root, "store", id)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| v["step"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The only hand-off with a log on the store: its id.
fn only_id(root: &Path) -> String {
    let d = store_of(root, "store").join("handoff");
    let mut ids: Vec<String> = std::fs::read_dir(d)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_str()?.to_string();
            (n.starts_with('h') && (n.ends_with(".wal") || n.ends_with(".done"))).then(|| n[..13].to_string())
        })
        .collect();
    ids.dedup();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids[0].clone()
}

/// An offer interrupted after each step of its log: the next `borg handoff` publishes it when its state
/// was on the remote and read back, else rolls it back and leaves nothing there (also after its state
/// and brief were pushed).
#[test]
fn an_interrupted_offer_is_finished_or_rolled_back() {
    for (step, published) in
        [("offer", false), ("snapshot", false), ("brief", false), ("pushing", false), ("pushed", false), ("verified", true), ("logged", true)]
    {
        let root = setup(&format!("crash-offer-{step}"));
        let app = root.join("work/app");
        work_in_progress(&app);
        let r = offer_in(&root, "store", &app, "s1", "note", &[("BORG_TEST_HANDOFF_STOP", step)]);
        if step == "pushed" {
            assert!(remote_refs(&root).contains("/state") && remote_refs(&root).contains("/brief"), "pushed before the stop");
        }
        assert_eq!(r.code, Some(3), "{step}: {}{}", r.out, r.err);
        let id = only_id(&root);
        let listed = borg(&root, &app, "s1", &["handoff"]);
        assert!(listed.out.contains(&format!("interrupted {id}")), "{step}: {}{}", listed.out, listed.err);
        assert_eq!(steps(&root, &id).last().map(String::as_str), Some(if published { "done" } else { "ended" }), "{step}");
        assert_eq!(listed.out.contains("open "), published, "{step}: {}", listed.out);
        assert_eq!(remote_refs(&root).contains("/state"), published, "{step}: {}", remote_refs(&root));
        assert_eq!(remote_refs(&root).contains("/brief"), published, "{step}: {}", remote_refs(&root));
        assert!(!remote_refs(&root).contains("probe"), "{step}");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A take interrupted after each step of its log: the next run decides it from the remote (a claim
/// never pushed leaves the offer open, and a later take wins it; one pushed whose reply never came is
/// found won) or finishes it.
#[test]
fn an_interrupted_take_is_finished_or_ended() {
    for (step, finished) in [("take", false), ("claim", false), ("sent", true), ("won", true), ("logged", true), ("checkout", true), ("told", true)] {
        let root = setup(&format!("crash-take-{step}"));
        let (app, app2) = (root.join("work/app"), root.join("work/app2"));
        work_in_progress(&app);
        let id = id_in(&offer(&root, &app, "s1", "note").out);
        let r = borg_in(&root, "store", &app2, Some("s2"), &["handoff", "take", &id], &[("BORG_TEST_HANDOFF_STOP", step)]);
        assert_eq!(r.code, Some(3), "{step}: {}{}", r.out, r.err);
        // Another session's run finishes it, in the taker's name.
        let listed = borg(&root, &app, "s9", &["handoff"]);
        assert!(listed.out.contains(&format!("interrupted {id}")), "{step}: {}{}", listed.out, listed.err);
        assert_eq!(remote_refs(&root).contains("/taken"), finished, "{step}");
        if finished {
            assert_eq!(git(&app2, &["branch", "--show-current"]), format!("borg/handoff/{id}"), "{step}");
            assert!(listed.out.contains("by bob@y (session s2,"), "{step}: {}", listed.out);
            let won: Vec<String> = log_text(&root, "store", &id).lines().filter(|l| l.contains("\"step\":\"won\"")).map(str::to_string).collect();
            let claim: String = log_text(&root, "store", &id).lines().find(|l| l.contains("\"step\":\"claim\"")).unwrap_or_default().to_string();
            let sha = |l: &str| serde_json::from_str::<Value>(l).ok().and_then(|v| v["sha"].as_str().map(str::to_string));
            assert!(won.iter().any(|w| sha(w) == sha(&claim)), "{step}: won with its own claim: {won:?}");
        } else {
            let t = borg(&root, &app2, "s2", &["handoff", "take", &id]);
            assert_eq!(t.code, Some(0), "{step}: the offer stayed open: {}{}", t.out, t.err);
        }
        assert!(refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")})), "{step}: the original is fenced once the take is done");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A prompt of the session whose take was interrupted finishes it in the background: nobody waits.
#[test]
fn a_prompt_of_the_session_finishes_its_interrupted_take() {
    let root = setup("crash-hook");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    let r = borg_in(&root, "store", &app2, Some("s2"), &["handoff", "take", &id], &[("BORG_TEST_HANDOFF_STOP", "won")]);
    assert_eq!(r.code, Some(3), "{}{}", r.out, r.err);
    hook(&root, "store", "prompt", &json!({"session_id": "s2", "cwd": app2, "prompt": "go on"}));
    // Liveness only: the detached run ends in well under this; the loop ends on the file, not a margin.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while steps(&root, &id).last().map(String::as_str) != Some("done") {
        assert!(std::time::Instant::now() < until, "the background run never finished: {:?}", steps(&root, &id));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(git(&app2, &["branch", "--show-current"]), format!("borg/handoff/{id}"));
    let _ = std::fs::remove_dir_all(&root);
}

/// An original off the mesh: no line of the take ever reaches its machine. Its next stop starts a
/// `borg handoff` nobody waits for, which asks the remote, finds the claim, writes its outcome line and
/// fences the original; the stop itself answered at once.
#[test]
fn an_original_off_the_mesh_learns_of_the_take_at_its_stop() {
    let root = setup("offmesh");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    let t = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    let edit = || refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")}));
    assert!(!edit(), "nothing reached A yet");
    hook(&root, "store", "stop", &json!({"session_id": "s1", "cwd": app, "hook_event_name": "Stop"}));
    // The background run's effect is the fence file: wait for it (a liveness bound only).
    let fences = store_of(&root, "store").join("handoff/fences.jsonl");
    let mut waited = 0;
    while !fences.exists() {
        assert!(waited < 6000, "the stop's background run never fenced");
        std::thread::sleep(std::time::Duration::from_millis(10));
        waited += 1;
    }
    assert!(edit(), "fenced from the remote's answer");
    let log = std::fs::read_to_string(store_of(&root, "store").join("handoffs.jsonl")).unwrap();
    assert!(log.lines().any(|l| l.contains("\"kind\":\"taken\"") && l.contains("\"session\":\"s2\"")), "{log}");
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains("by bob@y") && listed.out.contains("the remote holds it"), "{}", listed.out);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------------------------------
// Several repos, refresh, abort, the switch, done

/// Waits until `done` holds: a liveness bound only, the loop ends on the condition.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !done() {
        assert!(std::time::Instant::now() < until, "never: {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// What the bare remote `bare` holds at `name` (empty: nothing).
fn at(root: &Path, bare: &str, name: &str) -> String {
    let out = Command::new("git").arg("-C").arg(root.join(bare)).args(["rev-parse", "--verify", "--quiet", name]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A second repo alice works in: the bare remote `lib.git` and her clone `work/lib`, with a commit.
fn add_lib(root: &Path) -> PathBuf {
    git(root, &["init", "-q", "--bare", "-b", "main", "lib.git"]);
    let lib = root.join("work/lib");
    git(root, &["init", "-q", "-b", "main", &lib.to_string_lossy()]);
    identity(&lib, "alice@x");
    std::fs::write(lib.join("api.rs"), "pub fn api() {}\n").unwrap();
    git(&lib, &["add", "-A"]);
    git(&lib, &["commit", "-q", "-m", "lib"]);
    git(&lib, &["remote", "add", "origin", &root.join("lib.git").to_string_lossy()]);
    git(&lib, &["push", "-q", "-u", "origin", "main"]);
    lib
}

/// `session` edited `path` on `store`: the hook's post_tool of an Edit, as the harness sends it.
fn edited(root: &Path, store: &str, session: &str, path: &Path) {
    let ev = json!({"session_id": session, "cwd": path.parent().unwrap(), "tool_name": "Edit", "tool_input": {"file_path": path}});
    hook(root, store, "post_tool", &ev);
}

/// A session's first hook in `cwd` on `store`: its clone is recorded there (handoff/clones.rs).
fn first_hook(root: &Path, store: &str, session: &str, cwd: &Path) {
    hook(root, store, "prompt", &json!({"session_id": session, "cwd": cwd, "prompt": "hello"}));
}

/// The membership the mesh of `store` saved (borg reads it as saved, the mesh verified it): `members`
/// with their roles, hand-offs switched off by `off` when given; `node`, when given, the store's node
/// key (written before the store makes any line).
fn membership(root: &Path, store: &str, node: Option<&str>, members: &[(&str, &str)], off: Option<&str>) {
    let mesh = store_of(root, store).join("mesh");
    std::fs::create_dir_all(&mesh).unwrap();
    if let Some(n) = node {
        std::fs::write(mesh.join("node.pub"), n).unwrap();
    }
    let members: Vec<Value> = members.iter().map(|(k, r)| json!({"key": k, "role": r})).collect();
    let mut doc = json!({"collective": "corp", "admin": "a".repeat(64), "version": 2, "members": members, "sig": ""});
    if let Some(by) = off {
        doc["handoffs_off"] = json!({"by": by, "at_ms": 1});
    }
    std::fs::write(mesh.join("membership.json"), doc.to_string()).unwrap();
}

/// The claim the remote's `taken` ref of `id` holds, and the state it took (its parent).
fn claimed(root: &Path, id: &str) -> (String, String) {
    (at(root, "remote.git", &format!("refs/borg/handoff/{id}/taken")), at(root, "remote.git", &format!("refs/borg/handoff/{id}/taken^")))
}

/// A session whose work spans two repos hands off both: the arbiter's state and the other's, each
/// pushed to its own remote and named in the brief. A taker with a clone of only one gets it checked
/// out and the other's clone URL; once it clones the other, the same take checks that out too, its
/// tree the offering one.
#[test]
fn a_session_in_two_repos_hands_off_both_and_a_missing_clone_is_listed() {
    let root = setup("tworepos");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    let lib = add_lib(&root);
    work_in_progress(&app);
    std::fs::write(lib.join("api.rs"), "pub fn api() { todo!() }\n").unwrap();
    std::fs::write(lib.join("new.rs"), "pub fn new() {}\n").unwrap();
    edited(&root, "store", "s1", &lib.join("api.rs"));
    // A repo the session only read stays out, even with work of someone else's in it.
    let other = root.join("work/other");
    std::fs::write(other.join("README"), "someone's notes\n").unwrap();
    let read = json!({"session_id": "s1", "cwd": app, "tool_name": "Read", "tool_input": {"file_path": other.join("README")}});
    hook(&root, "store", "post_tool", &read);
    let cat = json!({"session_id": "s1", "cwd": other, "tool_name": "Bash", "tool_input": {"command": "cat README"}});
    hook(&root, "store", "post_tool", &cat);
    let r = offer(&root, &app, "s1", "app and lib");
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    let id = id_in(&r.out);
    assert!(r.out.contains("and lib (main at") && !r.out.contains("and other"), "{}", r.out);
    let lib_state = at(&root, "lib.git", &format!("refs/borg/handoff/{id}/state"));
    assert!(!lib_state.is_empty() && at(&root, "lib.git", &format!("refs/borg/handoff/{id}/brief")).is_empty(), "the brief on the arbiter only");
    // The taker's machine: a store of its own, which knows no clone of lib, and whose collective's paths
    // hold none (this test's machines share one disk: the offering lib is not the taker's).
    carry(&root, "b");
    let cfg_b = root.join("collectives-b.json");
    let paths = [root.join("work/app2"), root.join("work/lib2")];
    std::fs::write(&cfg_b, json!({"default": "off", "collectives": [{"name": "corp", "paths": paths, "share": "team"}]}).to_string()).unwrap();
    let on_b = [("BORG_CONFIG", cfg_b.to_str().unwrap())];
    let t = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &on_b);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert!(t.out.contains(&format!("lib: no clone of it here; `git clone {}`", root.join("lib.git").display())), "{}", t.out);
    assert_eq!(git(&app2, &["branch", "--show-current"]), format!("borg/handoff/{id}"));
    // The claim names the state of each repo it took.
    let (claim, _) = claimed(&root, &id);
    let message = git(&root.join("remote.git"), &["cat-file", "commit", &claim]);
    assert!(message.contains(&format!("states: {} {lib_state}", at(&root, "remote.git", &format!("refs/borg/handoff/{id}/state")))), "{message}");
    git(&root.join("work"), &["clone", "-q", &root.join("lib.git").to_string_lossy(), "lib2"]);
    let lib2 = root.join("work/lib2");
    first_hook(&root, "b", "s2-lib", &lib2);
    let again = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &on_b);
    assert_eq!(again.code, Some(0), "{}{}", again.out, again.err);
    assert!(again.out.contains(&format!("lib: branch borg/handoff/{id} in {}", lib2.display())) && !again.out.contains("no clone"), "{}", again.out);
    assert_eq!(files(&lib2, &[]), files(&lib, &[]), "the taker's lib is the offering lib");
    borg(&root, &app, "s1", &["handoff"]);
    assert!(refused(&root, "s1", &lib, "Edit", json!({"file_path": lib.join("api.rs")})), "the original is fenced in both repos");
    let _ = std::fs::remove_dir_all(&root);
}

/// The taker's clone was never opened in a session, its remote spelled another way than the offer's,
/// and beside it sits a retired repo copied from the same history (one root commit, as a successor repo
/// and the one it replaced share): the take finds the clone in the collective's paths and proves it by
/// its remote holding the hand-off, and never touches the retired one. An offer without root commits (as
/// borg 0.8.0 wrote them) leaves only the clone the take runs in to ask.
#[test]
fn a_take_finds_an_unopened_clone_by_its_remote_and_never_the_retired_one() {
    let root = setup("unopened");
    let app = root.join("work/app");
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    let line = std::fs::read_to_string(store_of(&root, "store").join("handoffs.jsonl")).unwrap();
    assert!(line.contains("\"roots\":["), "the offer names its history: {line}");
    // The taker's machine: its own paths, a retired repo of the same history, and the current one cloned
    // through another spelling of its URL (a link to the same remote), neither opened in a session.
    git(&root, &["clone", "-q", "--bare", &root.join("remote.git").to_string_lossy(), "retired.git"]);
    std::os::unix::fs::symlink(&root, root.join("link")).unwrap();
    let takers = root.join("takers");
    std::fs::create_dir_all(&takers).unwrap();
    git(&takers, &["clone", "-q", &root.join("retired.git").to_string_lossy(), "old-app"]);
    git(&takers, &["clone", "-q", &root.join("link/remote.git").to_string_lossy(), "new-app"]);
    let (retired, current) = (takers.join("old-app"), takers.join("new-app"));
    let cfg_b = root.join("collectives-b.json");
    std::fs::write(&cfg_b, json!({"default": "off", "collectives": [{"name": "corp", "paths": [&takers], "share": "team"}]}).to_string()).unwrap();
    let on_b = [("BORG_CONFIG", cfg_b.to_str().unwrap())];
    // An offer of 0.8.0 names no root commits: from outside any clone, nobody is asked.
    let mut old: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    old["repos"][0].as_object_mut().unwrap().remove("roots");
    std::fs::create_dir_all(store_of(&root, "c")).unwrap();
    std::fs::write(store_of(&root, "c").join("handoffs.jsonl"), format!("{old}\n")).unwrap();
    let t = borg_in(&root, "c", &takers, Some("s3"), &["handoff", "take", &id], &on_b);
    assert!(t.code != Some(0) && t.err.contains("no clone of app") && !t.err.contains("asked"), "{}{}", t.out, t.err);
    // With them, the clones of its history are asked, and the one whose remote holds it takes it.
    carry(&root, "b");
    let t = borg_in(&root, "b", &takers, Some("s2"), &["handoff", "take", &id], &on_b);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert_eq!(git(&current, &["branch", "--show-current"]), format!("borg/handoff/{id}"), "{}", t.out);
    assert_eq!(std::fs::read_to_string(current.join("lib.rs")).unwrap(), std::fs::read_to_string(app.join("lib.rs")).unwrap());
    assert_eq!(git(&retired, &["branch", "--list", &format!("borg/handoff/{id}")]), "", "the retired repo is untouched");
    assert_eq!(git(&root.join("retired.git"), &["for-each-ref", "refs/borg"]), "", "and its remote too");
    let _ = std::fs::remove_dir_all(&root);
}

/// The owner's stop refreshes an open offer: its working tree changed, so the state and the brief
/// move together (the brief on the new state), a new offer line names them, and a take gets the work
/// as it is now.
#[test]
fn a_stop_refreshes_the_open_offer_state_and_brief_together() {
    let root = setup("refresh");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    let (state, brief) = (format!("refs/borg/handoff/{id}/state"), format!("refs/borg/handoff/{id}/brief"));
    let (s1, b1) = (at(&root, "remote.git", &state), at(&root, "remote.git", &brief));
    // A stop with nothing changed moves nothing.
    std::fs::write(app.join("lib.rs"), "fn a() { b(); c() }\n").unwrap();
    hook(&root, "store", "stop", &json!({"session_id": "s1", "cwd": app, "hook_event_name": "Stop"}));
    wait_for("the refresh at the stop", || at(&root, "remote.git", &state) != s1);
    let (s2, b2) = (at(&root, "remote.git", &state), at(&root, "remote.git", &brief));
    wait_for("the brief with it", || at(&root, "remote.git", &brief) != b1);
    assert_eq!(at(&root, "remote.git", &format!("{brief}^")), at(&root, "remote.git", &state), "the brief sits on the new state");
    assert_eq!(git(&root.join("remote.git"), &["cat-file", "blob", &format!("{b2}:HANDOFF.md")]), BRIEF.trim_end(), "the same brief");
    let log = store_of(&root, "store").join("handoffs.jsonl");
    wait_for("the new offer line", || std::fs::read_to_string(&log).unwrap().contains(&s2));
    let t = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert_eq!(std::fs::read_to_string(app2.join("lib.rs")).unwrap(), "fn a() { b(); c() }\n", "the work as it was at the last stop");
    assert_eq!(claimed(&root, &id).1, s2);
    let _ = std::fs::remove_dir_all(&root);
}

/// The owner's refresh races a take, 20 times: whichever comes first, the claim names a state the
/// taker holds and checks out, the remote's state ref ends as the claim names it (a refresh that lost
/// is put back), and the owner learns which of its changes came after the state taken.
#[test]
fn a_refresh_racing_a_take_never_moves_the_claimed_state() {
    let root = setup("refreshrace");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    let (mut taken_before, mut taken_after, rounds) = (0, 0, 20);
    for round in 0..rounds {
        let session = format!("s{round}");
        std::fs::write(app.join("lib.rs"), format!("fn a() {{ {round} }}\n")).unwrap();
        let id = id_in(&offer(&root, &app, &session, "round").out);
        carry(&root, "storeB");
        let edit = format!("fn a() {{ {round} + 1 }}\n");
        std::fs::write(app.join("lib.rs"), &edit).unwrap();
        let (r1, s1) = (root.clone(), session.clone());
        let refresh = std::thread::spawn(move || borg_in(&r1, "store", &r1.join("work/app"), Some(&s1), &["handoff"], &[]));
        let (r2, id2) = (root.clone(), id.clone());
        let take = std::thread::spawn(move || borg_in(&r2, "storeB", &r2.join("work/app2"), Some("b1"), &["handoff", "take", &id2], &[]));
        let (refresh, take) = (refresh.join().unwrap(), take.join().unwrap());
        assert_eq!((refresh.code, take.code), (Some(0), Some(0)), "round {round}: {}{} / {}{}", refresh.out, refresh.err, take.out, take.err);
        let (_, state) = claimed(&root, &id);
        assert_eq!(at(&root, "remote.git", &format!("refs/borg/handoff/{id}/state")), state, "round {round}: the remote's state is the claim's");
        assert_eq!(git(&app2, &["rev-parse", "HEAD^{tree}"]), git(&root.join("remote.git"), &["rev-parse", &format!("{state}^{{tree}}")]), "round {round}");
        let got = std::fs::read_to_string(app2.join("lib.rs")).unwrap();
        let listed = borg(&root, &app, &session, &["handoff"]);
        if got == edit {
            taken_after += 1;
        } else {
            taken_before += 1;
            assert!(
                listed.out.contains("after the state taken, not in the take: M lib.rs"),
                "round {round}: the owner learns its later change: {}",
                listed.out
            );
        }
        git(&app2, &["checkout", "-q", "main"]);
    }
    eprintln!("{rounds} rounds: the take got the refreshed state {taken_after} times, the state before it {taken_before} times");
    let _ = std::fs::remove_dir_all(&root);
}

/// The original learns which of its changes came after the state taken: off the mesh it kept editing
/// before it knew; its next `borg handoff` asks the remote, and the listing and the refusal name them.
#[test]
fn the_original_is_told_its_changes_after_the_state_taken() {
    let root = setup("after");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    assert_eq!(borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[]).code, Some(0));
    std::fs::write(app.join("later.rs"), "fn later() {}\n").unwrap();
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains("changes of the offering tree after the state taken, not in the take: A later.rs"), "{}", listed.out);
    let ev = json!({"session_id": "s1", "cwd": app, "tool_name": "Edit", "tool_input": {"file_path": app.join("lib.rs")}});
    let why = hook(&root, "store", "pre_tool", &ev)["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap_or_default().to_string();
    assert!(why.contains("not in the take: A later.rs"), "{why}");
    let _ = std::fs::remove_dir_all(&root);
}

/// An admin's abort races a take through the same `taken` ref, 20 times: exactly one wins. An abort
/// that wins deletes the work refs, keeps its claim as the record, and fences nobody.
#[test]
fn an_abort_racing_a_take_has_exactly_one_winner() {
    let root = setup("abortrace");
    let app = root.join("work/app");
    git(&root.join("work"), &["clone", "-q", &root.join("remote.git").to_string_lossy(), "app3"]);
    identity(&root.join("work/app3"), "carol@z");
    let admin = "c".repeat(64);
    membership(&root, "storeC", Some(&admin), &[(&admin, "admin")], None);
    let (mut aborts, rounds) = (0, 20);
    for round in 0..rounds {
        std::fs::write(app.join("lib.rs"), format!("fn a() {{ {round} }}\n")).unwrap();
        let id = id_in(&offer(&root, &app, "s1", "round").out);
        carry(&root, "storeB");
        carry(&root, "storeC");
        let run = |store: &'static str, clone: &'static str, session: &'static str, args: Vec<String>| {
            let root = root.clone();
            std::thread::spawn(move || borg_in(&root, store, &root.join(clone), Some(session), &args.iter().map(String::as_str).collect::<Vec<_>>(), &[]))
        };
        let take = run("storeB", "work/app2", "b1", vec!["handoff".into(), "take".into(), id.clone()]);
        let abort = run("storeC", "work/app3", "c1", vec!["handoff".into(), "abort".into(), id.clone(), "--why".into(), "release freeze".into()]);
        let (take, abort) = (take.join().unwrap(), abort.join().unwrap());
        assert_eq!(
            usize::from(take.code == Some(0)) + usize::from(abort.code == Some(0)),
            1,
            "round {round}: {}{} / {}{}",
            take.out,
            take.err,
            abort.out,
            abort.err
        );
        let state = at(&root, "remote.git", &format!("refs/borg/handoff/{id}/state"));
        if abort.code == Some(0) {
            aborts += 1;
            assert!(take.err.contains("aborted by"), "round {round}: {}", take.err);
            assert!(state.is_empty() && !claimed(&root, &id).0.is_empty(), "round {round}: work refs gone, the claim kept");
            assert!(!refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")})), "round {round}: the owner keeps the work");
        } else {
            assert!(abort.err.contains("was taken by"), "round {round}: {}", abort.err);
            git(&root.join("work/app2"), &["checkout", "-q", "main"]);
        }
    }
    eprintln!("{rounds} rounds, exactly one winner each; the abort won {aborts}");
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(aborts == 0 || listed.out.contains("aborted") && listed.out.contains("release freeze"), "{}", listed.out);
    let _ = std::fs::remove_dir_all(&root);
}

/// An abort after the take: the take's claim stands, the abort is the hand-off's second register;
/// both sides learn of it and their edits on the hand-off are refused (the taker's on its branch only),
/// nothing is deleted; `done` resolves it and lifts both fences.
#[test]
fn an_abort_after_the_take_fences_both_sides_until_done() {
    let root = setup("abortafter");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    assert_eq!(borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[]).code, Some(0));
    let mine = borg(&root, &app, "s1", &["handoff", "abort", &id, "--why", "wrong base branch"]);
    assert_eq!(mine.code, Some(0), "{}{}", mine.out, mine.err);
    assert!(mine.out.contains("after its take") && !at(&root, "remote.git", &format!("refs/borg/handoff/{id}/abort")).is_empty(), "{}", mine.out);
    let again = borg(&root, &app, "s1", &["handoff", "abort", &id]);
    assert!(again.code == Some(1) && again.err.contains("aborted after its take already"), "{}", again.err);
    let refused_in = |store: &str, session: &str, cwd: &Path| {
        let ev = json!({"session_id": session, "cwd": cwd, "tool_name": "Edit", "tool_input": {"file_path": cwd.join("lib.rs")}});
        hook(&root, store, "pre_tool", &ev)["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap_or_default().to_string()
    };
    assert!(refused_in("store", "s1", &app).contains("aborted after its take"), "the original");
    // The taker learns it from the remote at its next `borg handoff`.
    let listed = borg_in(&root, "b", &app2, Some("s2"), &["handoff"], &[]);
    assert!(listed.out.contains("aborted after its take by alice@x") && listed.out.contains("wrong base branch"), "{}", listed.out);
    assert!(refused_in("b", "s2", &app2).contains("wrong base branch"), "the taker, on the hand-off's branch");
    assert_eq!(std::fs::read_to_string(app2.join("lib.rs")).unwrap(), "fn a() { b() }\n", "nothing deleted");
    // Checked out in place: the taker may leave the branch, and off it edits again.
    let leave = |cmd: &str| {
        let ev = json!({"session_id": "s2", "cwd": app2, "tool_name": "Bash", "tool_input": {"command": cmd}});
        hook(&root, "b", "pre_tool", &ev)["hookSpecificOutput"]["permissionDecision"] != "deny"
    };
    assert!(leave("git switch main") && !leave("git checkout lib.rs"), "leaving passes, restoring a file does not");
    git(&app2, &["switch", "-q", "main"]);
    assert!(refused_in("b", "s2", &app2).is_empty(), "off the branch the taker edits");
    git(&app2, &["switch", "-q", &format!("borg/handoff/{id}")]);
    assert!(refused_in("b", "s2", &app2).contains("wrong base branch"), "back on it, fenced again");
    let done = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "done", &id], &[]);
    assert_eq!(done.code, Some(0), "{}{}", done.out, done.err);
    assert!(refused_in("b", "s2", &app2).is_empty(), "resolved: the taker edits again");
    assert_eq!(remote_refs(&root), "", "every ref deleted");
    borg(&root, &app, "s1", &["handoff"]);
    assert!(refused_in("store", "s1", &app).is_empty(), "and the original");
    let _ = std::fs::remove_dir_all(&root);
}

/// Only the offering engineer, an admin or a steerer may abort: a plain member is refused with the
/// roles that may, and so is a node without a membership; nothing moves on the remote.
#[test]
fn a_member_without_admin_or_steer_may_not_abort_another_s_hand_off() {
    let root = setup("abortrole");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    let before = remote_refs(&root);
    let r = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "abort", &id], &[]);
    assert!(r.code == Some(1) && r.err.contains("admin or steer role") && r.err.contains("no membership"), "{}", r.err);
    let bob = "b".repeat(64);
    membership(&root, "b", Some(&bob), &[(&bob, "member")], None);
    let r = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "abort", &id], &[]);
    assert!(r.code == Some(1) && r.err.contains("admin or steer role") && r.err.contains("role member may not steer"), "{}", r.err);
    assert_eq!(remote_refs(&root), before, "nothing moved");
    membership(&root, "b", None, &[(&bob, "steer")], None);
    let r = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "abort", &id, "--why", "duplicate"], &[]);
    assert_eq!(r.code, Some(0), "a steerer may: {}{}", r.out, r.err);
    let _ = std::fs::remove_dir_all(&root);
}

/// The collective's switch: while hand-offs are off, a take and an offer are refused naming the admin,
/// each node aborts its own open offers at its next `borg handoff`, a taken one stays its taker's;
/// switched on again nothing reopens, and a new offer works.
#[test]
fn switching_hand_offs_off_aborts_open_offers_and_refuses_new_ones() {
    let root = setup("switch");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let open = id_in(&offer(&root, &app, "s1", "open one").out);
    let taken = id_in(&offer(&root, &app, "s2", "taken one").out);
    let asked = id_in(&offer(&root, &app, "s3", "third").out);
    carry(&root, "b");
    assert_eq!(borg_in(&root, "b", &app2, Some("b1"), &["handoff", "take", &taken], &[]).code, Some(0));
    git(&app2, &["checkout", "-q", "main"]);
    let admin = "a".repeat(64);
    membership(&root, "b", None, &[], Some(&admin));
    let r = borg_in(&root, "b", &app2, Some("b1"), &["handoff", "take", &asked], &[]);
    assert!(r.code == Some(1) && r.err.contains("switched off in this collective by admin aaaaaaaaaaaa"), "{}", r.err);
    membership(&root, "store", None, &[], Some(&admin));
    let listed = borg(&root, &app, "s9", &["handoff"]);
    assert!(listed.out.contains("hand-offs are switched off"), "{}", listed.out);
    for id in [&open, &asked] {
        assert!(listed.out.lines().any(|l| l.starts_with(id.as_str()) && l.contains("aborted")), "{id}: {}", listed.out);
        assert!(at(&root, "remote.git", &format!("refs/borg/handoff/{id}/state")).is_empty(), "{id}: its work refs deleted");
    }
    assert!(listed.out.lines().any(|l| l.starts_with(taken.as_str()) && l.contains("taken ")), "{}", listed.out);
    let r = offer(&root, &app, "s4", "while off");
    assert!(r.code == Some(1) && r.err.contains("switched off"), "{}", r.err);
    membership(&root, "store", None, &[], None);
    let listed = borg(&root, &app, "s9", &["handoff"]);
    assert!(listed.out.lines().any(|l| l.starts_with(open.as_str()) && l.contains("aborted")), "nothing reopens: {}", listed.out);
    assert_eq!(offer(&root, &app, "s5", "on again").code, Some(0));
    let _ = std::fs::remove_dir_all(&root);
}

/// `done` deletes every ref of the hand-off, removes the clean work tree the take made and the local
/// refs, lists the branch, and lifts the fence; an open offer is not done; refs the remote holds of an
/// aborted hand-off, or of one this machine never heard of, are listed with what deletes them; `borg
/// doctor` names a branch left from a done hand-off; once it is merged, `borg handoff` deletes it and
/// the local refs a `done` elsewhere left.
#[test]
fn done_deletes_the_refs_and_what_the_take_left() {
    let root = setup("done");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    let r = borg(&root, &app2, "s2", &["handoff", "done", &id]);
    assert!(r.code == Some(1) && r.err.contains("is open"), "{}", r.err);
    std::fs::write(app2.join("scratch.txt"), "dirty\n").unwrap();
    assert_eq!(borg(&root, &app2, "s2", &["handoff", "take", &id]).code, Some(0));
    let wt = root.join(format!("work/app2-{id}"));
    assert!(wt.is_dir() && git(&app2, &["for-each-ref", "refs/borg/handoff/"]).contains(&id));
    assert!(refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")})), "taken: the original is fenced");
    let r = borg(&root, &app2, "s2", &["handoff", "done", &id]);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(r.out.contains(&format!("removed the work tree {}", wt.display())) && r.out.contains(&format!("branch borg/handoff/{id} stays")), "{}", r.out);
    assert!(!wt.exists() && !git(&app2, &["for-each-ref", "refs/borg/handoff/"]).contains(&id), "work tree and local refs gone");
    assert_eq!(remote_refs(&root), "");
    assert!(!refused(&root, "s1", &app, "Edit", json!({"file_path": app.join("lib.rs")})), "done: nobody fenced");
    assert!(!borg(&root, &app, "s1", &["handoff"]).out.contains(&id), "a done hand-off leaves the lists");
    // An aborted one keeps its claim on the remote until `done`; one nobody here knows is listed too.
    let other = id_in(&offer(&root, &app, "s3", "to abort").out);
    assert_eq!(borg(&root, &app, "s3", &["handoff", "abort", &other]).code, Some(0));
    git(&app, &["push", "-q", "origin", "HEAD:refs/borg/handoff/h0123456789ab/state"]);
    let listed = borg(&root, &app, "s3", &["handoff"]);
    assert!(listed.out.contains(&format!("{other}: its refs remain on origin")), "{}", listed.out);
    assert!(listed.out.contains("h0123456789ab: origin holds refs of a hand-off this machine does not know"), "{}", listed.out);
    assert_eq!(borg(&root, &app, "s3", &["handoff", "done", &other]).code, Some(0));
    assert!(!remote_refs(&root).contains(&other));
    first_hook(&root, "store", "s2-doctor", &app2);
    let doctor = || {
        let out = Command::new(env!("CARGO_BIN_EXE_borg"))
            .arg("doctor")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", root.join("home"))
            .env("BORG_UPDATE_CHECK", "off")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("BORG_HOME", root.join("store"))
            .env("BORG_CONFIG", root.join("collectives.json"))
            .current_dir(&app2)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let branch = format!("borg/handoff/{id}");
    let text = doctor();
    assert!(text.contains(&format!("hand-off {id} is done: branch {branch} stays in {}: it holds commits no other branch", app2.display())), "{text}");
    // Merged, and with local refs a `done` elsewhere left: doctor names them, `borg handoff` deletes them.
    git(&app2, &["-c", "user.name=t", "-c", "user.email=t@example.com", "merge", "-q", "--no-edit", &branch]);
    git(&app2, &["update-ref", &format!("refs/borg/handoff/{id}/state"), &branch]);
    // A hand-off unknown here may be an offer this machine never heard of: named, never deleted.
    git(&app2, &["update-ref", "refs/borg/handoff/h0123456789ab/state", &branch]);
    let text = doctor();
    assert!(
        text.contains(&format!("hand-off {id} is done, but {} still holds 2 of its refs or merged branches: `borg handoff` deletes them", app2.display())),
        "{text}"
    );
    assert_eq!(borg(&root, &app2, "s2", &["handoff"]).code, Some(0));
    let left = git(&app2, &["for-each-ref", "--format=%(refname)", "refs/heads/borg/handoff/", "refs/borg/handoff/"]);
    assert_eq!(left, "refs/borg/handoff/h0123456789ab/state", "merged branch and local refs gone, the unknown one's kept");
    let text = doctor();
    assert!(!text.contains(&id), "nothing left to name: {text}");
    assert!(text.contains(&format!("hand-off h0123456789ab is unknown here, but {} still holds its branch or refs", app2.display())), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The snapshot's rules for big things: an untracked file larger than git's big-file threshold and any
/// tracked file is listed, never committed; a submodule goes in as the commit it is on.
#[test]
fn a_large_untracked_file_is_listed_and_a_submodule_goes_by_its_commit() {
    let root = setup("large");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    git(&root, &["init", "-q", "--bare", "-b", "main", "sub.git"]);
    let seed = root.join("seed");
    git(&root, &["init", "-q", "-b", "main", &seed.to_string_lossy()]);
    identity(&seed, "alice@x");
    git(&seed, &["commit", "-q", "--allow-empty", "-m", "sub"]);
    git(&seed, &["push", "-q", &root.join("sub.git").to_string_lossy(), "main"]);
    git(&app, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &root.join("sub.git").to_string_lossy(), "sub"]);
    git(&app, &["commit", "-q", "-m", "sub"]);
    git(&app, &["push", "-q", "origin", "main"]);
    git(&app2, &["pull", "-q"]);
    // The submodule moves on in the offering tree, a commit only it has.
    identity(&app.join("sub"), "alice@x");
    git(&app.join("sub"), &["commit", "-q", "--allow-empty", "-m", "moved"]);
    let moved = git(&app.join("sub"), &["rev-parse", "HEAD"]);
    git(&app, &["config", "core.bigFileThreshold", "1k"]);
    std::fs::write(app.join("data.bin"), vec![7u8; 4096]).unwrap();
    std::fs::write(app.join("small.rs"), "fn s() {}\n").unwrap();
    let r = offer(&root, &app, "s1", "big");
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(r.out.contains("big-file threshold") && r.out.contains("\"data.bin\""), "{}", r.out);
    let id = id_in(&r.out);
    let state = at(&root, "remote.git", &format!("refs/borg/handoff/{id}/state"));
    let tree = git(&root.join("remote.git"), &["ls-tree", &state]);
    assert!(tree.contains(&format!("160000 commit {moved}\tsub")) && !tree.contains("data.bin") && tree.contains("small.rs"), "{tree}");
    let t = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert!(t.out.contains("\"data.bin\"") && !app2.join("data.bin").exists(), "{}", t.out);
    let _ = std::fs::remove_dir_all(&root);
}

/// A refresh that pushed and then finds `taken` holding a claim of the state before it (a taker that
/// fetched before the refresh pushed) puts the state and the brief back to what the claim names; the
/// owner learns its change after that state. The claim here is pushed as such a taker's would be.
#[test]
fn a_refresh_that_lost_to_a_claim_puts_the_claimed_state_back() {
    let root = setup("restore");
    let app = root.join("work/app");
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    let (state, brief, taken) = (format!("refs/borg/handoff/{id}/state"), format!("refs/borg/handoff/{id}/brief"), format!("refs/borg/handoff/{id}/taken"));
    let (s1, b1) = (at(&root, "remote.git", &state), at(&root, "remote.git", &brief));
    std::fs::write(app.join("lib.rs"), "fn a() { later() }\n").unwrap();
    let r = borg_in(&root, "store", &app, Some("s1"), &["handoff"], &[("BORG_TEST_HANDOFF_STOP", "pushed")]);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert_ne!(at(&root, "remote.git", &state), s1, "the refresh pushed");
    // The taker fetched s1 and b1 before that push; its claim lands now.
    let bare = root.join("remote.git");
    let tree = git(&bare, &["rev-parse", &format!("{s1}^{{tree}}")]);
    let message = format!(
        "borg hand-off {id}: take\n\nid: {id}\nverb: take\nnode: {}\nsession: b1\nengineer: bob@y\nharness: claude\nin-place: no\nbrief: {b1}\nstates: {s1}\nnonce: 0011223344556677\n",
        "b".repeat(64)
    );
    let mut c = Command::new("git");
    c.arg("-C").arg(&bare).args(["commit-tree", &tree, "-p", &s1]).env("GIT_AUTHOR_NAME", "bob").env("GIT_AUTHOR_EMAIL", "bob@y");
    let mut child = c.env("GIT_COMMITTER_NAME", "bob").env("GIT_COMMITTER_EMAIL", "bob@y").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(message.as_bytes()).unwrap();
    let claim = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).trim().to_string();
    git(&bare, &["update-ref", &taken, &claim]);
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains(&format!("interrupted {id}: put back")), "{}{}", listed.out, listed.err);
    assert_eq!((at(&root, "remote.git", &state), at(&root, "remote.git", &brief)), (s1, b1), "the claim's state and brief again");
    assert!(listed.out.contains("not in the take: M lib.rs"), "{}", listed.out);
    let _ = std::fs::remove_dir_all(&root);
}

/// A remote that cannot be reached at the take: no claim, the offer stays open, and a take once it
/// answers wins it.
#[test]
fn an_unreachable_remote_at_the_take_claims_nothing() {
    let root = setup("unreachable");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    let url = root.join("remote.git").to_string_lossy().into_owned();
    std::fs::rename(root.join("remote.git"), root.join("away.git")).unwrap();
    let r = borg(&root, &app2, "s2", &["handoff", "take", &id]);
    assert!(r.code == Some(1) && r.err.contains("cannot reach origin") && r.err.contains("nothing claimed; the offer stays open"), "{}", r.err);
    std::fs::rename(root.join("away.git"), &url).unwrap();
    assert!(claimed(&root, &id).0.is_empty(), "no claim");
    assert!(borg(&root, &app, "s1", &["handoff"]).out.lines().any(|l| l.starts_with(id.as_str()) && l.contains("open")));
    assert_eq!(borg(&root, &app2, "s2", &["handoff", "take", &id]).code, Some(0));
    let _ = std::fs::remove_dir_all(&root);
}

/// An abort, a `done` and a refresh interrupted after a step of their logs: the next `borg handoff`
/// decides the abort by its ref and finishes it (or ends it, before its commit was pushed), runs the
/// `done` again, and puts back a refresh that was not read back (then refreshes anew).
#[test]
fn an_interrupted_abort_done_or_refresh_is_finished_or_put_back() {
    let root = setup("crash-close");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let stop = |step: &'static str| [("BORG_TEST_HANDOFF_STOP", step)];
    let early = id_in(&offer(&root, &app, "s1", "early").out);
    assert_eq!(borg_in(&root, "store", &app, Some("s1"), &["handoff", "abort", &early], &stop("abort")).code, Some(3));
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains(&format!("interrupted {early}: ended the abort")), "{}{}", listed.out, listed.err);
    assert!(claimed(&root, &early).0.is_empty(), "nothing pushed: still open");
    let sent = id_in(&offer(&root, &app, "s2", "sent").out);
    assert_eq!(borg_in(&root, "store", &app, Some("s2"), &["handoff", "abort", &sent, "--why", "dup"], &stop("sent")).code, Some(3));
    let listed = borg(&root, &app, "s2", &["handoff"]);
    assert!(listed.out.contains(&format!("interrupted {sent}: aborted hand-off {sent} (dup)")), "{}{}", listed.out, listed.err);
    assert!(at(&root, "remote.git", &format!("refs/borg/handoff/{sent}/state")).is_empty(), "finished: the work refs deleted");
    let taken = id_in(&offer(&root, &app, "s3", "taken").out);
    assert_eq!(borg(&root, &app2, "b1", &["handoff", "take", &taken]).code, Some(0));
    assert_eq!(borg_in(&root, "store", &app2, Some("b1"), &["handoff", "done", &taken], &stop("cleared")).code, Some(3));
    let listed = borg(&root, &app2, "b1", &["handoff"]);
    assert!(listed.out.contains(&format!("interrupted {taken}: hand-off {taken} is done")), "{}{}", listed.out, listed.err);
    assert!(!remote_refs(&root).contains(&taken), "every ref deleted");
    git(&app2, &["checkout", "-q", "main"]);
    let open = id_in(&offer(&root, &app, "s4", "refresh").out);
    let state = format!("refs/borg/handoff/{open}/state");
    let s1 = at(&root, "remote.git", &state);
    std::fs::write(app.join("lib.rs"), "fn a() { refreshed() }\n").unwrap();
    assert_eq!(borg_in(&root, "store", &app, Some("s4"), &["handoff"], &stop("pushed")).code, Some(0));
    let s2 = at(&root, "remote.git", &state);
    assert_ne!(s2, s1, "pushed before the stop");
    // Another session's run puts it back; the offering session's next run refreshes anew.
    let listed = borg(&root, &app, "s9", &["handoff"]);
    assert!(listed.out.contains(&format!("interrupted {open}: put back")), "{}{}", listed.out, listed.err);
    assert_eq!(at(&root, "remote.git", &state), s1, "put back");
    borg(&root, &app, "s4", &["handoff"]);
    assert_ne!(at(&root, "remote.git", &state), s1, "refreshed again");
    let _ = std::fs::remove_dir_all(&root);
}

/// The claim commit a take logged on `store` (its WAL's `claim` step).
fn logged_claim(root: &Path, store: &str, id: &str) -> String {
    log_text(root, store, id)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["step"] == "claim")
        .and_then(|v| v["sha"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("no claim logged on {store}"))
}

/// A refresh of a two-repo offer pushes only what moved and names only what the remotes hold: a run of
/// the offering session after only lib changed, then (another offer) after only the arbiter changed;
/// each time a take checks out both repos as the offering trees are.
#[test]
fn a_refresh_of_one_repo_of_two_names_only_what_the_remotes_hold() {
    let root = setup("refreshtwo");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    let lib = add_lib(&root);
    git(&root.join("work"), &["clone", "-q", &root.join("lib.git").to_string_lossy(), "lib2"]);
    let lib2 = root.join("work/lib2");
    first_hook(&root, "b", "s2-lib", &lib2);
    for (round, session, lib_moves) in [(0, "s1", true), (1, "s3", false)] {
        std::fs::write(app.join("lib.rs"), format!("fn a() {{ {round} }}\n")).unwrap();
        std::fs::write(lib.join("api.rs"), format!("pub fn api() {{ {round} }}\n")).unwrap();
        edited(&root, "store", session, &lib.join("api.rs"));
        let r = offer(&root, &app, session, "two repos");
        assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
        let id = id_in(&r.out);
        let state = format!("refs/borg/handoff/{id}/state");
        let (app_s, lib_s) = (at(&root, "remote.git", &state), at(&root, "lib.git", &state));
        if lib_moves {
            std::fs::write(lib.join("api.rs"), "pub fn api() { refreshed() }\n").unwrap();
        } else {
            std::fs::write(app.join("lib.rs"), "fn a() { refreshed() }\n").unwrap();
        }
        let r = borg(&root, &app, session, &["handoff"]);
        assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
        assert_eq!(at(&root, "remote.git", &state) != app_s, !lib_moves, "round {round}: the arbiter's state moves only when it moved");
        assert_eq!(at(&root, "lib.git", &state) != lib_s, lib_moves, "round {round}: lib's likewise");
        carry(&root, "b");
        let t = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[]);
        assert_eq!(t.code, Some(0), "round {round}: {}{}", t.out, t.err);
        assert!(t.out.contains(&format!("lib: branch borg/handoff/{id} in {}", lib2.display())), "{}", t.out);
        assert_eq!(files(&app2, &[]), files(&app, &[]), "round {round}: the taker's app is the offering app");
        assert_eq!(files(&lib2, &[]), files(&lib, &[]), "round {round}: and its lib the offering lib");
        git(&app2, &["checkout", "-q", "main"]);
        git(&lib2, &["checkout", "-q", "main"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The ordering a refresh cannot see: a take fetched the offer and made its claim; the owner's refresh
/// then pushed, read back, found no claim and published; only then did the claim land. The take puts
/// the remote's state and brief back to what its claim names and checks that out; the owner learns its
/// change after it.
#[test]
fn a_claim_that_lands_after_a_refresh_published_puts_the_claimed_state_back() {
    let root = setup("claimafter");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    let (state, brief) = (format!("refs/borg/handoff/{id}/state"), format!("refs/borg/handoff/{id}/brief"));
    let (s0, b0) = (at(&root, "remote.git", &state), at(&root, "remote.git", &brief));
    let r = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[("BORG_TEST_HANDOFF_STOP", "claim")]);
    assert_eq!(r.code, Some(3), "{}{}", r.out, r.err);
    let claim = logged_claim(&root, "b", &id);
    std::fs::write(app.join("lib.rs"), "fn a() { later() }\n").unwrap();
    assert_eq!(borg(&root, &app, "s1", &["handoff"]).code, Some(0));
    assert_ne!(at(&root, "remote.git", &state), s0, "the refresh published");
    // The claim lands now, as the take's push would have.
    git(&app2, &["push", "-q", "origin", &format!("{claim}:refs/borg/handoff/{id}/taken")]);
    let t = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert_eq!((at(&root, "remote.git", &state), at(&root, "remote.git", &brief)), (s0.clone(), b0), "the remote holds what the claim names");
    assert_eq!(git(&app2, &["rev-parse", "HEAD^{tree}"]), git(&root.join("remote.git"), &["rev-parse", &format!("{s0}^{{tree}}")]));
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains("not in the take: M lib.rs"), "{}", listed.out);
    let _ = std::fs::remove_dir_all(&root);
}

/// The reverse ordering: the refresh pushed and read back its state, a take claimed that state before
/// the refresh asked for `taken` (it stops there), and the run that finishes the refresh finds the claim
/// and leaves the claimed state where it is.
#[test]
fn a_take_between_a_refreshs_read_back_and_its_publish_keeps_the_claimed_state() {
    let root = setup("takebetween");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    let state = format!("refs/borg/handoff/{id}/state");
    let s0 = at(&root, "remote.git", &state);
    std::fs::write(app.join("lib.rs"), "fn a() { refreshed() }\n").unwrap();
    assert_eq!(borg_in(&root, "store", &app, Some("s1"), &["handoff"], &[("BORG_TEST_HANDOFF_STOP", "verified")]).code, Some(0));
    let s1 = at(&root, "remote.git", &state);
    assert_ne!(s1, s0, "the refresh pushed and read back");
    let t = borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[]);
    assert_eq!(t.code, Some(0), "{}{}", t.out, t.err);
    assert_eq!(claimed(&root, &id).1, s1, "the take claimed the refreshed state");
    let listed = borg(&root, &app, "s1", &["handoff"]);
    assert!(listed.out.contains(&format!("interrupted {id}: put back")), "{}{}", listed.out, listed.err);
    assert_eq!(at(&root, "remote.git", &state), s1, "the claimed state stays");
    assert_eq!(std::fs::read_to_string(app2.join("lib.rs")).unwrap(), "fn a() { refreshed() }\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// Two made-up lines of a third node, a take and its done, never close an open offer: a done counts only
/// beside the claim the remote was seen holding; the next `borg handoff` asks the remote, which refutes
/// the take, and the offer stays open and takeable.
#[test]
fn a_made_up_take_and_done_leave_the_offer_open() {
    let root = setup("madeup");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    let fake = "f".repeat(40);
    let taken = json!({"v": 1, "id": id, "kind": "taken", "node": "n3", "session": "x1", "claim": fake, "state": fake});
    let done = json!({"v": 1, "id": id, "kind": "done", "node": "n3", "session": "x1", "claim": fake});
    let log = store_of(&root, "b").join("handoffs.jsonl");
    std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(format!("{taken}\n{done}\n").as_bytes()).unwrap();
    let listed = borg_in(&root, "b", &app2, Some("s2"), &["handoff"], &[]);
    let row = listed.out.lines().find(|l| l.starts_with(id.as_str())).unwrap_or_default();
    assert!(row.contains("open") && listed.out.contains(&format!("take it: borg handoff take {id}")), "{}", listed.out);
    assert_eq!(borg_in(&root, "b", &app2, Some("s2"), &["handoff", "take", &id], &[]).code, Some(0));
    let _ = std::fs::remove_dir_all(&root);
}

/// `done` keeps a work tree that holds ignored files (a local database, an `.env`), which `git worktree
/// remove` would delete, and lists them.
#[test]
fn done_keeps_a_work_tree_that_holds_ignored_files() {
    let root = setup("doneignored");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    std::fs::write(app2.join("scratch.txt"), "dirty\n").unwrap();
    assert_eq!(borg(&root, &app2, "s2", &["handoff", "take", &id]).code, Some(0));
    let wt = root.join(format!("work/app2-{id}"));
    std::fs::create_dir_all(wt.join("build")).unwrap();
    std::fs::write(wt.join("build/local.db"), "rows\n").unwrap();
    let r = borg(&root, &app2, "s2", &["handoff", "done", &id]);
    assert_eq!(r.code, Some(0), "{}{}", r.out, r.err);
    assert!(r.out.contains(&format!("the work tree {} stays: it holds ignored files (build/)", wt.display())), "{}", r.out);
    assert!(wt.join("build/local.db").exists(), "nothing of it deleted");
    let _ = std::fs::remove_dir_all(&root);
}

/// A take that read the offer before another's take and `done` deleted its refs: its claim lands on the
/// emptied register, finds no offer there, is withdrawn, and nothing is checked out.
#[test]
fn a_claim_that_lands_after_done_is_withdrawn() {
    let root = setup("afterdone");
    let (app, app2) = (root.join("work/app"), root.join("work/app2"));
    git(&root.join("work"), &["clone", "-q", &root.join("remote.git").to_string_lossy(), "app3"]);
    let app3 = root.join("work/app3");
    identity(&app3, "carol@z");
    work_in_progress(&app);
    let id = id_in(&offer(&root, &app, "s1", "note").out);
    carry(&root, "b");
    carry(&root, "c");
    assert_eq!(borg_in(&root, "b", &app2, Some("b1"), &["handoff", "take", &id], &[("BORG_TEST_HANDOFF_STOP", "claim")]).code, Some(3));
    let claim = logged_claim(&root, "b", &id);
    assert_eq!(borg_in(&root, "c", &app3, Some("c1"), &["handoff", "take", &id], &[]).code, Some(0));
    assert_eq!(borg_in(&root, "c", &app3, Some("c1"), &["handoff", "done", &id], &[]).code, Some(0));
    assert!(!remote_refs(&root).contains(&id), "done deleted every ref");
    git(&app2, &["push", "-q", "origin", &format!("{claim}:refs/borg/handoff/{id}/taken")]);
    let r = borg_in(&root, "b", &app2, Some("b1"), &["handoff", "take", &id], &[]);
    assert!(r.out.contains(&format!("{id} was closed (done) after this take read it: the claim is withdrawn")), "{}{}", r.out, r.err);
    assert_ne!(r.code, Some(0));
    assert!(!remote_refs(&root).contains(&id), "the late claim is gone again");
    assert_eq!(git(&app2, &["branch", "--show-current"]), "main", "nothing checked out");
    let _ = std::fs::remove_dir_all(&root);
}

/// An admin's `borg handoff off` is refused while a member machine seen in the last day runs a borg
/// without hand-offs: it could not verify the next membership version, and would stop following every
/// later one.
#[test]
fn switching_off_is_refused_while_a_machine_without_hand_offs_is_seen() {
    let root = setup("switchold");
    let app = root.join("work/app");
    let mesh = store_of(&root, "store").join("mesh");
    std::fs::create_dir_all(&mesh).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let p = json!({"session": "o1", "harness": "claude", "engineer": "old@x", "collective": "corp", "event": "prompt", "ms": now, "started": now});
    std::fs::write(mesh.join("presence.jsonl"), format!("{}\n", json!({"p": p, "key": "e".repeat(64), "sig": ""}))).unwrap();
    let r = borg(&root, &app, "s1", &["handoff", "off"]);
    assert!(r.code == Some(1) && r.err.contains("refused") && r.err.contains("old@x") && r.err.contains("revocations"), "{}{}", r.out, r.err);
    let _ = std::fs::remove_dir_all(&root);
}
