//! Was it a fix? The signals regress.rs weighs for a commit that changed an earlier decision's
//! lines: a revert, a fix-shaped subject, a test that failed and then passed in the session, or the
//! engineer's own words about a bug. A revert or a fix subject makes the link strong (`fix`) on its
//! own; tests and words only together (`strong`): red, then green is also how a feature is built
//! test first, and a prompt names failures a feature handles. Otherwise the link is weak
//! (`revisited`), stored but never learned from.
//!
//! Only the engineer's words count, never the drone's proposal; "error", "issue" and a bare "fail"
//! are left out as too common ("error handling", "retry when a call fails").
//!
//! The tests signal is captured by the hook, 0 tokens: a shell command that runs tests
//! (`test_outcome`) leaves `fail <ms>` or `pass <ms>` in `<state>/<session>.tests`; a build that
//! did not compile is no test run.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

pub const REVERT: &str = "revert";
pub const SUBJECT: &str = "subject";
pub const TESTS: &str = "tests";
pub const WORDS: &str = "words";

static FIX_SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(fix(e[sd])?|bug ?fix|hot ?fix)\b|^[a-z]+(\([^)]*\))?!?:\s*(fix|bug|hotfix)\b|\b(fix(es|ed)?|resolv(es|ed)|closes?)\s+(#\d+|[A-Z][A-Z0-9]+-\d+)|\bregression\b",
    )
    .expect("fix subject")
});

static BUG_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(bugs?|regress(ion|ed|es)?|broken|crash(es|ed|ing)?|panic(s|ked)?|deadlock|race condition|leak(s|ing)?|(tests?|build|ci|checks?) (is |are |keeps? )?fail(s|ed|ing)?|fail(ed|ing) (tests?|builds?|checks?)|flaky|stack ?trace|traceback|wrong (result|output|value|answer)|doesn'?t work|not working|incorrect)\b",
    )
    .expect("bug words")
});

/// Design words, not failures: "fail open", "fails fast", "fail-safe".
static DESIGN_WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bfail(s|ing)?[ -](open|closed|fast|safe)\b").expect("design words"));

/// A test runner at the start of the command or of one of its parts (after `&&`, `;`, `|`, `(`),
/// behind environment assignments and a runner (`npx`, `uv run`, ...): `grep -rn "cargo test"`
/// or `cat tox.ini` only mention one.
static TEST_CMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[;&|(])\s*([A-Za-z_][A-Za-z0-9_]*=\S*\s+)*((time|npx|bunx|uv run|poetry run|pipenv run|bundle exec|python3? -m|\./)\s*)?(cargo (test|nextest)|pytest|py\.test|go test|(npm|pnpm|yarn|bun) (run )?test|jest|vitest|mocha|rspec|mvn\b[^;&|]*\btest|(\./)?gradlew? test|ctest|make (check|test)|dotnet test|mix test|phpunit|tox|scripts/check\.sh)(\s|$|[;&|)])",
    )
    .expect("test command")
});

static TEST_FAILED: LazyLock<Regex> = LazyLock::new(|| {
    // Case matters: "0 failed" in a passing summary is no failure, "FAILED" is.
    Regex::new(r"(?m)test result: FAILED|\b[1-9]\d* (failed|failing)\b|^FAIL\b|\bFAILED\b|Exit code:? [1-9]").expect("test failed")
});

/// A build that stopped before any test ran.
static NOT_BUILT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"error\[E\d+\]|error: could not compile|SyntaxError:|ModuleNotFoundError:").expect("not built"));

/// A subject that says it fixes something (`fix: x`, `Fix race`, `resolves #12`, `ABC-12`).
pub fn subject_signal(subject: &str) -> bool {
    FIX_SUBJECT.is_match(subject.trim())
}

/// A `git revert` commit message.
#[must_use]
pub fn revert_signal(subject: &str, body: &str) -> bool {
    subject.starts_with("Revert \"") || body.contains("This reverts commit ")
}

/// The engineer's text speaks of a bug, a crash, a regression or a failure.
pub fn words_signal(text: &str) -> bool {
    BUG_WORDS.is_match(&DESIGN_WORDS.replace_all(text, ""))
}

/// A test run that failed and a later one that passed, both inside one of `windows` (epoch ms) and
/// before `before_ms`, from `<state>/<session>.tests` lines (`fail <ms>` / `pass <ms>`).
#[must_use]
pub fn tests_signal(tests: &str, windows: &[(i64, i64)], before_ms: i64) -> bool {
    let mut failed = false;
    for (what, ms) in tests.lines().filter_map(|l| l.split_once(' ')).filter_map(|(w, m)| Some((w, m.trim().parse::<i64>().ok()?))) {
        if ms > before_ms || !windows.iter().any(|&(a, b)| ms >= a && ms <= b) {
            continue;
        }
        match what {
            "fail" => failed = true,
            "pass" if failed => return true,
            _ => {}
        }
    }
    false
}

/// Whether `command` runs tests (the hook checks this before it reads the output).
#[must_use]
pub fn runs_tests(command: &str) -> bool {
    TEST_CMD.is_match(command)
}

/// `"fail"` or `"pass"` when `command` runs tests: failed when the tool call failed (`failed`) or
/// its output says so; `None` for any other command and for a build that did not compile.
#[must_use]
pub fn test_outcome(command: &str, output: &str, failed: bool) -> Option<&'static str> {
    if !runs_tests(command) || NOT_BUILT.is_match(output) {
        return None;
    }
    Some(if failed || TEST_FAILED.is_match(output) { "fail" } else { "pass" })
}

/// Whether `signals` make a link strong (module doc).
#[must_use]
pub fn strong(signals: &[String]) -> bool {
    let has = |s: &str| signals.iter().any(|x| x == s);
    has(REVERT) || has(SUBJECT) || (has(TESTS) && has(WORDS))
}

/// A tool call's output as text: its response (a string, or an object's `stdout` and `stderr`) and
/// its `error`.
#[must_use]
pub fn tool_output(input: &Value) -> String {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    let response = input.get("tool_response");
    let out = match response {
        Some(Value::Object(o)) => format!("{}\n{}", text(o.get("stdout")), text(o.get("stderr"))),
        r => text(r),
    };
    format!("{out}\n{}", text(input.get("error")))
}

/// `2d`, `36h`, `90m` or plain seconds, as seconds.
#[must_use]
pub fn parse_age(s: &str) -> Option<i64> {
    let s = s.trim();
    let units = [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)];
    let (n, mult) = units.iter().find_map(|(u, m)| s.strip_suffix(u).map(|n| (n, *m))).unwrap_or((s, 1));
    n.parse::<i64>().ok().filter(|n| *n >= 0).and_then(|n| n.checked_mul(mult))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_subjects() {
        for s in [
            "fix: take the lock",
            "Fix race in sync",
            "fix(sync): order",
            "fixes the flag",
            "chore: fix typo",
            "resolves #12",
            "closes ABC-12",
            "a regression in 1.2",
        ] {
            assert!(subject_signal(s), "{s}");
        }
        for s in ["prefix handling", "add fixture", "rename flag", "Fixture cleanup"] {
            assert!(!subject_signal(s), "{s}");
        }
        assert!(revert_signal("Revert \"take the lock\"", "") && revert_signal("undo", "This reverts commit abc.") && !revert_signal("revert later", ""));
    }

    #[test]
    fn test_runs_and_their_outcome() {
        assert_eq!(test_outcome("cargo test -p borg-ai", "test result: ok. 3 passed; 0 failed", false), Some("pass"));
        assert_eq!(test_outcome("cargo test", "Exit code 101", false), Some("fail"));
        assert_eq!(test_outcome("cd x && cargo test", "test result: FAILED. 1 passed; 1 failed", false), Some("fail"));
        assert_eq!(test_outcome("pytest -q", "", true), Some("fail"), "a failed tool call");
        assert_eq!(test_outcome("npm run test", "FAIL src/a.test.ts", false), Some("fail"));
        assert_eq!(test_outcome("ls -la", "FAILED", true), None);
        assert_eq!(test_outcome("cargo build", "error[E0425]", false), None);
        assert_eq!(test_outcome("cargo test", "error[E0425]: cannot find value\nerror: could not compile `x`", true), None, "it did not build");
        for c in ["RUST_LOG=debug cargo test -q", "cd x && npx jest", "make check", "./gradlew test", "uv run pytest -x", "(cd a; go test ./...)", "tox -e py3"]
        {
            assert!(runs_tests(c), "{c}");
        }
        for c in ["cat tox.ini", "grep -rn \"cargo test\" docs", "ls jest.config.js", "git log --grep pytest", "echo run the tests", "vim pytest.ini"] {
            assert!(!runs_tests(c), "{c}");
        }
        let obj = serde_json::json!({"tool_response": {"stdout": "test result: FAILED", "stderr": "x"}});
        assert!(tool_output(&obj).contains("test result: FAILED"));
        let failure = serde_json::json!({"tool_response": "out", "error": "Exit code: 101"});
        assert_eq!(test_outcome("cargo test", &tool_output(&failure), false), Some("fail"));
    }

    #[test]
    fn engineer_words() {
        assert!(!words_signal("add error handling to the parser"));
        assert!(words_signal("it's broken again") && words_signal("the sync test fails") && words_signal("there is a race condition"));
        assert!(words_signal("the sync test is flaky") && words_signal("CI is failing on main"));
        for feature in
            ["calls fail under load", "when a request fails, retry up to 3 times; make sure tests don't fail", "handle the failure", "add retry with backoff"]
        {
            assert!(!words_signal(feature), "{feature}");
        }
        assert!(!words_signal("hooks fail open and the job fails fast, fail-safe"), "design words");
    }

    #[test]
    fn tests_fail_then_pass_inside_a_window() {
        let w = [(100, 200), (300, 400)];
        assert!(tests_signal("fail 110\npass 150\n", &w, 1000));
        assert!(!tests_signal("pass 110\nfail 150\n", &w, 1000), "pass then fail");
        assert!(!tests_signal("fail 110\n", &w, 1000), "fail only");
        assert!(!tests_signal("fail 210\npass 250\n", &w, 1000), "outside the windows");
        assert!(!tests_signal("fail 110\npass 350\n", &w, 300), "after the fix was committed");
        assert!(tests_signal("fail 110\npass 350\n", &w, 1000), "across windows of one session");
    }

    #[test]
    fn ages() {
        assert_eq!(parse_age("2d"), Some(172_800));
        assert_eq!(parse_age("36h"), Some(129_600));
        assert_eq!(parse_age("90m"), Some(5_400));
        assert_eq!(parse_age("3600"), Some(3_600));
        assert_eq!(parse_age("soon"), None);
        assert_eq!(parse_age("2w"), None);
        assert_eq!(parse_age(""), None);
        assert_eq!(parse_age("999999999999999d"), None, "no overflow");
    }

    #[test]
    fn tests_or_words_alone_are_no_fix() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        assert!(strong(&s(&[SUBJECT])) && strong(&s(&[REVERT])) && strong(&s(&[TESTS, WORDS])));
        assert!(!strong(&s(&[TESTS])) && !strong(&s(&[WORDS])) && !strong(&[]), "a test-first feature, a prompt about failures");
    }
}
