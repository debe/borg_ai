//! `borg service` and the paths that install services (`borg install <h> --apply`, as an older borg's
//! update runs it on the new binary) as processes, against a stub `systemctl` (Linux) or `launchctl`
//! (macOS) first on PATH that logs each call and keeps the state a manager would. HOME, the store and
//! the config are temporary: no test reaches a real unit directory or service manager.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const LAUNCHD: bool = cfg!(target_os = "macos");

fn supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

const SYSTEMCTL: &str = r#"echo "systemctl $*" >> "$SERVICE_LOG"
[ "$1" = --user ] && shift
verb=$1; shift
case "$verb" in
  show-environment)
    [ -f "$SERVICE_STATE/nobus" ] && { echo "Failed to connect to bus: No medium found" >&2; exit 1; }
    echo "HOME=$HOME" ;;
  show)
    for u in "$@"; do
      case "$u" in -*) continue ;; esac
      a=inactive; [ -f "$SERVICE_STATE/$u.active" ] && a=active
      e=disabled; [ -f "$SERVICE_STATE/$u.enabled" ] && e=enabled
      printf 'Id=%s\nActiveState=%s\nUnitFileState=%s\nMainPID=0\n\n' "$u" "$a" "$e"
    done ;;
  enable) for u in "$@"; do touch "$SERVICE_STATE/$u.enabled"; done ;;
  start|restart) for u in "$@"; do touch "$SERVICE_STATE/$u.active"; done ;;
  disable) for u in "$@"; do case "$u" in -*) continue ;; esac; rm -f "$SERVICE_STATE/$u.enabled" "$SERVICE_STATE/$u.active"; done ;;
esac"#;

const LAUNCHCTL: &str = r#"echo "launchctl $*" >> "$SERVICE_LOG"
case "$1" in
  managername) echo Aqua ;;
  print) [ -f "$SERVICE_STATE/${2##*/}.loaded" ] || exit 113; printf '\tstate = running\n' ;;
  bootstrap) touch "$SERVICE_STATE/$(basename "$3" .plist).loaded" ;;
  bootout) rm -f "$SERVICE_STATE/${2##*/}.loaded" ;;
esac"#;

/// A machine: a team collective `corp` with a mesh identity and a local one `mine`, an installed
/// borg (a stub file: only its path is written into the units), the stub managers.
fn machine(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("borg-service-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["home", "bin", "inst", "state", "run", "work", "mine"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let cfg = serde_json::json!({"default": "off", "collectives": [
        {"name": "corp", "paths": [root.join("work")]},
        {"name": "mine", "paths": [root.join("mine")], "share": "local"},
    ]});
    std::fs::write(root.join("collectives.json"), cfg.to_string()).unwrap();
    let key = root.join("store/collectives/corp/mesh/node.key");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(key, "00").unwrap();
    script(&root.join("inst/borg"), "exit 0");
    script(&root.join("bin/systemctl"), SYSTEMCTL);
    script(&root.join("bin/launchctl"), LAUNCHCTL);
    root
}

/// `borg <args>` on `root`'s machine, `bin` the installed borg.
fn borg_as(root: &Path, args: &[&str], bin: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_borg"))
        .args(args)
        .env_clear()
        .env("BORG_ASYNC", "off")
        .env("BORG_UPDATE_CHECK", "off")
        .env("BORG_VECTORS", "off")
        .env("PATH", format!("{}:/usr/bin:/bin", root.join("bin").display()))
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("BORG_HOME", root.join("store"))
        .env("BORG_CONFIG", root.join("collectives.json"))
        .env("BORG_BIN", bin)
        .env("SERVICE_LOG", root.join("log"))
        .env("SERVICE_STATE", root.join("state"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn borg(root: &Path, args: &[&str]) -> Output {
    borg_as(root, args, &root.join("inst/borg"))
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The unit directory and each service's file name.
fn units(root: &Path) -> (PathBuf, &'static str, &'static str) {
    match LAUNCHD {
        true => (root.join("home/Library/LaunchAgents"), "borg.mesh.corp.plist", "borg.web.plist"),
        false => (root.join("home/.config/systemd/user"), "borg-mesh-corp.service", "borg-web.service"),
    }
}

/// The manager calls that change something (queries left out), and the log cleared.
fn changes(root: &Path) -> Vec<String> {
    let log = std::fs::read_to_string(root.join("log")).unwrap_or_default();
    let _ = std::fs::remove_file(root.join("log"));
    let reads = ["show-environment", "--user show ", "managername", "launchctl print "];
    log.lines().filter(|l| !reads.iter().any(|r| l.contains(r))).map(str::to_string).collect()
}

/// The `services.json` borg keeps.
fn record(root: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(root.join("store/services.json")).unwrap_or_else(|_| "null".into())).unwrap()
}

#[test]
fn install_takes_over_a_hand_written_unit_then_does_nothing_and_restarts_on_another_version() {
    if !supported() {
        return;
    }
    let root = machine("takeover");
    let (dir, mesh, web) = units(&root);
    std::fs::create_dir_all(&dir).unwrap();
    let mine = "[Service]\nExecStart=%h/.cargo/bin/borg web\nEnvironment=PATH=/home/ann/bin:/usr/bin\n";
    std::fs::write(dir.join(web), mine).unwrap();
    // It runs and is enabled, as the engineer left it.
    let label = web.trim_end_matches(".plist");
    for f in [format!("{web}.active"), format!("{web}.enabled"), format!("{label}.loaded")] {
        std::fs::write(root.join("state").join(f), "").unwrap();
    }

    let out = borg(&root, &["service", "install"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("took it over; yours is kept at"), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(dir.join(format!("{web}.bak"))).unwrap(), mine, "the engineer's unit kept");
    for f in [mesh, web] {
        let t = std::fs::read_to_string(dir.join(f)).unwrap();
        assert!(t.contains(&format!("managed by borg {}", env!("CARGO_PKG_VERSION"))) && t.contains(&root.join("inst/borg").display().to_string()), "{f}: {t}");
    }
    assert!(!dir.join(if LAUNCHD { "borg.mesh.mine.plist" } else { "borg-mesh-mine.service" }).exists(), "a local collective has no mesh daemon");
    let did = changes(&root);
    if LAUNCHD {
        assert_eq!(did.len(), 3, "{did:?}");
        assert!(did[0].starts_with("launchctl bootstrap gui/") && did[0].ends_with(mesh), "{did:?}");
        assert!(did[1].starts_with("launchctl bootout gui/") && did[1].ends_with("/borg.web"), "{did:?}");
        assert!(did[2].starts_with("launchctl bootstrap gui/") && did[2].ends_with(web), "{did:?}");
    } else {
        assert_eq!(
            did,
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable borg-mesh-corp.service",
                "systemctl --user start borg-mesh-corp.service",
                "systemctl --user restart borg-web.service",
            ]
        );
    }
    assert_eq!(record(&root), serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "off": false}));

    // A second run: nothing written, started or restarted.
    let files = |d: &Path| std::fs::read_dir(d).unwrap().map(|e| std::fs::read(e.unwrap().path()).unwrap()).collect::<Vec<_>>();
    let before = files(&dir);
    let out = borg(&root, &["service", "install"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("up to date"), "{}", text(&out));
    assert_eq!(changes(&root), Vec::<String>::new());
    assert_eq!(files(&dir), before);

    // Another borg last (re)started them: both restart, their files unchanged.
    std::fs::write(root.join("store/services.json"), r#"{"version": "0.0.1", "off": false}"#).unwrap();
    let out = borg(&root, &["service", "install"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let did = changes(&root);
    if LAUNCHD {
        assert!(did.len() == 2 && did.iter().all(|c| c.starts_with("launchctl kickstart -k gui/")), "{did:?}");
    } else {
        assert_eq!(did, ["systemctl --user restart borg-mesh-corp.service borg-web.service"]);
    }
    assert_eq!(files(&dir), before);

    let out = borg(&root, &["service"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("the mesh daemon of collective corp runs") && said.contains("borg web runs") && said.contains(".bak"), "{said}");
    assert_eq!(changes(&root), Vec::<String>::new(), "status changes nothing");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn remove_stays_off_through_install_apply_until_service_install() {
    if !supported() {
        return;
    }
    let root = machine("remove");
    let (dir, mesh, web) = units(&root);
    assert_eq!(borg(&root, &["service", "install"]).status.code(), Some(0));
    changes(&root);

    let out = borg(&root, &["service", "remove"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!dir.join(mesh).exists() && !dir.join(web).exists(), "{}", text(&out));
    assert_eq!(record(&root)["off"], true);
    let did = changes(&root);
    if LAUNCHD {
        assert!(did.len() == 2 && did.iter().all(|c| c.starts_with("launchctl bootout gui/")), "{did:?}");
    } else {
        assert_eq!(did, ["systemctl --user disable --now borg-mesh-corp.service borg-web.service", "systemctl --user daemon-reload"]);
    }

    // An update's `borg install <h> --apply` leaves them off.
    let out = borg(&root, &["install", "claude", "--apply"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("services: off (borg service remove)"), "{}", text(&out));
    assert!(!dir.join(web).exists());
    assert_eq!(changes(&root), Vec::<String>::new());
    let out = borg(&root, &["service", "restart"]);
    assert!(text(&out).contains("services: off"), "{}", text(&out));
    assert_eq!(changes(&root), Vec::<String>::new());

    // The engineer turns them on again.
    let out = borg(&root, &["service", "install"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(dir.join(mesh).is_file() && dir.join(web).is_file());
    assert_eq!(record(&root)["off"], false);
    let _ = std::fs::remove_dir_all(&root);
}

/// An older borg's `borg update` runs only `borg install <h> --apply` and `borg doctor` on the new
/// binary: that install is what sets the services up.
#[test]
fn an_older_borgs_update_path_installs_the_units() {
    if !supported() {
        return;
    }
    let root = machine("old-update");
    let (dir, mesh, web) = units(&root);
    let out = borg(&root, &["install", "claude", "--apply"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(root.join("home/.claude/settings.json").is_file(), "the harness is wired");
    assert!(dir.join(mesh).is_file() && dir.join(web).is_file(), "{}", text(&out));
    let did = changes(&root);
    if LAUNCHD {
        assert!(did.len() == 2 && did.iter().all(|c| c.starts_with("launchctl bootstrap gui/")), "{did:?}");
    } else {
        assert_eq!(
            did,
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable borg-mesh-corp.service borg-web.service",
                "systemctl --user start borg-mesh-corp.service borg-web.service",
            ]
        );
    }
    // A dry run and `--home` (a sandbox) touch no service.
    let home = root.join("sandbox");
    std::fs::create_dir_all(&home).unwrap();
    for args in [&["install", "claude"][..], &["install", "claude", "--apply", "--home", home.to_str().unwrap()]] {
        let out = borg(&root, args);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        assert!(!text(&out).contains("services"), "{args:?}: {}", text(&out));
    }
    assert_eq!(changes(&root), Vec::<String>::new());
    // The collective loses its mesh identity: its daemon goes.
    std::fs::remove_file(root.join("store/collectives/corp/mesh/node.key")).unwrap();
    let out = borg(&root, &["service", "install"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!dir.join(mesh).exists() && dir.join(web).is_file(), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_user_bus_or_a_development_build_installs_nothing_and_says_why() {
    if !supported() {
        return;
    }
    let root = machine("skip");
    let (dir, _, _) = units(&root);
    let dev = root.join("repo/target/release");
    std::fs::create_dir_all(&dev).unwrap();
    script(&dev.join("borg"), "exit 0");
    let out = borg_as(&root, &["service", "install"], &dev.join("borg"));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("development build"), "{}", text(&out));
    assert!(!dir.exists() && changes(&root).is_empty());
    if !LAUNCHD {
        std::fs::write(root.join("state/nobus"), "").unwrap();
        for args in [&["service", "install"][..], &["install", "claude", "--apply"]] {
            let out = borg(&root, args);
            assert_eq!(out.status.code(), Some(0), "{args:?}: {}", text(&out));
            assert!(text(&out).contains("no systemd user session here"), "{}", text(&out));
        }
        assert!(!dir.exists() && changes(&root).is_empty());
    }
    let _ = std::fs::remove_dir_all(&root);
}
