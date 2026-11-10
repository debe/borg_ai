//! borg-eval: run drone arms over an eval workload and record cost, solve
//! rate and exploration per task.
//!
//! usage: borg-eval --workload eval/trap-ledger [--arm vanilla] [--seeds 3]
//!                  [--model sonnet] [--tasks 4] [--budget-usd 2] [--timeout-s 900]
//!                  [--settings arm.json] [--run NAME]
//!
//! Sequential mode: per seed, one fresh repo; tasks run in order, each in a
//! fresh drone session. Between tasks the harness commits tracked changes and
//! runs `git clean -fdx` (a fresh clone, as far as untracked state goes), and
//! applies `drift/before-NN-*.sh` scripts. Results go to
//! `target/eval-runs/<run>/results.jsonl`; `summary.md` is printed.
//!
//! Subcommands: `report`, `sandbox-exec`, `sandbox-probe`, `net-bridge` (inside a proxied sandbox),
//! `dogfood` (dogfood.rs), and the replay evals when their private modules are present (build.rs).
//!
//! Every number in this crate is an experiment parameter (or a display width): chosen for an eval,
//! stated in its report, never a product default. A per-task `--budget-usd` and `--timeout-s` are
//! the eval's guards against a runaway arm, recorded with each run, not a cost bar the product meets.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use borg_trace::{Kind, claude};
use serde_json::{Value, json};

mod dogfood;
// Replay evals over private data: optional, see build.rs. rustfmt cannot resolve absent module files, so
// `cargo fmt` skips these; scripts/check.sh checks them with rustfmt directly when they are present.
#[cfg(replay_evals)]
#[rustfmt::skip]
mod preempt;
#[cfg(replay_evals)]
#[rustfmt::skip]
mod queen_replay;
#[cfg(replay_evals)]
#[rustfmt::skip]
mod queen_test;

const SYSTEM: &str = "You are working autonomously in a sandboxed repository. Do the task fully without asking questions. \
Do not use plan mode, do not create task-tracking files, and do not commit.";

struct Args {
    workload: PathBuf,
    arm: String,
    seeds: u32,
    model: String,
    tasks: usize,
    budget_usd: String,
    timeout_s: u32,
    settings: Option<PathBuf>,
    run: String,
}

fn args() -> Args {
    let mut a = Args {
        workload: "eval/trap-ledger".into(),
        arm: "vanilla".into(),
        seeds: 1,
        model: "sonnet".into(),
        tasks: usize::MAX,
        budget_usd: "2".into(),
        timeout_s: 900,
        settings: None,
        run: String::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let v = it.next().unwrap_or_default();
        match flag.as_str() {
            "--workload" => a.workload = v.into(),
            "--arm" => a.arm = v,
            "--seeds" => a.seeds = v.parse().expect("--seeds"),
            "--model" => a.model = v,
            "--tasks" => a.tasks = v.parse().expect("--tasks"),
            "--budget-usd" => a.budget_usd = v,
            "--timeout-s" => a.timeout_s = v.parse().expect("--timeout-s"),
            "--settings" => a.settings = Some(fs::canonicalize(v).expect("--settings")),
            "--run" => a.run = v,
            _ => panic!("unknown flag {flag}"),
        }
    }
    if a.run.is_empty() {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        a.run = format!("{}-{}-{secs}", a.arm, a.model);
    }
    a.workload = fs::canonicalize(&a.workload).expect("--workload");
    a
}

pub(crate) fn sh(dir: &Path, cmd: &str) -> (bool, String) {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .envs([("GIT_AUTHOR_NAME", "eval"), ("GIT_AUTHOR_EMAIL", "eval@borg"), ("GIT_COMMITTER_NAME", "eval"), ("GIT_COMMITTER_EMAIL", "eval@borg")])
        .output()
        .expect("sh");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn sorted_files(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.retain(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix)));
    v.sort();
    v
}

/// Fresh git repo from the workload's starting state.
fn fresh_repo(workload: &Path, work: &Path) {
    let _ = fs::remove_dir_all(work);
    fs::create_dir_all(work.parent().unwrap()).unwrap();
    let (ok, out) = sh(Path::new("/"), &format!("cp -r '{}' '{}'", workload.join("repo").display(), work.display()));
    assert!(ok, "copy repo: {out}");
    let (ok, out) = sh(work, "git init -q && git add -A && git commit -qm init");
    assert!(ok, "git init: {out}");
}

/// The eval sandbox: the system read-only but NOT the user's home. No original repos
/// (future commits), no transcripts (the corrections), no eval data (criteria, later lessons),
/// no git/gh/ssh credentials. Only the claude binary, its credentials (none with the model proxy), a
/// sanitized `~/.claude.json`, `cwd`, `readonly` and `writable` are bound. Returns the bwrap command up to
/// `--chdir cwd --` (append the program) and a private dir to remove afterwards.
#[cfg(replay_evals)]
pub(crate) fn sandbox(cwd: &Path, writable: &[&Path], readonly: &[&Path], timeout_s: u32) -> (Command, PathBuf) {
    sandbox_with(cwd, writable, readonly, &Extra::default(), timeout_s)
}

/// What some arms add to the plain sandbox.
#[derive(Default, Clone)]
pub(crate) struct Extra {
    /// Read-only binds into the hidden HOME: (source, destination relative to HOME), e.g. an
    /// installed skill at `.claude/skills/<name>`.
    pub home_ro: Vec<(PathBuf, PathBuf)>,
    /// `--setting-sources` for claude (default "project"); user skills load only with "user".
    pub setting_sources: Option<String>,
    /// A persistent HOME of its own (dogfood.rs): bound read-write at its own path and exported as
    /// `$HOME`, so Claude Code reads its *user* settings (hooks) from there and borg resolves its
    /// store and config under it. The real HOME stays hidden (tmpfs) and its credentials are not
    /// bound: the caller keeps a copy outside this HOME and binds it in with `file_rw` for the
    /// sandbox's lifetime only (`dogfood::Secrets`), so no token is left in the persistent HOME and
    /// nothing is written back. The environment is cleared down to `SANDBOX_HOME_ENV`, so no harness
    /// or borg variable of the caller (`CLAUDE_*`, `BORG_*`, `XDG_*`) reaches inside.
    pub sandbox_home: Option<PathBuf>,
    /// Read-write file binds (source, absolute destination), mounted last: e.g. a credentials copy
    /// kept outside `sandbox_home` and visible inside it only while the sandbox runs.
    pub file_rw: Vec<(PathBuf, PathBuf)>,
    /// Variables set inside a sandbox with its own HOME (`sandbox_home`), after the environment is
    /// cleared; ignored otherwise.
    pub env: Vec<(String, String)>,
    /// No network inside (`--unshare-net`): for a scorer that runs agent-written code.
    pub unshare_net: bool,
    /// A PID namespace of its own (`--unshare-pid`): every process started inside (a Gradle daemon an
    /// agent left, a build's orphan) is killed when the sandboxed command ends, and the host's
    /// processes are not visible.
    pub unshare_pid: bool,
}

/// The only variables a sandbox with its own HOME inherits (besides HOME and PATH, which it sets).
pub(crate) const SANDBOX_HOME_ENV: [&str; 4] = ["LANG", "LC_ALL", "TERM", "USER"];

/// The eval's model proxy: `--model-proxy URL` (preempt, sandbox-probe) or `BORG_EVAL_MODEL_PROXY`.
/// When set, every `claude` in a sandbox (drone, judge, expert, and the `claude -p` that borg's hooks
/// start inside it) talks to this Anthropic-compatible endpoint instead of Anthropic: no login of the
/// engineer enters the sandbox (see [`sandbox_with`]), so no call can be billed to it.
static MODEL_PROXY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// The proxy's key unless `BORG_EVAL_MODEL_PROXY_TOKEN` names another: a local LiteLLM master key.
const MODEL_PROXY_TOKEN: &str = "sk-hsg-local";

/// Claude Code's output cap through the proxy: Gemini answers at most 65,536 tokens, and Claude Code
/// asks for more by default (the call then fails).
const MODEL_PROXY_MAX_OUTPUT: &str = "32000";

/// Set the proxy from a flag; before the first sandbox only (a later set would leave some calls direct).
pub(crate) fn set_model_proxy(url: String) {
    let url = url.trim().trim_end_matches('/').to_string();
    assert!(url.starts_with("http://") || url.starts_with("https://"), "--model-proxy {url}: not an http(s) URL");
    if MODEL_PROXY.set(Some(url)).is_err() {
        panic!("--model-proxy given after the first sandbox was built");
    }
}

pub(crate) fn model_proxy() -> Option<&'static str> {
    MODEL_PROXY
        .get_or_init(|| std::env::var("BORG_EVAL_MODEL_PROXY").ok().map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty()))
        .as_deref()
}

/// The environment a sandbox gets with the proxy on: the proxy's address and key, Claude Code's output
/// cap, and no traffic besides the model calls.
fn model_proxy_env(url: &str) -> Vec<(&'static str, String)> {
    let token = std::env::var("BORG_EVAL_MODEL_PROXY_TOKEN").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| MODEL_PROXY_TOKEN.into());
    vec![
        ("ANTHROPIC_BASE_URL", url.to_string()),
        ("ANTHROPIC_AUTH_TOKEN", token),
        ("CLAUDE_CODE_MAX_OUTPUT_TOKENS", MODEL_PROXY_MAX_OUTPUT.into()),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".into()),
        ("DISABLE_TELEMETRY", "1".into()),
    ]
}

/// The port of a plain-http loopback URL (`http://127.0.0.1:4000`, `http://localhost:8891/t/...`): the
/// only kind of address the sandbox's bridge carries.
fn loopback_port(url: &str) -> Result<u16, String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| format!("{url}: only a plain http loopback URL can be bridged into the sandbox"))?;
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    let (host, port) = authority.rsplit_once(':').ok_or_else(|| format!("{url}: needs an explicit port"))?;
    if !matches!(host, "127.0.0.1" | "localhost") {
        return Err(format!("{url}: not a loopback address (127.0.0.1 or localhost)"));
    }
    port.parse().map_err(|_| format!("{url}: bad port"))
}

/// The host ports a proxied sandbox reaches: the model proxy's and the loopback services listed in
/// `BORG_EVAL_NET_ALLOW` (comma-separated loopback URLs), nothing else.
fn net_ports(proxy: &str) -> Vec<u16> {
    let mut ports = vec![loopback_port(proxy).unwrap_or_else(|e| panic!("--model-proxy {e}"))];
    for u in std::env::var("BORG_EVAL_NET_ALLOW").unwrap_or_default().split(',').map(str::trim).filter(|u| !u.is_empty()) {
        ports.push(loopback_port(u).unwrap_or_else(|e| panic!("BORG_EVAL_NET_ALLOW {e}")));
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Copy both ways between a Unix and a TCP connection until both sides are done.
fn splice(unix: std::os::unix::net::UnixStream, tcp: std::net::TcpStream) {
    use std::net::Shutdown;
    let (Ok(mut u_in), Ok(mut t_out)) = (unix.try_clone(), tcp.try_clone()) else { return };
    let (mut u_out, mut t_in) = (unix, tcp);
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut u_in, &mut t_out);
        let _ = t_out.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut t_in, &mut u_out);
    let _ = u_out.shutdown(Shutdown::Write);
    let _ = up.join();
}

/// Host side of a proxied sandbox's network: one Unix socket per allowed port, in a directory of this
/// process (bound into the sandbox), forwarding each connection to `127.0.0.1:<port>` on the host. Built
/// once per port and process; the sandbox itself has no network (`--unshare-net`), so it reaches the
/// host only through these sockets ([`net_bridge`] listens on its own loopback).
fn host_bridge(port: u16) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    static BRIDGES: std::sync::Mutex<std::collections::BTreeMap<u16, PathBuf>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
    let mut bridges = BRIDGES.lock().unwrap();
    if let Some(sock) = bridges.get(&port) {
        return sock.clone();
    }
    let tmp = std::env::temp_dir();
    let dir = tmp.join(format!("borg-eval-net-{}", std::process::id()));
    if bridges.is_empty() {
        // Directories of earlier runs whose process is gone.
        for e in fs::read_dir(&tmp).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(pid) = name.strip_prefix("borg-eval-net-")
                && !Path::new("/proc").join(pid).exists()
            {
                let _ = fs::remove_dir_all(e.path());
            }
        }
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let sock = dir.join(format!("{port}.sock"));
    let _ = fs::remove_file(&sock);
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("network bridge socket");
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || {
                if let Ok(tcp) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                    splice(conn, tcp);
                }
            });
        }
    });
    bridges.insert(port, sock.clone());
    sock
}

/// `borg-eval net-bridge PORT=SOCKET... -- argv`: inside a proxied sandbox (no network of its own), listen on
/// `127.0.0.1:PORT` for each pair and carry every connection to the host through SOCKET ([`host_bridge`]),
/// then run argv and exit with its status. Everything else on the network is unreachable from inside.
fn net_bridge(raw: &[String]) {
    let split = raw.iter().position(|a| a == "--").expect("net-bridge PORT=SOCKET... -- argv");
    for pair in &raw[..split] {
        let (port, sock) = pair.split_once('=').expect("PORT=SOCKET");
        let port: u16 = port.parse().expect("PORT");
        let sock = PathBuf::from(sock);
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| panic!("net-bridge 127.0.0.1:{port}: {e}"));
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let sock = sock.clone();
                std::thread::spawn(move || {
                    if let Ok(unix) = std::os::unix::net::UnixStream::connect(&sock) {
                        splice(unix, conn);
                    }
                });
            }
        });
    }
    let argv = &raw[split + 1..];
    let mut cmd = Command::new(argv.first().expect("net-bridge: no command"));
    cmd.args(&argv[1..]);
    // SAFETY: prctl is async-signal-safe and touches no memory of the parent. Linux only, as bwrap is.
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let status = cmd.status().unwrap_or_else(|e| panic!("net-bridge: {}: {e}", argv[0]));
    use std::os::unix::process::ExitStatusExt as _;
    std::process::exit(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(1)));
}

/// The caller's variables a proxied sandbox must not inherit: every `ANTHROPIC_*` and `CLAUDE*` (an API
/// key, an OAuth token, a Bedrock/Vertex switch, a config dir pointing at the real login).
fn proxy_unset(vars: impl Iterator<Item = std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    vars.filter(|k| k.to_str().is_some_and(|k| k.starts_with("ANTHROPIC_") || k.starts_with("CLAUDE"))).collect()
}

/// With the model proxy on ([`model_proxy`]), neither the engineer's Claude login
/// (`~/.claude/.credentials.json`) nor their `~/.claude.json` is bound, every `ANTHROPIC_*`/`CLAUDE*`
/// variable of the caller is dropped, and the proxy's address and key are set instead
/// ([`model_proxy_env`]): the hooks inherit them, so a model call borg makes inside goes there too.
/// The sandbox then also has no network and no view of the host's processes (`--unshare-net`,
/// `--unshare-pid`): it reaches only the proxy and the `BORG_EVAL_NET_ALLOW` services ([`net_ports`]), through
/// [`host_bridge`] sockets and `borg-eval net-bridge` (this binary, bound read-only) wrapping the command.
/// So no drone can fetch a public repo's later history or read another sandbox's arguments.
pub(crate) fn sandbox_with(cwd: &Path, writable: &[&Path], readonly: &[&Path], extra: &Extra, timeout_s: u32) -> (Command, PathBuf) {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let proxy = model_proxy();
    let private = std::env::temp_dir().join(format!(
        "borg-eval-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(&private).unwrap();
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout_s.to_string()).arg("bwrap");
    // If `timeout` dies (its caller was killed, see `sandbox_exec`), the drone dies with it.
    cmd.arg("--die-with-parent");
    cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"]);
    cmd.arg("--tmpfs").arg(&home);
    // The host's Unix sockets live under /run (/var/run links there, or is masked too): a read-only
    // bind does not stop `connect()`, and the Docker daemon (root on the host: it mounts the real
    // HOME), ssh-agent, gpg-agent and the session bus all answer there, network namespace or not.
    // Only the resolver file comes back where resolv.conf points into /run (systemd-resolved).
    // Abstract sockets (X11's @/tmp/.X11-unix) are per network namespace: closed to the scorer
    // (`unshare_net`), not to an agent, which needs the API.
    cmd.args(["--tmpfs", "/run"]);
    if fs::symlink_metadata("/var/run").is_ok_and(|m| m.is_dir()) {
        cmd.args(["--tmpfs", "/var/run"]);
    }
    if let Some(resolv) = fs::canonicalize("/etc/resolv.conf").ok().filter(|p| p.starts_with("/run") || p.starts_with("/var/run")) {
        cmd.arg("--ro-bind").arg(&resolv).arg(&resolv);
    }
    for ro in [home.join(".local/share/claude"), home.join(".local/bin")] {
        cmd.arg("--ro-bind").arg(&ro).arg(&ro);
    }
    if let Some(own) = &extra.sandbox_home {
        cmd.arg("--bind").arg(own).arg(own);
        cmd.arg("--clearenv");
        for k in SANDBOX_HOME_ENV {
            if let Some(v) = std::env::var_os(k) {
                cmd.arg("--setenv").arg(k).arg(v);
            }
        }
        cmd.arg("--setenv").arg("HOME").arg(own);
        // `<own home>/bin` first (dogfood links borg there), then the claude binary.
        cmd.arg("--setenv").arg("PATH").arg(format!("{}:{}:/usr/local/bin:/usr/bin:/bin", own.join("bin").display(), home.join(".local/bin").display()));
        cmd.args(["--setenv", "DISABLE_AUTOUPDATER", "1"]);
        for (k, v) in &extra.env {
            cmd.arg("--setenv").arg(k).arg(v);
        }
    } else if proxy.is_some() {
        // No login at all: an empty `~/.claude` (tmpfs) and a minimal `~/.claude.json` of its own.
        fs::write(private.join("claude.json"), json!({"hasCompletedOnboarding": true}).to_string()).unwrap();
        cmd.arg("--dir").arg(home.join(".claude"));
        cmd.arg("--bind").arg(private.join("claude.json")).arg(home.join(".claude.json"));
    } else {
        let mut cfg: Value = fs::read_to_string(home.join(".claude.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        if let Some(o) = cfg.as_object_mut() {
            o.remove("projects");
            o.remove("mcpServers");
        }
        fs::write(private.join("claude.json"), cfg.to_string()).unwrap();
        cmd.arg("--dir").arg(home.join(".claude"));
        let creds = home.join(".claude/.credentials.json");
        cmd.arg("--bind").arg(&creds).arg(&creds);
        cmd.arg("--bind").arg(private.join("claude.json")).arg(home.join(".claude.json"));
    }
    if let Some(url) = proxy {
        for k in proxy_unset(std::env::vars_os().map(|(k, _)| k)) {
            cmd.arg("--unsetenv").arg(k);
        }
        for (k, v) in model_proxy_env(url) {
            cmd.arg("--setenv").arg(k).arg(v);
        }
    }
    if extra.unshare_net || proxy.is_some() {
        cmd.arg("--unshare-net");
    }
    if extra.unshare_pid || proxy.is_some() {
        cmd.arg("--unshare-pid");
    }
    // Into the real (hidden) HOME; with a HOME of its own, into that one instead, after the writable
    // binds below (which would otherwise shadow them).
    if extra.sandbox_home.is_none() {
        for (src, rel) in &extra.home_ro {
            cmd.arg("--ro-bind").arg(src).arg(home.join(rel));
        }
    }
    // A cwd that a `file_rw` bind provides (a copy mounted onto another path) is never
    // bound from the host: the host's own directory there is the real repository.
    if !writable.iter().any(|w| cwd.starts_with(w)) && !extra.file_rw.iter().any(|(_, dst)| cwd.starts_with(dst)) {
        cmd.arg("--ro-bind").arg(cwd).arg(cwd);
    }
    for ro in readonly.iter().filter(|p| p.exists()) {
        cmd.arg("--ro-bind").arg(ro).arg(ro);
    }
    for rw in writable.iter().filter(|p| p.exists()) {
        cmd.arg("--bind").arg(rw).arg(rw);
    }
    if let Some(own) = &extra.sandbox_home {
        for (src, rel) in &extra.home_ro {
            cmd.arg("--ro-bind").arg(src).arg(own.join(rel));
        }
    }
    for (src, dst) in &extra.file_rw {
        cmd.arg("--bind").arg(src).arg(dst);
    }
    // The proxy's network (none for a sandbox asked to have none at all).
    let bridge: Vec<(u16, PathBuf)> = match proxy {
        Some(url) if !extra.unshare_net => net_ports(url).into_iter().map(|p| (p, host_bridge(p))).collect(),
        _ => Vec::new(),
    };
    let exe = std::env::current_exe().unwrap();
    if let Some((_, sock)) = bridge.first() {
        let dir = sock.parent().unwrap();
        cmd.arg("--ro-bind").arg(dir).arg(dir);
        cmd.arg("--ro-bind").arg(&exe).arg(&exe);
    }
    cmd.arg("--chdir").arg(cwd).arg("--");
    if !bridge.is_empty() {
        cmd.arg(&exe).arg("net-bridge");
        for (port, sock) in &bridge {
            cmd.arg(format!("{port}={}", sock.display()));
        }
        cmd.arg("--");
    }
    (cmd, private)
}

/// Run `claude -p` in the eval sandbox. Streams JSON to `stream`; returns the final
/// `result` message and wall time.
pub(crate) fn bwrap_claude(
    cwd: &Path,
    writable: &[&Path],
    readonly: &[&Path],
    claude_args: &[String],
    prompt: &str,
    stream: &Path,
    timeout_s: u32,
) -> (Option<Value>, f64) {
    bwrap_claude_with(cwd, writable, readonly, &Extra::default(), claude_args, prompt, stream, timeout_s)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn bwrap_claude_with(
    cwd: &Path,
    writable: &[&Path],
    readonly: &[&Path],
    extra: &Extra,
    claude_args: &[String],
    prompt: &str,
    stream: &Path,
    timeout_s: u32,
) -> (Option<Value>, f64) {
    let (result, secs, _) = bwrap_claude_run(cwd, writable, readonly, extra, claude_args, prompt, stream, timeout_s);
    (result, secs)
}

/// `bwrap_claude_with`, also returning the exit code (`timeout`'s 124 when the wall cap fired). The
/// prompt goes in on stdin (`claude -p` reads it there), never as an argument: an argument is capped by
/// the kernel (128 KiB) and visible in `ps`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bwrap_claude_run(
    cwd: &Path,
    writable: &[&Path],
    readonly: &[&Path],
    extra: &Extra,
    claude_args: &[String],
    prompt: &str,
    stream: &Path,
    timeout_s: u32,
) -> (Option<Value>, f64, Option<i32>) {
    let (mut cmd, private) = sandbox_with(cwd, writable, readonly, extra, timeout_s);
    cmd.arg("claude").args(claude_args);
    cmd.args(["--output-format", "stream-json", "--verbose"]);
    cmd.args(["--setting-sources", extra.setting_sources.as_deref().unwrap_or("project"), "--strict-mcp-config", "--no-session-persistence"]);
    let started = Instant::now();
    let out = fs::File::create(stream).unwrap();
    let mut child = cmd.stdin(Stdio::piped()).stdout(out).stderr(Stdio::null()).spawn().expect("spawn bwrap/claude");
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let status = child.wait().expect("wait bwrap/claude");
    let secs = started.elapsed().as_secs_f64();
    if !status.success() {
        eprintln!("    claude exited with {status}");
    }
    let _ = fs::remove_dir_all(&private);
    let result = fs::read_to_string(stream)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .rfind(|v| v.get("type").and_then(Value::as_str) == Some("result"));
    (result, secs, status.code())
}

/// What a drone run under the wake driver (`eval/preempt/drone_wake.sh`) produced: every `result` message in
/// order (its answer, then each wake round's), the driver's log lines, and the wall time.
#[cfg(replay_evals)] // preempt only
pub(crate) struct Woken {
    pub results: Vec<Value>,
    pub log: Vec<String>,
    pub secs: f64,
}

/// `bwrap_claude_run` through the wake driver: the sandbox outlives the drone's answer until borg's detached
/// check is done (`wait_s`), and a queued ask resumes the session, at most `rounds` times. The session
/// persists for that (in the sandbox's tmpfs HOME), so `--no-session-persistence` is left out.
#[allow(clippy::too_many_arguments)]
#[cfg(replay_evals)] // preempt only
pub(crate) fn bwrap_claude_wake(
    cwd: &Path,
    writable: &[&Path],
    readonly: &[&Path],
    extra: &Extra,
    claude_args: &[String],
    prompt: &str,
    stream: &Path,
    timeout_s: u32,
    (driver, state, rounds, wait_s): (&Path, &Path, u32, u32),
) -> Woken {
    // The driver's log, beside the stream.
    let log = stream.with_file_name("wake.log");
    let _ = fs::remove_file(&log);
    fs::write(&log, "").unwrap();
    let (mut cmd, private) = sandbox_with(cwd, writable, readonly, extra, timeout_s);
    cmd.arg("sh").arg(driver).arg(state).arg(&log).arg(rounds.to_string()).arg(wait_s.to_string()).arg("--");
    cmd.arg("claude").args(claude_args);
    cmd.args(["--output-format", "stream-json", "--verbose"]);
    cmd.args(["--setting-sources", extra.setting_sources.as_deref().unwrap_or("project"), "--strict-mcp-config"]);
    let started = Instant::now();
    let out = fs::File::create(stream).unwrap();
    let mut child = cmd.stdin(Stdio::piped()).stdout(out).stderr(Stdio::null()).spawn().expect("spawn bwrap/wake driver");
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let status = child.wait().expect("wait bwrap/wake driver");
    let secs = started.elapsed().as_secs_f64();
    if !status.success() {
        eprintln!("    wake driver exited with {status}");
    }
    let _ = fs::remove_dir_all(&private);
    let results = fs::read_to_string(stream)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("result"))
        .collect();
    let log = fs::read_to_string(&log).unwrap_or_default().lines().map(str::to_string).collect();
    Woken { results, log, secs }
}

fn run_drone(a: &Args, work: &Path, prompt: &str, stream: &Path) -> (Option<Value>, f64) {
    let mut args: Vec<String> = ["-p", "--model", &a.model, "--max-budget-usd", &a.budget_usd].map(String::from).to_vec();
    args.extend(["--permission-mode", "bypassPermissions", "--append-system-prompt", SYSTEM].map(String::from));
    if let Some(s) = &a.settings {
        args.extend(["--settings".to_string(), s.display().to_string()]);
    }
    bwrap_claude(work, &[work], &[], &args, prompt, stream, a.timeout_s)
}

/// Without the model proxy a sandbox binds the engineer's Claude login: like `preempt`, the sandbox commands
/// refuse to start then unless `--direct-claude` says so, and a first argument that is a flag (`--help`) is
/// never taken for the working directory.
fn sandbox_guard(cmd: &str, raw: &[String], direct: bool) {
    if raw.first().is_none_or(|a| a.starts_with('-')) {
        eprintln!("usage: borg-eval {cmd} <cwd> [--model-proxy URL | --direct-claude] ...");
        std::process::exit(2);
    }
    if model_proxy().is_none() && !direct {
        eprintln!("borg-eval {cmd}: no --model-proxy URL (or BORG_EVAL_MODEL_PROXY): pass --direct-claude to run on your own Claude login");
        std::process::exit(2);
    }
}

/// `borg-eval report <results.jsonl>...`: per-arm, per-task table.
/// `borg-eval sandbox-exec <cwd> [--rw P]... [--ro P]... [--timeout S] [--direct-claude] -- argv...`: run any
/// command in the eval sandbox (e.g. a memory system's warm-up hook), inheriting the environment.
fn sandbox_exec(raw: &[String]) {
    let direct = raw.iter().take_while(|a| *a != "--").any(|a| a == "--direct-claude");
    sandbox_guard("sandbox-exec", raw, direct);
    let cwd = PathBuf::from(&raw[0]);
    let (mut rw, mut ro, mut timeout) = (Vec::new(), Vec::new(), 1800u32);
    let mut extra = Extra::default();
    let mut it = raw[1..].iter();
    let mut argv = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rw" => rw.push(PathBuf::from(it.next().unwrap())),
            // `--bind SRC DST`: SRC read-write at DST (e.g. a per-run ~/.claude/projects, so a later call
            // can --resume the session while the real one stays hidden).
            "--bind" => {
                let src = PathBuf::from(it.next().unwrap());
                extra.file_rw.push((src, PathBuf::from(it.next().unwrap())));
            }
            "--ro" => ro.push(PathBuf::from(it.next().unwrap())),
            "--timeout" => timeout = it.next().unwrap().parse().unwrap(),
            "--direct-claude" => {}
            "--" => {
                argv = it.cloned().collect();
                break;
            }
            other => panic!("unknown flag {other}"),
        }
    }
    let rw: Vec<&Path> = rw.iter().map(PathBuf::as_path).collect();
    let ro: Vec<&Path> = ro.iter().map(PathBuf::as_path).collect();
    let (mut cmd, private) = sandbox_with(&cwd, &rw, &ro, &extra, timeout);
    // A caller that kills us on its own timeout (the handbook runner SIGKILLs its claude) must not
    // leave the sandboxed drone running: `timeout` gets SIGKILL when we die, and bwrap
    // (--die-with-parent) takes the sandbox down with it.
    // SAFETY: prctl is async-signal-safe and touches no memory of the parent. Linux only, as bwrap is.
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let status = cmd.args(&argv).status().expect("spawn sandbox");
    let _ = fs::remove_dir_all(&private);
    std::process::exit(status.code().unwrap_or(1));
}

/// `borg-eval sandbox-probe <cwd> [--model-proxy URL | --direct-claude]`: a cheap model lists what a drone
/// can see from the sandbox, to prove that nothing past the decision point is reachable. With the proxy it
/// also lists the model variables the drone sees (names only) and its Claude Code config.
fn sandbox_probe(raw: &[String]) {
    let mut direct = false;
    let mut it = raw.iter().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--model-proxy" => set_model_proxy(it.next().expect("--model-proxy URL").clone()),
            "--direct-claude" => direct = true,
            other => panic!("unknown flag {other}"),
        }
    }
    sandbox_guard("sandbox-probe", raw, direct);
    let cwd = PathBuf::from(&raw[0]);
    // The engineer's real HOME: its repos must not be visible from inside.
    let home = std::env::var("HOME").unwrap_or_default();
    let mut probe = format!(
        "Run exactly this one Bash command and reply with its full output verbatim, nothing else: \
ls -A ~ ; ls {home}/* 2>&1 | head; ls ~/.claude ~/.local/share/borg 2>&1; \
git log --all --oneline | wc -l; git branch -a; git remote -v; git stash list; gh auth status 2>&1 | head -3; \
ls ~/.ssh ~/.config 2>&1"
    );
    if model_proxy().is_some() {
        probe.push_str(" ; ls -A ~/.claude; cat ~/.claude.json; echo; env | grep -oE '^(ANTHROPIC|CLAUDE|DISABLE)[A-Z_]*' | sort");
    }
    let stream = std::env::temp_dir().join("borg-sandbox-probe.jsonl");
    let args: Vec<String> = ["-p", "--model", "haiku", "--permission-mode", "bypassPermissions", "--max-budget-usd", "0.3"].map(String::from).to_vec();
    let (result, _) = bwrap_claude(&cwd, &[], &[], &args, &probe, &stream, 300);
    println!("{}", result.and_then(|r| r["result"].as_str().map(str::to_string)).unwrap_or_else(|| "no result (see stream)".into()));
}

fn report(files: &[String]) {
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Agg {
        n: u32,
        solved: u32,
        cost: f64,
        explore: u64,
        failed_bash: u32,
        turns: u64,
    }
    let mut by: BTreeMap<(String, u64), Agg> = BTreeMap::new();
    for f in files {
        for line in fs::read_to_string(f).unwrap_or_default().lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let key = (v["arm"].as_str().unwrap_or("?").to_string(), v["task"].as_u64().unwrap_or(0));
            let g = by.entry(key).or_default();
            g.n += 1;
            g.solved += v["solved"].as_bool().unwrap_or(false) as u32;
            g.cost += v["cost_usd"].as_f64().unwrap_or(0.0);
            g.explore += v["explore_tokens"].as_u64().unwrap_or(0);
            g.failed_bash += v["failed_bash"].as_u64().unwrap_or(0) as u32;
            g.turns += v["turns"].as_u64().unwrap_or(0);
        }
    }
    println!("| Arm | Task | Runs | Solved | Mean cost | Mean turns | Mean explore tok | Failed cmds/run |");
    println!("|---|---|---|---|---|---|---|---|");
    let mut arms: BTreeMap<String, (u32, f64)> = BTreeMap::new();
    for ((arm, task), g) in &by {
        let n = g.n.max(1) as f64;
        println!(
            "| {arm} | {task:02} | {} | {}/{} | ${:.3} | {:.1} | {:.0} | {:.2} |",
            g.n,
            g.solved,
            g.n,
            g.cost / n,
            g.turns as f64 / n,
            g.explore as f64 / n,
            g.failed_bash as f64 / n
        );
        let a = arms.entry(arm.clone()).or_default();
        a.0 += g.solved;
        a.1 += g.cost;
    }
    println!();
    for (arm, (solved, cost)) in arms {
        println!("**{arm}**: {solved} solved, total ${cost:.2}, cost per solved task ${:.3}", cost / solved.max(1) as f64);
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match raw.first().map(String::as_str) {
        Some("report") => return report(&raw[1..]),
        #[cfg(replay_evals)]
        Some("preempt") => return preempt::main(&raw[1..]),
        #[cfg(replay_evals)]
        Some("queen-replay") => return queen_replay::main(&raw[1..]),
        #[cfg(replay_evals)]
        Some("queen-test") => return queen_test::main(&raw[1..]),
        Some("sandbox-exec") => return sandbox_exec(&raw[1..]),
        #[cfg(replay_evals)]
        Some("expert-hook") => return preempt::expert_hook(),
        Some("dogfood") => return dogfood::main(&raw[1..]),
        Some("sandbox-probe") => return sandbox_probe(&raw[1..]),
        Some("net-bridge") => return net_bridge(&raw[1..]),
        _ => {}
    }
    let a = args();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out_dir = root.join("target/eval-runs").join(&a.run);
    fs::create_dir_all(&out_dir).unwrap();
    let mut results = fs::OpenOptions::new().create(true).append(true).open(out_dir.join("results.jsonl")).unwrap();
    let tasks: Vec<PathBuf> = sorted_files(&a.workload.join("tasks"), "").into_iter().take(a.tasks).collect();
    let hidden = a.workload.join("hidden");
    let drift = sorted_files(&a.workload.join("drift"), "before-");
    eprintln!("run {} · arm {} · {} tasks × {} seeds · {}", a.run, a.arm, tasks.len(), a.seeds, out_dir.display());

    for seed in 0..a.seeds {
        let work = out_dir.join(format!("s{seed}/repo"));
        fresh_repo(&a.workload, &work);
        for (i, task) in tasks.iter().enumerate() {
            let n = format!("{:02}", i + 1);
            for d in drift.iter().filter(|d| d.file_name().unwrap().to_string_lossy().starts_with(&format!("before-{n}"))) {
                let (ok, out) = sh(&work, &format!("sh '{}'", d.display()));
                eprintln!("  drift {} → {}", d.file_name().unwrap().to_string_lossy(), if ok { "applied" } else { out.trim() });
            }
            let prompt = fs::read_to_string(task).unwrap();
            let stream = out_dir.join(format!("s{seed}/task{n}.stream.jsonl"));
            eprintln!("  seed {seed} task {n} …");
            let (result, secs) = run_drone(&a, &work, &prompt, &stream);

            // Grade: hidden tests for this task plus invariants and all earlier tasks.
            let mut tests = vec![hidden.join("test_invariants.py")];
            tests.extend((1..=i + 1).map(|k| hidden.join(format!("test_{k:02}.py"))).filter(|p| p.exists()));
            let grade = a.workload.join("grade.sh");
            let list: Vec<String> = tests.iter().map(|p| format!("'{}'", p.display())).collect();
            let (passed, grade_out) = sh(&work, &format!("'{}' '{}' {}", grade.display(), work.display(), list.join(" ")));
            let (own_passed, _) = sh(&work, &format!("'{}' '{}' '{}'", grade.display(), work.display(), hidden.join(format!("test_{n}.py")).display()));

            let sess = claude::parse(&stream).ok();
            let (explore, failed_bash, reads) = sess.as_ref().map_or((0, 0, 0), |s| {
                s.events.iter().fold((0u64, 0u32, 0u32), |acc, e| match &e.kind {
                    Kind::Read { .. } => (acc.0 + e.result_tokens, acc.1, acc.2 + 1),
                    Kind::Search => (acc.0 + e.result_tokens, acc.1, acc.2),
                    Kind::Bash { failed, .. } => (acc.0 + e.result_tokens, acc.1 + *failed as u32, acc.2),
                    _ => acc,
                })
            });
            let r = result.unwrap_or(Value::Null);
            let rec = json!({
                "run": a.run, "arm": a.arm, "model": a.model, "seed": seed, "task": i + 1,
                "solved": own_passed, "all_passed": passed,
                "cost_usd": r.get("total_cost_usd").and_then(Value::as_f64),
                "turns": r.get("num_turns"), "is_error": r.get("is_error"),
                "usage": r.get("usage"), "wall_s": secs,
                "explore_tokens": explore, "failed_bash": failed_bash, "reads": reads,
                "grade_tail": grade_out.lines().rev().take(3).collect::<Vec<_>>(),
            });
            writeln!(results, "{rec}").unwrap();
            eprintln!(
                "    solved={own_passed} all={passed} cost=${:.3} turns={} explore={explore} failed_bash={failed_bash} {:.0}s",
                rec["cost_usd"].as_f64().unwrap_or(0.0),
                rec["turns"],
                secs
            );

            let (ok, out) = sh(&work, "git add -A && git commit -qm task --allow-empty && git clean -fdxq");
            assert!(ok, "commit/clean: {out}");
        }
    }
    eprintln!("results: {}", out_dir.join("results.jsonl").display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_drops_every_claude_and_anthropic_variable() {
        let vars = ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDECODE", "CLAUDE_CONFIG_DIR", "HOME", "BORG_MATCH", "XANTHROPIC_"];
        let dropped = proxy_unset(vars.iter().map(std::ffi::OsString::from));
        assert_eq!(dropped, ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDECODE", "CLAUDE_CONFIG_DIR"].map(std::ffi::OsString::from));
    }

    #[test]
    fn proxy_env_points_claude_at_the_proxy_with_gemini_output_cap() {
        let env: std::collections::HashMap<_, _> = model_proxy_env("http://127.0.0.1:4000").into_iter().collect();
        assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4000");
        assert!(!env["ANTHROPIC_AUTH_TOKEN"].is_empty());
        assert_eq!(env["CLAUDE_CODE_MAX_OUTPUT_TOKENS"], "32000");
        assert_eq!(env["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1");
        assert_eq!(env["DISABLE_TELEMETRY"], "1");
    }

    #[test]
    fn only_loopback_http_ports_are_bridged() {
        assert_eq!(loopback_port("http://127.0.0.1:4000"), Ok(4000));
        assert_eq!(loopback_port("http://localhost:8891/t/abc?x=1"), Ok(8891));
        for bad in ["https://127.0.0.1:4000", "http://127.0.0.1", "http://api.github.com:80", "http://10.0.0.1:4000", "http://127.0.0.1:x"] {
            assert!(loopback_port(bad).is_err(), "{bad}");
        }
    }
}
