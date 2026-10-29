//! Deterministic fingerprints: the same error seen by two drones (or two
//! sessions weeks apart) must hash the same, regardless of line numbers,
//! temp paths, ids or durations.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::LazyLock;

use regex::Regex;

static KEYWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(error|failed|failure|panicked|exception|traceback|not found|no such|cannot|denied)\b").unwrap());
static PATHLIKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w.~-]*(/[\w.@+-]+)+/?").unwrap());
static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-fA-F]{6,}\b").unwrap());
static NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());
static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

fn hash<T: Hash>(t: T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// Normalize one error line so incidental details don't split fingerprints.
pub fn normalize_error_line(line: &str) -> String {
    let s = PATHLIKE.replace_all(line, "P");
    let s = HEX.replace_all(&s, "H");
    let s = NUM.replace_all(&s, "N");
    let s = WS.replace_all(&s, " ");
    s.trim().to_lowercase().chars().take(NORMAL_CHARS).collect()
}

/// The most characters of a normalized error line that enter its fingerprint. Protocol: stored
/// scars are keyed by the fingerprint, so a change splits every one learned before.
const NORMAL_CHARS: usize = 160;
/// The most characters of the error line a scar keeps and shows. Display: one line.
const LINE_CHARS: usize = 200;

static FAILURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(Traceback \(most recent call last\)|FAILED|FAIL:|ERROR:|error(\[\w+\])?:|\w*(Error|Exception): |thread '.*' panicked|npm ERR!|fatal: )")
        .unwrap()
});

/// A command can fail behind a pipe (`pytest | tail` exits 0), so judge by
/// output too: tracebacks, test-runner failures, compiler errors.
pub fn looks_failed(output: &str) -> bool {
    FAILURE.is_match(output)
}

/// Lines that name the actual error (exception type with message, compiler error, fatal…).
static SPECIFIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:E\s+)?(?:[\w.]*(?:Error|Exception|Exit)\b\s*:\s*\S|error(?:\[\w+\])?:\s*\S|fatal:\s*\S|npm ERR!\s*\S|.*\bcommand not found\b|.*\bNo such file or directory\b|.*\bnot found\b.*)").unwrap()
});
/// Headers and summaries that say *that* something failed, not *what*.
static GENERIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(traceback \(most recent call last\)|build failed|failure: build failed|\* what went wrong|exit code \d+|failed \(|error: process completed|> task .* failed|\d+ (tests? )?failed|error:?\s*$)").unwrap()
});
/// Messages from the agent harness, not from the command.
static HARNESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(permission to use \w+ has been denied|claude code is running in|<tool_use_error>|user (rejected|doesn't want)|interrupted by user)")
        .unwrap()
});

/// The most specific error line of a failed command's output: the last line naming the error
/// (Python tracebacks end with it), else the first error-looking line that is not a generic
/// header. None for harness messages (permission denials, rejections).
pub fn error_line(output: &str) -> Option<String> {
    if HARNESS.is_match(output) {
        return None;
    }
    let specific = output.lines().rev().find(|l| SPECIFIC.is_match(l) && !GENERIC.is_match(l));
    specific.or_else(|| output.lines().find(|l| KEYWORD.is_match(l) && !GENERIC.is_match(l))).map(|l| l.trim().chars().take(LINE_CHARS).collect())
}

/// Fingerprint of the most specific error line (see `error_line`).
pub fn error_fingerprint(output: &str) -> Option<u64> {
    error_line(output).map(|l| normalize_error_line(&l)).filter(|s| s.len() >= 8).map(hash)
}

/// Test failures say the code under development is not done yet; they are not reusable knowledge.
static TEST_FAILURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(assertion(failed)?error|assert(ion)? failed|test(s)? failed|failed tests|expected:? .* but (was|got)|panicked at .*assert|\btests? .* FAILED$)",
    )
    .unwrap()
});

/// Whether an error line is a failing test assertion rather than an environment/tooling error.
pub fn is_test_failure(line: &str) -> bool {
    TEST_FAILURE.is_match(line)
}

/// The invocation structure of a command: program, subcommand, env assignments and flags,
/// without test selectors, paths or quoted values. Two commands that differ only in which
/// tests/files they target have the same invocation.
pub fn invocation(cmd: &str) -> String {
    let mut out = Vec::new();
    let mut skip_next = false;
    for t in cmd.split_whitespace() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(t, "--tests" | "-k" | "-p" | "--package" | "-m" | "--filter" | "-t") {
            skip_next = true;
            continue;
        }
        if t.contains('=') && !t.starts_with('-') && t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            out.push(t.to_string()); // env assignment: part of how the tool must be run
        } else if t.starts_with('-') {
            out.push(t.split('=').next().unwrap_or(t).to_string());
        } else if out.len() < 2 && !t.contains('/') && !t.contains('\'') && !t.contains('"') {
            out.push(t.to_string());
        }
    }
    out.join(" ")
}

const TOOLS: &[&str] = &[
    "gradle",
    "gradlew",
    "mvn",
    "mvnw",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "npx",
    "pytest",
    "uv",
    "pip",
    "pip3",
    "poetry",
    "go",
    "make",
    "docker",
    "kubectl",
    "terraform",
    "gh",
    "java",
    "javac",
    "node",
    "deno",
    "bun",
    "set",
    "source",
    "lake",
    "tsc",
];

/// Whether a command shape is a reusable tool invocation (build, test, package, env sourcing),
/// as opposed to an ad-hoc script or a one-off inspection that teaches nothing to the next drone.
pub fn is_tool_shape(shape: &str) -> bool {
    let mut it = shape.split_whitespace();
    let prog = it.next().unwrap_or("").trim_start_matches("./");
    let sub = it.next().unwrap_or("");
    TOOLS.contains(&prog) || prog.ends_with("gradlew") || prog.ends_with("mvnw") || (prog.starts_with("python") && TOOLS.contains(&sub))
}

/// Program plus subcommand (`cargo test`, `npm run`, `git status`), the
/// unit at which "how do I build/test this repo" knowledge transfers.
pub fn command_shape(cmd: &str) -> String {
    let first = cmd
        .split(['\n', ';', '|'])
        .flat_map(|seg| seg.split("&&"))
        .map(str::trim)
        .find(|seg| !seg.is_empty() && !seg.starts_with("cd ") && !seg.starts_with('#'))
        .unwrap_or("");
    let mut toks = first.split_whitespace().filter(|t| !t.contains('=') || t.starts_with('-'));
    let prog = toks.next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
    match toks.find(|t| !t.starts_with('-')) {
        Some(sub) if sub.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':') => format!("{prog} {sub}"),
        _ => prog,
    }
}

const READERS: &[&str] = &["cat", "sed", "head", "tail", "nl", "bat", "less", "awk", "wc", "jq", "rg", "grep"];

/// True when every segment of the command just reads or lists files.
pub fn only_reads(cmd: &str) -> bool {
    cmd.split(['\n', ';', '|'])
        .flat_map(|s| s.split("&&"))
        .filter_map(|seg| seg.split_whitespace().next())
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .all(|p| READERS.contains(&p) || matches!(p, "cd" | "ls" | "echo" | "printf" | "find" | "tree" | "pwd"))
}

/// Files a shell command pulls into context (`cat a.rs`, `sed -n 1,80p src/x.rs | …`).
/// Agents read through Bash far more often than through the Read tool, so
/// recall-on-touch must see these too.
pub fn bash_read_paths(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    for seg in cmd.split(['\n', ';', '|']).flat_map(|s| s.split("&&")) {
        let mut toks = seg.split_whitespace();
        let Some(prog) = toks.next().map(|p| p.rsplit('/').next().unwrap_or(p)) else { continue };
        if !READERS.contains(&prog) {
            continue;
        }
        let toks: Vec<&str> = toks.collect();
        // Skip the pattern argument of grep/rg/sed/awk scripts.
        let skip_first = matches!(prog, "rg" | "grep" | "sed" | "awk" | "jq");
        let takes_arg: &[&str] = match prog {
            "head" | "tail" => &["-n", "-c"],
            "rg" | "grep" => &["-e", "-f", "-A", "-B", "-C", "-m", "-g", "-t", "--type", "--glob"],
            "sed" | "awk" => &["-e", "-f", "-v"],
            _ => &[],
        };
        let mut first_positional = true;
        for (i, t) in toks.iter().enumerate() {
            if t.starts_with('-') || t.starts_with('>') || t.starts_with('<') || t.starts_with('$') {
                continue;
            }
            if i > 0 && takes_arg.contains(&toks[i - 1]) {
                // `-e PATTERN` / `-f SCRIPT` supply the pattern, so no positional one follows.
                first_positional &= !matches!(toks[i - 1], "-e" | "-f");
                continue;
            }
            if first_positional && skip_first {
                first_positional = false;
                continue;
            }
            first_positional = false;
            let t = t.trim_matches(|c| c == '"' || c == '\'');
            let looks_like_file = t
                .rsplit('/')
                .next()
                .is_some_and(|base| base.contains('.') && !base.starts_with('.') || base.chars().all(|c| c.is_ascii_uppercase() || c == '_') && base.len() > 3);
            if looks_like_file && !t.contains(['*', '?', '{', '(', '`', '\\', '=']) && !t.ends_with(".") {
                out.push(t.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_behind_pipe() {
        assert!(looks_failed("  File \"x.py\", line 3\nValueError: naive datetime"));
        assert!(looks_failed("Ran 3 tests\n\nFAILED (errors=1)"));
        assert!(looks_failed("error[E0425]: cannot find value"));
        assert!(!looks_failed("Ran 3 tests in 0.001s\n\nOK"));
        assert!(only_reads("cat a.py | head; ls src"));
        assert!(!only_reads("cat a.py; python3 -m unittest | tail"));
    }

    #[test]
    fn shell_reads() {
        assert_eq!(bash_read_paths("sed -n '1,80p' src/main.rs | head"), vec!["src/main.rs"]);
        assert_eq!(bash_read_paths("cat Cargo.toml README.md"), vec!["Cargo.toml", "README.md"]);
        assert_eq!(bash_read_paths("rg -n 'fn parse' src/lib.rs"), vec!["src/lib.rs"]);
        assert_eq!(bash_read_paths("cargo test && ls src"), Vec::<String>::new());
        assert_eq!(bash_read_paths("head -n 20 /abs/x.py 2>/dev/null"), vec!["/abs/x.py"]);
    }

    #[test]
    fn same_error_different_details() {
        let a = "error[E0425]: cannot find value `x` in /tmp/a1/src/main.rs:12:5";
        let b = "error[E0425]: cannot find value `x` in /home/user/r/src/main.rs:99:1";
        assert_eq!(error_fingerprint(a), error_fingerprint(b));
        assert_ne!(error_fingerprint(a), error_fingerprint("error: linker `cc` not found"));
        assert_eq!(error_fingerprint("all good\nok"), None);
    }

    #[test]
    fn specific_error_line_wins() {
        let py = "Traceback (most recent call last):\n  File \"x.py\", line 3\nModuleNotFoundError: No module named 'aiortc'";
        assert_eq!(error_line(py).unwrap(), "ModuleNotFoundError: No module named 'aiortc'");
        let other = "Traceback (most recent call last):\n  File \"y.py\", line 9\nKeyError: 'id'";
        assert_ne!(error_fingerprint(py), error_fingerprint(other), "generic header must not collide");
        assert_eq!(error_line("Permission to use Bash has been denied because Claude Code is running in don't ask mode"), None);
        assert!(is_tool_shape("./gradlew test") && is_tool_shape("cargo test") && !is_tool_shape("python3") && !is_tool_shape("grep"));
    }

    #[test]
    fn command_shapes() {
        assert_eq!(command_shape("cd foo && cargo test -p x -- --nocapture"), "cargo test");
        assert_eq!(command_shape("RUST_LOG=debug /usr/bin/npm run build"), "npm run");
        assert_eq!(command_shape("ls -la"), "ls");
        assert_eq!(command_shape("git status | head"), "git status");
    }
}
