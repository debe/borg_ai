//! Borg's own background services: `borg service [status|install|restart|remove]`.
//!
//! Borg runs two kinds of long-lived process: one mesh daemon per team collective this machine is a
//! member of (`<store>/mesh/node.key` and `membership.json`), and `borg web`. Here they become services of the machine's own
//! manager, as the engineer's user:
//!
//! - **systemd** (Linux): user units `borg-mesh-<collective>.service` and `borg-web.service` in
//!   `$XDG_CONFIG_HOME/systemd/user` (default `~/.config/systemd/user`), applied with
//!   `systemctl --user daemon-reload`, `enable`, `start` and `restart`. Without a user bus (no
//!   `XDG_RUNTIME_DIR`, or `systemctl --user show-environment` fails: WSL, containers) nothing is
//!   installed, and borg says so.
//! - **launchd** (macOS): agents `borg.mesh.<collective>.plist` and `borg.web.plist` in
//!   `~/Library/LaunchAgents`, logging to `~/Library/Logs/borg/<label>.log`; loaded with
//!   `launchctl bootstrap gui/<uid>`, a changed one `bootout` first (and waited for until launchd let
//!   go of its label), restarted with `kickstart -k`.
//!
//! Each runs `<borg> mesh serve --collective <c>` or `<borg> web`, `<borg>` being the resolved
//! install.rs `bin` (the path an update replaces in place); a build under `target/` installs none. The
//! PATH a unit sets is the one at its first write (kept after, so callers with other PATHs never churn
//! it), with borg's directory first, only absolute entries and a hand-written unit's `%h` expanded. The
//! variables that locate the store and the mesh (`BORG_HOME`, `BORG_CONFIG`, `BORG_RELAY`) are the
//! caller's: the units serve the store whose collectives decided them. Mesh and web stop on SIGINT
//! under systemd and on SIGTERM under launchd; both trap either.
//!
//! **Ownership.** Every file starts with a marker ("managed by borg: rewritten by borg; edits are
//! lost"; an older borg's carries its version). A same-named file without it is the engineer's: install renames it to `<file>.bak` (once: an
//! existing `.bak` is kept) and writes borg's. Borg's own files that no wanted service names (a
//! collective gone, or not a member now) are stopped and removed.
//!
//! **Restarts.** `<root store>/services.json` records the borg version that last (re)started the
//! services, and `off`; the files carry no version, so an update restarts (systemd `restart`, launchd
//! `kickstart -k`) instead of reloading every file. A unit is restarted when its file changed or that
//! version is not this borg's, started when it does not run (a unit systemd restarts after failures
//! does not), so the several `borg install <h> --apply` of one update restart once.
//! `borg service remove` writes `off: true` and removes the units; `borg setup` and `borg install <h>
//! --apply` (`auto`) then leave them off, and `borg update` skips its `borg service install` step.
//! `borg service install` turns them on again: it records that first, whatever its steps do.
//!
//! Every path comes from HOME (and XDG_CONFIG_HOME) and every command goes through a `Run`, so tests
//! use a temporary home and stub managers; a systemd user manager whose HOME is another is left alone,
//! by `remove` too (it removes the files only).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::collective::{self, Route, Share};
use crate::harness::Level;
use crate::update::VERSION;

const USAGE: &str = "usage: borg service [status|install|restart|remove]";

/// The verbs (completions.rs lists these).
pub const SUBS: &[&str] = &["status", "install", "restart", "remove"];

/// Baseline: how long a stopping service may take before its manager kills it (systemd
/// `TimeoutStopSec`, launchd `ExitTimeOut`): a mesh daemon finishing a sync round.
const STOP_SECS: u32 = 30;

/// Baseline: the pause before a failed service starts again (systemd `RestartSec`, launchd
/// `ThrottleInterval`): a crash loop costs one start per 10 s.
const RESTART_SECS: u32 = 10;

/// The marker's fixed part, in every file borg writes (an older borg's put its version after it).
const MARK: &str = "managed by borg";

/// The marker line's text. It names no version: a file changes only when what it runs changes, so an
/// update restarts the services instead of reloading their files.
const MARKER: &str = "managed by borg: rewritten by borg; edits are lost";

/// Whether `text` is a file borg wrote (its marker in the first lines).
fn owned(text: &str) -> bool {
    text.lines().take(3).any(|l| l.contains(MARK) && l.contains("edits are lost"))
}

// ---------------------------------------------------------------- services

/// One service borg runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Service {
    /// The mesh daemon of the named team collective.
    Mesh(String),
    Web,
}

impl Service {
    /// The arguments after `borg`.
    fn args(&self) -> Vec<&str> {
        match self {
            Service::Mesh(c) => vec!["mesh", "serve", "--collective", c],
            Service::Web => vec!["web"],
        }
    }

    /// The same process started by hand, which a service of it competes with (a port, a store).
    fn by_hand(&self) -> String {
        match self {
            Service::Mesh(c) => format!("a `borg mesh serve --collective {c}`"),
            Service::Web => "a `borg web`".into(),
        }
    }

    fn what(&self) -> String {
        match self {
            Service::Mesh(c) => format!("the mesh daemon of collective {c}"),
            Service::Web => "borg web".into(),
        }
    }
}

/// A collective's name as a unit or label part: its store directory's name (collective.rs), which is
/// the name itself when it is a safe one.
fn id(collective: &str) -> String {
    collective::store(Path::new(""), collective).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Whether this machine is a member of the collective stored at `home`: its mesh identity and the
/// membership `borg mesh serve` needs (borg-mesh membership.rs `path`). The key alone is no proof:
/// `borg mesh id` and a failed `join` write it.
fn member(home: &Path) -> bool {
    let mesh = home.join("mesh");
    mesh.join("node.key").is_file() && mesh.join("membership.json").is_file()
}

/// The services this machine wants: a mesh daemon per team collective it is a member of, then
/// `borg web` while any collective exists (with none it has nothing to show and exits).
#[must_use]
pub fn wanted(cfg: Option<&collective::Config>, base: &Path) -> Vec<Service> {
    let routes = crate::install::collectives(cfg, base);
    let mesh = routes.iter().filter_map(|r| match r {
        Route::Collective { name, home, share: Share::Team } if member(home) => Some(Service::Mesh(name.clone())),
        _ => None,
    });
    let mut out: Vec<Service> = mesh.collect();
    if !routes.is_empty() {
        out.push(Service::Web);
    }
    out
}

// ---------------------------------------------------------------- backends

/// The service manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Systemd,
    Launchd,
}

impl Backend {
    /// This platform's; none where borg knows no user service manager.
    #[must_use]
    pub fn native() -> Option<Backend> {
        match std::env::consts::OS {
            "linux" => Some(Backend::Systemd),
            "macos" => Some(Backend::Launchd),
            _ => None,
        }
    }

    /// The service's name to the manager: a unit name, a label.
    fn name(self, s: &Service) -> String {
        match (self, s) {
            (Backend::Systemd, Service::Mesh(c)) => format!("borg-mesh-{}.service", id(c)),
            (Backend::Systemd, Service::Web) => "borg-web.service".into(),
            (Backend::Launchd, Service::Mesh(c)) => format!("borg.mesh.{}", id(c)),
            (Backend::Launchd, Service::Web) => "borg.web".into(),
        }
    }

    /// Its file's name.
    fn file(self, name: &str) -> String {
        match self {
            Backend::Systemd => name.to_string(),
            Backend::Launchd => format!("{name}.plist"),
        }
    }

    /// The service name of a file in the unit directory that has one of borg's names; none otherwise
    /// (a `.bak`, anyone else's).
    fn named(self, file: &str) -> Option<&str> {
        let (mesh, web, ext) = match self {
            Backend::Systemd => ("borg-mesh-", "borg-web", ".service"),
            Backend::Launchd => ("borg.mesh.", "borg.web", ".plist"),
        };
        let stem = file.strip_suffix(ext)?;
        let ours = stem == web || stem.strip_prefix(mesh).is_some_and(|c| !c.is_empty());
        match self {
            Backend::Systemd => ours.then_some(file),
            Backend::Launchd => ours.then_some(stem),
        }
    }

    /// The file borg writes for `s` on `h`, with `path` as PATH.
    fn render(self, s: &Service, h: &Host, path: &str) -> String {
        match self {
            Backend::Systemd => systemd_unit(s, &h.bin, path, &h.env),
            Backend::Launchd => plist(&self.name(s), s, &h.bin, path, &h.env, &h.home),
        }
    }

    /// The PATH a file sets, as its process sees it (borg's form, or a hand-written `Environment=PATH=`
    /// with its specifiers expanded against `home`).
    fn path_in(self, text: &str, home: &Path) -> Option<String> {
        match self {
            Backend::Systemd => systemd_path(text, home),
            Backend::Launchd => plist_path(text),
        }
    }
}

/// The variables a unit sets besides PATH, as `KEY=value` pairs.
type Env = [(&'static str, String)];

/// `s` as a systemd user unit, PATH then `env` set.
fn systemd_unit(s: &Service, bin: &Path, path: &str, env: &Env) -> String {
    let exec: Vec<String> = std::iter::once(bin.display().to_string()).chain(s.args().into_iter().map(str::to_string)).map(|a| sd_word(&a, true)).collect();
    let (what, after, dir) = match s {
        Service::Mesh(c) => (format!("borg mesh daemon of collective {}", id(c)), "After=network-online.target\nWants=network-online.target\n", ""),
        Service::Web => ("borg web, the live page on 127.0.0.1".to_string(), "", "WorkingDirectory=%h\n"),
    };
    let vars: String = std::iter::once(("PATH", path))
        .chain(env.iter().map(|(k, v)| (*k, v.as_str())))
        .map(|(k, v)| format!("Environment={}\n", sd_word(&format!("{k}={v}"), false)))
        .collect();
    format!(
        "# {MARKER}\n[Unit]\nDescription={what}\n{after}\n[Service]\nExecStart={}\n{dir}{vars}KillSignal=SIGINT\nTimeoutStopSec={STOP_SECS}\nRestart=on-failure\nRestartSec={RESTART_SECS}\n\n[Install]\nWantedBy=default.target\n",
        exec.join(" "),
    )
}

/// One word of a unit line: bare when it is plain, else double-quoted with C escapes. `%` is a
/// specifier everywhere (`%%`); in a command line (`exec`) so is `$` a variable (`$$`).
fn sd_word(w: &str, exec: bool) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "/._:+=,@-".contains(c)) {
        return w.to_string();
    }
    let mut out = String::from("\"");
    for c in w.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' if exec => out.push_str("$$"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", u32::from(c) & 0xff)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The PATH of an `Environment=PATH=...` line, quoted (borg's form) or bare (a hand-written one), as
/// systemd hands it to the process: C escapes undone, then each entry's specifiers expanded
/// (`specifiers`, `%h` as `home`); an entry with a specifier borg cannot tell is left out.
fn systemd_path(text: &str, home: &Path) -> Option<String> {
    let value = text.lines().filter_map(|l| l.trim().strip_prefix("Environment=")).find_map(|v| {
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|q| q.strip_suffix('"')).map_or_else(|| v.to_string(), unescape);
        v.strip_prefix("PATH=").map(str::to_string)
    })?;
    let home = home.display().to_string();
    Some(value.split(':').filter_map(|d| specifiers(d, &home)).collect::<Vec<_>>().join(":"))
}

/// `word` with systemd's specifiers `%%` and `%h` (`home`) expanded; none when it holds another.
fn specifiers(word: &str, home: &str) -> Option<String> {
    let mut out = String::with_capacity(word.len());
    let mut cs = word.chars();
    while let Some(c) = cs.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match cs.next()? {
            '%' => out.push('%'),
            'h' => out.push_str(home),
            _ => return None,
        }
    }
    Some(out)
}

/// `sd_word`'s C escapes undone (without `$$`, which an `Environment=` value never has); its `%%` is a
/// specifier, left to `specifiers`.
fn unescape(q: &str) -> String {
    let mut out = String::new();
    let mut cs = q.chars();
    while let Some(c) = cs.next() {
        match c {
            '\\' => match cs.next() {
                Some('x') => {
                    let hex: String = cs.by_ref().take(2).collect();
                    out.extend(u8::from_str_radix(&hex, 16).ok().map(char::from));
                }
                Some(e) => out.push(e),
                None => {}
            },
            c => out.push(c),
        }
    }
    out
}

/// `s` as a launchd agent labelled `label`, PATH then `env` set.
fn plist(label: &str, s: &Service, bin: &Path, path: &str, env: &Env, home: &Path) -> String {
    let args: String = std::iter::once(bin.display().to_string())
        .chain(s.args().into_iter().map(str::to_string))
        .map(|a| format!("    <string>{}</string>\n", xml(&a)))
        .collect();
    let vars: String = std::iter::once(("PATH", path))
        .chain(env.iter().map(|(k, v)| (*k, v.as_str())))
        .map(|(k, v)| format!("    <key>{k}</key>\n    <string>{}</string>\n", xml(v)))
        .collect();
    let log = xml(&logs(home).join(format!("{label}.log")).display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- {marker} -->
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>{RESTART_SECS}</integer>
  <key>ExitTimeOut</key>
  <integer>{STOP_SECS}</integer>
  <key>EnvironmentVariables</key>
  <dict>
{vars}  </dict>
  <key>WorkingDirectory</key>
  <string>{home}</string>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        marker = MARKER,
        label = xml(label),
        home = xml(&home.display().to_string()),
    )
}

/// Where the agents log: `~/Library/Logs/borg`.
fn logs(home: &Path) -> PathBuf {
    home.join("Library/Logs/borg")
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The string after `<key>PATH</key>`, unescaped.
fn plist_path(text: &str) -> Option<String> {
    let after = &text[text.find("<key>PATH</key>")? + "<key>PATH</key>".len()..];
    let start = after.find("<string>")? + "<string>".len();
    let value = &after[start..start + after[start..].find("</string>")?];
    Some(value.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&"))
}

/// The PATH a unit sets: `from`'s absolute entries (a relative one would resolve against the service's
/// working directory), `bin`'s directory first (and nowhere else).
fn unit_path(bin: &Path, from: &str) -> String {
    let dir = bin.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let rest = from.split(':').filter(|d| d.starts_with('/') && *d != dir);
    std::iter::once(dir.as_str()).chain(rest).collect::<Vec<_>>().join(":")
}

// ---------------------------------------------------------------- the record

/// `<root store>/services.json`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Record {
    /// The borg that last (re)started the services.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `borg service remove` turned them off.
    pub off: bool,
}

/// `Record` as read.
#[derive(Deserialize)]
struct Raw<'a> {
    #[serde(borrow, default)]
    version: Option<Cow<'a, str>>,
    #[serde(default)]
    off: bool,
}

impl Record {
    fn path(base: &Path) -> PathBuf {
        base.join("services.json")
    }

    /// The record under `base`; the default when there is none or it does not parse.
    #[must_use]
    pub fn read(base: &Path) -> Record {
        let text = std::fs::read_to_string(Self::path(base)).unwrap_or_default();
        match serde_json::from_str::<Raw>(&text) {
            Ok(r) => Record { version: r.version.map(Cow::into_owned), off: r.off },
            Err(_) => Record::default(),
        }
    }

    fn save(&self, base: &Path) -> Result<(), String> {
        let text = serde_json::to_string(self).map_err(|e| e.to_string())?;
        crate::update::write_atomic(&Self::path(base), text.as_bytes()).map_err(|e| format!("{}: {e}", Self::path(base).display()))
    }
}

// ---------------------------------------------------------------- the host

/// What a manager command printed; `ok` when it exited 0.
#[derive(Debug, Default, Clone)]
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a manager command (program, arguments); none when the program cannot run (not on PATH).
pub type Run<'a> = &'a dyn Fn(&str, &[&str]) -> Option<Out>;

/// `prog` on PATH with `args`: the real `Run`.
#[must_use]
pub fn system(prog: &str, args: &[&str]) -> Option<Out> {
    let p = crate::install::which(prog)?;
    let o = std::process::Command::new(p).args(args).stdin(std::process::Stdio::null()).output().ok()?;
    Some(Out { ok: o.status.success(), stdout: String::from_utf8_lossy(&o.stdout).into_owned(), stderr: String::from_utf8_lossy(&o.stderr).into_owned() })
}

/// Where the services live and what writes and runs them.
pub struct Host<'a> {
    pub backend: Backend,
    pub home: PathBuf,
    /// The unit (agent) directory.
    pub dir: PathBuf,
    /// The root store: `services.json`, the collectives' stores.
    pub base: PathBuf,
    /// The borg the services run, resolved.
    pub bin: PathBuf,
    /// PATH now: a new unit's.
    pub path: String,
    /// The variables of `LOCATING` set now, every unit's (paths made absolute).
    pub env: Vec<(&'static str, String)>,
    pub uid: u32,
    /// Whether a user session's bus can be there at all (systemd: `XDG_RUNTIME_DIR` or
    /// `DBUS_SESSION_BUS_ADDRESS` set, without which `systemctl --user` cannot connect); launchd: always.
    pub session: bool,
    pub run: Run<'a>,
}

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// The variables that tell a service which store, config and relay to use: a daemon started without
/// them would serve the default store, not the one whose collectives decided which services run.
/// The paths among them (all but `BORG_RELAY`, a URL) are made absolute.
const LOCATING: &[&str] = &["BORG_HOME", "BORG_CONFIG", "BORG_RELAY"];

/// The `LOCATING` variables set now.
fn locating() -> Vec<(&'static str, String)> {
    LOCATING
        .iter()
        .filter_map(|&k| {
            let v = var(k)?;
            let v = match k {
                "BORG_RELAY" => v,
                _ => std::path::absolute(&v).map_or(v, |p| p.display().to_string()),
            };
            Some((k, v))
        })
        .collect()
}

impl<'a> Host<'a> {
    /// This machine's, from the environment; why there is none (no manager borg knows, no HOME).
    pub fn from_env(run: Run<'a>) -> Result<Host<'a>, String> {
        let backend = Backend::native().ok_or_else(|| format!("no user service manager borg knows on {}", std::env::consts::OS))?;
        let home = PathBuf::from(var("HOME").ok_or("HOME is not set")?);
        let dir = match backend {
            Backend::Systemd => var("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from).join("systemd/user"),
            Backend::Launchd => home.join("Library/LaunchAgents"),
        };
        // SAFETY: getuid(2) takes nothing and cannot fail.
        let uid = unsafe { libc::getuid() };
        let base = collective::root_of(&crate::learn::home());
        let session = backend == Backend::Launchd || var("XDG_RUNTIME_DIR").is_some() || var("DBUS_SESSION_BUS_ADDRESS").is_some();
        Ok(Host { backend, dir, base, bin: crate::install::real_bin(), path: var("PATH").unwrap_or_default(), env: locating(), uid, session, run, home })
    }

    /// Why this borg installs no services: a development build.
    fn dev(&self) -> Option<String> {
        self.bin
            .parent()
            .is_some_and(crate::install::under_target)
            .then(|| format!("{} is a development build (under target/); an installed borg sets them up", self.bin.display()))
    }

    fn manager(&self) -> &'static str {
        match self.backend {
            Backend::Systemd => "systemctl",
            Backend::Launchd => "launchctl",
        }
    }

    /// A manager command; systemd's as `--user`.
    fn call(&self, args: &[&str]) -> Option<Out> {
        match self.backend {
            Backend::Systemd => (self.run)("systemctl", &[&["--user"], args].concat()),
            Backend::Launchd => (self.run)("launchctl", args),
        }
    }

    /// Whether the manager answers here, for this HOME; why not.
    fn reach(&self) -> Result<(), String> {
        let by_hand = "run `borg mesh serve` and `borg web` by hand";
        if !self.session {
            return Err(format!("no systemd user session here (XDG_RUNTIME_DIR is not set): none installed; {by_hand}"));
        }
        let probe: &[&str] = match self.backend {
            Backend::Systemd => &["show-environment"],
            Backend::Launchd => &["managername"],
        };
        let Some(out) = self.call(probe) else {
            return Err(format!("no {} on PATH: none installed; {by_hand}", self.manager()));
        };
        if !out.ok {
            let why = out.stderr.lines().next().unwrap_or_default().trim().to_string();
            return Err(match self.backend {
                Backend::Systemd => format!("no systemd user session here (systemctl --user show-environment: {why}): none installed; {by_hand}"),
                Backend::Launchd => format!("no launchd session here (launchctl managername: {why}): none installed; {by_hand}"),
            });
        }
        if let Some(theirs) = out.stdout.lines().find_map(|l| l.strip_prefix("HOME="))
            && !same(Path::new(theirs), &self.home)
        {
            return Err(format!("the systemd user manager's HOME is {theirs}, not {}: services left alone", self.home.display()));
        }
        Ok(())
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self, label: &str) -> String {
        format!("gui/{}/{label}", self.uid)
    }

    /// The managed web's process, if it runs.
    fn web_pid(&self) -> Option<u32> {
        let name = self.backend.name(&Service::Web);
        // No unit of borg's: no manager to ask.
        if !std::fs::read_to_string(self.dir.join(self.backend.file(&name))).is_ok_and(|t| owned(&t)) {
            return None;
        }
        self.states(&[name.as_str()]).remove(&name).and_then(|s| s.pid)
    }

    /// What the manager says of each named service.
    fn states(&self, names: &[&str]) -> std::collections::HashMap<String, State> {
        if names.is_empty() {
            return std::collections::HashMap::new();
        }
        match self.backend {
            Backend::Systemd => {
                let args = [&["show", "--property=Id,ActiveState,SubState,NRestarts,UnitFileState,MainPID"], names].concat();
                self.call(&args).filter(|o| o.ok).map(|o| systemd_states(&o.stdout)).unwrap_or_default()
            }
            Backend::Launchd => names
                .iter()
                .filter_map(|n| {
                    let out = self.call(&["print", &self.target(n)]).filter(|o| o.ok)?;
                    Some(((*n).to_string(), launchd_state(&out.stdout)))
                })
                .collect(),
        }
    }
}

/// Whether two paths name one directory (resolved where they exist).
fn same(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// One service as its manager sees it. A launchd agent that is loaded counts as enabled (it loads at
/// login: `RunAtLoad`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct State {
    /// The manager's word for it (systemd `ActiveState`; launchd `state`).
    active: String,
    running: bool,
    /// It failed and its manager starts it again (systemd `activating`/`auto-restart`): a crash loop,
    /// which does not run.
    looping: bool,
    /// How often its manager restarted it after a failure (systemd `NRestarts`).
    restarts: u32,
    enabled: bool,
    pid: Option<u32>,
}

/// `systemctl show`'s blocks, one per unit, by `Id`. Only `active` (and `reloading`) runs: between the
/// starts of a crash loop systemd says `activating` with the sub-state `auto-restart`.
fn systemd_states(out: &str) -> std::collections::HashMap<String, State> {
    let mut map = std::collections::HashMap::new();
    for block in out.split("\n\n") {
        let field = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('='));
        let Some(id) = field("Id") else { continue };
        let active = field("ActiveState").unwrap_or_default();
        let state = State {
            active: active.to_string(),
            running: matches!(active, "active" | "reloading"),
            looping: field("SubState") == Some("auto-restart"),
            restarts: field("NRestarts").and_then(|n| n.parse().ok()).unwrap_or_default(),
            enabled: matches!(field("UnitFileState"), Some("enabled" | "enabled-runtime")),
            pid: field("MainPID").and_then(|p| p.parse().ok()).filter(|p| *p != 0),
        };
        map.insert(id.to_string(), state);
    }
    map
}

/// `launchctl print`'s `state = ...` and `pid = ...` of a loaded agent.
fn launchd_state(out: &str) -> State {
    let field = |k: &str| out.lines().find_map(|l| l.trim().strip_prefix(k)?.trim_start().strip_prefix('=').map(str::trim));
    let active = field("state").unwrap_or("loaded");
    State { active: active.to_string(), running: active == "running", enabled: true, pid: field("pid").and_then(|p| p.parse().ok()), ..State::default() }
}

// ---------------------------------------------------------------- plans

/// One wanted service as this borg writes it, and what its file holds now.
struct Unit {
    service: Service,
    name: String,
    path: PathBuf,
    text: String,
    now: Option<String>,
}

impl Unit {
    fn changed(&self) -> bool {
        self.now.as_deref() != Some(self.text.as_str())
    }

    /// The engineer's file under borg's name.
    fn foreign(&self) -> bool {
        self.now.as_deref().is_some_and(|t| !owned(t))
    }
}

/// Where a taken-over file is kept.
fn bak(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Each wanted service's unit: the PATH of the file there (whoever wrote it) kept, else `h.path`.
fn units(h: &Host, wanted: &[Service]) -> Vec<Unit> {
    wanted
        .iter()
        .map(|s| {
            let name = h.backend.name(s);
            let path = h.dir.join(h.backend.file(&name));
            let now = std::fs::read_to_string(&path).ok();
            let from = now.as_deref().and_then(|t| h.backend.path_in(t, &h.home)).unwrap_or_else(|| h.path.clone());
            let text = h.backend.render(s, h, &unit_path(&h.bin, &from));
            Unit { service: s.clone(), name, path, text, now }
        })
        .collect()
}

/// Borg's own files in the unit directory that no wanted unit is: (name, path).
fn orphans(h: &Host, units: &[Unit]) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(&h.dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let name = h.backend.named(&file)?.to_string();
            let path = e.path();
            let ours = !units.iter().any(|u| u.path == path) && std::fs::read_to_string(&path).is_ok_and(|t| owned(&t));
            ours.then_some((name, path))
        })
        .collect();
    out.sort();
    out
}

/// What a wanted service needs once its file is in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Keep,
    Start,
    Restart,
}

/// A service that does not run is started; one that runs is restarted when its file changed or another
/// borg last (re)started the services (`services.json`).
fn act(running: bool, changed: bool, other_version: bool) -> Act {
    match (running, changed || other_version) {
        (false, _) => Act::Start,
        (true, true) => Act::Restart,
        (true, false) => Act::Keep,
    }
}

/// What a command did: its lines, and whether any step failed.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub failed: bool,
    /// Nothing could be done here, and the lines say why (no manager, a development build, off).
    pub skipped: bool,
}

impl Report {
    fn skip(line: String) -> Report {
        Report { lines: vec![line], failed: false, skipped: true }
    }

    fn failure(line: String) -> Report {
        Report { lines: vec![line], failed: true, skipped: false }
    }

    fn fail(&mut self, line: String) {
        self.lines.push(line);
        self.failed = true;
    }

    /// Runs a manager command, a failure reported as `what`.
    fn call(&mut self, h: &Host, args: &[&str], what: &str) -> bool {
        match h.call(args) {
            Some(o) if o.ok => true,
            Some(o) => {
                self.fail(format!("{what} failed: {} {}: {}", h.manager(), args.join(" "), o.stderr.trim()));
                false
            }
            None => {
                self.fail(format!("{what} failed: {} is not on PATH", h.manager()));
                false
            }
        }
    }
}

/// Writes `u`'s file, the engineer's one under that name kept as `.bak` first (once).
fn put(h: &Host, u: &Unit) -> Result<String, String> {
    let mut line = format!("service {}: wrote {}", u.name, u.path.display());
    if u.foreign() {
        let b = bak(&u.path);
        if b.exists() {
            line.push_str(&format!(" over a file borg did not write ({} from an earlier takeover is kept)", b.display()));
        } else {
            std::fs::rename(&u.path, &b).map_err(|e| format!("{}: {e}", u.path.display()))?;
            line.push_str(&format!(" (took it over; yours is kept at {})", b.display()));
        }
    }
    if h.backend == Backend::Launchd {
        std::fs::create_dir_all(logs(&h.home)).map_err(|e| format!("{}: {e}", logs(&h.home).display()))?;
    }
    crate::update::write_atomic(&u.path, u.text.as_bytes()).map_err(|e| format!("{}: {e}", u.path.display()))?;
    Ok(line)
}

/// Stops and removes borg's files `files` (name, path); with `manager` false only removes them (the
/// manager here is not this HOME's, or there is none to ask).
fn take_down(h: &Host, files: &[(String, PathBuf)], why: &str, manager: bool, r: &mut Report) {
    if files.is_empty() {
        return;
    }
    match h.backend {
        _ if !manager => {}
        Backend::Systemd => {
            let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
            r.call(h, &[&["disable", "--now"], &names[..]].concat(), "stopping");
        }
        Backend::Launchd => {
            for (n, _) in files {
                // Not loaded is fine: there is nothing to stop.
                let _ = h.call(&["bootout", &h.target(n)]);
            }
        }
    }
    for (n, p) in files {
        match std::fs::remove_file(p) {
            Ok(()) => r.lines.push(format!("service {n}: removed {}{why}", p.display())),
            Err(e) => r.fail(format!("service {n}: {}: {e}", p.display())),
        }
        let b = bak(p);
        if b.exists() {
            r.lines.push(format!("service {n}: the unit borg took over is still at {}", b.display()));
        }
    }
    if manager && h.backend == Backend::Systemd {
        r.call(h, &["daemon-reload"], "reloading");
    }
}

/// How often `unload` asks whether launchd let go of a label.
const UNLOAD_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Unloads agent `label` and waits until launchd let go of it: `bootout` returns while a stopping
/// daemon still holds the label (up to `STOP_SECS`), and a `bootstrap` then fails ("5: Input/output
/// error"), leaving the agent unloaded.
fn unload(h: &Host, label: &str, r: &mut Report) -> bool {
    let target = h.target(label);
    if !r.call(h, &["bootout", &target], "unloading") {
        return false;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(STOP_SECS.into()) + UNLOAD_POLL * 10;
    while h.call(&["print", &target]).is_some_and(|o| o.ok) {
        if std::time::Instant::now() >= until {
            r.fail(format!("unloading failed: {label} is still loaded {STOP_SECS} s after launchctl bootout"));
            return false;
        }
        std::thread::sleep(UNLOAD_POLL);
    }
    true
}

/// `borg service install` on a reachable host: every wanted unit written, enabled and running, the
/// changed ones (or all, after another borg) restarted, borg's others removed. The record is saved
/// whatever the steps did, with this borg's version once every start and restart worked: a unit whose
/// file could not be written is restarted when it is, and one whose restart failed is tried again.
fn install(h: &Host, wanted: &[Service], rec: &mut Record) -> Report {
    let mut r = Report::default();
    let units = units(h, wanted);
    let orphans = orphans(h, &units);
    let mut written: Vec<&str> = Vec::new();
    for u in units.iter().filter(|u| u.changed()) {
        match put(h, u) {
            Ok(line) => {
                r.lines.push(line);
                written.push(&u.name);
            }
            Err(e) => r.fail(format!("service {}: {e}", u.name)),
        }
    }
    take_down(h, &orphans, " (no wanted service names it)", true, &mut r);
    if h.backend == Backend::Systemd && !written.is_empty() {
        r.call(h, &["daemon-reload"], "reloading");
    }
    let other = rec.version.as_deref() != Some(VERSION);
    let live: Vec<&Unit> = units.iter().filter(|u| !u.changed() || written.contains(&u.name.as_str())).collect();
    let names: Vec<&str> = live.iter().map(|u| u.name.as_str()).collect();
    let states = h.states(&names);
    let state = |u: &Unit| states.get(&u.name).cloned().unwrap_or_default();
    let decide = |u: &Unit| act(state(u).running, written.contains(&u.name.as_str()), other);
    // Every start and restart worked: the services run this borg.
    let mut moved = true;
    match h.backend {
        Backend::Systemd => {
            let pick = |f: &dyn Fn(&Unit) -> bool| -> Vec<&str> { live.iter().filter(|u| f(u)).map(|u| u.name.as_str()).collect() };
            for (verb, names, done) in [
                ("enable", pick(&|u| !state(u).enabled), "enabled"),
                ("start", pick(&|u| decide(u) == Act::Start), "started"),
                ("restart", pick(&|u| decide(u) == Act::Restart), "restarted"),
            ] {
                if names.is_empty() {
                    continue;
                }
                let ok = r.call(h, &[&[verb], &names[..]].concat(), verb);
                if ok {
                    r.lines.push(format!("{done} {}", names.join(", ")));
                }
                moved &= ok || verb == "enable";
            }
        }
        Backend::Launchd => {
            for u in &live {
                let loaded = states.contains_key(&u.name);
                let file = u.path.display().to_string();
                let done = match (decide(u), loaded) {
                    (Act::Keep, _) => continue,
                    // A changed file is read again only by loading it again.
                    (_, true) if written.contains(&u.name.as_str()) => unload(h, &u.name, &mut r) && r.call(h, &["bootstrap", &h.domain(), &file], "loading"),
                    (_, false) => r.call(h, &["bootstrap", &h.domain(), &file], "loading"),
                    (_, true) => r.call(h, &["kickstart", "-k", &h.target(&u.name)], "restarting"),
                };
                if done {
                    r.lines.push(format!("{} {}", if loaded { "restarted" } else { "started" }, u.name));
                }
                moved &= done;
            }
        }
    }
    if moved {
        rec.version = Some(VERSION.to_string());
    }
    if let Err(e) = rec.save(&h.base) {
        r.fail(format!("services: {e}"));
    }
    // Decided by what is wanted, not by what was written: a unit that could not be written is a
    // failure above, not "none wanted".
    if units.is_empty() && orphans.is_empty() {
        r.skipped = true;
        r.lines.push("services: none wanted (no collective here)".into());
    } else if r.lines.is_empty() {
        r.lines.push(format!("services: {} up to date", names.join(", ")));
    }
    r
}

/// `borg service restart`: every installed borg unit restarted (started when it does not run).
fn restart(h: &Host, wanted: &[Service], rec: &mut Record) -> Report {
    let mut r = Report::default();
    let units = units(h, wanted);
    let (here, missing): (Vec<&Unit>, Vec<&Unit>) = units.iter().partition(|u| u.now.as_deref().is_some_and(owned));
    for u in missing {
        r.lines.push(format!("service {}: not installed (borg service install)", u.name));
    }
    let names: Vec<&str> = here.iter().map(|u| u.name.as_str()).collect();
    if names.is_empty() {
        return r;
    }
    let ok = match h.backend {
        Backend::Systemd => r.call(h, &[&["restart"], &names[..]].concat(), "restarting"),
        Backend::Launchd => {
            let states = h.states(&names);
            here.iter().fold(true, |ok, u| {
                let did = match states.contains_key(&u.name) {
                    true => r.call(h, &["kickstart", "-k", &h.target(&u.name)], "restarting"),
                    false => r.call(h, &["bootstrap", &h.domain(), &u.path.display().to_string()], "loading"),
                };
                ok && did
            })
        }
    };
    if ok {
        r.lines.push(format!("restarted {}", names.join(", ")));
        rec.version = Some(VERSION.to_string());
        if let Err(e) = rec.save(&h.base) {
            r.fail(format!("services: {e}"));
        }
    }
    r
}

/// `borg service remove`: off recorded, every borg unit stopped and removed (the engineer's `.bak`
/// stays). A manager that is not this HOME's (or none) is not asked: the files go, nothing is stopped.
fn remove(h: &Host, rec: &mut Record) -> Report {
    let mut r = Report::default();
    rec.off = true;
    if let Err(e) = rec.save(&h.base) {
        r.fail(format!("services: {e}"));
        return r;
    }
    let files = orphans(h, &[]);
    if files.is_empty() {
        r.lines.push("services: none installed".into());
    }
    let manager = match h.reach() {
        Ok(()) => true,
        Err(why) if !files.is_empty() => {
            r.lines.push(format!("services: nothing stopped, only the files removed ({why})"));
            false
        }
        Err(_) => false,
    };
    take_down(h, &files, "", manager, &mut r);
    r.lines.push("services: off; updates leave them off, `borg service install` turns them on again".into());
    r
}

/// The checks `borg doctor` and `borg service status` print; read-only.
fn checks_on(h: &Host, wanted: Result<Vec<Service>, String>) -> Vec<(Level, String)> {
    if Record::read(&h.base).off {
        return vec![(Level::Ok, "services: off (borg service remove); `borg service install` turns them on".into())];
    }
    if let Some(why) = h.dev() {
        return vec![(Level::Ok, format!("services: none, {why}"))];
    }
    if let Err(why) = h.reach() {
        return vec![(Level::Warn, format!("services: {why}"))];
    }
    // An invalid config is a FAIL of its own (install.rs `collective_checks`); nothing to compare.
    let Ok(wanted) = wanted else { return Vec::new() };
    let units = units(h, &wanted);
    // Only borg's own units are asked about: none installed, nothing to ask.
    let names: Vec<&str> = units.iter().filter(|u| u.now.as_deref().is_some_and(owned)).map(|u| u.name.as_str()).collect();
    let states = h.states(&names);
    let mut out = Vec::new();
    for u in &units {
        let (n, p) = (&u.name, u.path.display());
        out.push(match u.now.as_deref() {
            None => (Level::Warn, format!("service {n}: not installed, so {} does not run as a service (borg service install)", u.service.what())),
            Some(t) if !owned(t) => (Level::Warn, format!("service {n}: {p} was not written by borg (borg service install takes it over, keeping a .bak)")),
            Some(_) if u.changed() => (Level::Warn, format!("service {n}: {p} is another borg's or was edited (borg service install)")),
            Some(_) => match states.get(n.as_str()) {
                Some(s) if s.running => (Level::Ok, format!("service {n}: {} runs", u.service.what())),
                Some(s) if s.looping => (
                    Level::Warn,
                    format!(
                        "service {n}: {} keeps failing, restarted {} times so far; `journalctl --user -u {n}` says why (often {} started by hand holds what it needs: stop that one)",
                        u.service.what(),
                        s.restarts,
                        u.service.by_hand()
                    ),
                ),
                s => (Level::Warn, format!("service {n}: {} (borg service restart)", s.map_or("not loaded", |s| s.active.as_str()))),
            },
        });
        let b = bak(&u.path);
        if b.exists() {
            out.push((Level::Ok, format!("service {n}: the unit borg took over is kept at {}", b.display())));
        }
    }
    for (n, p) in orphans(h, &units) {
        out.push((
            Level::Warn,
            format!("service {n}: {} names no wanted service (a collective gone, or not a member now; borg service install removes it)", p.display()),
        ));
    }
    if h.backend == Backend::Systemd
        && !names.is_empty()
        && let Some(o) = (h.run)("loginctl", &["show-user", &h.uid.to_string(), "--property=Linger"])
        && o.stdout.trim() == "Linger=no"
    {
        out.push((Level::Ok, "services: they stop when you log out (Linger=no); `loginctl enable-linger` keeps them running".into()));
    }
    out
}

// ---------------------------------------------------------------- entry points

/// The services this machine wants, or why that cannot be told.
fn wanted_here(base: &Path) -> Result<Vec<Service>, String> {
    collective::load().map(|cfg| wanted(cfg.as_ref(), base))
}

/// `borg doctor`'s service lines (read-only).
#[must_use]
pub fn checks() -> Vec<(Level, String)> {
    match Host::from_env(&system) {
        Err(why) => vec![(Level::Ok, format!("services: none, {why}"))],
        Ok(h) => {
            let wanted = wanted_here(&h.base);
            checks_on(&h, wanted)
        }
    }
}

/// Whether `borg service remove` turned the services off on this machine.
#[must_use]
pub fn off() -> bool {
    Record::read(&collective::root_of(&crate::learn::home())).off
}

/// The process of the `borg web` service, if it runs.
#[must_use]
pub fn web_pid() -> Option<u32> {
    Host::from_env(&system).ok()?.web_pid()
}

/// `borg setup` and `borg install <h> --apply`: the services installed and refreshed, unless they are
/// off or cannot be here, each said in a line. Whether they run now; never a failure of the caller.
pub fn auto() -> bool {
    let h = match Host::from_env(&system) {
        Ok(h) => h,
        Err(why) => {
            println!("services: none, {why}");
            return false;
        }
    };
    let mut rec = Record::read(&h.base);
    if rec.off {
        println!("services: off (borg service remove); `borg service install` turns them on");
        return false;
    }
    let r = ensure(&h, &mut rec);
    for l in &r.lines {
        println!("{l}");
    }
    !r.failed && !r.skipped
}

/// `install` where it can run: not a development build, a reachable manager, a valid config.
fn ensure(h: &Host, rec: &mut Record) -> Report {
    if let Some(why) = h.dev() {
        return Report::skip(format!("services: none, {why}"));
    }
    if let Err(why) = h.reach() {
        return Report::skip(format!("services: {why}"));
    }
    match wanted_here(&h.base) {
        Ok(w) => install(h, &w, rec),
        Err(e) => Report::failure(format!("services: left as they are, the collectives config is invalid ({e})")),
    }
}

/// `borg service [status|install|restart|remove]`; the exit code.
#[must_use]
pub fn main(args: &[&str]) -> i32 {
    let verb = match args {
        [] => "status",
        [v] if SUBS.contains(v) => v,
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let h = match Host::from_env(&system) {
        Ok(h) => h,
        Err(why) => {
            println!("services: none, {why}");
            return 0;
        }
    };
    let mut rec = Record::read(&h.base);
    let r = match verb {
        "status" => {
            let kind = match h.backend {
                Backend::Systemd => "systemd user units",
                Backend::Launchd => "launchd agents",
            };
            let by = rec.version.as_deref().map_or_else(|| "never (re)started by borg".to_string(), |v| format!("last (re)started by borg {v}"));
            println!("services: {kind} in {}; {by}", h.dir.display());
            let checks = checks_on(&h, wanted_here(&h.base));
            for (l, m) in &checks {
                println!("{} {m}", l.tag());
            }
            return i32::from(checks.iter().any(|(l, _)| *l == Level::Fail));
        }
        // On before anything else: a step that fails, or an install that cannot run here, must not
        // leave `off` on disk (updates would leave the services alone).
        "install" if rec.off => {
            rec.off = false;
            match rec.save(&h.base) {
                Ok(()) => ensure(&h, &mut rec),
                Err(e) => Report::failure(format!("services: {e}")),
            }
        }
        "install" => ensure(&h, &mut rec),
        "restart" if rec.off => Report::skip("services: off (borg service remove); `borg service install` turns them on".into()),
        "restart" => match (h.dev(), h.reach(), wanted_here(&h.base)) {
            (Some(why), _, _) => Report::skip(format!("services: none, {why}")),
            (_, Err(why), _) => Report::skip(format!("services: {why}")),
            (_, _, Err(e)) => Report::failure(format!("services: the collectives config is invalid ({e})")),
            (None, Ok(()), Ok(w)) => restart(&h, &w, &mut rec),
        },
        _ => remove(&h, &mut rec),
    };
    for l in &r.lines {
        if r.failed && !l.starts_with("service") {
            eprintln!("borg service: {l}");
        } else {
            println!("{l}");
        }
    }
    i32::from(r.failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-service-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A stub manager: logs each call, answers from `running`/`enabled`/`loaded` sets it keeps as the
    /// real one would. `looping` units are in systemd's crash loop; the verbs in `fail` fail; a launchd
    /// label booted out stays loaded for `linger` more `print`s, and a `bootstrap` of it fails until then.
    #[derive(Default)]
    struct Stub {
        log: RefCell<Vec<String>>,
        running: RefCell<Vec<String>>,
        enabled: RefCell<Vec<String>>,
        looping: RefCell<Vec<String>>,
        fail: RefCell<Vec<&'static str>>,
        linger: Cell<u32>,
        leaving: RefCell<Option<(String, u32)>>,
        no_bus: bool,
        home: PathBuf,
    }

    impl Stub {
        fn run(&self, prog: &str, args: &[&str]) -> Option<Out> {
            self.log.borrow_mut().push(format!("{prog} {}", args.join(" ")));
            let ok = |stdout: String| Some(Out { ok: true, stdout, stderr: String::new() });
            let no = |stderr: &str| Some(Out { ok: false, stdout: String::new(), stderr: stderr.into() });
            let (mut run, mut en) = (self.running.borrow_mut(), self.enabled.borrow_mut());
            let names = || args.iter().filter(|a| !a.starts_with('-') && a.contains('.')).map(|a| a.to_string()).collect::<Vec<_>>();
            let verb = if prog == "systemctl" { args.get(1) } else { args.first() };
            if verb.is_some_and(|v| self.fail.borrow().contains(v)) {
                return no("Bootstrap failed: 5: Input/output error");
            }
            let label = || args.get(1).and_then(|a| a.rsplit('/').next()).unwrap_or_default().to_string();
            match (prog, verb.copied()) {
                ("systemctl", Some("show-environment")) if self.no_bus => {
                    Some(Out { ok: false, stdout: String::new(), stderr: "Failed to connect to bus".into() })
                }
                ("systemctl", Some("show-environment")) => ok(format!("HOME={}\n", self.home.display())),
                ("systemctl", Some("show")) => ok(names()
                    .iter()
                    .map(|n| {
                        let (a, sub, restarts) = match (run.contains(n), self.looping.borrow().contains(n)) {
                            (true, _) => ("active", "running", 0),
                            (false, true) => ("activating", "auto-restart", 7),
                            (false, false) => ("inactive", "dead", 0),
                        };
                        let e = if en.contains(n) { "enabled" } else { "disabled" };
                        let pid = if run.contains(n) { 42 } else { 0 };
                        format!("MainPID={pid}\nId={n}\nActiveState={a}\nSubState={sub}\nNRestarts={restarts}\nUnitFileState={e}\n\n")
                    })
                    .collect()),
                ("systemctl", Some("enable")) => {
                    en.extend(names());
                    ok(String::new())
                }
                ("systemctl", Some("start" | "restart")) => {
                    let names = names();
                    self.looping.borrow_mut().retain(|n| !names.contains(n));
                    run.extend(names);
                    ok(String::new())
                }
                ("systemctl", Some("disable")) => {
                    let gone = names();
                    run.retain(|n| !gone.contains(n));
                    en.retain(|n| !gone.contains(n));
                    ok(String::new())
                }
                ("launchctl", Some("managername")) => ok("Aqua\n".into()),
                ("launchctl", Some("print")) => {
                    let label = label();
                    let mut leaving = self.leaving.borrow_mut();
                    match leaving.as_mut() {
                        Some((l, 0)) if *l == label => *leaving = None,
                        Some((l, left)) if *l == label => {
                            *left -= 1;
                            return ok("\tstate = stopping\n".into());
                        }
                        _ => {}
                    }
                    run.contains(&label).then(|| Out { ok: true, stdout: "\tstate = running\n\tpid = 7\n".into(), stderr: String::new() })
                }
                ("launchctl", Some("bootstrap")) => {
                    let label = Path::new(args[2]).file_stem().unwrap().to_string_lossy().into_owned();
                    if self.leaving.borrow().as_ref().is_some_and(|(l, _)| *l == label) {
                        return no("Bootstrap failed: 5: Input/output error");
                    }
                    run.push(label);
                    ok(String::new())
                }
                ("launchctl", Some("bootout")) => {
                    let label = label();
                    run.retain(|n| *n != label);
                    if self.linger.get() > 0 {
                        *self.leaving.borrow_mut() = Some((label, self.linger.get()));
                    }
                    ok(String::new())
                }
                _ => ok(String::new()),
            }
        }

        /// The calls that change something (no queries).
        fn changes(&self) -> Vec<String> {
            let reads = ["show-environment", " show ", "managername", " print ", "loginctl"];
            self.log.borrow().iter().filter(|l| !reads.iter().any(|r| l.contains(r))).cloned().collect()
        }
    }

    fn host<'a>(backend: Backend, root: &Path, run: Run<'a>) -> Host<'a> {
        let dir = match backend {
            Backend::Systemd => root.join("home/.config/systemd/user"),
            Backend::Launchd => root.join("home/Library/LaunchAgents"),
        };
        Host {
            backend,
            home: root.join("home"),
            dir,
            base: root.join("store"),
            bin: PathBuf::from("/opt/borg/bin/borg"),
            path: "/usr/bin:/bin".into(),
            env: Vec::new(),
            uid: 501,
            session: true,
            run,
        }
    }

    fn corp() -> Vec<Service> {
        vec![Service::Mesh("corp".into()), Service::Web]
    }

    #[test]
    fn units_render_with_the_marker_the_command_and_the_path() {
        let unit = systemd_unit(&Service::Mesh("corp".into()), Path::new("/opt/b dir/borg"), "/opt/b dir:/usr/bin", &[]);
        assert!(owned(&unit), "{unit}");
        assert!(unit.starts_with("# managed by borg: rewritten by borg; edits are lost\n"), "{unit}");
        assert!(owned("# managed by borg 0.8.3: rewritten by borg; edits are lost\n[Unit]\n"), "an older borg's file is borg's");
        assert!(unit.contains("\nExecStart=\"/opt/b dir/borg\" mesh serve --collective corp\n"), "{unit}");
        assert!(unit.contains("\nEnvironment=\"PATH=/opt/b dir:/usr/bin\"\n"), "{unit}");
        for line in ["KillSignal=SIGINT", "TimeoutStopSec=30", "Restart=on-failure", "RestartSec=10", "After=network-online.target", "WantedBy=default.target"]
        {
            assert!(unit.lines().any(|l| l == line), "{line}: {unit}");
        }
        assert_eq!(systemd_path(&unit, Path::new("/home/ann")).as_deref(), Some("/opt/b dir:/usr/bin"), "read back");
        let web = systemd_unit(&Service::Web, Path::new("/b/borg"), "/b", &[]);
        assert!(web.contains("\nExecStart=/b/borg web\nWorkingDirectory=%h\n") && web.contains("KillSignal=SIGINT"), "{web}");
        // A name that is not a plain word is quoted, its specifiers and variables escaped.
        assert_eq!(sd_word("a b%c$d\"e", true), "\"a b%%c$$d\\\"e\"");
        assert_eq!(unescape("PATH=/a%%b \\\"c\\\\ \\x07"), "PATH=/a%%b \"c\\ \u{7}");
        // A hand-written unit's bare PATH.
        let home = Path::new("/home/ann");
        assert_eq!(systemd_path("[Service]\nEnvironment=PATH=/home/ann/.cargo/bin:/usr/bin\n", home).as_deref(), Some("/home/ann/.cargo/bin:/usr/bin"));
        // A path borg wrote with a `%` in it reads back as written.
        let odd = systemd_unit(&Service::Web, Path::new("/b/borg"), "/b:/x%y", &[]);
        assert_eq!(systemd_path(&odd, home).as_deref(), Some("/b:/x%y"), "{odd}");
        assert!(!owned("[Unit]\nDescription=mine\n"));
    }

    #[test]
    fn a_plist_has_the_agent_settings_and_escapes_its_strings() {
        let p = plist("borg.mesh.a&b", &Service::Mesh("a&b".into()), Path::new("/Users/ann/bin/borg"), "/Users/ann/bin:/usr/bin", &[], Path::new("/Users/ann"));
        assert!(owned(&p), "{p}");
        assert!(p.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- managed by borg: "), "the declaration stays first: {p}");
        assert!(p.contains("<string>/Users/ann/bin/borg</string>\n    <string>mesh</string>\n    <string>serve</string>\n    <string>--collective</string>\n    <string>a&amp;b</string>"), "{p}");
        assert!(p.contains("<key>RunAtLoad</key>\n  <true/>") && p.contains("<key>SuccessfulExit</key>\n    <false/>"), "{p}");
        assert!(p.contains("<string>/Users/ann/Library/Logs/borg/borg.mesh.a&amp;b.log</string>"), "{p}");
        assert!(p.contains("<key>WorkingDirectory</key>\n  <string>/Users/ann</string>"), "{p}");
        assert_eq!(plist_path(&p).as_deref(), Some("/Users/ann/bin:/usr/bin"));
        assert_eq!(Backend::Launchd.named("borg.mesh.corp.plist"), Some("borg.mesh.corp"));
        assert_eq!(Backend::Launchd.named("borg.web.plist.bak"), None);
        assert_eq!(Backend::Systemd.named("borg-mesh-corp.service"), Some("borg-mesh-corp.service"));
        assert_eq!(Backend::Systemd.named("borg-mesh-.service"), None);
        assert_eq!(Backend::Systemd.named("borg-web.service.bak"), None);
    }

    #[test]
    fn the_path_puts_borg_first_once() {
        assert_eq!(unit_path(Path::new("/o/bin/borg"), "/usr/bin:/o/bin::/bin"), "/o/bin:/usr/bin:/bin");
        assert_eq!(unit_path(Path::new("/o/bin/borg"), ""), "/o/bin");
        // A relative entry would resolve against the service's working directory.
        assert_eq!(unit_path(Path::new("/o/bin/borg"), ".:bin:%h/x:/usr/bin"), "/o/bin:/usr/bin");
    }

    #[test]
    fn wanted_is_a_mesh_per_team_collective_with_an_identity_then_web() {
        let base = tmp("wanted");
        let cfg: collective::Config = serde_json::from_str(
            r#"{"collectives":[{"name":"corp","paths":["/w"]},{"name":"idle","paths":["/i"]},{"name":"mine","paths":["/p"],"share":"local"}]}"#,
        )
        .unwrap();
        for c in ["corp", "mine", "idle"] {
            let mesh = collective::store(&base, c).join("mesh");
            std::fs::create_dir_all(&mesh).unwrap();
            std::fs::write(mesh.join("node.key"), "k").unwrap();
            if c != "idle" {
                std::fs::write(mesh.join("membership.json"), "{}").unwrap();
            }
        }
        // idle has a key (`borg mesh id`, a failed join) but no membership: its daemon would exit.
        assert_eq!(wanted(Some(&cfg), &base), corp(), "idle is no member yet, mine is local");
        assert_eq!(wanted(None, &base), [Service::Web], "default without a mesh");
        let none: collective::Config = serde_json::from_str(r#"{"collectives":[]}"#).unwrap();
        assert_eq!(wanted(Some(&none), &base), [], "no collective: borg web would exit");
        assert_eq!(Backend::Systemd.name(&Service::Mesh("a b".into())), format!("borg-mesh-{}.service", id("a b")));
        assert!(id("a b").starts_with("a_b-"), "a safe word");
    }

    #[test]
    fn restart_follows_a_changed_file_or_another_version() {
        assert_eq!(act(false, false, false), Act::Start);
        assert_eq!(act(false, true, true), Act::Start, "not running: started, never restarted");
        assert_eq!(act(true, false, false), Act::Keep);
        assert_eq!(act(true, true, false), Act::Restart);
        assert_eq!(act(true, false, true), Act::Restart);
    }

    #[test]
    fn install_takes_over_once_then_does_nothing_and_restarts_on_another_version() {
        let root = tmp("takeover");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        std::fs::create_dir_all(&h.dir).unwrap();
        let mine = "[Service]\nExecStart=%h/.cargo/bin/borg web\nEnvironment=PATH=/home/ann/bin:/usr/bin\n";
        std::fs::write(h.dir.join("borg-web.service"), mine).unwrap();
        stub.running.borrow_mut().push("borg-web.service".into());
        stub.enabled.borrow_mut().push("borg-web.service".into());
        let mut rec = Record::default();
        let r = install(&h, &corp(), &mut rec);
        assert!(!r.failed, "{:?}", r.lines);
        assert_eq!(std::fs::read_to_string(h.dir.join("borg-web.service.bak")).unwrap(), mine, "the engineer's unit kept");
        let web = std::fs::read_to_string(h.dir.join("borg-web.service")).unwrap();
        assert!(owned(&web) && web.contains("\nEnvironment=PATH=/opt/borg/bin:/home/ann/bin:/usr/bin\n"), "its PATH kept, borg first: {web}");
        assert_eq!(
            stub.changes(),
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable borg-mesh-corp.service",
                "systemctl --user start borg-mesh-corp.service",
                "systemctl --user restart borg-web.service",
            ]
        );
        assert_eq!(Record::read(&h.base), Record { version: Some(VERSION.into()), off: false });
        // Again: nothing written, nothing started.
        stub.log.borrow_mut().clear();
        let r = install(&h, &corp(), &mut rec);
        assert!(
            stub.changes().is_empty() && r.lines == ["services: borg-mesh-corp.service, borg-web.service up to date"],
            "{:?} {:?}",
            stub.changes(),
            r.lines
        );
        // The engineer writes a unit of their own again: replaced, the first .bak kept.
        std::fs::write(h.dir.join("borg-web.service"), "[Service]\nExecStart=/x\n").unwrap();
        install(&h, &corp(), &mut rec);
        assert_eq!(std::fs::read_to_string(h.dir.join("borg-web.service.bak")).unwrap(), mine, "the .bak is made once");
        // Another borg last (re)started them: both restart, files unchanged.
        Record { version: Some("0.0.1".into()), off: false }.save(&h.base).unwrap();
        let mut rec = Record::read(&h.base);
        stub.log.borrow_mut().clear();
        install(&h, &corp(), &mut rec);
        assert_eq!(stub.changes(), ["systemctl --user restart borg-mesh-corp.service borg-web.service"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_removes_borg_units_no_collective_wants_and_leaves_others() {
        let root = tmp("orphans");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        let mut rec = Record::default();
        install(&h, &corp(), &mut rec);
        std::fs::write(h.dir.join("borg-mesh-theirs.service"), "[Service]\nExecStart=/x\n").unwrap();
        stub.log.borrow_mut().clear();
        let r = install(&h, &[Service::Web], &mut rec);
        assert!(!h.dir.join("borg-mesh-corp.service").exists(), "{:?}", r.lines);
        assert!(h.dir.join("borg-mesh-theirs.service").exists(), "not borg's: left alone");
        assert_eq!(stub.changes(), ["systemctl --user disable --now borg-mesh-corp.service", "systemctl --user daemon-reload"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_turns_them_off_and_keeps_the_backup() {
        let root = tmp("remove");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        std::fs::create_dir_all(&h.dir).unwrap();
        std::fs::write(h.dir.join("borg-web.service"), "[Service]\nExecStart=/mine\n").unwrap();
        let mut rec = Record::default();
        install(&h, &corp(), &mut rec);
        stub.log.borrow_mut().clear();
        let r = remove(&h, &mut rec);
        assert!(!r.failed, "{:?}", r.lines);
        assert!(Record::read(&h.base).off);
        assert_eq!(stub.changes(), ["systemctl --user disable --now borg-mesh-corp.service borg-web.service", "systemctl --user daemon-reload"]);
        let left: Vec<String> = std::fs::read_dir(&h.dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["borg-web.service.bak"]);
        assert!(r.lines.iter().any(|l| l.contains("borg-web.service.bak")), "{:?}", r.lines);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn launchd_loads_reloads_a_changed_agent_and_kickstarts_on_another_version() {
        let root = tmp("launchd");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Launchd, &root, &run);
        let mut rec = Record::default();
        install(&h, &corp(), &mut rec);
        let plist = |l: &str| h.dir.join(format!("{l}.plist")).display().to_string();
        assert_eq!(
            stub.changes(),
            [format!("launchctl bootstrap gui/501 {}", plist("borg.mesh.corp")), format!("launchctl bootstrap gui/501 {}", plist("borg.web"))]
        );
        assert!(logs(&h.home).is_dir(), "the log directory exists for launchd to write into");
        stub.log.borrow_mut().clear();
        install(&h, &corp(), &mut rec);
        assert!(stub.changes().is_empty(), "{:?}", stub.changes());
        // A changed agent: unloaded, loaded again.
        std::fs::write(h.dir.join("borg.web.plist"), "<plist>mine</plist>").unwrap();
        stub.log.borrow_mut().clear();
        install(&h, &corp(), &mut rec);
        assert_eq!(stub.changes(), ["launchctl bootout gui/501/borg.web".to_string(), format!("launchctl bootstrap gui/501 {}", plist("borg.web"))]);
        Record { version: Some("0.0.1".into()), off: false }.save(&h.base).unwrap();
        let mut rec = Record::read(&h.base);
        stub.log.borrow_mut().clear();
        install(&h, &corp(), &mut rec);
        assert_eq!(stub.changes(), ["launchctl kickstart -k gui/501/borg.mesh.corp", "launchctl kickstart -k gui/501/borg.web"]);
        assert_eq!(h.web_pid(), Some(7));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_user_bus_and_another_home_install_nothing() {
        let root = tmp("nobus");
        let stub = Stub { home: root.join("home"), no_bus: true, ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        assert!(h.reach().unwrap_err().contains("no systemd user session"));
        let checks = checks_on(&h, Ok(corp()));
        assert!(matches!(checks.as_slice(), [(Level::Warn, m)] if m.contains("no systemd user session")), "{checks:?}");
        let other = Stub { home: root.join("elsewhere"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| other.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        assert!(h.reach().unwrap_err().contains("services left alone"), "a test's HOME never reaches the real manager");
        // No XDG_RUNTIME_DIR: systemctl --user could not connect, so it is not even asked.
        other.log.borrow_mut().clear();
        let h = Host { session: false, ..host(Backend::Systemd, &root, &run) };
        assert!(h.reach().unwrap_err().contains("XDG_RUNTIME_DIR"));
        assert!(other.log.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn checks_name_missing_stale_foreign_and_orphaned_units() {
        let root = tmp("checks");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        let warns = |c: &[(Level, String)]| c.iter().filter(|(l, _)| *l == Level::Warn).map(|(_, m)| m.clone()).collect::<Vec<_>>();
        let c = checks_on(&h, Ok(corp()));
        assert!(warns(&c).len() == 2 && warns(&c).iter().all(|m| m.contains("not installed") && m.contains("borg service install")), "{c:?}");
        let mut rec = Record::default();
        install(&h, &corp(), &mut rec);
        assert!(warns(&checks_on(&h, Ok(corp()))).is_empty());
        std::fs::write(h.dir.join("borg-web.service"), std::fs::read_to_string(h.dir.join("borg-web.service")).unwrap() + "# edited\n").unwrap();
        let w = warns(&checks_on(&h, Ok(corp())));
        assert!(matches!(w.as_slice(), [m] if m.contains("edited")), "{w:?}");
        let w = warns(&checks_on(&h, Ok(vec![Service::Web])));
        assert!(w.iter().any(|m| m.contains("borg-mesh-corp.service") && m.contains("removes it")), "{w:?}");
        let mut off = Record::read(&h.base);
        off.off = true;
        off.save(&h.base).unwrap();
        assert!(matches!(checks_on(&h, Ok(corp())).as_slice(), [(Level::Ok, m)] if m.contains("off")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_development_build_installs_none() {
        let root = tmp("dev");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let mut h = host(Backend::Systemd, &root, &run);
        h.bin = PathBuf::from("/src/borg/target/release/borg");
        let r = ensure(&h, &mut Record::default());
        assert!(r.lines[0].contains("development build") && !r.failed, "{:?}", r.lines);
        assert!(stub.log.borrow().is_empty() && !h.dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_record_reads_what_it_wrote_and_tolerates_none() {
        let d = tmp("record");
        assert_eq!(Record::read(&d), Record::default());
        let r = Record { version: Some("1.2.3".into()), off: true };
        r.save(&d).unwrap();
        assert_eq!(Record::read(&d), r);
        std::fs::write(d.join("services.json"), "{\"off\": true, \"later\": 1}").unwrap();
        assert_eq!(Record::read(&d), Record { version: None, off: true }, "fields a later borg adds are ignored");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The managers' answers as they print them (systemd 257's `show`, launchd's `print`).
    #[test]
    fn manager_answers_parse() {
        let shown = "MainPID=1051993\nId=borg-web.service\nActiveState=active\nUnitFileState=enabled\n\nMainPID=0\nId=borg-mesh-x.service\nActiveState=inactive\nUnitFileState=\n";
        let s = systemd_states(shown);
        assert_eq!(s["borg-web.service"], State { active: "active".into(), running: true, enabled: true, pid: Some(1_051_993), ..State::default() });
        assert_eq!(s["borg-mesh-x.service"], State { active: "inactive".into(), running: false, enabled: false, pid: None, ..State::default() });
        let printed = "gui/501/borg.web = {\n\tactive count = 1\n\tpath = /Users/ann/Library/LaunchAgents/borg.web.plist\n\tstate = running\n\n\tprogram = /Users/ann/.local/bin/borg\n\tpid = 812\n}\n";
        assert_eq!(launchd_state(printed), State { active: "running".into(), running: true, enabled: true, pid: Some(812), ..State::default() });
    }

    /// Systemd's crash loop: `activating` between the starts of a unit that keeps failing.
    #[test]
    fn a_crash_loop_is_not_running() {
        let shown = "Id=borg-web.service\nActiveState=activating\nSubState=auto-restart\nNRestarts=12\nUnitFileState=enabled\nMainPID=0\n";
        let s = &systemd_states(shown)["borg-web.service"];
        assert!(!s.running && s.looping && s.restarts == 12, "{s:?}");
        // A unit that starts (no failure yet) is not a loop either.
        let starting = "Id=borg-web.service\nActiveState=activating\nSubState=start\nNRestarts=0\nUnitFileState=enabled\n";
        assert!(!systemd_states(starting)["borg-web.service"].looping);
    }

    #[test]
    fn checks_name_a_crash_loop_and_where_to_look() {
        let root = tmp("loop-checks");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        install(&h, &corp(), &mut Record::default());
        stub.running.borrow_mut().retain(|n| n != "borg-web.service");
        stub.looping.borrow_mut().push("borg-web.service".into());
        let c = checks_on(&h, Ok(corp()));
        let web: Vec<&(Level, String)> = c.iter().filter(|(_, m)| m.contains("borg-web.service")).collect();
        assert!(
            matches!(web.as_slice(), [(Level::Warn, m)] if m.contains("keeps failing") && m.contains("restarted 7 times") && m.contains("journalctl --user -u borg-web.service") && m.contains("a `borg web`")),
            "{c:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_starts_a_unit_in_a_crash_loop_instead_of_keeping_it() {
        let root = tmp("loop-install");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        let mut rec = Record::default();
        install(&h, &corp(), &mut rec);
        stub.running.borrow_mut().retain(|n| n != "borg-web.service");
        stub.looping.borrow_mut().push("borg-web.service".into());
        stub.log.borrow_mut().clear();
        install(&h, &corp(), &mut rec);
        assert_eq!(stub.changes(), ["systemctl --user start borg-web.service"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn units_carry_the_store_config_and_relay_they_were_decided_for() {
        let root = tmp("env");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let mut h = host(Backend::Systemd, &root, &run);
        h.env = vec![("BORG_HOME", "/s/store".into()), ("BORG_CONFIG", "/s/my config.json".into()), ("BORG_RELAY", "https://relay.example.com".into())];
        let mut rec = Record::default();
        install(&h, &corp(), &mut rec);
        let mesh = std::fs::read_to_string(h.dir.join("borg-mesh-corp.service")).unwrap();
        for line in [
            "Environment=PATH=/opt/borg/bin:/usr/bin:/bin",
            "Environment=BORG_HOME=/s/store",
            "Environment=\"BORG_CONFIG=/s/my config.json\"",
            "Environment=BORG_RELAY=https://relay.example.com",
        ] {
            assert!(mesh.lines().any(|l| l == line), "{line}: {mesh}");
        }
        assert_eq!(systemd_path(&mesh, &h.home).as_deref(), Some("/opt/borg/bin:/usr/bin:/bin"), "PATH still reads back");
        // Another store: the files change, so the services restart on it.
        h.env.truncate(1);
        h.env[0].1 = "/t/store".into();
        stub.log.borrow_mut().clear();
        install(&h, &corp(), &mut rec);
        let mesh = std::fs::read_to_string(h.dir.join("borg-mesh-corp.service")).unwrap();
        assert!(mesh.contains("\nEnvironment=BORG_HOME=/t/store\n") && !mesh.contains("BORG_CONFIG"), "{mesh}");
        assert_eq!(stub.changes(), ["systemctl --user daemon-reload", "systemctl --user restart borg-mesh-corp.service borg-web.service"]);
        let p = plist("borg.web", &Service::Web, Path::new("/b/borg"), "/b", &[("BORG_CONFIG", "/s/a&b.json".into())], Path::new("/Users/ann"));
        assert!(
            p.contains("<dict>\n    <key>PATH</key>\n    <string>/b</string>\n    <key>BORG_CONFIG</key>\n    <string>/s/a&amp;b.json</string>\n  </dict>"),
            "{p}"
        );
        assert_eq!(plist_path(&p).as_deref(), Some("/b"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_taken_over_units_specifiers_are_expanded_not_kept() {
        let root = tmp("specifiers");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        std::fs::create_dir_all(&h.dir).unwrap();
        std::fs::write(
            h.dir.join("borg-web.service"),
            "[Service]\nExecStart=%h/.cargo/bin/borg web\nEnvironment=PATH=%h/.cargo/bin:/usr/bin:/srv/%u/bin:/a%%b\n",
        )
        .unwrap();
        install(&h, &corp(), &mut Record::default());
        let web = std::fs::read_to_string(h.dir.join("borg-web.service")).unwrap();
        let path = format!("/opt/borg/bin:{}/.cargo/bin:/usr/bin:/a%b", h.home.display());
        assert_eq!(systemd_path(&web, &h.home).as_deref(), Some(path.as_str()), "{web}");
        assert!(!web.contains("%%h") && !web.contains("%u"), "no specifier kept as text: {web}");
        // Quoted, as systemd reads it too: unquoted first, then the specifiers.
        assert_eq!(systemd_path("Environment=\"PATH=%h/bin:/x\"\n", Path::new("/home/ann")).as_deref(), Some("/home/ann/bin:/x"));
        assert_eq!(specifiers("/a%", "/h"), None, "a lone % is no value borg can tell");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_step_still_saves_the_record_and_the_version_follows_the_restarts() {
        let root = tmp("partial");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        // An enable that fails: the services were still started by this borg.
        stub.fail.borrow_mut().push("enable");
        let r = install(&h, &corp(), &mut Record::default());
        assert!(r.failed, "{:?}", r.lines);
        assert_eq!(Record::read(&h.base), Record { version: Some(VERSION.into()), off: false });
        // A restart that fails after `borg service remove` (`off` on disk, `borg service install` turned
        // it on in memory): the record is saved, its version left so the next install tries again.
        Record { version: Some("0.0.1".into()), off: true }.save(&h.base).unwrap();
        stub.fail.borrow_mut().push("restart");
        let mut rec = Record { version: Some("0.0.1".into()), off: false };
        let r = install(&h, &corp(), &mut rec);
        assert!(r.failed, "{:?}", r.lines);
        assert_eq!(Record::read(&h.base), Record { version: Some("0.0.1".into()), off: false });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn launchd_waits_for_a_booted_out_agent_before_loading_it_again() {
        let root = tmp("linger");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Launchd, &root, &run);
        let mut rec = Record::default();
        install(&h, &corp(), &mut rec);
        std::fs::write(h.dir.join("borg.web.plist"), "<plist>mine</plist>").unwrap();
        // launchd holds the label for two more answers while the daemon stops.
        stub.linger.set(2);
        stub.log.borrow_mut().clear();
        let r = install(&h, &corp(), &mut rec);
        assert!(!r.failed, "{:?}", r.lines);
        let prints = stub.log.borrow().iter().filter(|l| l.as_str() == "launchctl print gui/501/borg.web").count();
        assert!(prints >= 3, "asked until launchd let go: {:?}", stub.log.borrow());
        assert_eq!(h.web_pid(), Some(7), "loaded again");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_leaves_another_homes_manager_alone() {
        let root = tmp("remove-other");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        install(&host(Backend::Systemd, &root, &run), &corp(), &mut Record::default());
        // The manager here is another HOME's (a sandbox's HOME on the engineer's session).
        let other = Stub { home: root.join("elsewhere"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| other.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        let r = remove(&h, &mut Record::default());
        assert!(!r.failed, "{:?}", r.lines);
        assert_eq!(other.changes(), Vec::<String>::new(), "nothing stopped");
        assert!(!h.dir.join("borg-web.service").exists() && Record::read(&h.base).off, "{:?}", r.lines);
        assert!(r.lines.iter().any(|l| l.contains("nothing stopped")), "{:?}", r.lines);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_unit_that_cannot_be_written_is_a_failure_not_none_wanted() {
        let root = tmp("unwritable");
        let stub = Stub { home: root.join("home"), ..Stub::default() };
        let run = |p: &str, a: &[&str]| stub.run(p, a);
        let h = host(Backend::Systemd, &root, &run);
        // A directory where each file goes: no write lands.
        for f in ["borg-mesh-corp.service", "borg-web.service"] {
            std::fs::create_dir_all(h.dir.join(f)).unwrap();
        }
        let r = install(&h, &corp(), &mut Record::default());
        assert!(r.failed && !r.skipped, "{:?}", r.lines);
        assert!(!r.lines.iter().any(|l| l.contains("none wanted")), "{:?}", r.lines);
        let _ = std::fs::remove_dir_all(&root);
    }
}
