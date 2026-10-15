//! What `borg doctor` and `borg install --apply` say about vector recall (vectors.rs): the model, and
//! each store's index. Vectors are on by default, so a missing model or index is a WARN with its fix;
//! off by choice (`BORG_VECTORS=off`) or a lean build is fine. Read-only: nothing here fetches or builds.

use std::path::Path;

use crate::collective::Route;
use crate::harness::Level;
use crate::vectors::model::{built_with, enabled, model_dir, model_present};

/// The lessons file of a store, as the hooks name it.
const LESSONS: &str = "lessons.jsonl";

/// `borg doctor`'s vector lines for the stores of `routes` (`install::collectives`).
#[must_use]
pub fn checks(routes: &[Route]) -> Vec<(Level, String)> {
    if !cfg!(feature = "vectors") {
        return vec![(Level::Ok, "vectors: not in this build (--no-default-features): end-of-turn checks match words only".into())];
    }
    if !enabled() {
        return vec![(Level::Ok, "vectors: off (BORG_VECTORS=off): end-of-turn checks match words only".into())];
    }
    against(routes, &model_dir())
}

/// With vectors on: the model, then one WARN per store with lessons whose index is missing or was built
/// with another model.
pub(super) fn against(routes: &[Route], model: &Path) -> Vec<(Level, String)> {
    if !model_present(model) {
        let msg = format!("vectors: model missing at {} (end-of-turn checks match words only): run `borg vectors --fetch`", model.display());
        return vec![(Level::Warn, msg)];
    }
    let warns: Vec<(Level, String)> = routes
        .iter()
        .filter_map(|r| match r {
            Route::Collective { name, home, .. } => index_problem(&home.join(LESSONS), model).map(|p| (name, home.join(LESSONS), p)),
            Route::Off => None,
        })
        .map(|(name, lessons, p)| (Level::Warn, format!("vectors: collective {name:?} {p}: run `borg vectors --rebuild --store {}`", lessons.display())))
        .collect();
    let ok = (Level::Ok, format!("vectors: model at {}", model.display()));
    std::iter::once(ok).chain(warns).collect()
}

/// What is wrong with the index of `lessons`, if anything. A store without lessons needs none yet: its
/// first lesson builds it (`refresh`).
fn index_problem(lessons: &Path, model: &Path) -> Option<&'static str> {
    if !lessons.is_file() {
        return None;
    }
    match built_with(lessons, model) {
        None => Some("has no vector index"),
        Some(false) => Some("has a vector index built with another model"),
        Some(true) => None,
    }
}

/// The line `borg install --apply` ends with while vectors are on but the model is missing (install
/// itself never downloads, so it stays fast and reversible).
#[must_use]
pub fn hint() -> Option<String> {
    let model = model_dir();
    (enabled() && !model_present(&model))
        .then(|| format!("vectors: model missing at {}: `borg setup` or `borg vectors --fetch` turns on vector recall", model.display()))
}
