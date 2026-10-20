//! What `borg setup` does once the engineer confirmed: the config (a backup first), the team's mesh
//! command, the vector model and indexes, the wiring, an optional import. Only a failed write is an
//! error; a mesh or vector step that fails is a WARN with its fix in the summary, never the end of setup.

use std::path::{Path, PathBuf};

use crate::collective::{self, Route};
use crate::install::{self, Change};
use crate::learn;
use crate::vectors::model::{built_with, enabled, model_dir, model_present};

use super::collectives::Mesh;
use super::prompt::Prompt;

/// Writes the config change, reported on one line; 1 if it failed.
#[must_use]
pub fn config(c: &Change, stamp: &str) -> i32 {
    match install::apply(c, stamp) {
        Ok(backup) => {
            let b = backup.map(|b| format!(" (backup {})", b.display())).unwrap_or_default();
            println!("wrote {}{b}", c.path.display());
            0
        }
        Err(e) => {
            eprintln!("{}: {e}", c.path.display());
            1
        }
    }
}

/// Runs `borg mesh <args>`; a missing borg-mesh or a failure becomes a WARN in `warns`.
fn run_mesh(args: &[String], warns: &mut Vec<String>) -> bool {
    let line = format!("borg mesh {}", args.join(" "));
    let Some(bin) = install::sibling("mesh") else {
        warns.push(format!("borg-mesh is not installed: cargo install borg-mesh --locked, then `{line}`"));
        return false;
    };
    println!("running: {line}");
    let ok = std::process::Command::new(bin).args(args).status().is_ok_and(|s| s.success());
    if !ok {
        warns.push(format!("`{line}` did not finish: fix what it printed and run it again"));
    }
    ok
}

/// The team's mesh command; whether it finished. Its admin may print a first invite right away, unless
/// the collective's relay is `disabled`: an invite then needs `borg mesh serve` running first.
pub fn mesh(m: &Mesh, ask: &mut Prompt, warns: &mut Vec<String>) -> bool {
    let ok = run_mesh(&m.args(), warns);
    let Mesh::Init(name) = m else { return ok };
    let invite = format!("borg mesh invite --collective {name}");
    if ok && direct_only(name) {
        println!("relay disabled: start `borg mesh serve --collective {name}` first, then `{invite}`");
    } else if ok && ask.yes_no("Print an invite ticket for a teammate now?", false) {
        run_mesh(&["invite".into(), "--collective".into(), name.clone()], warns);
    }
    ok
}

/// Whether the configured collective `name` reaches its members by direct addresses only.
fn direct_only(name: &str) -> bool {
    let cfg = collective::load().ok().flatten();
    cfg.is_some_and(|c| c.collectives.iter().any(|e| e.name == name && e.relay.as_deref() == Some("disabled")))
}

/// Whether `m` is done already: a collective is founded once, and its membership in the store
/// (borg-mesh's `mesh/membership.json`) is the mark. A join always runs (it changes nothing twice).
#[must_use]
pub fn done(m: &Mesh) -> bool {
    matches!(m, Mesh::Init(name) if collective::store(&learn::home(), name).join("mesh/membership.json").is_file())
}

/// The vector model, then an index for every store with lessons but none current. On by default:
/// skipped only by `--no-vectors`, `BORG_VECTORS=off` or a lean build (doctor names those).
pub fn vectors(no_vectors: bool, ask: &mut Prompt, warns: &mut Vec<String>) {
    if no_vectors {
        println!("vectors: skipped (--no-vectors); `borg vectors --fetch` turns vector recall on later");
        return;
    }
    if !enabled() {
        return;
    }
    let model = model_dir();
    if !model_present(&model) && !fetch(ask) {
        // `borg doctor` names the missing model below; only the proxy hint is setup's.
        println!("vectors: the model was not fetched; behind a proxy: BORG_VECTOR_URL=<mirror> borg vectors --fetch");
        return;
    }
    for lessons in unindexed(&model) {
        let p = lessons.display().to_string();
        if crate::vectors::main(&["--rebuild", "--store", &p]) != 0 {
            warns.push(format!("vectors: no index for {p}: `borg vectors --rebuild --store {p}`"));
        }
    }
}

/// What the vector step will do, for the plan shown before the confirmation; None for nothing.
#[must_use]
pub fn vectors_plan(no_vectors: bool) -> Option<String> {
    let model = model_dir();
    if no_vectors || !enabled() {
        None
    } else if !model_present(&model) {
        Some(format!("download the vector model (~490 MB, once, into {}) and index the stores' lessons", model.display()))
    } else {
        (!unindexed(&model).is_empty()).then(|| "index the stores' lessons for vector recall".into())
    }
}

/// Fetches the model; after a failure, asks once for a mirror (`BORG_VECTOR_URL`) and retries with it.
fn fetch(ask: &mut Prompt) -> bool {
    if crate::vectors::main(&["--fetch"]) == 0 {
        return true;
    }
    let mirror = ask.line("The model download failed. A mirror URL (BORG_VECTOR_URL, e.g. behind a proxy), or - to skip", "-");
    if mirror == "-" {
        return false;
    }
    // SAFETY: single-threaded CLI; nothing reads the environment concurrently.
    unsafe { std::env::set_var("BORG_VECTOR_URL", &mirror) };
    crate::vectors::main(&["--fetch"]) == 0
}

/// The lessons files of the configured stores whose index is missing or another model's.
fn unindexed(model: &Path) -> Vec<PathBuf> {
    let cfg = collective::load().ok().flatten();
    install::collectives(cfg.as_ref(), &learn::home())
        .into_iter()
        .filter_map(|r| match r {
            Route::Collective { home, .. } => Some(home.join("lessons.jsonl")),
            Route::Off => None,
        })
        .filter(|l| l.is_file() && built_with(l, model) != Some(true))
        .collect()
}

/// Writes each harness's changes (`install::write`); 1 if any failed.
#[must_use]
pub fn wire(wiring: &[super::Wire], stamp: &str) -> i32 {
    wiring.iter().map(|(h, changes)| install::write(*h, changes, stamp)).max().unwrap_or(0)
}

/// Offers to import corrections from past sessions (model calls, so never under `--yes`).
pub fn import(ask: &mut Prompt) {
    if ask.yes_no("Import corrections from your past sessions now (borg assimilate --apply --limit 50; model calls)?", false) {
        crate::assimilate::main(&["--apply", "--limit", "50"], &learn::ask_claude);
    }
}
