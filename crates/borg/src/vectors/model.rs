//! Where vector recall's model and indexes live and whether they are there, without loading either:
//! what the hooks check before a query, and what `borg doctor` reads (doctor.rs). No dependency on
//! lessons or the engine.

use std::path::{Path, PathBuf};

/// The Hugging Face repository of the default model and the revision `--fetch` pins.
pub const MODEL_REPO: &str = "minishlab/potion-multilingual-128M";
pub const MODEL_REVISION: &str = "73908c3438cf03b6a01bcb9611d62b23d0726f08";
/// The files `--fetch` downloads, with their sha256 at `MODEL_REVISION`.
pub const MODEL_FILES: [(&str, &str); 3] = [
    ("config.json", "595e4cab2093732efd5dbe084fd5c1826b5eea693b73b4c1fd971672867d2e54"),
    ("tokenizer.json", "19f1909063da3cfe3bd83a782381f040dccea475f4816de11116444a73e1b6a1"),
    ("model.safetensors", "14b5eb39cb4ce5666da8ad1f3dc6be4346e9b2d601c073302fa0a31bf7943397"),
];

/// Beside the `SeekStorm` files: which model the vectors come from and which lesson versions they hold.
pub const MANIFEST: &str = "borg-vectors.json";

/// Off only with `BORG_VECTORS=off` (or a build without the `vectors` feature).
#[must_use]
pub fn enabled() -> bool {
    cfg!(feature = "vectors") && std::env::var("BORG_VECTORS").as_deref() != Ok("off")
}

/// The model directory: `BORG_VECTOR_MODEL`, default `~/.local/share/borg/models/potion-multilingual-128M`.
#[must_use]
pub fn model_dir() -> PathBuf {
    std::env::var_os("BORG_VECTOR_MODEL")
        .map_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/borg/models/potion-multilingual-128M"), PathBuf::from)
}

/// Whether the model's files are there (no load).
#[must_use]
pub fn model_present(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|(f, _)| dir.join(f).is_file())
}

/// The vector index of a lessons file: `vectors/` beside it.
#[must_use]
pub fn index_dir(lessons: &Path) -> PathBuf {
    lessons.with_file_name("vectors")
}

/// The model's identity in the manifest: its pinned revision (REVISION file) or the matrix's size.
pub(crate) fn identity(model: &Path) -> String {
    match std::fs::read_to_string(model.join("REVISION")) {
        Ok(r) if !r.trim().is_empty() => r.trim().to_string(),
        _ => format!("size:{}", std::fs::metadata(model.join("model.safetensors")).map_or(0, |m| m.len())),
    }
}

/// Whether the index of `lessons` exists (`None` if not) and was built with `model`: the one rule for a
/// current index, the engine's as doctor's and setup's.
#[must_use]
pub fn built_with(lessons: &Path, model: &Path) -> Option<bool> {
    let index = index_dir(lessons);
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(index.join(MANIFEST)).ok()?).ok()?;
    let built = manifest["model"].as_str()?;
    index.join("index.json").is_file().then(|| built == identity(model))
}
