//! The one way the hive's verdict, integration and hidden oracle call git: `git -C <dir>` under the
//! configs that would change what a call reads, writes or runs, each set to git's default.
//!
//! A drone shares the repo's `.git` and the engineer's `~/.gitconfig` shapes every call: a planted
//! hook (`core.hooksPath`, a checkout's `post-checkout`, `update-ref`'s `reference-transaction`) or
//! fsmonitor would run; `diff.noprefix` or `diff.mnemonicPrefix` makes a patch `git apply` cannot
//! read; `diff.relative` hides the paths outside a subdirectory from a verdict;
//! `diff.suppressBlankEmpty` drops the space a blank context line starts with; `core.quotePath`
//! quotes the paths a content diff is keyed by; a planted `refs/replace/<head>` would make a done's
//! snapshot read as its base, so every diff of it empty (`--no-replace-objects`).

use std::path::Path;
use std::process::Command;

/// Each `-c key=value` every hive git call runs under.
const NEUTRAL: &[&str] = &[
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "core.quotePath=false",
    "diff.noprefix=false",
    "diff.mnemonicPrefix=false",
    "diff.relative=false",
    "diff.suppressBlankEmpty=false",
];

/// The arguments after `git`: `-C dir`, `--no-replace-objects`, then [`NEUTRAL`].
pub fn args(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut out: Vec<std::ffi::OsString> = vec!["-C".into(), dir.into(), "--no-replace-objects".into()];
    for kv in NEUTRAL {
        out.extend(["-c".into(), (*kv).into()]);
    }
    out
}

/// `git -C dir` under [`NEUTRAL`], never prompting for credentials.
pub fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args(dir)).env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn a_planted_replace_ref_hides_no_change() {
        // A drone shares `.git`: `git replace <head> <base>` would make every diff of base..head empty.
        let dir = std::env::temp_dir().join(format!("borg-hive-git-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        sh(&dir, &["init", "-q"]);
        let commit = |file: &str| {
            std::fs::write(dir.join(file), "x\n").unwrap();
            sh(&dir, &["add", "-A"]);
            sh(&dir, &["-c", "user.email=e@x", "-c", "user.name=e", "commit", "-qm", file]);
            sh(&dir, &["rev-parse", "HEAD"])
        };
        let (base, head) = (commit("a.txt"), commit("tests/b.py"));
        sh(&dir, &["replace", &head, &base]);
        assert_eq!(sh(&dir, &["diff", "--name-only", &base, &head]), "", "plain git is fooled");
        let got = crate::hive::verdict::changes(&dir, &base, &head).unwrap();
        assert_eq!(got.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["tests/b.py"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
