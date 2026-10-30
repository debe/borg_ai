//! `borg update` and the prompt hook's release check as processes, against `file://` fixtures: a
//! release feed (`BORG_RELEASES_URL`), a release tarball of stub binaries and its `.sha256`
//! (`BORG_DOWNLOAD_URL`), a stub `cargo` on PATH. The binaries replaced live in a temporary install
//! directory named by `BORG_BIN`, so no test touches a real `~/.cargo/bin` or `~/.local/bin`, and none
//! reaches GitHub.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The release version every fixture serves: newer than any borg this test builds.
const LATEST: &str = "99.0.0";

fn tmp(name: &str) -> PathBuf {
    // Not under target/: a borg there is a development build, which `borg update` never replaces.
    let d = std::env::temp_dir().join(format!("borg-update-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for sub in ["home", "store", "bin", "feed", "dl", "inst"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    d
}

/// The release target scripts/install.sh picks here, as update.rs does; None where no release exists.
fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") if !cfg!(target_env = "musl") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") if !cfg!(target_env = "musl") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A stub binary `name` that logs each run's arguments to `$UPDATE_LOG` as `<name>@<tag> <args>`.
fn stub(path: &Path, name: &str, tag: &str) {
    script(path, &format!("echo \"{name}@{tag} $*\" >> \"$UPDATE_LOG\""));
}

/// The feed says `tag` is the latest release.
fn feed(root: &Path, tag: &str) {
    std::fs::write(root.join("feed/latest.json"), format!(r#"{{"tag_name": "{tag}", "name": "borg {tag}"}}"#)).unwrap();
}

/// The release tarball of `LATEST` for `target` (stub binaries tagged `new`), and its `.sha256`: the
/// right sum, or a wrong one.
fn release(root: &Path, target: &str, right_sum: bool) {
    let name = format!("borg-v{LATEST}-{target}");
    let build = root.join("build").join(&name);
    std::fs::create_dir_all(&build).unwrap();
    for b in ["borg", "borg-mesh", "borg-web"] {
        stub(&build.join(b), b, "new");
    }
    let dir = root.join(format!("dl/v{LATEST}"));
    std::fs::create_dir_all(&dir).unwrap();
    let tgz = dir.join(format!("{name}.tar.gz"));
    assert!(Command::new("tar").arg("-czf").arg(&tgz).arg("-C").arg(root.join("build")).arg(&name).status().unwrap().success());
    let sum = if right_sum { borg::update::sha256_file(&tgz).unwrap() } else { "0".repeat(64) };
    std::fs::write(dir.join(format!("{name}.tar.gz.sha256")), format!("{sum}  {name}.tar.gz\n")).unwrap();
}

/// The installed binaries in `dir`, each a stub tagged `old`.
fn installed(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for b in ["borg", "borg-mesh", "borg-web"] {
        stub(&dir.join(b), b, "old");
    }
}

/// `borg <args>` in a clean environment: the fixtures' feed and downloads, `bin` as the installed borg,
/// the stub `cargo` first on PATH, then `extra`.
fn borg(root: &Path, args: &[&str], bin: &Path, extra: &[(&str, &str)]) -> Output {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_RELEASES_URL", format!("file://{}", root.join("feed/latest.json").display()))
        .env("BORG_DOWNLOAD_URL", format!("file://{}", root.join("dl").display()))
        .env("BORG_BIN", bin)
        .env("CARGO_HOME", root.join("cargo"))
        .env("UPDATE_LOG", root.join("log"))
        .envs(extra.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn log(root: &Path) -> String {
    std::fs::read_to_string(root.join("log")).unwrap_or_default()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn check_exits_by_what_the_feed_says() {
    let root = tmp("check");
    let bin = root.join("inst/borg");
    feed(&root, &format!("v{LATEST}"));
    let out = borg(&root, &["update", "--check"], &bin, &[]);
    assert_eq!(out.status.code(), Some(10), "{}", text(&out));
    let t = text(&out);
    assert!(t.contains(&format!("this borg   {}", env!("CARGO_PKG_VERSION"))) && t.contains(&format!("{LATEST}, newer: borg update")), "{t}");
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("store/update.json")).unwrap()).unwrap();
    assert_eq!(written["latest"], LATEST);

    let out = borg(&root, &["update", "--check", "--json"], &bin, &[]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((v["update"].as_bool(), v["latest"].as_str(), v["version"].as_str()), (Some(true), Some(LATEST), Some(env!("CARGO_PKG_VERSION"))));

    let out = borg(&root, &["update", "--check", "--quiet"], &bin, &[]);
    assert_eq!((out.status.code(), out.stdout.len(), out.stderr.len()), (Some(10), 0, 0), "the hook's form says nothing");

    feed(&root, &format!("v{}", env!("CARGO_PKG_VERSION")));
    assert_eq!(borg(&root, &["update", "--check"], &bin, &[]).status.code(), Some(0), "current");
    let out = borg(&root, &["update"], &bin, &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out).contains("is current"), "{}", text(&out));

    std::fs::remove_file(root.join("feed/latest.json")).unwrap();
    assert_eq!(borg(&root, &["update", "--check"], &bin, &[]).status.code(), Some(1), "the check failed");

    let _ = std::fs::remove_file(root.join("store/update.json"));
    let out = borg(&root, &["update", "--check"], &bin, &[("BORG_UPDATE_CHECK", "off")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("BORG_UPDATE_CHECK=off") && !root.join("store/update.json").exists(), "off: nothing asked, nothing written");
    assert_eq!(borg(&root, &["update", "--json"], &bin, &[]).status.code(), Some(2), "--json goes with --check");
    assert_eq!(borg(&root, &["update", "--now"], &bin, &[]).status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_release_tarball_replaces_all_three_and_refreshes_only_the_wired_harness() {
    let Some(t) = target() else { return };
    let root = tmp("tarball");
    let inst = root.join("inst");
    installed(&inst);
    let bin = inst.join("borg");
    // Claude Code is wired to the installed borg; no other harness is.
    let wired = borg(&root, &["install", "claude", "--apply", "--home", root.join("home").to_str().unwrap()], &bin, &[]);
    assert!(wired.status.success(), "{}", text(&wired));
    feed(&root, &format!("v{LATEST}"));
    release(&root, t, true);
    // An install a signal ended (no process has this pid) left its temporary directory behind.
    std::fs::create_dir_all(inst.join(format!(".borg-update.{}/borg-v{LATEST}-{t}", i32::MAX))).unwrap();

    let out = borg(&root, &["update"], &bin, &[]);
    let said = text(&out);
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert!(said.contains("sha256 ok") && said.contains(&format!("installed borg, borg-mesh and borg-web {LATEST}")), "{said}");
    for b in ["borg", "borg-mesh", "borg-web"] {
        let body = std::fs::read_to_string(inst.join(b)).unwrap();
        assert!(body.contains(&format!("{b}@new")), "{b} was replaced: {body}");
    }
    // The new borg refreshed the wired harness, then ran doctor; nothing else.
    assert_eq!(log(&root), "borg@new install claude --apply\nborg@new doctor\n");
    let left: Vec<_> = std::fs::read_dir(&inst).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left.len(), 3, "no temporary directory left beside the binaries, an earlier one's neither: {left:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_wrong_sha256_changes_nothing() {
    let Some(t) = target() else { return };
    let root = tmp("badsum");
    let inst = root.join("inst");
    installed(&inst);
    feed(&root, &format!("v{LATEST}"));
    release(&root, t, false);
    let out = borg(&root, &["update"], &inst.join("borg"), &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("sha256 mismatch"), "{}", text(&out));
    for b in ["borg", "borg-mesh", "borg-web"] {
        assert!(std::fs::read_to_string(inst.join(b)).unwrap().contains(&format!("{b}@old")), "{b} untouched");
    }
    assert_eq!(std::fs::read_dir(&inst).unwrap().count(), 3, "no temporary directory left");
    assert_eq!(log(&root), "", "nothing ran");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_cargo_install_runs_cargo_for_the_three_crates() {
    let root = tmp("cargo");
    let cargo_bin = root.join("cargo/bin");
    installed(&cargo_bin);
    stub(&root.join("bin/cargo"), "cargo", "stub");
    feed(&root, &format!("v{LATEST}"));
    let out = borg(&root, &["update"], &cargo_bin.join("borg"), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    // `borg-ai` last: a borg on the release means the mesh and web beside it are installed too.
    let want: String = ["borg-mesh", "borg-web", "borg-ai"].iter().map(|c| format!("cargo@stub install {c} --locked --force --version {LATEST}\n")).collect();
    // No harness is wired here: the new borg runs doctor only.
    assert_eq!(log(&root), format!("{want}borg@old doctor\n"));
    let _ = std::fs::remove_dir_all(&root);
}

/// A cargo install that stopped after `borg` left `borg-mesh` old: `borg update` on the current release
/// installs it again instead of saying all is current.
#[test]
fn an_install_that_stopped_part_way_is_completed() {
    let root = tmp("half");
    let cargo_bin = root.join("cargo/bin");
    installed(&cargo_bin);
    let version = env!("CARGO_PKG_VERSION");
    for (b, v) in [("borg-mesh", "0.0.1"), ("borg-web", version)] {
        script(&cargo_bin.join(b), &format!("[ \"$1\" = --version ] && echo \"{b} {v}\" && exit 0\necho \"{b}@old $*\" >> \"$UPDATE_LOG\""));
    }
    stub(&root.join("bin/cargo"), "cargo", "stub");
    feed(&root, &format!("v{version}"));
    let out = borg(&root, &["update"], &cargo_bin.join("borg"), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("borg-mesh beside it is not"), "{}", text(&out));
    assert_eq!(log(&root), format!("cargo@stub install borg-mesh --locked --force --version {version}\nborg@old doctor\n"));

    // All three on the release: current, nothing runs.
    script(&cargo_bin.join("borg-mesh"), &format!("[ \"$1\" = --version ] && echo \"borg-mesh {version}\""));
    std::fs::remove_file(root.join("log")).unwrap();
    let out = borg(&root, &["update"], &cargo_bin.join("borg"), &[]);
    assert!(text(&out).contains("is current") && log(&root).is_empty(), "{} / {}", text(&out), log(&root));
    let _ = std::fs::remove_dir_all(&root);
}

/// `BORG_BIN` may be a symlink (install.rs `bin`): the build it names is what counts, so a symlink to a
/// development build is refused like that build, and the symlink stays.
#[test]
fn a_symlink_to_a_development_build_is_one() {
    if target().is_none() {
        return;
    }
    let root = tmp("symlink");
    let dev = root.join("repo/target/release");
    installed(&dev);
    let link = root.join("inst/borg");
    std::os::unix::fs::symlink(dev.join("borg"), &link).unwrap();
    feed(&root, &format!("v{LATEST}"));
    let out = borg(&root, &["update"], &link, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("development build"), "{}", text(&out));
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the symlink is not replaced");
    assert_eq!(std::fs::read_dir(root.join("inst")).unwrap().count(), 1, "nothing installed beside it");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_development_build_gets_the_commands_and_nothing_changes() {
    let root = tmp("dev");
    let dev = root.join("repo/target/release");
    installed(&dev);
    stub(&root.join("bin/cargo"), "cargo", "stub");
    feed(&root, &format!("v{LATEST}"));
    let out = borg(&root, &["update"], &dev.join("borg"), &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("development build") && said.contains(&format!("cargo install borg-ai --locked --force --version {LATEST}")), "{said}");
    assert_eq!(log(&root), "", "nothing ran");
    assert!(std::fs::read_to_string(dev.join("borg")).unwrap().contains("borg@old"));
    let _ = std::fs::remove_dir_all(&root);
}

/// `borg hook claude prompt` in the store at `root`, with the release check on.
fn prompt(root: &Path, session: &str) -> String {
    let path = std::env::var("PATH").unwrap_or_default();
    let mut c = Command::new(env!("CARGO_BIN_EXE_borg"));
    c.args(["hook", "claude", "prompt"])
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("home"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_RELEASES_URL", format!("file://{}", root.join("feed/latest.json").display()))
        .env("BORG_LEARN", "off")
        .env("BORG_QUEEN", "off")
        .env("BORG_VECTORS", "off")
        .env("BORG_EARLY", "off")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    let input = serde_json::json!({"session_id": session, "cwd": root.join("work"), "prompt": "fix the parser"});
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).to_string();
    serde_json::from_str::<serde_json::Value>(&out)
        .ok()
        .and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from))
        .unwrap_or_default()
}

#[test]
fn a_prompt_starts_the_daily_check_and_tells_the_drone_once_per_session() {
    let root = tmp("hook");
    std::fs::create_dir_all(root.join("work")).unwrap();
    feed(&root, &format!("v{LATEST}"));
    // No update.json yet: the prompt starts the check in the background and says nothing yet.
    assert_eq!(prompt(&root, "s1"), "");
    let update = root.join("store/update.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !update.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let written = std::fs::read_to_string(&update).expect("the detached check wrote update.json");
    assert!(written.contains(LATEST), "{written}");
    // The next prompt tells the drone, once; another session hears it once too.
    let said = prompt(&root, "s1");
    assert!(said.contains(&format!("borg {LATEST} is released")) && said.contains("offer to run `borg update`"), "{said}");
    assert_eq!(prompt(&root, "s1"), "", "once per session");
    assert!(prompt(&root, "s2").contains("is released"));
    // A fresh check is not repeated: the marker of a started check is gone, and no second one starts.
    assert!(!root.join("store/update.checking").exists());
    let _ = std::fs::remove_dir_all(&root);
}
