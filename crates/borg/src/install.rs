//! Wiring borg into a harness, and seeing whether it works:
//! `borg install|uninstall <claude|omp|opencode|all> [--apply] [--home DIR]`, `borg status`, `borg doctor`.
//!
//! The wiring is exactly what each adapter README describes, written into the engineer's *user* config:
//! - Claude Code: one hook group per event in `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR`), each
//!   running `<this borg> hook claude <event>`;
//! - omp: `adapters/omp/borg.ts` as `~/.omp/agent/extensions/borg.ts` (`$PI_CODING_AGENT_DIR/extensions`);
//! - opencode: `adapters/opencode/borg.ts` as `~/.config/opencode/plugins/borg.ts`
//!   (`$OPENCODE_CONFIG_DIR`, `$XDG_CONFIG_HOME/opencode`).
//!
//! The adapters are compiled into the binary, so an installed borg needs no checkout.
//!
//! Someone else's config is precious, so: the default is a dry run that prints the exact diff;
//! `--apply` first writes a timestamped backup next to every file it changes; merging keeps every
//! other key and hook as it was; re-running changes nothing; uninstall removes only what install adds
//! (borg's own hook commands, and adapter files whose content is this borg's adapter). `--home`
//! replaces HOME *and ignores every config-dir variable* (the harnesses' and `$BORG_HOME`/`$BORG_CONFIG`,
//! see [`home_overrides`]), so a test can never reach a real config or store.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::collective::{self, Route, Share};
use crate::{learn, outcome};

pub const OMP_ADAPTER: &str = include_str!("../../../adapters/omp/borg.ts");
pub const OPENCODE_ADAPTER: &str = include_str!("../../../adapters/opencode/borg.ts");

/// Claude Code hook events and the canonical borg events they become.
pub const CLAUDE_EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "prompt"),
    // Only a paused drone's tool calls are refused here (steer.rs); otherwise a stat and out.
    ("PreToolUse", "pre_tool"),
    ("PostToolUse", "post_tool"),
    ("PostToolUseFailure", "post_tool_failure"),
    ("Stop", "stop"),
    ("SessionEnd", "end"),
];

/// Seconds Claude Code gives borg's Stop hook (its own default for a command hook is 60 s). The
/// expert drone keeps inside it (expert_route.rs `drone_cap`), so a slow drone never costs the
/// conscience's answer.
pub const CLAUDE_STOP_TIMEOUT_S: u64 = 90;

/// borg's hook entry for one canonical event.
fn claude_hook_entry(bin: &Path, ev: &str) -> Value {
    match ev {
        "stop" => json!({"type": "command", "command": claude_command(bin, ev), "timeout": CLAUDE_STOP_TIMEOUT_S}),
        _ => json!({"type": "command", "command": claude_command(bin, ev)}),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Omp,
    Opencode,
}

pub const HARNESSES: [Harness; 3] = [Harness::Claude, Harness::Omp, Harness::Opencode];

impl Harness {
    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Omp => "omp",
            Harness::Opencode => "opencode",
        }
    }

    fn parse(s: &str) -> Option<Vec<Harness>> {
        match s {
            "all" => Some(HARNESSES.to_vec()),
            _ => HARNESSES.iter().copied().find(|h| h.name() == s).map(|h| vec![h]),
        }
    }
}

/// Where each harness reads its user config.
#[derive(Debug, Clone)]
pub struct Targets {
    pub claude_settings: PathBuf,
    pub omp_extension: PathBuf,
    pub opencode_plugin: PathBuf,
}

impl Targets {
    /// Everything under `home`, no environment consulted (tests, `--home`).
    pub fn under(home: &Path) -> Self {
        Targets {
            claude_settings: home.join(".claude/settings.json"),
            omp_extension: home.join(".omp/agent/extensions/borg.ts"),
            opencode_plugin: home.join(".config/opencode/plugins/borg.ts"),
        }
    }

    /// The real locations, honouring each harness's own config-dir variables.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = var("HOME").unwrap_or_default();
        let mut t = Targets::under(&home);
        if let Some(d) = var("CLAUDE_CONFIG_DIR") {
            t.claude_settings = d.join("settings.json");
        }
        if let Some(d) = var("PI_CODING_AGENT_DIR") {
            t.omp_extension = d.join("extensions/borg.ts");
        }
        if let Some(d) = var("OPENCODE_CONFIG_DIR").or_else(|| var("XDG_CONFIG_HOME").map(|x| x.join("opencode"))) {
            t.opencode_plugin = d.join("plugins/borg.ts");
        }
        t
    }

    pub fn path(&self, h: Harness) -> &Path {
        match h {
            Harness::Claude => &self.claude_settings,
            Harness::Omp => &self.omp_extension,
            Harness::Opencode => &self.opencode_plugin,
        }
    }
}

fn adapter(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "",
        Harness::Omp => OMP_ADAPTER,
        Harness::Opencode => OPENCODE_ADAPTER,
    }
}

/// One file as it is and as it would be (`None` = absent).
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: Option<String>,
    /// Settings files are often symlinks into a dotfiles repo: write through them. An adapter file
    /// symlinked into a borg checkout (README's `ln -s`) is replaced instead, never written through.
    pub through_links: bool,
}

// ---------------------------------------------------------------- Claude Code settings.json

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    match s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        Some(inner) => inner.replace(r"'\''", "'"),
        None => s.to_string(),
    }
}

pub fn claude_command(bin: &Path, event: &str) -> String {
    format!("{} hook claude {event}", shell_quote(&bin.display().to_string()))
}

/// `(binary, canonical event)` if this hook command is one borg installs: `<…/borg> hook claude <event>`.
pub fn borg_command(cmd: &str) -> Option<(PathBuf, &'static str)> {
    CLAUDE_EVENTS.iter().find_map(|(_, ev)| {
        let prog = cmd.trim().strip_suffix(&format!(" hook claude {ev}"))?;
        let prog = PathBuf::from(unquote(prog));
        let base = prog.file_name()?.to_string_lossy().into_owned();
        (base == "borg" || base == "borg.exe").then_some((prog, *ev))
    })
}

fn parse_settings(text: Option<&str>) -> Result<Map<String, Value>, String> {
    match text.filter(|t| !t.trim().is_empty()) {
        None => Ok(Map::new()),
        Some(t) => match serde_json::from_str(t) {
            Ok(Value::Object(m)) => Ok(m),
            Ok(_) => Err("settings.json is not a JSON object".into()),
            Err(e) => Err(format!("settings.json is not valid JSON ({e}); fix it first, nothing was changed")),
        },
    }
}

/// Serialises like Claude Code does (2-space JSON), keeping the original's trailing newline.
fn render(m: Map<String, Value>, original: Option<&str>) -> String {
    let mut s = serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default();
    if original.is_none_or(|o| o.trim().is_empty() || o.ends_with('\n')) {
        s.push('\n');
    }
    s
}

/// Removes borg's hook entries for `ev` from one event's groups; a group left empty by that goes too.
fn remove_borg(groups: &mut Vec<Value>, ev: &str) -> usize {
    let mut removed = 0;
    groups.retain_mut(|g| {
        let Some(hooks) = g.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
        let n = hooks.len();
        hooks.retain(|h| h.get("command").and_then(Value::as_str).and_then(borg_command).is_none_or(|(_, e)| e != ev));
        removed += n - hooks.len();
        !(hooks.is_empty() && n > 0)
    });
    removed
}

/// settings.json with borg's hooks merged in (`Ok(None)`: already exactly so).
///
/// `bin` must be named `borg`: that basename is how [`borg_command`] recognises its own hooks, so a
/// binary named otherwise would be duplicated on every install and never removed by uninstall.
pub fn claude_install(text: Option<&str>, bin: &Path) -> Result<Option<String>, String> {
    let base = bin.file_name().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
    if base != "borg" && base != "borg.exe" {
        return Err(format!(
            "{} is not named borg, so its hooks could not be recognised again (duplicates, no uninstall); point BORG_BIN at a binary named borg (a symlink will do)",
            bin.display()
        ));
    }
    let mut m = parse_settings(text)?;
    let orig = m.clone();
    let hooks = m.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return Err("settings.json: \"hooks\" is not an object".into()) };
    for (event, ev) in CLAUDE_EVENTS {
        let want = claude_hook_entry(bin, ev);
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else { return Err(format!("settings.json: hooks.{event} is not an array")) };
        let ours: Vec<Value> = groups
            .iter()
            .flat_map(|g| g.get("hooks").and_then(Value::as_array).cloned().unwrap_or_default())
            .filter(|h| h.get("command").and_then(Value::as_str).and_then(borg_command).is_some_and(|(_, e)| e == ev))
            .collect();
        if ours == [want.clone()] {
            continue; // already wired to this binary, with this timeout
        }
        remove_borg(groups, ev); // stale binary, timeout or duplicates: replace with exactly one
        groups.push(json!({"hooks": [want]}));
    }
    Ok((m != orig).then(|| render(m, text)))
}

/// settings.json without borg's hooks (`Ok(None)`: there were none; `Ok(Some(None))`: nothing else
/// was in it, so the file goes). An event list or `hooks` object that only borg filled goes with them.
pub fn claude_uninstall(text: Option<&str>) -> Result<Option<Option<String>>, String> {
    if text.is_none() {
        return Ok(None);
    }
    let mut m = parse_settings(text)?;
    let Some(hooks) = m.get_mut("hooks").and_then(Value::as_object_mut) else { return Ok(None) };
    let mut removed = 0;
    for (event, ev) in CLAUDE_EVENTS {
        let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else { continue };
        let n = remove_borg(groups, ev);
        if n > 0 && groups.is_empty() {
            hooks.shift_remove(event);
        }
        removed += n;
    }
    if removed == 0 {
        return Ok(None);
    }
    if hooks.is_empty() {
        m.shift_remove("hooks");
    }
    Ok(Some((!m.is_empty()).then(|| render(m, text))))
}

/// Which canonical events settings.json sends to borg, and to which binary.
pub fn claude_wired(text: Option<&str>) -> Vec<(&'static str, PathBuf)> {
    let Ok(m) = parse_settings(text) else { return Vec::new() };
    let mut out = Vec::new();
    for (event, _) in CLAUDE_EVENTS {
        for g in m.get("hooks").and_then(|h| h.get(event)).and_then(Value::as_array).into_iter().flatten() {
            for h in g.get("hooks").and_then(Value::as_array).into_iter().flatten() {
                if let Some((bin, ev)) = h.get("command").and_then(Value::as_str).and_then(borg_command) {
                    out.push((ev, bin));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- plans

/// A file's text; `Ok(None)` only when it does not exist. Unreadable (permissions, not UTF-8) is an
/// error, never "absent": treating it as absent would replace the file without a backup.
fn read(p: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(p) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Uninstall,
}

/// What `action` would change for `h`, plus notes for things deliberately left alone.
pub fn plan(action: Action, h: Harness, t: &Targets, bin: &Path) -> Result<(Option<Change>, Vec<String>), String> {
    let path = t.path(h).to_path_buf();
    let before = read(&path).map_err(|e| format!("{}: cannot read ({e}); nothing was changed", path.display()))?;
    let mut notes = Vec::new();
    let after = match (h, action) {
        (Harness::Claude, Action::Install) => claude_install(before.as_deref(), bin)?.map(Some),
        (Harness::Claude, Action::Uninstall) => claude_uninstall(before.as_deref())?,
        (_, Action::Install) => (before.as_deref() != Some(adapter(h))).then(|| Some(adapter(h).to_string())),
        (_, Action::Uninstall) => match before.as_deref() {
            Some(b) if b == adapter(h) => Some(None),
            Some(_) => {
                notes.push(format!("{}: left alone, its content is not this borg's adapter (yours, or an older borg's)", path.display()));
                None
            }
            None => None,
        },
    };
    Ok((after.map(|after| Change { path, before, after, through_links: h == Harness::Claude }), notes))
}

/// UTC `YYYYMMDDTHHMMSS` for an epoch-ms time (no time crate for one format).
pub fn utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn human(ms: i64) -> String {
    let s = utc(ms);
    format!("{}-{}-{} {}:{}:{} UTC", &s[0..4], &s[4..6], &s[6..8], &s[9..11], &s[11..13], &s[13..15])
}

/// Mode for files borg creates where none existed: settings.json often holds secrets in `env`.
#[cfg(unix)]
const NEW_FILE_MODE: u32 = 0o600;

/// Creates `p` (never an existing file) with `text` and permissions `mode` (unix; ignored elsewhere).
/// The mode is set at creation, so the file is never readable by more people than intended, and
/// set again afterwards, because creation is narrowed by the umask.
fn create_with_mode(p: &Path, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(m);
    }
    let mut f = o.open(p)?;
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(m))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

/// `<file>.borg-backup-<stamp>`, or `-1`, `-2`, … after it: an existing backup is never overwritten
/// (an install and an uninstall in the same second would otherwise lose the original).
fn write_backup(path: &Path, name: &str, stamp: &str, text: &str, mode: Option<u32>) -> std::io::Result<PathBuf> {
    for n in 0u32.. {
        let suffix = if n == 0 { String::new() } else { format!("-{n}") };
        let p = path.with_file_name(format!("{name}.borg-backup-{stamp}{suffix}"));
        match create_with_mode(&p, text, mode) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("u32 backups")
}

#[cfg(unix)]
fn mode_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_: &Path) -> Option<u32> {
    None
}

/// Writes one change: a backup of the existing file first, then an atomic replace. Returns the backup.
/// The replacement and the backup keep the original's permissions (a 0600 file stays 0600); a file
/// that did not exist is created 0600.
pub fn apply(c: &Change, stamp: &str) -> std::io::Result<Option<PathBuf>> {
    let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mode = mode_of(&c.path); // follows a symlink: the mode of the file actually read
    let backup = match &c.before {
        Some(b) => Some(write_backup(&c.path, &name, stamp, b, mode)?),
        None => None,
    };
    let is_link = std::fs::symlink_metadata(&c.path).is_ok_and(|m| m.file_type().is_symlink());
    match &c.after {
        None => std::fs::remove_file(&c.path)?,
        Some(text) => {
            let target = if is_link && c.through_links { std::fs::canonicalize(&c.path)? } else { c.path.clone() };
            if let Some(d) = target.parent() {
                std::fs::create_dir_all(d)?;
            }
            #[cfg(unix)]
            let mode = mode.or(Some(NEW_FILE_MODE));
            let tmp = target.with_file_name(format!(".{name}.borg-tmp"));
            let _ = std::fs::remove_file(&tmp); // a leftover of an interrupted run, with whatever mode it had
            create_with_mode(&tmp, text, mode)?;
            std::fs::rename(&tmp, &target)?; // replaces a symlink itself when not writing through it
        }
    }
    Ok(backup)
}

/// A unified diff of two texts, 3 lines of context.
pub fn diff(path: &Path, before: Option<&str>, after: Option<&str>) -> String {
    let a: Vec<&str> = before.map(|t| t.lines().collect()).unwrap_or_default();
    let b: Vec<&str> = after.map(|t| t.lines().collect()).unwrap_or_default();
    let (n, m) = (a.len(), b.len());
    // Longest common subsequence, then walk it into ' ', '-', '+' lines.
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut ops) = (0, 0, Vec::new());
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', a[i]));
            (i, j) = (i + 1, j + 1);
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            ops.push(('+', b[j]));
            j += 1;
        } else {
            ops.push(('-', a[i]));
            i += 1;
        }
    }
    let label = |t: Option<&str>| if t.is_some() { path.display().to_string() } else { "/dev/null".into() };
    let mut out = format!("--- {}\n+++ {}\n", label(before), label(after));
    let near = |k: usize| ops[k.saturating_sub(3)..(k + 4).min(ops.len())].iter().any(|(o, _)| *o != ' ');
    let mut gap = true;
    for (k, (o, line)) in ops.iter().enumerate() {
        if near(k) {
            if gap {
                out.push_str("@@\n");
            }
            out.push_str(&format!("{o}{line}\n"));
            gap = false;
        } else {
            gap = true;
        }
    }
    out
}

// ---------------------------------------------------------------- status

#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub lessons: usize,
    pub episodes: usize,
    pub outcomes: usize,
    pub last_capture_ms: Option<i64>,
    /// When, and which lessons (`id: question`), the conscience last asked.
    pub last_asked: Option<(i64, Vec<String>)>,
}

pub fn stats(home: &Path) -> Stats {
    let lessons = learn::lessons(&home.join("lessons.jsonl"));
    let episodes: Vec<Value> =
        read(&home.join("episodes.jsonl")).ok().flatten().unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let outcomes = outcome::read(&home.join("outcomes.jsonl"));
    let last_asked = outcomes.iter().filter(|o| o.kind == outcome::ASKED).max_by_key(|o| o.ms).map(|o| {
        let named = o.lessons.iter().map(|id| match lessons.iter().find(|l| &l.id == id) {
            Some(l) => format!("{id}: {}", l.question.chars().take(100).collect::<String>()),
            None => id.clone(),
        });
        (o.ms, named.collect())
    });
    Stats {
        lessons: lessons.len(),
        episodes: episodes.len(),
        outcomes: outcomes.len(),
        last_capture_ms: episodes.iter().filter_map(|e| e.get("ms").and_then(Value::as_i64)).max(),
        last_asked,
    }
}

/// Every collective with its store: those in the config, or the one "default" without a config.
pub fn collectives(cfg: Option<&collective::Config>, base: &Path) -> Vec<Route> {
    match cfg {
        None => vec![Route::Collective { name: "default".into(), home: base.to_path_buf(), share: Share::Team }],
        Some(c) => c.collectives.iter().map(|e| Route::Collective { name: e.name.clone(), home: collective::store(base, &e.name), share: e.share }).collect(),
    }
}

/// How `h` is wired under `t`, in one line.
pub fn wiring(h: Harness, t: &Targets) -> String {
    let path = t.path(h);
    let text = match read(path) {
        Ok(t) => t,
        Err(e) => return format!("UNREADABLE: {} ({e})", path.display()),
    };
    match h {
        Harness::Claude => {
            let w = claude_wired(text.as_deref());
            let events: Vec<&str> = CLAUDE_EVENTS.iter().map(|(_, e)| *e).filter(|e| w.iter().any(|(x, _)| x == e)).collect();
            let mut bins: Vec<String> = w.iter().map(|(_, b)| b.display().to_string()).collect();
            bins.dedup();
            match events.len() {
                0 => format!("not wired ({})", path.display()),
                n if n == CLAUDE_EVENTS.len() => format!("wired: {} -> {}", path.display(), bins.join(", ")),
                _ => format!("PARTIAL ({} only): {} -> {}", events.join(","), path.display(), bins.join(", ")),
            }
        }
        _ => match text {
            None => format!("not wired ({})", path.display()),
            Some(t) if t == adapter(h) => format!("wired: {}", path.display()),
            Some(_) => format!("wired, but {} differs from this borg's adapter (older or edited)", path.display()),
        },
    }
}

fn print_status(t: &Targets) {
    let base = learn::home();
    let cfg = collective::load();
    match &cfg {
        Err(e) => println!("COLLECTIVES CONFIG INVALID: {e}\n  every repo routes OFF: borg captures and asks nothing until it is fixed (borg doctor)"),
        Ok(None) => println!("config: {} (absent: every repo is in collective \"default\")", collective::config_path().display()),
        Ok(Some(c)) => println!("config: {} (default: {}, off: {:?})", collective::config_path().display(), c.default, c.off),
    }
    let routes = match &cfg {
        Ok(c) => collectives(c.as_ref(), &base),
        Err(_) => Vec::new(), // nothing is routed anywhere; listing stores would suggest otherwise
    };
    for r in routes {
        let Route::Collective { name, home, share } = &r else { continue };
        let s = stats(home);
        println!("\ncollective {name} ({share:?}, may sync: {})", collective::may_sync(&r));
        println!("  store        {}", home.display());
        let m = crate::live::of(cfg.as_ref().ok().and_then(Option::as_ref), name);
        let off = (m.mode == crate::live::Mode::Live).then(crate::live::unsupported).flatten();
        let note = match (&m.warning, off) {
            (Some(_), _) => "; unknown value, see borg doctor".to_string(),
            (None, Some(why)) => format!("; off here: {why} replaces the consequence model, so checks run at the turn end only"),
            (None, None) => String::new(),
        };
        println!("  mode         {} ({}){note}", m.mode.name(), m.source);
        println!("  lessons      {}", s.lessons);
        println!("  episodes     {}", s.episodes);
        println!("  outcomes     {}", s.outcomes);
        println!("  queen open   {}", crate::queen::open(home).len());
        println!("  trust        {}", crate::trust::summary(home));
        println!("  last capture {}", s.last_capture_ms.map(human).unwrap_or_else(|| "never".into()));
        let g = crate::prefilter::tally(&home.join("state"));
        println!("  stop gate    {} turn ends skipped at 0 tokens, {} asked the model (${:.2} so far)", g.skipped, g.asked, g.cost);
        if g.live_checks > 0 {
            println!("  live checks  {} during the turn, {} lessons read (${:.2} so far)", g.live_checks, g.live_lessons, g.live_cost);
        }
        println!(
            "  cost         {} extra rounds (Stop blocks, {} questions, ~{} tokens); {} free deliveries ({} lessons, ~{} tokens, no extra round)",
            g.rounds,
            g.questions,
            g.block_chars / 4,
            g.free,
            g.free_lessons,
            g.free_chars / 4
        );
        println!(
            "  judge        {} model calls also judged {} asked lessons ({} settled; ~{} prompt tokens added, no extra call)",
            g.judge_calls,
            g.judged,
            g.judge_resolved,
            g.judge_chars / 4
        );
        match s.last_asked {
            None => println!("  last asked   never"),
            Some((ms, ids)) => {
                println!("  last asked   {}", human(ms));
                for l in ids {
                    println!("               {l}");
                }
            }
        }
    }
    println!("\nharnesses");
    for h in HARNESSES {
        println!("  {:<9} {}", h.name(), wiring(h, t));
    }
}

// ---------------------------------------------------------------- doctor

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[cfg(unix)]
fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(p: &Path) -> bool {
    p.is_file()
}

pub fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| executable(p))
}

/// First line of `<cli> --version`, if it answers within 5 s.
fn version(cli: &Path) -> Option<String> {
    let mut child = std::process::Command::new(cli)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    for _ in 0..100 {
        if child.try_wait().ok()?.is_some() {
            let out = child.wait_with_output().ok()?;
            return String::from_utf8_lossy(&out.stdout).lines().next().map(|l| l.trim().to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    None
}

/// Checks on the wiring alone (pure: no PATH, no processes), so they can be tested on fixtures.
/// `borg_found`: whether the omp/opencode adapters can find a borg (`$BORG_BIN` or PATH).
pub fn wiring_checks(t: &Targets, borg_found: bool) -> Vec<(Level, String)> {
    let mut out = Vec::new();
    let text = read(&t.claude_settings).unwrap_or_else(|e| {
        out.push((Level::Fail, format!("claude: {}: cannot read ({e}); borg install will not touch it", t.claude_settings.display())));
        None
    });
    if text.is_some()
        && let Err(e) = parse_settings(text.as_deref())
    {
        out.push((Level::Fail, format!("claude: {}: {e}", t.claude_settings.display())));
    }
    let wired = claude_wired(text.as_deref());
    if !wired.is_empty() {
        let missing: Vec<&str> = CLAUDE_EVENTS.iter().map(|(_, e)| *e).filter(|e| !wired.iter().any(|(x, _)| x == e)).collect();
        if !missing.is_empty() {
            out.push((Level::Warn, format!("claude: hooks missing for {} (borg install claude)", missing.join(", "))));
        }
        for (ev, bin) in &wired {
            if !executable(bin) {
                out.push((
                    Level::Fail,
                    format!("claude: hook {ev} runs {}, which does not exist or is not executable (borg install claude --apply)", bin.display()),
                ));
            }
        }
        if out.iter().all(|(l, _)| *l == Level::Ok) {
            out.push((Level::Ok, format!("claude: hooks wired to {}", wired[0].1.display())));
        }
    }
    for h in [Harness::Omp, Harness::Opencode] {
        let text = match read(t.path(h)) {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            Err(e) => {
                out.push((Level::Fail, format!("{}: {}: cannot read ({e})", h.name(), t.path(h).display())));
                continue;
            }
        };
        if text != adapter(h) {
            out.push((Level::Warn, format!("{}: {} differs from this borg's adapter (borg install {})", h.name(), t.path(h).display(), h.name())));
        } else {
            out.push((Level::Ok, format!("{}: adapter installed at {}", h.name(), t.path(h).display())));
        }
        if !borg_found {
            out.push((Level::Fail, format!("{}: the adapter finds borg via $BORG_BIN or PATH, and neither has one: it silently does nothing", h.name())));
        }
    }
    if out.is_empty() {
        out.push((Level::Warn, "no harness is wired to borg (borg install all)".into()));
    }
    out
}

/// Checks on the collectives config and stores.
pub fn collective_checks(cfg: &Result<Option<collective::Config>, String>, base: &Path) -> Vec<(Level, String)> {
    let mut out = Vec::new();
    let cfg = match cfg {
        Err(e) => {
            out.push((
                Level::Fail,
                format!("COLLECTIVES CONFIG INVALID ({e}): EVERY repo routes OFF, borg captures and asks NOTHING anywhere until it is fixed"),
            ));
            return out;
        }
        Ok(c) => c.as_ref(),
    };
    match cfg {
        None => out.push((Level::Ok, format!("collectives: no config at {}, every repo is in \"default\"", collective::config_path().display()))),
        Some(c) => {
            if c.default != "off" && !c.collectives.iter().any(|e| e.name == c.default) {
                out.push((Level::Warn, format!("collectives: default \"{}\" names no collective, so unmatched repos route off", c.default)));
            }
            let mut names: Vec<&str> = c.collectives.iter().map(|e| e.name.as_str()).collect();
            names.sort();
            if names.windows(2).any(|w| w[0] == w[1]) {
                out.push((Level::Warn, "collectives: a name appears twice; their stores are shared".into()));
            }
            out.push((Level::Ok, format!("collectives: {} configured ({})", c.collectives.len(), names.join(", "))));
        }
    }
    for r in collectives(cfg, base) {
        let Route::Collective { name, home, .. } = &r else { continue };
        let m = crate::live::of(cfg, name);
        match m.warning {
            Some(w) => out.push((Level::Warn, format!("collective {name}: {w}"))),
            None => out.push((Level::Ok, format!("collective {name}: delivery mode {} ({})", m.mode.name(), m.source))),
        }
        // Live mode cannot run without the consequence model; the hook only notes it on stderr, which the
        // agent's UI does not show.
        if m.mode == crate::live::Mode::Live
            && let Some(why) = crate::live::unsupported()
        {
            out.push((Level::Warn, format!("collective {name}: live mode is off while {why} is set; its drones check at the turn end only (hook mode)")));
        }
        let probe = home.join(".borg-doctor-probe");
        let ok = std::fs::create_dir_all(home).and_then(|_| std::fs::write(&probe, b"ok")).and_then(|_| std::fs::remove_file(&probe));
        match ok {
            Ok(()) => out.push((Level::Ok, format!("store {name}: {} writable", home.display()))),
            Err(e) => out.push((Level::Fail, format!("store {name}: {} not writable ({e})", home.display()))),
        }
    }
    out
}

/// Every engineer email a store records (lessons' authors and joiners, episodes), as written.
pub fn store_emails(home: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for l in learn::lessons(&home.join("lessons.jsonl")) {
        out.extend(l.engineers.into_iter().chain(l.joined.into_keys()));
    }
    let episodes = read(&home.join("episodes.jsonl")).ok().flatten().unwrap_or_default();
    for v in episodes.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        out.extend(v.get("engineer").and_then(Value::as_str).map(str::to_string));
    }
    out.retain(|e| !e.trim().is_empty());
    out
}

/// Hints for emails in the stores that share a git user.name (`identities`: name, email pairs) but
/// are not yet one engineer under the config's `engineers` (collective.rs "Engineers").
pub fn alias_hints(emails: &BTreeSet<String>, identities: &[(String, String)], aliases: &collective::Aliases) -> Vec<String> {
    let stored: HashMap<String, &String> = emails.iter().map(|e| (e.trim().to_lowercase(), e)).collect();
    // name (lowercased) -> (name as written, canonical engineer -> an email as the store has it)
    let mut by_name: BTreeMap<String, (&str, BTreeMap<String, &str>)> = BTreeMap::new();
    for (name, email) in identities {
        let Some(raw) = stored.get(&email.trim().to_lowercase()) else { continue };
        if name.trim().is_empty() {
            continue;
        }
        let entry = by_name.entry(name.trim().to_lowercase()).or_insert((name.trim(), BTreeMap::new()));
        entry.1.entry(aliases.canon(raw)).or_insert(raw.as_str());
    }
    by_name
        .into_values()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(name, ids)| {
            let listed: Vec<String> = ids.values().map(|e| format!("{e:?}")).collect();
            format!(
                "engineers: git user {name:?} appears in the stores as {}; if that is one engineer, add {{\"engineers\": [{{\"name\": \"...\", \"emails\": [{}]}}]}} to {} so trust and status count them once",
                listed.join(" and "),
                listed.join(", "),
                collective::config_path().display()
            )
        })
        .collect()
}

/// (user.name, user.email) pairs git knows here: the configured identity (global and the current
/// repo's) and the authors of the current repo's recent history. Best effort: no git, no pairs.
fn git_identities() -> Vec<(String, String)> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut out: Vec<(String, String)> =
        git(&["log", "-n", "2000", "--format=%aN%x09%aE"]).lines().filter_map(|l| l.split_once('\t')).map(|(n, e)| (n.to_string(), e.to_string())).collect();
    for scope in [&["config", "--global"][..], &["config"][..]] {
        let get = |k: &str| git(&[scope, &[k]].concat()).trim().to_string();
        out.push((get("user.name"), get("user.email")));
    }
    out.sort();
    out.dedup();
    out
}

fn doctor(t: &Targets) -> i32 {
    let mut checks = Vec::new();
    let env_bin = std::env::var_os("BORG_BIN").filter(|v| !v.is_empty()).map(PathBuf::from);
    let on_path = which("borg");
    match (&env_bin, &on_path) {
        (Some(b), _) if !executable(b) => checks.push((Level::Fail, format!("BORG_BIN={} does not exist or is not executable", b.display()))),
        (Some(b), _) => checks.push((Level::Ok, format!("borg: BORG_BIN={}", b.display()))),
        (None, Some(p)) => checks.push((Level::Ok, format!("borg: on PATH at {}", p.display()))),
        (None, None) => checks.push((Level::Warn, "borg: not on PATH and BORG_BIN unset (omp and opencode adapters need one)".into())),
    }
    let borg_found = env_bin.as_deref().is_some_and(executable) || on_path.is_some();
    for (cli, tested) in [("claude", ""), ("omp", " (adapter tested with 18.8.5 and 18.10.4; live mode needs 18.4.4 or newer)"), ("opencode", " (adapter tested with 1.18.32, 1.19.0, 2.0.23 and 2.0.29)")] {
        match which(cli) {
            None => checks.push((Level::Ok, format!("{cli}: not installed"))),
            Some(p) => match version(&p) {
                Some(v) => checks.push((Level::Ok, format!("{cli}: {v}{tested}"))),
                None => checks.push((Level::Warn, format!("{cli}: {} did not answer --version", p.display()))),
            },
        }
    }
    checks.extend(wiring_checks(t, borg_found));
    let cfg = collective::load();
    checks.extend(collective_checks(&cfg, &learn::home()));
    if let Ok(c) = &cfg {
        let emails: BTreeSet<String> = collectives(c.as_ref(), &learn::home())
            .iter()
            .filter_map(|r| match r {
                Route::Collective { home, .. } => Some(store_emails(home)),
                Route::Off => None,
            })
            .flatten()
            .collect();
        let aliases = c.as_ref().map(collective::Config::aliases).unwrap_or_default();
        checks.extend(alias_hints(&emails, &git_identities(), &aliases).into_iter().map(|h| (Level::Warn, h)));
    }
    match which("claude") {
        Some(_) => checks.push((Level::Ok, "claude CLI available: borg learn and the consequence model at Stop can run".into())),
        None => {
            checks.push((Level::Warn, "claude CLI not on PATH: borg learn cannot turn episodes into lessons, and the conscience stays silent at Stop".into()))
        }
    }
    for (l, msg) in &checks {
        let tag = match l {
            Level::Ok => "ok  ",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        };
        println!("{tag} {msg}");
    }
    i32::from(checks.iter().any(|(l, _)| *l == Level::Fail))
}

// ---------------------------------------------------------------- CLI

const USAGE: &str = "usage: borg install|uninstall <claude|omp|opencode|all> [--apply] [--home DIR] | borg status [--home DIR] | borg doctor [--home DIR]";

/// `borg install|uninstall|status|doctor`; returns the exit code.
pub fn main(cmd: &str, args: &[&str]) -> i32 {
    let mut apply_flag = false;
    let mut home: Option<PathBuf> = None;
    let mut which_h: Option<&str> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--apply" => apply_flag = true,
            "--home" => match it.next() {
                Some(d) => home = Some(PathBuf::from(d)),
                None => return usage(),
            },
            h if which_h.is_none() && !h.starts_with('-') => which_h = Some(h),
            _ => return usage(),
        }
    }
    let targets = match &home {
        Some(h) => {
            isolate(cmd, h);
            Targets::under(h)
        }
        None => Targets::from_env(),
    };
    match cmd {
        "status" if which_h.is_none() && !apply_flag => {
            print_status(&targets);
            0
        }
        "doctor" if which_h.is_none() && !apply_flag => doctor(&targets),
        "install" | "uninstall" => {
            let Some(hs) = which_h.and_then(Harness::parse) else { return usage() };
            let action = if cmd == "install" { Action::Install } else { Action::Uninstall };
            let bin = match std::env::var_os("BORG_BIN").filter(|v| !v.is_empty()) {
                Some(b) => PathBuf::from(b),
                None => std::env::current_exe().and_then(std::fs::canonicalize).unwrap_or_else(|_| PathBuf::from("borg")),
            };
            run(action, &hs, &targets, &bin, apply_flag)
        }
        _ => usage(),
    }
}

/// Variables `--home` removes, so every path borg resolves falls under the given HOME: the store
/// (`$BORG_HOME`, default `HOME/.local/share/borg`) and the collectives config (`$BORG_CONFIG`, default
/// `HOME/.config/borg/collectives.json`). For status and doctor `$BORG_BIN` goes too (a real binary to
/// check); install keeps it, since there it only names the binary to wire, not a place to write.
/// Harness config-dir variables need no removing: `Targets::under` never reads them.
pub fn home_overrides(cmd: &str) -> &'static [&'static str] {
    match cmd {
        "install" | "uninstall" => &["BORG_HOME", "BORG_CONFIG"],
        _ => &["BORG_HOME", "BORG_CONFIG", "BORG_BIN"],
    }
}

fn isolate(cmd: &str, home: &Path) {
    // SAFETY: single-threaded CLI start, before anything reads the environment.
    unsafe {
        std::env::set_var("HOME", home);
        for k in home_overrides(cmd) {
            std::env::remove_var(k);
        }
    }
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn run(action: Action, hs: &[Harness], t: &Targets, bin: &Path, apply_flag: bool) -> i32 {
    let stamp = format!("{}Z", utc(learn::now_ms()));
    let mut code = 0;
    for &h in hs {
        match plan(action, h, t, bin) {
            Err(e) => {
                eprintln!("{}: {e}", h.name());
                code = 1;
            }
            Ok((change, notes)) => {
                for n in notes {
                    println!("{}: {n}", h.name());
                }
                let Some(c) = change else {
                    println!("{}: nothing to change ({})", h.name(), t.path(h).display());
                    continue;
                };
                if !apply_flag {
                    print!("{}", diff(&c.path, c.before.as_deref(), c.after.as_deref()));
                    continue;
                }
                match apply(&c, &stamp) {
                    Ok(backup) => {
                        let b = backup.map(|b| format!(" (backup {})", b.display())).unwrap_or_default();
                        println!("{}: wrote {}{b}", h.name(), c.path.display());
                    }
                    Err(e) => {
                        eprintln!("{}: {}: {e}", h.name(), c.path.display());
                        code = 1;
                    }
                }
            }
        }
    }
    if !apply_flag && code == 0 {
        println!("(dry run: nothing written; add --apply to write, with a backup of every changed file)");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("borg-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A settings.json as Claude Code writes it, with someone else's hooks and settings.
    const EXISTING: &str = r#"{
  "model": "opus",
  "permissions": {
    "allow": [
      "Bash(git status)"
    ]
  },
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "notify-send done"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/usr/local/bin/guard"
          }
        ]
      }
    ]
  },
  "zz_last": true
}"#;

    fn all_files(home: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(d: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() { walk(&p, out) } else { out.push((p.clone(), std::fs::read(&p).unwrap())) }
            }
        }
        let mut out = Vec::new();
        walk(home, &mut out);
        out.sort();
        out
    }

    fn install_all(t: &Targets, bin: &Path, action: Action, stamp: &str) -> usize {
        let mut n = 0;
        for h in HARNESSES {
            if let (Some(c), _) = plan(action, h, t, bin).unwrap() {
                apply(&c, stamp).unwrap();
                n += 1;
            }
        }
        n
    }

    #[test]
    fn install_preserves_existing_hooks_and_settings() {
        let home = tmp("preserve");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.claude_settings.parent().unwrap()).unwrap();
        std::fs::write(&t.claude_settings, EXISTING).unwrap();
        let bin = Path::new("/opt/borg dir/borg");
        assert_eq!(install_all(&t, bin, Action::Install, "s1"), 3);

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&t.claude_settings).unwrap()).unwrap();
        assert_eq!(v["model"], "opus");
        assert_eq!(v["zz_last"], true);
        assert_eq!(v["permissions"]["allow"][0], "Bash(git status)");
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "/usr/local/bin/guard");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "notify-send done", "their Stop hook stays first");
        assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["command"], "'/opt/borg dir/borg' hook claude stop");
        assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["timeout"], CLAUDE_STOP_TIMEOUT_S, "the stop hook's budget is explicit");
        assert!(v["hooks"]["PostToolUse"][0]["hooks"][0].get("timeout").is_none());
        // An install from before the timeout is brought up to it.
        let old = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/b/borg hook claude stop"}]}]}}"#;
        let up: Value = serde_json::from_str(&claude_install(Some(old), Path::new("/b/borg")).unwrap().unwrap()).unwrap();
        assert_eq!(up["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(up["hooks"]["Stop"][0]["hooks"][0]["timeout"], CLAUDE_STOP_TIMEOUT_S);
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "permissions", "hooks", "zz_last"], "key order kept");
        let wired = claude_wired(Some(&std::fs::read_to_string(&t.claude_settings).unwrap()));
        assert_eq!(wired.len(), CLAUDE_EVENTS.len());
        assert!(wired.iter().all(|(_, b)| b == bin), "quoted path parsed back");
        assert_eq!(std::fs::read_to_string(&t.omp_extension).unwrap(), OMP_ADAPTER);
        assert_eq!(std::fs::read_to_string(&t.opencode_plugin).unwrap(), OPENCODE_ADAPTER);
        let backup = t.claude_settings.with_file_name("settings.json.borg-backup-s1");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), EXISTING, "backup of the original");
    }

    #[test]
    fn install_is_idempotent_and_replaces_a_stale_binary() {
        let home = tmp("idem");
        let t = Targets::under(&home);
        let bin = Path::new("/usr/bin/borg");
        assert_eq!(install_all(&t, bin, Action::Install, "a"), 3);
        let snapshot = all_files(&home);
        assert_eq!(install_all(&t, bin, Action::Install, "b"), 0, "second install changes nothing");
        assert_eq!(all_files(&home), snapshot, "and writes nothing, not even a backup");

        // A moved binary: each event points at the new one, never twice.
        let text = claude_install(read(&t.claude_settings).unwrap().as_deref(), Path::new("/new/borg")).unwrap().unwrap();
        let wired = claude_wired(Some(&text));
        assert_eq!(wired.len(), CLAUDE_EVENTS.len());
        assert!(wired.iter().all(|(_, b)| b == Path::new("/new/borg")));
    }

    #[test]
    fn uninstall_restores_the_original_exactly() {
        for (name, original) in [("existing", Some(EXISTING)), ("existing-nl", Some(&*format!("{EXISTING}\n"))), ("absent", None)] {
            let home = tmp(&format!("restore-{name}"));
            let t = Targets::under(&home);
            if let Some(o) = original {
                std::fs::create_dir_all(t.claude_settings.parent().unwrap()).unwrap();
                std::fs::write(&t.claude_settings, o).unwrap();
            }
            install_all(&t, Path::new("/usr/bin/borg"), Action::Install, "i");
            assert_eq!(install_all(&t, Path::new("/elsewhere/borg"), Action::Uninstall, "u"), 3);
            assert_eq!(read(&t.claude_settings).unwrap().as_deref(), original, "{name}: byte for byte");
            assert!(!t.omp_extension.exists() && !t.opencode_plugin.exists());
            assert_eq!(install_all(&t, Path::new("/usr/bin/borg"), Action::Uninstall, "u2"), 0, "nothing left to remove");
        }
    }

    #[test]
    fn uninstall_leaves_foreign_files_alone() {
        let home = tmp("foreign");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.omp_extension.parent().unwrap()).unwrap();
        std::fs::write(&t.omp_extension, "// my own borg.ts").unwrap();
        let (change, notes) = plan(Action::Uninstall, Harness::Omp, &t, Path::new("/b/borg")).unwrap();
        assert!(change.is_none());
        assert_eq!(notes.len(), 1);
        // A hook that merely mentions borg but is not borg's own command stays.
        let s = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/x/notborg hook claude stop"}]}]}}"#;
        assert_eq!(claude_uninstall(Some(s)).unwrap(), None);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let home = tmp("dry");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.claude_settings.parent().unwrap()).unwrap();
        std::fs::write(&t.claude_settings, EXISTING).unwrap();
        let before = all_files(&home);
        assert_eq!(run(Action::Install, &HARNESSES, &t, Path::new("/usr/bin/borg"), false), 0);
        assert_eq!(all_files(&home), before);
        let (c, _) = plan(Action::Install, Harness::Claude, &t, Path::new("/usr/bin/borg")).unwrap();
        let c = c.unwrap();
        let d = diff(&c.path, c.before.as_deref(), c.after.as_deref());
        assert!(d.contains("+            \"command\": \"/usr/bin/borg hook claude stop\""), "{d}");
        assert!(!d.lines().any(|l| l.starts_with('-') && !l.starts_with("---")), "pure addition:\n{d}");
    }

    #[test]
    fn invalid_settings_are_never_overwritten() {
        assert!(claude_install(Some("{ not json"), Path::new("/b/borg")).is_err());
        assert!(claude_install(Some(r#"{"hooks": []}"#), Path::new("/b/borg")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn adapter_symlink_is_replaced_not_written_through() {
        let home = tmp("link");
        let t = Targets::under(&home);
        let src = home.join("checkout-borg.ts");
        std::fs::write(&src, "// older adapter in a checkout").unwrap();
        std::fs::create_dir_all(t.omp_extension.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&src, &t.omp_extension).unwrap();
        let (c, _) = plan(Action::Install, Harness::Omp, &t, Path::new("/b/borg")).unwrap();
        apply(&c.unwrap(), "x").unwrap();
        assert_eq!(std::fs::read_to_string(&src).unwrap(), "// older adapter in a checkout");
        assert_eq!(std::fs::read_to_string(&t.omp_extension).unwrap(), OMP_ADAPTER);
    }

    #[test]
    fn status_counts_a_store() {
        let home = tmp("stats");
        let w = |f: &str, s: &str| std::fs::write(home.join(f), s).unwrap();
        w("episodes.jsonl", "{\"ms\":5}\n{\"ms\":9}\n");
        w("lessons.jsonl", "");
        w(
            "outcomes.jsonl",
            "{\"kind\":\"asked\",\"session\":\"s\",\"lessons\":[\"L1\"],\"ms\":3,\"repo\":\"r\"}\n{\"kind\":\"asked\",\"session\":\"s\",\"lessons\":[\"L2\"],\"ms\":7,\"repo\":\"r\"}\n",
        );
        let s = stats(&home);
        assert_eq!((s.lessons, s.episodes, s.outcomes, s.last_capture_ms), (0, 2, 2, Some(9)));
        assert_eq!(s.last_asked, Some((7, vec!["L2".to_string()])));
        assert_eq!(stats(&home.join("absent")), Stats::default());
        let cfg: collective::Config =
            serde_json::from_str(r#"{"collectives":[{"name":"corp","paths":["/w"]},{"name":"mine","paths":["/p"],"share":"local"}]}"#).unwrap();
        let rs = collectives(Some(&cfg), Path::new("/b"));
        assert_eq!(rs.iter().map(collective::may_sync).collect::<Vec<_>>(), [true, false]);
        assert_eq!(rs[1], Route::Collective { name: "mine".into(), home: "/b/collectives/mine".into(), share: Share::Local });
        assert_eq!(utc(0), "19700101T000000");
        assert_eq!(utc(1_791_590_400_000), "20261010T000000");
    }

    #[test]
    fn doctor_hints_at_emails_sharing_a_git_name_until_they_are_one_engineer() {
        let home = tmp("aliases");
        let line = |v: Value| format!("{v}\n");
        std::fs::write(
            home.join("lessons.jsonl"),
            line(json!({"id": "a", "origin_ms": 1, "repo": "r", "question": "q", "situations": [], "consequence": "", "support": 1,
                "episodes": [], "history": [], "version": 2, "engineers": ["Dana.Example@corp.example", "ann@x"], "joined": {"ann@x": 5}})),
        )
        .unwrap();
        std::fs::write(home.join("episodes.jsonl"), line(json!({"ms": 1, "engineer": "da@home.example"})) + &line(json!({"ms": 2, "engineer": ""}))).unwrap();
        let emails = store_emails(&home);
        assert_eq!(emails.iter().map(String::as_str).collect::<Vec<_>>(), ["Dana.Example@corp.example", "ann@x", "da@home.example"]);
        let ids = |v: &[(&str, &str)]| v.iter().map(|(n, e)| (n.to_string(), e.to_string())).collect::<Vec<_>>();
        let known =
            ids(&[("Dana Example", "dana.example@corp.example"), ("Dana Example", "da@home.example"), ("Ann", "ann@x"), ("Dana Example", "elsewhere@x")]);
        let hints = alias_hints(&emails, &known, &collective::Aliases::default());
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert!(
            hints[0].contains("\"Dana Example\"") && hints[0].contains("Dana.Example@corp.example") && hints[0].contains("da@home.example"),
            "{}",
            hints[0]
        );
        assert!(!hints[0].contains("elsewhere@x"), "only emails the stores have");
        let cfg: collective::Config =
            serde_json::from_str(r#"{"engineers": [{"name": "dana", "emails": ["da@home.example", "Dana.Example@corp.example"]}]}"#).unwrap();
        assert!(alias_hints(&emails, &known, &cfg.aliases()).is_empty(), "already one engineer");
        assert!(alias_hints(&emails, &ids(&[("Ann", "ann@x"), ("Dana Example", "da@home.example")]), &collective::Aliases::default()).is_empty());
    }

    #[test]
    fn doctor_flags_a_missing_binary_and_an_invalid_config() {
        let home = tmp("doctor");
        let t = Targets::under(&home);
        plan(Action::Install, Harness::Claude, &t, Path::new("/nonexistent/borg")).unwrap().0.map(|c| apply(&c, "d").unwrap());
        let checks = wiring_checks(&t, true);
        assert!(checks.iter().any(|(l, m)| *l == Level::Fail && m.contains("/nonexistent/borg")), "{checks:?}");
        let bad = collective_checks(&Err("x.json: expected value".into()), &home);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].0 == Level::Fail && bad[0].1.contains("EVERY repo routes OFF"));
        assert!(collective_checks(&Ok(None), &home.join("store")).iter().all(|(l, _)| *l == Level::Ok));
    }

    #[cfg(unix)]
    fn chmod(p: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn full_claude_install_reports_wired_not_partial() {
        let home = tmp("wired-status");
        let t = Targets::under(&home);
        assert_eq!(run(Action::Install, &[Harness::Claude], &t, Path::new("/b/borg"), true), 0);
        assert!(wiring(Harness::Claude, &t).starts_with("wired: "), "{}", wiring(Harness::Claude, &t));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_are_errors_not_absent() {
        let home = tmp("unreadable");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.claude_settings.parent().unwrap()).unwrap();
        let precious = r#"{"model":"precious"}"#;
        std::fs::write(&t.claude_settings, precious).unwrap();
        chmod(&t.claude_settings, 0o000);
        if std::fs::read(&t.claude_settings).is_ok() {
            chmod(&t.claude_settings, 0o600);
            return; // running as root: permissions do not apply
        }
        let before = std::fs::read_dir(t.claude_settings.parent().unwrap()).unwrap().count();
        for action in [Action::Install, Action::Uninstall] {
            assert!(plan(action, Harness::Claude, &t, Path::new("/b/borg")).is_err());
        }
        assert_eq!(run(Action::Install, &[Harness::Claude], &t, Path::new("/b/borg"), true), 1);
        assert!(wiring(Harness::Claude, &t).starts_with("UNREADABLE"));
        assert!(wiring_checks(&t, true).iter().any(|(l, m)| *l == Level::Fail && m.contains("cannot read")));
        chmod(&t.claude_settings, 0o600);
        assert_eq!(std::fs::read_to_string(&t.claude_settings).unwrap(), precious, "untouched");
        assert_eq!(std::fs::read_dir(t.claude_settings.parent().unwrap()).unwrap().count(), before, "no new files");

        // Not UTF-8: an adapter file someone else wrote, likewise never replaced.
        std::fs::create_dir_all(t.omp_extension.parent().unwrap()).unwrap();
        std::fs::write(&t.omp_extension, [0xff, 0xfe, 0x00]).unwrap();
        assert!(plan(Action::Install, Harness::Omp, &t, Path::new("/b/borg")).is_err());
        assert_eq!(std::fs::read(&t.omp_extension).unwrap(), [0xff, 0xfe, 0x00]);
    }

    #[test]
    fn a_binary_not_named_borg_is_refused() {
        let home = tmp("borg-dev");
        let t = Targets::under(&home);
        for bin in ["/x/borg-dev", "/x/borg-ai", "/x/notborg"] {
            let e = plan(Action::Install, Harness::Claude, &t, Path::new(bin)).unwrap_err();
            assert!(e.contains("BORG_BIN"), "{e}");
        }
        assert!(!t.claude_settings.exists());
        // Named borg: installed once, however often it runs.
        for stamp in ["1", "2", "3"] {
            install_all(&t, Path::new("/x/dev/borg"), Action::Install, stamp);
        }
        let text = std::fs::read_to_string(&t.claude_settings).unwrap();
        assert_eq!(text.matches("hook claude stop").count(), 1);
        assert_eq!(claude_wired(Some(&text)).len(), CLAUDE_EVENTS.len());
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_kept_and_new_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let home = tmp("perms");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.claude_settings.parent().unwrap()).unwrap();
        std::fs::write(&t.claude_settings, r#"{"env":{"SECRET":"x"}}"#).unwrap();
        chmod(&t.claude_settings, 0o600);
        let (c, _) = plan(Action::Install, Harness::Claude, &t, Path::new("/usr/bin/borg")).unwrap();
        let backup = apply(&c.unwrap(), "p").unwrap().unwrap();
        assert_eq!(mode(&t.claude_settings), 0o600, "settings stay 0600");
        assert_eq!(mode(&backup), 0o600, "backup is 0600");

        chmod(&t.claude_settings, 0o640);
        let (c, _) = plan(Action::Uninstall, Harness::Claude, &t, Path::new("/usr/bin/borg")).unwrap();
        apply(&c.unwrap(), "q").unwrap();
        assert_eq!(mode(&t.claude_settings), 0o640, "whatever the original had");

        let fresh = tmp("perms-new");
        let t = Targets::under(&fresh);
        let (c, _) = plan(Action::Install, Harness::Claude, &t, Path::new("/usr/bin/borg")).unwrap();
        apply(&c.unwrap(), "n").unwrap();
        assert_eq!(mode(&t.claude_settings), 0o600, "a new settings.json is private");
    }

    #[test]
    fn a_backup_is_never_overwritten() {
        let home = tmp("backups");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.claude_settings.parent().unwrap()).unwrap();
        let orig = r#"{"model":"orig"}"#;
        std::fs::write(&t.claude_settings, orig).unwrap();
        // Install then uninstall within one stamp (the same second).
        let (c, _) = plan(Action::Install, Harness::Claude, &t, Path::new("/usr/bin/borg")).unwrap();
        let b1 = apply(&c.unwrap(), "same").unwrap().unwrap();
        let (c, _) = plan(Action::Uninstall, Harness::Claude, &t, Path::new("/usr/bin/borg")).unwrap();
        let b2 = apply(&c.unwrap(), "same").unwrap().unwrap();
        assert_ne!(b1, b2);
        assert_eq!(std::fs::read_to_string(&b1).unwrap(), orig, "the original survives");
        assert!(std::fs::read_to_string(&b2).unwrap().contains("hook claude stop"));
        assert!(b2.to_string_lossy().ends_with("borg-backup-same-1"));
    }

    #[test]
    fn home_overrides_cover_store_and_config() {
        for cmd in ["install", "uninstall", "status", "doctor"] {
            let v = home_overrides(cmd);
            assert!(v.contains(&"BORG_HOME") && v.contains(&"BORG_CONFIG"), "{cmd}");
        }
        assert!(home_overrides("doctor").contains(&"BORG_BIN"));
        assert!(!home_overrides("install").contains(&"BORG_BIN"), "install still wires $BORG_BIN");
    }
}
