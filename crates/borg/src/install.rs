//! Wiring borg into a harness, and seeing whether it works:
//! `borg install|uninstall <harness|all> [--apply] [--home DIR]`, `borg status`, `borg doctor`.
//!
//! The wiring is exactly what each adapter README describes, written into the engineer's *user* config,
//! as each harness's module says (harness/): command hooks merged into a JSON file (Claude Code's
//! `~/.claude/settings.json`, each running `<this borg> hook claude <event>`), or a compiled-in adapter
//! copied whole (omp's extension, opencode's plugin), plus the `borg` skill wherever the harness loads
//! skills (`install/skill.rs`). An installed borg needs no checkout.
//!
//! Someone else's config is precious, so: the default is a dry run that prints the exact diff;
//! `--apply` first writes a timestamped backup next to every file it changes; merging keeps every
//! other key and hook as it was; re-running changes nothing; uninstall removes only what install adds
//! (borg's own hook commands, and adapter and skill files whose content is this borg's). `--home`
//! replaces HOME *and ignores every config-dir variable* (the harnesses' and `$BORG_HOME`/`$BORG_CONFIG`,
//! see [`home_overrides`]), so a test can never reach a real config or store.

pub mod change;
pub mod setup;
pub mod skill;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::collective::{self, Route, Share};
pub use crate::harness::Level;
use crate::harness::wiring::Hooks;
use crate::harness::{self, Harness, Wiring};
use crate::{learn, outcome};
use change::read;
pub use change::{Action, Change, apply, diff};

/// `all` (every visible harness) or one harness by name.
fn parse(s: &str) -> Option<Vec<&'static dyn Harness>> {
    match s {
        "all" => Some(harness::visible().collect()),
        _ => harness::by_name(s).map(|h| vec![h]),
    }
}

/// Where the harnesses read their user config.
#[derive(Debug, Clone)]
pub struct Targets {
    home: PathBuf,
    /// Whether each harness's own config-dir variables count (not under `--home`).
    env: bool,
}

fn var(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl Targets {
    /// Everything under `home`, no environment consulted (tests, `--home`).
    pub fn under(home: &Path) -> Self {
        Targets { home: home.to_path_buf(), env: false }
    }

    /// The real locations, honouring each harness's own config-dir variables.
    pub fn from_env() -> Self {
        Targets { home: var("HOME").unwrap_or_default(), env: true }
    }

    pub fn path(&self, h: &dyn Harness) -> PathBuf {
        h.target(&self.home, &|k| if self.env { var(k) } else { None })
    }

    /// Where `h` loads the `borg` skill from; None for a harness without skills.
    pub fn skill_dir(&self, h: &dyn Harness) -> Option<PathBuf> {
        skill::dir(h, &self.home, &|k| if self.env { var(k) } else { None })
    }
}

// ---------------------------------------------------------------- plans

/// What `action` would change for `h`: its hook config or adapter first, then each skill file
/// (`install/skill.rs`); plus notes: things deliberately left alone, and on install the steps the harness
/// itself asks of the engineer (`Harness::install_notes`).
pub fn plan(action: Action, h: &dyn Harness, t: &Targets, bin: &Path) -> Result<(Vec<Change>, Vec<String>), String> {
    let path = t.path(h);
    let before = read(&path).map_err(|e| format!("{}: cannot read ({e}); nothing was changed", path.display()))?;
    let mut notes = Vec::new();
    let after = match (Hooks::of(h), action) {
        (Some(k), Action::Install) => k.install(before.as_deref(), bin)?.map(Some),
        (Some(k), Action::Uninstall) => k.uninstall(before.as_deref())?,
        (None, Action::Install) => (before.as_deref() != Some(h.wiring().adapter())).then(|| Some(h.wiring().adapter().to_string())),
        (None, Action::Uninstall) => match before.as_deref() {
            Some(b) if b == h.wiring().adapter() => Some(None),
            Some(_) => {
                notes.push(format!("{}: left alone, its content is not this borg's adapter (yours, or an older borg's)", path.display()));
                None
            }
            None => None,
        },
    };
    let through_links = matches!(h.wiring(), Wiring::CommandHooks { through_links: true, .. });
    let mut changes: Vec<Change> = after.map(|after| Change { path, before, after, through_links, backup: true }).into_iter().collect();
    let (skill, left) = skill::plan(action, t.skill_dir(h).as_deref())?;
    changes.extend(skill);
    notes.extend(left);
    if action == Action::Install {
        notes.extend(h.install_notes());
    }
    Ok((changes, notes))
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

/// How `h` is wired under `t`, in one line: its hooks or adapter, then its skill.
pub fn wiring(h: &dyn Harness, t: &Targets) -> String {
    let line = hooks_line(h, t);
    match skill::status(h.name(), t.skill_dir(h).as_deref()) {
        Some(skill) => format!("{line}; {skill}"),
        None => line,
    }
}

/// How `h`'s hooks or adapter are wired under `t`.
fn hooks_line(h: &dyn Harness, t: &Targets) -> String {
    let path = t.path(h);
    let text = match read(&path) {
        Ok(t) => t,
        Err(e) => return format!("UNREADABLE: {} ({e})", path.display()),
    };
    let Some(k) = Hooks::of(h) else {
        return match text {
            None => format!("not wired ({})", path.display()),
            Some(t) if t == h.wiring().adapter() => format!("wired: {}", path.display()),
            Some(_) => format!("wired, but {} differs from this borg's adapter (older or edited)", path.display()),
        };
    };
    let w = k.wired(text.as_deref());
    let missing = k.missing(&w);
    let events: Vec<&str> = k.events.iter().map(|(_, e)| *e).filter(|e| !missing.contains(e)).collect();
    let mut bins: Vec<String> = w.iter().map(|(_, b)| b.display().to_string()).collect();
    bins.dedup();
    match events.len() {
        0 => format!("not wired ({})", path.display()),
        n if n == k.events.len() => format!("wired: {} -> {}", path.display(), bins.join(", ")),
        _ => format!("PARTIAL ({} only): {} -> {}", events.join(","), path.display(), bins.join(", ")),
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
    for &h in harness::ALL {
        let line = wiring(h, t);
        // A hidden harness (the simulated one) only once someone wired it.
        if !h.hidden() || !line.starts_with("not wired") {
            println!("  {:<9} {line}", h.name());
        }
    }
}

// ---------------------------------------------------------------- doctor

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

/// `borg-<part>` beside this borg, else on PATH: main.rs runs it, `borg setup` runs borg-mesh.
#[must_use]
pub fn sibling(part: &str) -> Option<PathBuf> {
    let bin = format!("borg-{part}");
    let beside = std::env::current_exe().ok().map(|e| e.with_file_name(&bin)).filter(|p| p.exists());
    beside.or_else(|| which(&bin))
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
/// `borg_found`: whether the adapters can find a borg (`$BORG_BIN` or PATH).
pub fn wiring_checks(t: &Targets, borg_found: bool) -> Vec<(Level, String)> {
    let mut out = Vec::new();
    // Every harness: an unwired one adds nothing, so a hidden one shows only once someone wired it.
    for &h in harness::ALL {
        let found = match Hooks::of(h) {
            Some(k) => hook_checks(h, &k, t),
            None => adapter_checks(h, t, borg_found),
        };
        let skill = skill::checks(h.name(), t.skill_dir(h).as_deref(), !found.is_empty());
        out.extend(found.into_iter().chain(skill));
    }
    if out.is_empty() {
        out.push((Level::Warn, "no harness is wired to borg (borg install all)".into()));
    }
    out
}

/// A command-hook harness: its config readable and valid, every event wired, to a binary that runs.
fn hook_checks(h: &dyn Harness, k: &Hooks, t: &Targets) -> Vec<(Level, String)> {
    let (name, path) = (h.name(), t.path(h));
    let mut out = Vec::new();
    let text = read(&path).unwrap_or_else(|e| {
        out.push((Level::Fail, format!("{name}: {}: cannot read ({e}); borg install will not touch it", path.display())));
        None
    });
    if text.is_some()
        && let Err(e) = k.parse(text.as_deref())
    {
        out.push((Level::Fail, format!("{name}: {}: {e}", path.display())));
    }
    let wired = k.wired(text.as_deref());
    if wired.is_empty() {
        return out;
    }
    let missing = k.missing(&wired);
    if !missing.is_empty() {
        out.push((Level::Warn, format!("{name}: hooks missing for {} (borg install {name})", missing.join(", "))));
    }
    for (ev, bin) in &wired {
        if !executable(bin) {
            out.push((
                Level::Fail,
                format!("{name}: hook {ev} runs {}, which does not exist or is not executable (borg install {name} --apply)", bin.display()),
            ));
        }
    }
    if out.iter().all(|(l, _)| *l == Level::Ok) {
        out.push((Level::Ok, format!("{name}: hooks wired to {}", wired[0].1.display())));
    }
    out
}

/// An adapter harness: the installed file is this borg's adapter, and the adapter can find borg.
fn adapter_checks(h: &dyn Harness, t: &Targets, borg_found: bool) -> Vec<(Level, String)> {
    let (name, path) = (h.name(), t.path(h));
    let text = match read(&path) {
        Ok(Some(text)) => text,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![(Level::Fail, format!("{name}: {}: cannot read ({e})", path.display()))],
    };
    let mut out = Vec::new();
    if text != h.wiring().adapter() {
        out.push((Level::Warn, format!("{name}: {} differs from this borg's adapter (borg install {name})", path.display())));
    } else {
        out.push((Level::Ok, format!("{name}: adapter installed at {}", path.display())));
    }
    if !borg_found {
        out.push((Level::Fail, format!("{name}: the adapter finds borg via $BORG_BIN or PATH, and neither has one: it silently does nothing")));
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

/// Each harness's CLI (its version, as its [`harness::Probe`] says) and, once the CLI answered, its own
/// checks (`Harness::checks`: they may ask the CLI, which a missing or hung one cannot answer).
fn harness_checks<'a>(hs: impl Iterator<Item = &'a dyn Harness>, t: &Targets) -> Vec<(Level, String)> {
    let mut checks = Vec::new();
    for h in hs {
        let harness::Probe { cli, tested } = h.probe();
        match which(cli) {
            None => checks.push((Level::Ok, format!("{cli}: not installed"))),
            Some(p) => match version(&p) {
                Some(v) => {
                    checks.push((Level::Ok, format!("{cli}: {v}{tested}")));
                    checks.extend(h.checks(&t.path(h)));
                }
                None => checks.push((Level::Warn, format!("{cli}: {} did not answer --version", p.display()))),
            },
        }
    }
    checks
}

fn doctor(t: &Targets) -> i32 {
    let mut checks = Vec::new();
    let env_bin = std::env::var_os("BORG_BIN").filter(|v| !v.is_empty()).map(PathBuf::from);
    let on_path = which("borg");
    match (&env_bin, &on_path) {
        (Some(b), _) if !executable(b) => checks.push((Level::Fail, format!("BORG_BIN={} does not exist or is not executable", b.display()))),
        (Some(b), _) => checks.push((Level::Ok, format!("borg: BORG_BIN={}", b.display()))),
        (None, Some(p)) => checks.push((Level::Ok, format!("borg: on PATH at {}", p.display()))),
        (None, None) => {
            let adapters: Vec<&str> = harness::visible().filter(|h| matches!(h.wiring(), Wiring::AdapterFile { .. })).map(|h| h.name()).collect();
            checks.push((Level::Warn, format!("borg: not on PATH and BORG_BIN unset ({} adapters need one)", adapters.join(" and "))));
        }
    }
    let borg_found = env_bin.as_deref().is_some_and(executable) || on_path.is_some();
    checks.extend(harness_checks(harness::visible(), t));
    checks.extend(wiring_checks(t, borg_found));
    let cfg = collective::load();
    checks.extend(collective_checks(&cfg, &learn::home()));
    if let Ok(c) = &cfg {
        let routes = collectives(c.as_ref(), &learn::home());
        checks.extend(crate::vectors::doctor::checks(&routes));
        let emails: BTreeSet<String> = routes
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
    let spec = std::env::var("BORG_PREDICT_MODEL").ok();
    checks.push(match crate::runner::availability(spec.as_deref(), &crate::runner::on_path) {
        Ok(m) => (Level::Ok, m),
        Err(m) => (Level::Warn, m),
    });
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

fn usage_line() -> String {
    format!("usage: borg install|uninstall <{}|all> [--apply] [--home DIR] | borg status [--home DIR] | borg doctor [--home DIR]", harness::names("|"))
}

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
            let Some(hs) = which_h.and_then(parse) else { return usage() };
            let action = if cmd == "install" { Action::Install } else { Action::Uninstall };
            run(action, &hs, &targets, &bin(), apply_flag)
        }
        _ => usage(),
    }
}

/// The borg the hooks run: `$BORG_BIN`, else this binary.
pub(crate) fn bin() -> PathBuf {
    match std::env::var_os("BORG_BIN").filter(|v| !v.is_empty()) {
        Some(b) => PathBuf::from(b),
        None => std::env::current_exe().and_then(std::fs::canonicalize).unwrap_or_else(|_| PathBuf::from("borg")),
    }
}

/// Variables `--home` removes, so every path borg resolves falls under the given HOME: the store
/// (`$BORG_HOME`, default `HOME/.local/share/borg`), the collectives config (`$BORG_CONFIG`, default
/// `HOME/.config/borg/collectives.json`) and the vector model (`$BORG_VECTOR_MODEL`, default under
/// `HOME/.local/share/borg/models`). For status and doctor `$BORG_BIN` goes too (a real binary to
/// check); install (and `borg setup`, which installs) keeps it, since there it only names the binary to
/// wire, not a place to write.
/// Harness config-dir variables need no removing: `Targets::under` never reads them.
pub fn home_overrides(cmd: &str) -> &'static [&'static str] {
    match cmd {
        "install" | "uninstall" | "setup" => &["BORG_HOME", "BORG_CONFIG", "BORG_VECTOR_MODEL"],
        _ => &["BORG_HOME", "BORG_CONFIG", "BORG_VECTOR_MODEL", "BORG_BIN"],
    }
}

pub(crate) fn isolate(cmd: &str, home: &Path) {
    // SAFETY: single-threaded CLI start, before anything reads the environment.
    unsafe {
        std::env::set_var("HOME", home);
        for k in home_overrides(cmd) {
            std::env::remove_var(k);
        }
    }
}

fn usage() -> i32 {
    eprintln!("{}", usage_line());
    2
}

fn run(action: Action, hs: &[&dyn Harness], t: &Targets, bin: &Path, apply_flag: bool) -> i32 {
    let stamp = format!("{}Z", utc(learn::now_ms()));
    let mut code = 0;
    for &h in hs {
        let changes = match plan(action, h, t, bin) {
            Err(e) => {
                eprintln!("{}: {e}", h.name());
                code = 1;
                continue;
            }
            Ok((changes, notes)) => {
                for n in notes {
                    println!("{}: {n}", h.name());
                }
                changes
            }
        };
        if changes.is_empty() {
            println!("{}: nothing to change ({})", h.name(), t.path(h).display());
        } else if apply_flag {
            code = code.max(write(h, &changes, &stamp));
            t.skill_dir(h).inspect(|d| skill::tidy(d));
        } else {
            for c in &changes {
                print!("{}", diff(&c.path, c.before.as_deref(), c.after.as_deref()));
            }
        }
    }
    footer(action, apply_flag, code);
    code
}

/// After a clean run: the dry-run note, or after an install what is left that install never does.
fn footer(action: Action, apply_flag: bool, code: i32) {
    if code != 0 {
        return;
    }
    if !apply_flag {
        println!("(dry run: nothing written; add --apply to write, with a backup of every changed file)");
    } else if action == Action::Install
        && let Some(hint) = crate::vectors::doctor::hint()
    {
        println!("{hint}");
    }
}

/// Writes `h`'s changes in order, each reported on its own line; 1 if any failed.
pub(crate) fn write(h: &dyn Harness, changes: &[Change], stamp: &str) -> i32 {
    let mut code = 0;
    for c in changes {
        match apply(c, stamp) {
            Ok(backup) => {
                let b = backup.map(|b| format!(" (backup {})", b.display())).unwrap_or_default();
                let what = if c.after.is_some() { "wrote" } else { "removed" };
                println!("{}: {what} {}{b}", h.name(), c.path.display());
            }
            Err(e) => {
                eprintln!("{}: {}: {e}", h.name(), c.path.display());
                code = 1;
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{claude, omp, opencode};
    use serde_json::json;

    const CLAUDE: &dyn Harness = &claude::Claude;
    const OMP: &dyn Harness = &omp::Omp;
    const OPENCODE: &dyn Harness = &opencode::Opencode;

    fn claude_hooks() -> Hooks {
        Hooks::of(CLAUDE).unwrap()
    }

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

    /// Applies every visible harness's plan as `run` does; the number of files changed.
    fn install_all(t: &Targets, bin: &Path, action: Action, stamp: &str) -> usize {
        let mut n = 0;
        for h in harness::visible() {
            for c in plan(action, h, t, bin).unwrap().0 {
                apply(&c, stamp).unwrap();
                n += 1;
            }
            t.skill_dir(h).inspect(|d| skill::tidy(d));
        }
        n
    }

    /// The files a full install writes: one config or adapter per harness, and the skill's files
    /// wherever the harness loads skills.
    fn files_per_install(t: &Targets) -> usize {
        harness::visible().map(|h| 1 + t.skill_dir(h).map_or(0, |_| skill::FILES.len())).sum()
    }

    #[test]
    fn install_preserves_existing_hooks_and_settings() {
        let home = tmp("preserve");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.path(CLAUDE).parent().unwrap()).unwrap();
        std::fs::write(t.path(CLAUDE), EXISTING).unwrap();
        let bin = Path::new("/opt/borg dir/borg");
        assert_eq!(install_all(&t, bin, Action::Install, "s1"), files_per_install(&t), "one config per harness, and the skill");

        let v: Value = serde_json::from_str(&std::fs::read_to_string(t.path(CLAUDE)).unwrap()).unwrap();
        assert_eq!(v["model"], "opus");
        assert_eq!(v["zz_last"], true);
        assert_eq!(v["permissions"]["allow"][0], "Bash(git status)");
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "/usr/local/bin/guard");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "notify-send done", "their Stop hook stays first");
        assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["command"], "'/opt/borg dir/borg' hook claude stop");
        assert_eq!(v["hooks"]["Stop"][1]["hooks"][0]["timeout"], claude::STOP_TIMEOUT_S, "the stop hook's budget is explicit");
        assert!(v["hooks"]["PostToolUse"][0]["hooks"][0].get("timeout").is_none());
        // An install from before the timeout is brought up to it.
        let old = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/b/borg hook claude stop"}]}]}}"#;
        let up: Value = serde_json::from_str(&claude_hooks().install(Some(old), Path::new("/b/borg")).unwrap().unwrap()).unwrap();
        assert_eq!(up["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(up["hooks"]["Stop"][0]["hooks"][0]["timeout"], claude::STOP_TIMEOUT_S);
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "permissions", "hooks", "zz_last"], "key order kept");
        let wired = claude_hooks().wired(Some(&std::fs::read_to_string(t.path(CLAUDE)).unwrap()));
        assert_eq!(wired.len(), claude::EVENTS.len());
        assert!(wired.iter().all(|(_, b)| b == bin), "quoted path parsed back");
        assert_eq!(std::fs::read_to_string(t.path(OMP)).unwrap(), omp::ADAPTER);
        assert_eq!(std::fs::read_to_string(t.path(OPENCODE)).unwrap(), opencode::ADAPTER);
        let backup = t.path(CLAUDE).with_file_name("settings.json.borg-backup-s1");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), EXISTING, "backup of the original");
    }

    #[test]
    fn install_is_idempotent_and_replaces_a_stale_binary() {
        let home = tmp("idem");
        let t = Targets::under(&home);
        let bin = Path::new("/usr/bin/borg");
        assert_eq!(install_all(&t, bin, Action::Install, "a"), files_per_install(&t), "one config per harness, and the skill");
        let snapshot = all_files(&home);
        assert_eq!(install_all(&t, bin, Action::Install, "b"), 0, "second install changes nothing");
        assert_eq!(all_files(&home), snapshot, "and writes nothing, not even a backup");

        // A moved binary: each event points at the new one, never twice.
        let text = claude_hooks().install(read(&t.path(CLAUDE)).unwrap().as_deref(), Path::new("/new/borg")).unwrap().unwrap();
        let wired = claude_hooks().wired(Some(&text));
        assert_eq!(wired.len(), claude::EVENTS.len());
        assert!(wired.iter().all(|(_, b)| b == Path::new("/new/borg")));
    }

    #[test]
    fn uninstall_restores_the_original_exactly() {
        for (name, original) in [("existing", Some(EXISTING)), ("existing-nl", Some(&*format!("{EXISTING}\n"))), ("absent", None)] {
            let home = tmp(&format!("restore-{name}"));
            let t = Targets::under(&home);
            if let Some(o) = original {
                std::fs::create_dir_all(t.path(CLAUDE).parent().unwrap()).unwrap();
                std::fs::write(t.path(CLAUDE), o).unwrap();
            }
            install_all(&t, Path::new("/usr/bin/borg"), Action::Install, "i");
            assert_eq!(install_all(&t, Path::new("/elsewhere/borg"), Action::Uninstall, "u"), files_per_install(&t), "everything install wrote");
            assert_eq!(read(&t.path(CLAUDE)).unwrap().as_deref(), original, "{name}: byte for byte");
            assert!(!t.path(OMP).exists() && !t.path(OPENCODE).exists() && !t.path(&crate::harness::codex::Codex).exists());
            for h in harness::visible() {
                assert!(t.skill_dir(h).is_none_or(|d| !d.exists()), "{name}: {}'s skill directory removed", h.name());
            }
            assert_eq!(install_all(&t, Path::new("/usr/bin/borg"), Action::Uninstall, "u2"), 0, "nothing left to remove");
        }
    }

    #[test]
    fn uninstall_leaves_foreign_files_alone() {
        let home = tmp("foreign");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.path(OMP).parent().unwrap()).unwrap();
        std::fs::write(t.path(OMP), "// my own borg.ts").unwrap();
        let (change, notes) = plan(Action::Uninstall, OMP, &t, Path::new("/b/borg")).unwrap();
        assert!(change.is_empty());
        assert_eq!(notes.len(), 1);
        // The engineer's own file in borg's skill directory: left alone, and so is the directory.
        let skill = t.skill_dir(CLAUDE).unwrap();
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "my own skill").unwrap();
        std::fs::write(skill.join("notes.md"), "mine").unwrap();
        install_all(&t, Path::new("/b/borg"), Action::Install, "i");
        assert_eq!(install_all(&t, Path::new("/b/borg"), Action::Uninstall, "u"), files_per_install(&t));
        assert_eq!(std::fs::read_to_string(skill.join("notes.md")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(skill.join("SKILL.md.borg-backup-i")).unwrap(), "my own skill", "replaced with a backup");
        // A hook that merely mentions borg but is not borg's own command stays.
        let s = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/x/notborg hook claude stop"}]}]}}"#;
        assert_eq!(claude_hooks().uninstall(Some(s)).unwrap(), None);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let home = tmp("dry");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.path(CLAUDE).parent().unwrap()).unwrap();
        std::fs::write(t.path(CLAUDE), EXISTING).unwrap();
        let before = all_files(&home);
        assert_eq!(run(Action::Install, &harness::visible().collect::<Vec<_>>(), &t, Path::new("/usr/bin/borg"), false), 0);
        assert_eq!(all_files(&home), before);
        let (c, _) = plan(Action::Install, CLAUDE, &t, Path::new("/usr/bin/borg")).unwrap();
        let c = &c[0];
        let d = diff(&c.path, c.before.as_deref(), c.after.as_deref());
        assert!(d.contains("+            \"command\": \"/usr/bin/borg hook claude stop\""), "{d}");
        assert!(!d.lines().any(|l| l.starts_with('-') && !l.starts_with("---")), "pure addition:\n{d}");
    }

    #[test]
    fn invalid_settings_are_never_overwritten() {
        assert!(claude_hooks().install(Some("{ not json"), Path::new("/b/borg")).is_err());
        assert!(claude_hooks().install(Some(r#"{"hooks": []}"#), Path::new("/b/borg")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn adapter_symlink_is_replaced_not_written_through() {
        let home = tmp("link");
        let t = Targets::under(&home);
        let src = home.join("checkout-borg.ts");
        std::fs::write(&src, "// older adapter in a checkout").unwrap();
        std::fs::create_dir_all(t.path(OMP).parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&src, t.path(OMP)).unwrap();
        let (c, _) = plan(Action::Install, OMP, &t, Path::new("/b/borg")).unwrap();
        apply(&c[0], "x").unwrap();
        assert_eq!(std::fs::read_to_string(&src).unwrap(), "// older adapter in a checkout");
        assert_eq!(std::fs::read_to_string(t.path(OMP)).unwrap(), omp::ADAPTER);
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
        plan(Action::Install, CLAUDE, &t, Path::new("/nonexistent/borg")).unwrap().0.iter().for_each(|c| drop(apply(c, "d").unwrap()));
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
        assert_eq!(run(Action::Install, &[CLAUDE], &t, Path::new("/b/borg"), true), 0);
        assert!(wiring(CLAUDE, &t).starts_with("wired: "), "{}", wiring(CLAUDE, &t));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_are_errors_not_absent() {
        let home = tmp("unreadable");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.path(CLAUDE).parent().unwrap()).unwrap();
        let precious = r#"{"model":"precious"}"#;
        std::fs::write(t.path(CLAUDE), precious).unwrap();
        chmod(&t.path(CLAUDE), 0o000);
        if std::fs::read(t.path(CLAUDE)).is_ok() {
            chmod(&t.path(CLAUDE), 0o600);
            return; // running as root: permissions do not apply
        }
        let before = std::fs::read_dir(t.path(CLAUDE).parent().unwrap()).unwrap().count();
        for action in [Action::Install, Action::Uninstall] {
            assert!(plan(action, CLAUDE, &t, Path::new("/b/borg")).is_err());
        }
        assert_eq!(run(Action::Install, &[CLAUDE], &t, Path::new("/b/borg"), true), 1);
        assert!(wiring(CLAUDE, &t).starts_with("UNREADABLE"));
        assert!(wiring_checks(&t, true).iter().any(|(l, m)| *l == Level::Fail && m.contains("cannot read")));
        chmod(&t.path(CLAUDE), 0o600);
        assert_eq!(std::fs::read_to_string(t.path(CLAUDE)).unwrap(), precious, "untouched");
        assert_eq!(std::fs::read_dir(t.path(CLAUDE).parent().unwrap()).unwrap().count(), before, "no new files");

        // Not UTF-8: an adapter file someone else wrote, likewise never replaced.
        std::fs::create_dir_all(t.path(OMP).parent().unwrap()).unwrap();
        std::fs::write(t.path(OMP), [0xff, 0xfe, 0x00]).unwrap();
        assert!(plan(Action::Install, OMP, &t, Path::new("/b/borg")).is_err());
        assert_eq!(std::fs::read(t.path(OMP)).unwrap(), [0xff, 0xfe, 0x00]);
    }

    #[test]
    fn a_binary_not_named_borg_is_refused() {
        let home = tmp("borg-dev");
        let t = Targets::under(&home);
        for bin in ["/x/borg-dev", "/x/borg-ai", "/x/notborg"] {
            let e = plan(Action::Install, CLAUDE, &t, Path::new(bin)).unwrap_err();
            assert!(e.contains("BORG_BIN"), "{e}");
        }
        assert!(!t.path(CLAUDE).exists());
        // Named borg: installed once, however often it runs.
        for stamp in ["1", "2", "3"] {
            install_all(&t, Path::new("/x/dev/borg"), Action::Install, stamp);
        }
        let text = std::fs::read_to_string(t.path(CLAUDE)).unwrap();
        assert_eq!(text.matches("hook claude stop").count(), 1);
        assert_eq!(claude_hooks().wired(Some(&text)).len(), claude::EVENTS.len());
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_kept_and_new_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let home = tmp("perms");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.path(CLAUDE).parent().unwrap()).unwrap();
        std::fs::write(t.path(CLAUDE), r#"{"env":{"SECRET":"x"}}"#).unwrap();
        chmod(&t.path(CLAUDE), 0o600);
        let (c, _) = plan(Action::Install, CLAUDE, &t, Path::new("/usr/bin/borg")).unwrap();
        let backup = apply(&c[0], "p").unwrap().unwrap();
        assert_eq!(mode(&t.path(CLAUDE)), 0o600, "settings stay 0600");
        assert_eq!(mode(&backup), 0o600, "backup is 0600");

        chmod(&t.path(CLAUDE), 0o640);
        let (c, _) = plan(Action::Uninstall, CLAUDE, &t, Path::new("/usr/bin/borg")).unwrap();
        apply(&c[0], "q").unwrap();
        assert_eq!(mode(&t.path(CLAUDE)), 0o640, "whatever the original had");

        let fresh = tmp("perms-new");
        let t = Targets::under(&fresh);
        let (c, _) = plan(Action::Install, CLAUDE, &t, Path::new("/usr/bin/borg")).unwrap();
        apply(&c[0], "n").unwrap();
        assert_eq!(mode(&t.path(CLAUDE)), 0o600, "a new settings.json is private");
    }

    #[test]
    fn a_backup_is_never_overwritten() {
        let home = tmp("backups");
        let t = Targets::under(&home);
        std::fs::create_dir_all(t.path(CLAUDE).parent().unwrap()).unwrap();
        let orig = r#"{"model":"orig"}"#;
        std::fs::write(t.path(CLAUDE), orig).unwrap();
        // Install then uninstall within one stamp (the same second).
        let (c, _) = plan(Action::Install, CLAUDE, &t, Path::new("/usr/bin/borg")).unwrap();
        let b1 = apply(&c[0], "same").unwrap().unwrap();
        let (c, _) = plan(Action::Uninstall, CLAUDE, &t, Path::new("/usr/bin/borg")).unwrap();
        let b2 = apply(&c[0], "same").unwrap().unwrap();
        assert_ne!(b1, b2);
        assert_eq!(std::fs::read_to_string(&b1).unwrap(), orig, "the original survives");
        assert!(std::fs::read_to_string(&b2).unwrap().contains("hook claude stop"));
        assert!(b2.to_string_lossy().ends_with("borg-backup-same-1"));
    }

    #[test]
    fn home_overrides_cover_store_and_config() {
        for cmd in ["install", "uninstall", "setup", "status", "doctor"] {
            let v = home_overrides(cmd);
            assert!(v.contains(&"BORG_HOME") && v.contains(&"BORG_CONFIG") && v.contains(&"BORG_VECTOR_MODEL"), "{cmd}");
        }
        assert!(home_overrides("doctor").contains(&"BORG_BIN"));
        assert!(!home_overrides("install").contains(&"BORG_BIN"), "install still wires $BORG_BIN");
    }

    /// A harness with a step of its own in the harness (as Codex's hook trust will be).
    /// A harness with its own install notes and doctor check; its CLI is `.0`.
    struct Noted(&'static str);

    impl Harness for Noted {
        fn name(&self) -> &'static str {
            "noted"
        }
        fn caps(&self) -> harness::Caps {
            CLAUDE.caps()
        }
        fn wiring(&self) -> Wiring {
            CLAUDE.wiring()
        }
        fn target(&self, home: &Path, _var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
            home.join(".noted/hooks.json")
        }
        fn probe(&self) -> harness::Probe {
            harness::Probe { cli: self.0, tested: "" }
        }
        fn checks(&self, target: &Path) -> Vec<(Level, String)> {
            vec![(Level::Warn, format!("noted: {} is not trusted", target.display()))]
        }
        fn install_notes(&self) -> Vec<String> {
            vec!["run /hooks in noted and trust borg".into()]
        }
    }

    #[test]
    fn a_harness_adds_its_own_install_notes_and_doctor_checks() {
        let home = tmp("noted");
        let t = Targets::under(&home);
        let (change, notes) = plan(Action::Install, &Noted("git"), &t, Path::new("/b/borg")).unwrap();
        assert_eq!(change.len(), 1 + skill::FILES.len(), "the hook config and the skill");
        assert_eq!(notes, ["run /hooks in noted and trust borg"]);
        assert_eq!(plan(Action::Uninstall, &Noted("git"), &t, Path::new("/b/borg")).unwrap().1, Vec::<String>::new());
        let checks = harness_checks([&Noted("git") as &dyn Harness].into_iter(), &t);
        assert!(checks[0].0 == Level::Ok && checks[0].1.starts_with("git: git version"), "{checks:?}");
        assert_eq!(checks[1], (Level::Warn, format!("noted: {} is not trusted", home.join(".noted/hooks.json").display())));
        let missing = harness_checks([&Noted("borg-test-no-such-cli") as &dyn Harness].into_iter(), &t);
        assert_eq!(missing, [(Level::Ok, "borg-test-no-such-cli: not installed".into())], "no CLI to ask: its own checks wait");
    }
}
