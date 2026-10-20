//! The skill and the TS adapters live in `crates/borg/assets/`, inside the package that compiles them
//! in; the repo's `skills/borg` and `adapters/{omp,opencode}/borg.ts` are symlinks to them, so the
//! adapter tests, smoke scripts and docs keep their paths. A link replaced by a copy would drift: these
//! tests fail then, and when what borg ships differs from what either path holds.

use std::fs;
use std::path::PathBuf;

use borg::harness::{omp, opencode};
use borg::install::skill::FILES;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `old` (repo-relative) is a symlink to `asset` (crate-relative).
fn assert_link(old: &str, asset: &str) {
    let link = crate_dir().join("../..").join(old);
    assert!(fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()), "{old} is a symlink to crates/borg/{asset}");
    assert_eq!(fs::canonicalize(&link).unwrap(), fs::canonicalize(crate_dir().join(asset)).unwrap(), "{old} points at crates/borg/{asset}");
}

/// The file at `old` (repo-relative) and at `asset` (crate-relative) is `shipped`, byte for byte.
fn assert_same(old: &str, asset: &str, shipped: &str) {
    assert_eq!(fs::read_to_string(crate_dir().join("../..").join(old)).unwrap(), shipped, "{old} is what borg ships");
    assert_eq!(fs::read_to_string(crate_dir().join(asset)).unwrap(), shipped, "crates/borg/{asset} is what borg ships");
}

#[test]
fn the_adapters_at_their_old_paths_are_the_shipped_ones() {
    for (harness, shipped) in [("omp", omp::ADAPTER), ("opencode", opencode::ADAPTER)] {
        let (old, asset) = (format!("adapters/{harness}/borg.ts"), format!("assets/adapters/{harness}/borg.ts"));
        assert_link(&old, &asset);
        assert_same(&old, &asset, shipped);
    }
}

#[test]
fn the_skill_at_its_old_path_is_the_shipped_one() {
    assert_link("skills/borg", "assets/skills/borg");
    for (rel, shipped) in FILES {
        assert_same(&format!("skills/borg/{rel}"), &format!("assets/skills/borg/{rel}"), shipped);
    }
}
