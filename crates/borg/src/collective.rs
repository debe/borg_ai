//! Separate minds: which collective a repo belongs to, or none.
//!
//! An engineer can be part of several collectives (a work one shared with the team, a private
//! one that never leaves this machine) and can keep repos out of every collective. The mapping lives
//! only on this machine, in `$BORG_CONFIG` (default `~/.config/borg/collectives.json`):
//!
//! ```json
//! { "default": "off",
//!   "off": ["~/secret"],
//!   "collectives": [
//!     { "name": "work", "paths": ["~/work-repos"], "share": "team" },
//!     { "name": "private",   "paths": ["~/private"],    "share": "local" } ] }
//! ```
//!
//! Rules, in order: an `off` path wins over everything (also over a collective path above it);
//! otherwise the collective with the longest matching path; otherwise `default` (`"off"` or a
//! collective name). Paths match whole components, so `~/work` never matches `~/work-repos`.
//! Without a config file every repo belongs to one collective, "default", stored at `BORG_HOME`.
//!
//! Each collective has its own store (`<BORG_HOME>/collectives/<name>/`): lessons, episodes,
//! outcomes and scratch state. A drone only ever reads its own collective, so nothing crosses
//! between collectives in either direction. `share: "local"` means the collective must never be
//! synced to any peer: `may_sync` is the one gate a mesh has to ask.
//!
//! A collective may also set `"mode": "live"` (live.rs): its conscience then also judges the work during
//! the turn, not only at its end. `BORG_MODE` overrides it; absent or unknown is `"hook"`.
//!
//! A collective may also list `experts` (expert repos its drones consult at the Stop checkpoint,
//! see expert_route.rs); only the routed collective's experts are ever read, and never one whose
//! repo lies under an `off` path or in another collective with `share: "local"`.
//!
//! Store names must never collide: if "my team" (local) and "my_team" (team) shared a directory,
//! syncing the team collective would ship the local one's lessons. So a name that is not already a
//! safe directory name gets a hash of the raw name appended (`my_team-1a2b3c4d`), and a config in
//! which two collectives still resolve to one store (also by case, for case-insensitive
//! filesystems) is rejected at load, which routes everything off.
//!
//! **Engineers**: one engineer may commit under several git emails (work and personal). An optional
//! `engineers` section names them, so trust, lesson authorship, the queen's budget, `borg status`
//! and `borg top` count them as one:
//!
//! ```json
//! { "engineers": [ { "name": "dana", "emails": ["Dana.Example@corp.example", "da@home.example"] } ] }
//! ```
//!
//! Emails always compare case-insensitively (`Ann@X` is `ann@x`); a listed email reads as its
//! engineer's name. Stored data is never rewritten: lessons, episodes and presence keep the raw
//! email and the mapping is applied when they are read, so removing an entry restores the split.
//! The section is local config like the rest of this file and is never synced: a team peer reads
//! the same lessons with its own config (or none), so it may see the raw emails and count them as
//! separate engineers until it lists them too. An entry without a name or emails, a name listed
//! twice, or an email listed under two engineers makes the config invalid (everything routes off).

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Share {
    /// May be synced with the collective's members.
    #[default]
    Team,
    /// Never leaves this machine.
    Local,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub name: String,
    pub paths: Vec<String>,
    #[serde(default)]
    pub share: Share,
    /// Repos whose expertise this collective's drones may consult at the Stop checkpoint
    /// (expert_route.rs). Absent or empty: the feature is off for this collective.
    #[serde(default)]
    pub experts: Vec<ExpertSource>,
    /// How the mesh reaches this collective's members across NAT (borg-mesh): absent or
    /// `"default"` for n0's public relays and address lookup, `"disabled"` for direct addresses
    /// only (LAN or VPN), or the URL of a self-hosted `iroh-relay`. `BORG_RELAY` overrides it.
    #[serde(default)]
    pub relay: Option<String>,
    /// When the conscience judges the work (live.rs): `"hook"` at the turn end, or `"live"` (the
    /// default) also during the turn. `BORG_MODE` overrides it; any other value is the default, with a warning
    /// in `borg doctor`. A string, not an enum: an unknown value must not invalidate the whole config.
    #[serde(default)]
    pub mode: Option<String>,
}

/// An expert repo: the engineer's local checkout of a dependency, consulted when a drone's
/// manifests depend on it (`match`: group, artifact, package or crate names) and its plan touches it.
/// `{"name": "flowlib", "repo": "~/src/flowlib", "match": ["flowlib", "org.flowlib"], "drone": false}`
#[derive(Debug, Clone, Deserialize)]
pub struct ExpertSource {
    pub name: String,
    pub repo: String,
    #[serde(rename = "match", default)]
    pub matches: Vec<String>,
    /// Ask a read-only expert drone (budget-capped) when the index has nothing. Off by default.
    #[serde(default)]
    pub drone: bool,
}

impl ExpertSource {
    /// The repo path, `~` expanded.
    pub fn path(&self) -> PathBuf {
        expand(&self.repo)
    }
}

impl Config {
    /// The experts of the collective `cwd` routes to; none for an off repo or another collective's.
    /// An expert repo behind the boundary is never consulted (see [`Config::may_consult`]).
    pub fn experts_for(&self, cwd: &str) -> Vec<&ExpertSource> {
        let Route::Collective { name, .. } = route(Some(self), cwd, Path::new("/")) else { return Vec::new() };
        let Some(c) = self.collectives.iter().find(|c| c.name == name) else { return Vec::new() };
        c.experts.iter().filter(|e| self.may_consult(&c.name, e)).collect()
    }

    /// Whether `collective`'s drones may read expert `e`: its repo must not lie under an `off` path
    /// (no collective may ever see it) nor in another collective that never leaves this machine
    /// (`share: local`): its claims would land in this collective's transcripts, and from there in
    /// lessons that sync. Checked on the path as written and on the path symlinks resolve to. An
    /// expert repo outside every collective is fine: listing it as an expert is the opt-in.
    pub fn may_consult(&self, collective: &str, e: &ExpertSource) -> bool {
        let p = e.path();
        let real = p.canonicalize().ok();
        std::iter::once(p).chain(real).all(|p| {
            if self.off.iter().any(|o| inside(&normal(&p), o).is_some()) {
                return false;
            }
            !matches!(route(Some(self), &p.to_string_lossy(), Path::new("/")),
                Route::Collective { name, share: Share::Local, .. } if name != collective)
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// "off", or the name of the collective for repos no rule matches.
    #[serde(default = "off")]
    pub default: String,
    /// Repos no collective may ever see: the hooks do nothing there.
    #[serde(default)]
    pub off: Vec<String>,
    #[serde(default)]
    pub collectives: Vec<Entry>,
    /// Engineers known under several git emails (see the module doc).
    #[serde(default, deserialize_with = "engineers")]
    pub engineers: Vec<Engineer>,
    /// The local collective that receives personal lessons (reach.rs); optional when there is exactly one.
    #[serde(default)]
    pub personal: Option<String>,
}

/// The `engineers` section; its errors name it, so `borg status` and `borg doctor` point at it.
fn engineers<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Engineer>, D::Error> {
    Vec::<Engineer>::deserialize(d).map_err(|e| serde::de::Error::custom(format!("engineers: {e}")))
}

/// One engineer and every git email they commit under.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engineer {
    pub name: String,
    pub emails: Vec<String>,
}

/// Who an engineer is, as every count and comparison of engineers sees them: a listed email reads
/// as its engineer's name, any other email lowercased. Empty for no config or no `engineers`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Aliases {
    by_email: std::collections::HashMap<String, String>,
}

impl Aliases {
    pub fn new(engineers: &[Engineer]) -> Aliases {
        let by_email = engineers.iter().flat_map(|e| e.emails.iter().map(move |m| (m.trim().to_lowercase(), e.name.trim().to_string()))).collect();
        Aliases { by_email }
    }

    /// The canonical engineer for a raw git email: the listed name, else the email lowercased.
    /// Empty stays empty (unknown engineer).
    pub fn canon(&self, email: &str) -> String {
        let key = email.trim().to_lowercase();
        self.by_email.get(&key).cloned().unwrap_or(key)
    }

    /// For display: the listed name, else the email as written.
    pub fn display(&self, email: &str) -> String {
        self.by_email.get(&email.trim().to_lowercase()).cloned().unwrap_or_else(|| email.to_string())
    }

    /// Whether two raw emails are the same engineer.
    pub fn same(&self, a: &str, b: &str) -> bool {
        self.canon(a) == self.canon(b)
    }
}

impl Config {
    pub fn aliases(&self) -> Aliases {
        Aliases::new(&self.engineers)
    }
}

/// This machine's aliases, read once per process from the collectives config. No config, an
/// invalid one (which routes everything off anyway) or no `engineers` section: none. Unit tests
/// of this crate never read the engineer's real config.
pub fn aliases() -> &'static Aliases {
    static A: std::sync::OnceLock<Aliases> = std::sync::OnceLock::new();
    A.get_or_init(|| {
        if cfg!(test) {
            return Aliases::default();
        }
        load().ok().flatten().map(|c| c.aliases()).unwrap_or_default()
    })
}

fn off() -> String {
    "off".into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Not part of any collective: no capture, no recall, no state written.
    Off,
    Collective {
        name: String,
        home: PathBuf,
        share: Share,
    },
}

pub fn config_path() -> PathBuf {
    std::env::var("BORG_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/borg/collectives.json"))
}

/// The config, if present. An unreadable or invalid config routes everything off: a broken file
/// must never leak a private repo into a shared collective.
pub fn load() -> Result<Option<Config>, String> {
    load_from(&config_path())
}

/// `load` from an explicit path (the mesh re-reads it per session; tests use temp files).
pub fn load_from(p: &Path) -> Result<Option<Config>, String> {
    match std::fs::read_to_string(p) {
        Ok(t) => {
            let cfg: Config = serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))?;
            cfg.check().map_err(|e| format!("{}: {e}", p.display()))?;
            Ok(Some(cfg))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

impl Config {
    /// Every collective must have a store of its own (see the module doc).
    pub fn check(&self) -> Result<(), String> {
        if let Some(p) = &self.personal
            && !self.collectives.iter().any(|c| &c.name == p && c.share == Share::Local)
        {
            return Err(format!("personal: {p:?} is not a collective with \"share\": \"local\""));
        }
        let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
        for c in &self.collectives {
            if let Some(other) = seen.insert(store_name(&c.name).to_lowercase(), &c.name) {
                return Err(format!("collectives {other:?} and {:?} would share one store; rename one", c.name));
            }
        }
        let mut names = std::collections::HashSet::new();
        let mut owner: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
        for e in &self.engineers {
            if e.name.trim().is_empty() {
                return Err("engineers: an entry has no name".into());
            }
            if !names.insert(e.name.trim().to_lowercase()) {
                return Err(format!("engineers: {:?} is listed twice; merge the entries", e.name));
            }
            if e.emails.is_empty() {
                return Err(format!("engineers: {:?} lists no emails", e.name));
            }
            for m in e.emails.iter().map(|m| m.trim().to_lowercase()) {
                if m.is_empty() {
                    return Err(format!("engineers: {:?} lists an empty email", e.name));
                }
                if let Some(other) = owner.insert(m.clone(), &e.name).filter(|o| *o != e.name) {
                    return Err(format!("engineers: {m} is listed under both {other:?} and {:?}", e.name));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None if p == "~" => PathBuf::from(std::env::var("HOME").unwrap_or_default()),
        None => PathBuf::from(p),
    }
}

/// Lexically normalised absolute path (no filesystem access: the hook must stay fast).
fn normal(p: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    out
}

/// Number of components of `prefix` if `path` lies inside it (whole components only).
fn inside(path: &[String], prefix: &str) -> Option<usize> {
    let pre = normal(&expand(prefix));
    (!pre.is_empty() && path.len() >= pre.len() && path[..pre.len()] == pre[..]).then_some(pre.len())
}

/// The store directory of collective `name`: the name itself when it is already a safe directory
/// name (so existing stores stay put), otherwise the sanitised name plus a hash of the raw name.
fn store_name(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if safe == name && !safe.is_empty() {
        return safe;
    }
    // FNV-1a: stable across builds and platforms, no dependency in the hook binary. Collisions are
    // caught by `Config::check`; the hash only has to keep distinct names apart in practice.
    let h = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    format!("{safe}-{:08x}", h as u32 ^ (h >> 32) as u32)
}

/// A collective's own store under the root store `base`.
pub fn store(base: &Path, name: &str) -> PathBuf {
    base.join("collectives").join(store_name(name))
}

/// The store of the local collective that receives an engineer's personal lessons (reach.rs): the one
/// `personal` names, else the only `share: local` collective; none when there is none or several. `home`
/// is any collective's store (or the root store), the root is derived from it.
pub fn personal_store(cfg: Option<&Config>, home: &Path) -> Option<PathBuf> {
    let cfg = cfg?;
    let base = match home.parent() {
        Some(p) if p.file_name().is_some_and(|n| n == "collectives") => p.parent()?.to_path_buf(),
        _ => home.to_path_buf(),
    };
    let local: Vec<&Entry> = cfg.collectives.iter().filter(|c| c.share == Share::Local).collect();
    let pick = match &cfg.personal {
        Some(name) => local.iter().find(|c| &c.name == name).copied(),
        None if local.len() == 1 => local.first().copied(),
        None => None,
    }?;
    Some(store(&base, &pick.name))
}

/// A `share: local` collective of `cfg` whose store is `home`, if any. Second guard for a sync: even
/// a config that slipped past `Config::check` never ships a local collective's files.
pub fn local_owner_of<'a>(cfg: Option<&'a Config>, home: &Path, base: &Path) -> Option<&'a str> {
    cfg?.collectives.iter().find(|c| c.share == Share::Local && store(base, &c.name) == home).map(|c| c.name.as_str())
}

/// Which collective `cwd` belongs to. `base` is the root store (`BORG_HOME`).
pub fn route(cfg: Option<&Config>, cwd: &str, base: &Path) -> Route {
    let Some(cfg) = cfg else {
        return Route::Collective { name: "default".into(), home: base.to_path_buf(), share: Share::Team };
    };
    if cwd.trim().is_empty() {
        return Route::Off; // unknown location: never guess
    }
    let path = normal(&expand(cwd));
    if cfg.off.iter().any(|p| inside(&path, p).is_some()) {
        return Route::Off;
    }
    let best =
        cfg.collectives.iter().filter_map(|c| c.paths.iter().filter_map(|p| inside(&path, p)).max().map(|n| (n, c))).max_by_key(|(n, _)| *n).map(|(_, c)| c);
    let chosen = best.or_else(|| cfg.collectives.iter().find(|c| c.name == cfg.default && cfg.default != "off"));
    match chosen {
        Some(c) => Route::Collective { name: c.name.clone(), home: store(base, &c.name), share: c.share },
        None => Route::Off,
    }
}

/// The one gate any sync (mesh, export, gossip) must ask before a collective's data leaves this
/// machine. Local collectives and off repos never sync.
pub fn may_sync(r: &Route) -> bool {
    matches!(r, Route::Collective { share: Share::Team, .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(json: &str) -> Config {
        serde_json::from_str(json).unwrap()
    }

    const CFG: &str = r#"{ "default": "off",
        "off": ["/home/u/private/diary"],
        "collectives": [
          { "name": "work", "paths": ["/home/u/work-repos"], "share": "team" },
          { "name": "private", "paths": ["/home/u/private"], "share": "local" },
          { "name": "libs", "paths": ["/home/u/work-repos/shared-libs"] } ] }"#;

    fn name(r: &Route) -> &str {
        match r {
            Route::Off => "off",
            Route::Collective { name, .. } => name,
        }
    }

    #[test]
    fn routes_by_longest_whole_component_prefix() {
        let c = cfg(CFG);
        let base = Path::new("/b");
        assert_eq!(name(&route(Some(&c), "/home/u/work-repos/harbor_orion/src", base)), "work");
        assert_eq!(name(&route(Some(&c), "/home/u/work-repos/shared-libs/x", base)), "libs", "longest prefix wins");
        assert_eq!(name(&route(Some(&c), "/home/u/work", base)), "off", "no partial component match");
        assert_eq!(name(&route(Some(&c), "/home/u/work-repos-old/x", base)), "off", "work-repos-old is not work-repos");
        assert_eq!(name(&route(Some(&c), "/home/u/private/finances", base)), "private");
        assert_eq!(name(&route(Some(&c), "/home/u/work-repos/../private/x", base)), "private", "normalised before matching");
    }

    #[test]
    fn off_wins_and_unknown_is_off() {
        let c = cfg(CFG);
        let base = Path::new("/b");
        assert_eq!(route(Some(&c), "/home/u/private/diary/2026", base), Route::Off, "off beats the private collective above it");
        assert_eq!(route(Some(&c), "/somewhere/else", base), Route::Off, "default off");
        assert_eq!(route(Some(&c), "", base), Route::Off, "no cwd: never guess");
        let d = cfg(r#"{ "default": "work", "collectives": [ { "name": "work", "paths": ["/w"] } ] }"#);
        assert_eq!(name(&route(Some(&d), "/somewhere/else", base)), "work");
    }

    #[test]
    fn stores_are_separate_and_local_never_syncs() {
        let c = cfg(CFG);
        let base = Path::new("/b");
        let corp = route(Some(&c), "/home/u/work-repos/a", base);
        let private = route(Some(&c), "/home/u/private/a", base);
        let (Route::Collective { home: h1, .. }, Route::Collective { home: h2, .. }) = (&corp, &private) else { panic!() };
        assert_ne!(h1, h2);
        assert_eq!(h2, &PathBuf::from("/b/collectives/private"));
        assert!(may_sync(&corp));
        assert!(!may_sync(&private), "a local collective never leaves the machine");
        assert!(!may_sync(&Route::Off));
    }

    #[test]
    fn similar_names_get_separate_stores() {
        let c = cfg(r#"{ "collectives": [
              { "name": "my team", "paths": ["/l"], "share": "local" },
              { "name": "my_team", "paths": ["/t"], "share": "team" } ] }"#);
        let base = Path::new("/b");
        let (Route::Collective { home: local, .. }, Route::Collective { home: team, .. }) = (route(Some(&c), "/l/x", base), route(Some(&c), "/t/x", base))
        else {
            panic!()
        };
        assert_ne!(local, team, "a local collective never shares a store with a team one");
        assert_eq!(team, PathBuf::from("/b/collectives/my_team"), "safe names keep their store");
        assert!(local.to_string_lossy().starts_with("/b/collectives/my_team-"));
        assert_ne!(store_name("a.b"), store_name("a_b"));
        assert_ne!(store_name("équipe"), store_name("_quipe"));
        assert_eq!(store_name("a.b"), store_name("a.b"), "stable");
        assert!(c.check().is_ok());
        assert_eq!(local_owner_of(Some(&c), &team, base), None);
        assert_eq!(local_owner_of(Some(&c), &local, base), Some("my team"));
    }

    #[test]
    fn a_config_whose_collectives_share_a_store_is_rejected() {
        for json in [
            r#"{ "collectives": [ { "name": "x", "paths": ["/a"], "share": "local" }, { "name": "x", "paths": ["/b"] } ] }"#,
            r#"{ "collectives": [ { "name": "Team", "paths": ["/a"], "share": "local" }, { "name": "team", "paths": ["/b"] } ] }"#,
        ] {
            assert!(cfg(json).check().unwrap_err().contains("share one store"), "{json}");
            let p = std::env::temp_dir().join(format!("borg-collective-dup-{}.json", std::process::id()));
            std::fs::write(&p, json).unwrap();
            assert!(load_from(&p).is_err(), "load rejects it, so everything routes off");
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn expert_repos_behind_the_boundary_are_never_consulted() {
        let c = cfg(r#"{ "default": "off", "off": ["/home/u/secret"],
            "collectives": [
              { "name": "work", "paths": ["/home/u/work"], "share": "team", "experts": [
                  { "name": "open", "repo": "/home/u/src/flowlib" },
                  { "name": "mine", "repo": "/home/u/work/sdk" },
                  { "name": "offrepo", "repo": "/home/u/secret/sdk" },
                  { "name": "privaterepo", "repo": "/home/u/private/flowlib" },
                  { "name": "sneaky", "repo": "/home/u/work/../private/x" } ] },
              { "name": "private", "paths": ["/home/u/private"], "share": "local", "experts": [
                  { "name": "own", "repo": "/home/u/private/flowlib" },
                  { "name": "team", "repo": "/home/u/work/sdk" },
                  { "name": "off", "repo": "/home/u/secret/sdk" } ] } ] }"#);
        let names = |cwd: &str| c.experts_for(cwd).iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("/home/u/work/app"), ["open", "mine"], "off and local-private expert repos are dropped");
        assert_eq!(names("/home/u/private/app"), ["own", "team"], "a local collective may read its own and team repos, never off ones");
        assert!(names("/home/u/secret/app").is_empty());
        // A symlink into an off path is resolved.
        let d = std::env::temp_dir().canonicalize().unwrap().join(format!("borg-collective-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("secret/sdk")).unwrap();
        std::os::unix::fs::symlink(d.join("secret/sdk"), d.join("link")).unwrap();
        let c = cfg(&format!(
            r#"{{ "off": ["{0}/secret"], "collectives": [ {{ "name": "t", "paths": ["/w"], "experts": [ {{ "name": "x", "repo": "{0}/link" }} ] }} ] }}"#,
            d.display()
        ));
        assert!(c.experts_for("/w/app").is_empty(), "a symlink into an off path");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn engineers_map_aliases_case_insensitively() {
        let c = cfg(r#"{ "engineers": [ { "name": "dana", "emails": ["Dana.Example@corp.example", "da@home.example"] } ] }"#);
        assert!(c.check().is_ok());
        let a = c.aliases();
        assert_eq!(a.canon("dana.example@CORP.example"), "dana");
        assert_eq!(a.canon(" da@home.example "), "dana");
        assert!(a.same("Dana.Example@corp.example", "da@home.example"));
        assert_eq!(a.canon("Ann@X"), "ann@x", "unlisted emails still compare case-insensitively");
        assert_eq!(a.display("Ann@X"), "Ann@X", "and display as written");
        assert_eq!(a.display("DA@home.example"), "dana");
        assert_eq!(a.canon(""), "");
        assert_eq!(cfg(CFG).aliases(), Aliases::default(), "absent section: no aliases");
    }

    #[test]
    fn an_invalid_engineers_section_is_rejected_at_load() {
        for (json, why) in [
            (r#"{ "engineers": [ { "name": "", "emails": ["a@x"] } ] }"#, "has no name"),
            (r#"{ "engineers": [ { "name": "a", "emails": [] } ] }"#, "lists no emails"),
            (r#"{ "engineers": [ { "name": "a", "emails": ["a@x", " "] } ] }"#, "empty email"),
            (r#"{ "engineers": [ { "name": "a", "emails": ["a@x"] }, { "name": "A", "emails": ["b@x"] } ] }"#, "listed twice"),
            (r#"{ "engineers": [ { "name": "a", "emails": ["a@x"] }, { "name": "b", "emails": ["A@X"] } ] }"#, "listed under both"),
            (r#"{ "engineers": [ { "name": "a", "email": ["a@x"] } ] }"#, "unknown field"),
            (r#"{ "engineers": { "name": "a" } }"#, "invalid type"),
        ] {
            let p = std::env::temp_dir().join(format!("borg-collective-eng-{}.json", std::process::id()));
            std::fs::write(&p, json).unwrap();
            let e = load_from(&p).unwrap_err();
            assert!(e.contains(why) && e.contains(&p.display().to_string()), "{json}: {e}");
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn no_config_keeps_one_default_collective() {
        let r = route(None, "/anything", Path::new("/b"));
        assert_eq!(r, Route::Collective { name: "default".into(), home: PathBuf::from("/b"), share: Share::Team });
    }
}
