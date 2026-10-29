//! The system gate (internal evaluations "tags" and "concepts", not published): the system a session works on comes from the
//! repo it runs in and the repos of the paths it touches, never from what the prompts or the drone's
//! messages talk about. When that repo maps to a known system, a lesson scoped to other systems scores 0
//! (early delivery and Stop), however often the conversation names them. When it maps to none (an
//! unknown repo, or a handbook that spans systems), today's scope rule applies (main.rs `where_now`).
//!
//! Systems are the alias map plus the collective's own scope names that a known repo name contains
//! (`zeus_core` makes a scope `Zeus` a system). The map is the user's `$BORG_SYSTEMS` (default:
//! `systems.json` next to the collectives config, `~/.config/borg/systems.json`) when that file exists,
//! else the example in data/systems.json. `BORG_SYSTEM_GATE=off` disables the gate.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

use crate::conscience::{Challenge, Names, segments};

struct Map {
    /// (system, name parts that mean it)
    systems: Vec<(String, Vec<String>)>,
    /// Exact repo name -> its systems (empty: spans systems).
    repos: HashMap<String, Vec<String>>,
}

fn map() -> &'static Map {
    static M: OnceLock<Map> = OnceLock::new();
    M.get_or_init(|| {
        let v = user_map().unwrap_or_else(|| serde_json::from_str(include_str!("../data/systems.json")).expect("data/systems.json"));
        let list =
            |x: &Value| -> Vec<String> { x.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_lowercase)).collect()).unwrap_or_default() };
        let obj = |k: &str| v[k].as_object().cloned().unwrap_or_default().into_iter();
        Map { systems: obj("systems").map(|(k, x)| (k, list(&x))).collect(), repos: obj("repos").map(|(k, x)| (k.to_lowercase(), list(&x))).collect() }
    })
}

/// The user's own map: `$BORG_SYSTEMS`, else `systems.json` next to the collectives config. Absent or
/// invalid: none (an invalid file is reported once on stderr). Unit tests always use the shipped example.
fn user_map() -> Option<Value> {
    if cfg!(test) {
        return None;
    }
    let path =
        std::env::var_os("BORG_SYSTEMS").map(std::path::PathBuf::from).unwrap_or_else(|| crate::collective::config_path().with_file_name("systems.json"));
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).map_err(|e| eprintln!("[borg] ignoring {}: {e}", path.display())).ok()
}

/// `BORG_SYSTEM_GATE=off` turns the gate off (default on).
pub fn on() -> bool {
    !std::env::var("BORG_SYSTEM_GATE").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// The systems of the collective: the alias map, plus every scope name of `lessons` that one of `repos`
/// (the work's, and the repos learned lessons come from) contains as name parts.
pub struct Systems {
    derived: Vec<String>,
}

fn canon(name: &str) -> String {
    segments(name).join("_")
}

impl Systems {
    pub fn new(lessons: &[Challenge], repos: &[String]) -> Self {
        let names: Vec<Names> = repos.iter().map(|r| Names::of(r)).collect();
        let mut derived: Vec<String> = Vec::new();
        for s in lessons.iter().flat_map(|c| &c.scope) {
            let c = canon(s);
            if !c.is_empty() && !derived.contains(&c) && of_alias(s).is_empty() && names.iter().any(|n| n.contains(s)) {
                derived.push(c);
            }
        }
        Systems { derived }
    }

    /// The systems a repo name means: its exact entry, else the alias map's and the derived systems its
    /// name parts contain.
    pub fn of_repo(&self, repo: &str) -> BTreeSet<String> {
        if let Some(fixed) = map().repos.get(&repo.to_lowercase()) {
            return fixed.iter().cloned().collect();
        }
        let have = Names::of(repo);
        let mut out: BTreeSet<String> = of_alias(repo);
        out.extend(self.derived.iter().filter(|d| have.contains(d)).cloned());
        out
    }

    /// The systems a lesson's scope names: per name, the exact repo entry, the alias map, or the name
    /// itself when it is a derived system. Component names (`VoiceWorkflowConfig`) name none.
    pub fn of_scope(&self, scope: &[String]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for s in scope {
            match map().repos.get(&s.to_lowercase()) {
                Some(fixed) => out.extend(fixed.iter().cloned()),
                None => {
                    out.extend(of_alias(s));
                    let c = canon(s);
                    if self.derived.contains(&c) {
                        out.insert(c);
                    }
                }
            }
        }
        out
    }
}

fn of_alias(name: &str) -> BTreeSet<String> {
    let have = Names::of(name);
    map().systems.iter().filter(|(_, aliases)| aliases.iter().any(|a| have.contains(a))).map(|(k, _)| k.clone()).collect()
}

/// The most directories `work_repos` looks up. Guard: each is a walk up the tree (a `stat` per
/// level), so a session that touched files all over the disk costs the hook at most this many.
const MAX_DIRS: usize = 256;

/// The repos the work is in, from the session's `.where` file: repo names on lines of their own (the
/// hook notes the cwd's repo at each prompt), and the git repos of absolute touched paths (found on
/// disk; a path in no git repo says nothing).
pub fn work_repos(where_text: &str) -> Vec<String> {
    let mut repos: Vec<String> = Vec::new();
    let mut add = |r: String| {
        if !r.is_empty() && !repos.contains(&r) {
            repos.push(r);
        }
    };
    let mut dirs: HashMap<String, Option<String>> = HashMap::new();
    for line in where_text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !line.contains(['/', ' ']) {
            add(line.to_string());
            continue;
        }
        for w in line.split_whitespace().filter(|w| w.starts_with('/')) {
            let dir = Path::new(w).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            // Bounded: a long session's .where has hundreds of paths, mostly in few directories.
            if dirs.len() >= MAX_DIRS && !dirs.contains_key(&dir) {
                continue;
            }
            let repo = dirs.entry(dir.clone()).or_insert_with(|| git_repo(&dir)).clone();
            if let Some(r) = repo {
                add(r);
            }
        }
    }
    repos
}

/// The repo name of the git checkout containing `dir` (a worktree counts as its main repo), if any.
fn git_repo(dir: &str) -> Option<String> {
    let mut cur = Some(Path::new(dir));
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return borg_trace::repo_key(&d.to_string_lossy()).rsplit('/').next().map(String::from).filter(|s| !s.is_empty());
        }
        cur = d.parent();
    }
    None
}

/// The work's known systems, from its repos.
pub fn of_work(systems: &Systems, repos: &[String]) -> BTreeSet<String> {
    repos.iter().flat_map(|r| systems.of_repo(r)).collect()
}

/// The gate over a pool: marks `off_system` every lesson whose scope names known systems none of which is
/// the work's, when the work's system is known. Returns the work's systems (empty: not known, no lesson
/// marked).
pub fn gate(challenges: &mut [Challenge], repos: &[String]) -> BTreeSet<String> {
    let mut known: Vec<String> = repos.to_vec();
    for c in challenges.iter().filter(|c| c.source != "import") {
        if !c.repo.is_empty() && !known.contains(&c.repo) {
            known.push(c.repo.clone());
        }
    }
    let systems = Systems::new(challenges, &known);
    let work = of_work(&systems, repos);
    if work.is_empty() {
        return work;
    }
    for c in challenges.iter_mut().filter(|c| !c.scope.is_empty()) {
        let theirs = systems.of_scope(&c.scope);
        c.off_system = !theirs.is_empty() && theirs.is_disjoint(&work);
    }
    work
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(id: &str, scope: &[&str]) -> Challenge {
        Challenge { id: id.into(), scope: scope.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn set(xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn repos_map_to_systems_by_name_parts_and_exact_entries() {
        let s = Systems::new(&[], &[]);
        assert_eq!(s.of_repo("harbor_orion"), set(&["orion"]));
        assert_eq!(s.of_repo("lumen_orion"), set(&["orion"]));
        assert_eq!(s.of_repo("harbor_orion_gpt_live"), set(&["orion"]));
        assert_eq!(s.of_repo("borg-ai"), set(&["borg"]));
        assert!(s.of_repo("lumen_handbook").is_empty(), "a handbook spans systems");
        assert!(s.of_repo("press-queue").is_empty());
        assert_eq!(s.of_scope(&["Sr3ReviewSummary".into(), "VoiceWorkflowConfig".into()]), set(&["sr3"]));
        assert!(s.of_scope(&["VoiceWorkflowConfig".into()]).is_empty());
    }

    #[test]
    fn a_scope_a_known_repo_contains_is_a_system() {
        let pool = [lesson("z", &["Zeus"]), lesson("c", &["ChatPanel"])];
        let s = Systems::new(&pool, &["zeus_core".into()]);
        assert_eq!(s.of_repo("zeus_core"), set(&["zeus"]));
        assert_eq!(s.of_scope(&["Zeus".into()]), set(&["zeus"]));
        assert!(s.of_scope(&["ChatPanel".into()]).is_empty());
    }

    #[test]
    fn the_gate_drops_other_systems_only_when_the_work_system_is_known() {
        let mut pool = vec![lesson("m", &["Orion", "harbor_orion", "Beacon"]), lesson("b", &["borg"]), lesson("c", &["VoiceWorkflowConfig"]), lesson("u", &[])];
        assert_eq!(gate(&mut pool, &["borg-ai".into()]), set(&["borg"]));
        let off: Vec<&str> = pool.iter().filter(|c| c.off_system).map(|c| c.id.as_str()).collect();
        assert_eq!(off, ["m"], "a component scope and unscoped lessons are left to today's rule");
        for c in &mut pool {
            c.off_system = false;
        }
        assert_eq!(gate(&mut pool, &["harbor_orion".into()]), set(&["orion"]));
        let off: Vec<&str> = pool.iter().filter(|c| c.off_system).map(|c| c.id.as_str()).collect();
        assert_eq!(off, ["b"]);
        for c in &mut pool {
            c.off_system = false;
        }
        assert!(gate(&mut pool, &["press-queue".into(), "lumen_handbook".into()]).is_empty());
        assert!(pool.iter().all(|c| !c.off_system));
    }

    #[test]
    fn work_repos_come_from_repo_lines_and_git_roots_of_absolute_paths() {
        let d = std::env::temp_dir().join(format!("borg-systems-{}", std::process::id()));
        let repo = d.join("harbor_orion");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let text = format!("borg-ai\ncrates/borg/src/main.rs\n{}/src/A.java /nowhere/orion/B.java\nskills/lumen-knowledge/x.md\n", repo.display());
        assert_eq!(work_repos(&text), ["borg-ai", "harbor_orion"]);
        let _ = std::fs::remove_dir_all(d);
    }
}
