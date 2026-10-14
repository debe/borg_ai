//! The free gate before the Stop model call (reports/stop-prefilter.md).
//!
//! The consequence model (predict.rs) costs one model call (~6 s, ~$0.03-0.07) per turn end, and
//! most turn ends touch no lesson at all. So word matching first scores this turn alone: the best
//! BM25 match of its prompt, touches and final message against the lessons, divided by the largest
//! idf a word can have in this store (scores grow with the number of lessons; this keeps one
//! threshold valid for a store of 10 or of 100). Below the threshold the hook asks nothing and
//! costs 0 tokens; at or above it, predict decides as before. Calibrated on the graded preempt
//! cases and the engineer's real turn ends (eval/preempt/stop_prefilter.py).

use crate::conscience::{self, Challenge};

/// State-file markers that close a turn (main.rs appends one at every Stop that reaches the conscience).
pub const MARKERS: [&str; 2] = ["#borg-asked", "#borg-skip"];

/// Pre-registered in reports/stop-prefilter.md: 0.9 × the lowest score of any labelled checkpoint
/// with a relevant lesson in its shortlist (9.28), rounded down.
pub const THRESHOLD: f64 = 8.0;

/// What the state file gained since the last Stop: this turn's prompt and touches (plus the drone's
/// final message, appended by the caller). A live check's ask (`#borg-asked` followed by `#borg-live`,
/// main.rs `ask_live`) comes during the turn and does not end it.
pub fn turn(context: &str) -> &str {
    let mut start = 0;
    let mut at = 0;
    let mut lines = context.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        at += line.len();
        let live = lines.peek().is_some_and(|next| next.starts_with("#borg-live "));
        if MARKERS.iter().any(|m| line.starts_with(m)) && !live {
            start = at;
        }
    }
    &context[start..]
}

/// The gate's score of the state file + final message: this turn's best lesson match over the
/// store's largest idf. 0 for an empty store or a turn that matches nothing.
pub fn score(challenges: &[Challenge], context: &str) -> f64 {
    let n = challenges.len() as f64;
    let top = conscience::rank(challenges, turn(context)).first().map(|(s, _)| *s).unwrap_or(0.0);
    let idf_max = (1.0 + (n - 0.5) / 1.5).ln();
    if idf_max > 0.0 { top / idf_max } else { 0.0 }
}

/// The threshold in force: `BORG_PREFILTER` = "off" (None: always ask the model), a number, or unset
/// (`THRESHOLD`). An unparsable value keeps the default.
pub fn threshold() -> Option<f64> {
    match std::env::var("BORG_PREFILTER") {
        Ok(v) if v.trim().eq_ignore_ascii_case("off") => None,
        Ok(v) => Some(v.trim().parse().unwrap_or(THRESHOLD)),
        Err(_) => Some(THRESHOLD),
    }
}

/// Some(score) when the Stop hook should skip the model call for this state file + final message.
pub fn skip(challenges: &[Challenge], context: &str) -> Option<f64> {
    let threshold = threshold()?;
    let s = score(challenges, context);
    (s < threshold).then_some(s)
}

/// `BORG_STOP_GATE=edits` (reports/whole-store.md): instead of the lexical score, the model is asked
/// whenever this turn changed something (an edit tool, or a shell command that is not read-only), and a
/// read-only or talk-only turn skips. Default (unset or anything else): the lexical gate above.
pub fn edits_gate() -> bool {
    std::env::var("BORG_STOP_GATE").is_ok_and(|v| v.trim().eq_ignore_ascii_case("edits"))
}

/// Programs that only read (with the exceptions in `changes`). Anything else counts as a change.
const READ_ONLY: &[&str] = &[
    "ls", "cat", "head", "tail", "less", "more", "grep", "egrep", "rg", "ag", "find", "fd", "tree", "wc", "file", "stat", "du", "df", "pwd", "echo", "printf",
    "which", "type", "whereis", "env", "printenv", "date", "whoami", "id", "uname", "ps", "jq", "yq", "sort", "uniq", "cut", "tr", "diff", "cmp", "column",
    "nl", "realpath", "dirname", "basename", "sed", "awk", "cd", "true",
];

/// git subcommands that only read.
const GIT_READ: &[&str] = &["status", "log", "diff", "show", "blame", "branch", "rev-parse", "ls-files", "grep", "remote", "describe", "shortlog"];

/// Whether a tool call changes state: an edit tool (Edit, Write, MultiEdit, NotebookEdit), or a Bash
/// command any part of which is not read-only (`READ_ONLY`, read-only git), redirects output into a
/// file, or edits in place (`sed -i`, `find -delete/-exec`). Other tools (Read, Grep, Glob, ...) read.
pub fn changes(tool: &str, input: &serde_json::Value) -> bool {
    let tool = tool.to_ascii_lowercase();
    if ["edit", "write", "multiedit", "notebookedit"].contains(&tool.as_str()) {
        return true;
    }
    if tool != "bash" {
        return false;
    }
    let cmd = input.get("command").and_then(serde_json::Value::as_str).unwrap_or_default();
    let cmd = ["2>&1", "&>/dev/null", "2>/dev/null", ">/dev/null", "> /dev/null"].iter().fold(cmd.to_string(), |c, r| c.replace(r, " "));
    if cmd.contains('>') {
        return true;
    }
    cmd.split(['\n', ';', '|', '&', '(', ')', '`']).map(str::trim).filter(|p| !p.is_empty()).any(|part| {
        let words: Vec<&str> = part.split_whitespace().skip_while(|w| w.contains('=') && !w.starts_with('-')).collect();
        let Some(&prog) = words.first() else { return false };
        let prog = prog.rsplit('/').next().unwrap_or(prog);
        match prog {
            "git" => {
                // The subcommand: the first word after `git` that is not an option (or `-C`/`-c`'s value).
                let mut rest = words[1..].iter();
                let sub = loop {
                    match rest.next() {
                        Some(&("-C" | "-c")) => {
                            rest.next();
                        }
                        Some(w) if w.starts_with('-') => {}
                        other => break other.copied(),
                    }
                };
                let mutating_flag = rest.any(|w| ["-d", "-D", "-m", "-M", "--delete", "add", "remove", "rename", "set-url"].contains(w));
                !sub.is_some_and(|s| GIT_READ.contains(&s)) || mutating_flag
            }
            "sed" => words.iter().any(|w| w.starts_with("-i") || *w == "--in-place"),
            "find" => words.iter().any(|w| ["-delete", "-exec", "-execdir", "-ok"].contains(w)),
            p => !READ_ONLY.contains(&p),
        }
    })
}

/// What the gate did in a store's session state (`borg status`): turn ends skipped at 0 tokens, turn
/// ends that asked the model, and the model's cost as logged by `#borg-predict-cost` (USD for Claude,
/// tokens for `codex:<model>`). Live checks (live.rs) log `#borg-predict-cost` too, and a `live` cost
/// line each: they are counted apart (`live_checks`, `live_cost`), not as turn ends.
#[derive(Debug, Default, PartialEq)]
pub struct Tally {
    pub skipped: usize,
    pub asked: usize,
    pub cost: f64,
    /// R3b (main.rs `log_cost`): Stop blocks (extra rounds), the questions in them and their size;
    /// free deliveries (context on a hook that forces no round), their lessons and size.
    pub rounds: usize,
    pub questions: usize,
    pub block_chars: usize,
    pub free: usize,
    pub free_lessons: usize,
    pub free_chars: usize,
    /// R1 (main.rs `record_verdicts`): model calls that also judged pending lessons, the lessons judged,
    /// those that got a final verdict, and the characters the judging added to those prompts.
    pub judge_calls: usize,
    pub judged: usize,
    pub judge_resolved: usize,
    pub judge_chars: usize,
    /// Live mode (main.rs `live_check`): model calls during the turn, their cost and lessons shown.
    pub live_checks: usize,
    pub live_cost: f64,
    pub live_lessons: usize,
}

pub fn tally(state: &std::path::Path) -> Tally {
    let mut t = Tally::default();
    for e in std::fs::read_dir(state).into_iter().flatten().flatten() {
        if e.path().extension().is_some_and(|x| x == "cost") {
            for line in std::fs::read_to_string(e.path()).unwrap_or_default().lines() {
                let field = |k: &str| line.split_whitespace().find_map(|w| w.strip_prefix(k)).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                if line.starts_with("block ") {
                    t.rounds += 1;
                    t.questions += field("questions=");
                    t.block_chars += field("chars=");
                } else if line.starts_with("free ") {
                    t.free += 1;
                    t.free_lessons += field("lessons=");
                    t.free_chars += field("chars=");
                } else if line.starts_with("live ") {
                    t.live_checks += 1;
                    t.live_lessons += field("lessons=");
                    t.live_cost += line.split_whitespace().find_map(|w| w.strip_prefix("usd=")).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                } else if line.starts_with("judge ") {
                    t.judge_calls += 1;
                    t.judged += field("pending=");
                    t.judge_resolved += field("resolved=");
                    t.judge_chars += field("chars=");
                }
            }
            continue;
        }
        if e.path().extension().is_none_or(|x| x != "ctx") {
            continue;
        }
        for line in std::fs::read_to_string(e.path()).unwrap_or_default().lines() {
            if line.starts_with("#borg-skip") {
                t.skipped += 1;
            } else if let Some(c) = line.strip_prefix("#borg-predict-cost ") {
                t.asked += 1;
                t.cost += c.trim().parse::<f64>().unwrap_or(0.0);
            }
        }
    }
    t.asked = t.asked.saturating_sub(t.live_checks);
    t.cost = (t.cost - t.live_cost).max(0.0);
    t
}

/// The gate's score and what an eval needs beside it (eval/preempt/stop_prefilter.py).
pub fn features(challenges: &[Challenge], context: &str) -> serde_json::Value {
    let t = turn(context);
    let ranked = conscience::rank(challenges, t);
    let shortlist = conscience::select(challenges, context, 20);
    let q: std::collections::HashSet<String> = conscience::tokens(t).into_iter().collect();
    serde_json::json!({
        "score": score(challenges, context),
        "turn_scores": ranked.iter().take(20).map(|(s, _)| *s).collect::<Vec<_>>(),
        "turn_ids": ranked.iter().take(20).map(|(_, c)| c.id.clone()).collect::<Vec<_>>(),
        "qtokens": q.len(),
        "turn_chars": t.len(),
        "n": challenges.len(),
        "shortlist": shortlist.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(id: &str, q: &str) -> Challenge {
        Challenge { id: id.into(), question: q.into(), ..Default::default() }
    }

    #[test]
    fn a_turn_starts_after_the_last_stop_marker() {
        let ctx = "first prompt\nsrc/a.rs\n#borg-predict-cost 0.05\n#borg-asked L1\nsecond prompt\n#borg-skip 1.20\nthird\nsrc/b.rs\n";
        assert_eq!(turn(ctx), "third\nsrc/b.rs\n");
        assert_eq!(turn("only\n"), "only\n");
        assert_eq!(turn("x\n#borg-asked \n"), "");
        // A live check's ask is part of the turn, not its end.
        assert_eq!(turn("a\n#borg-asked L1\nb\n#borg-asked L2\n#borg-live L2 ms=5\nc\n"), "b\n#borg-asked L2\n#borg-live L2 ms=5\nc\n");
    }

    #[test]
    fn scores_this_turn_only_and_normalises_by_store_size() {
        let ls: Vec<Challenge> = (0..30)
            .map(|i| lesson(&format!("L{i}"), &format!("unrelated filler words number{i} about gardening")))
            .chain([lesson("lock", "Did I hold the websocket send lock across the await on the delivery queue?")])
            .collect();
        let hit = "plan: hold the websocket send lock while awaiting the delivery queue";
        assert!(score(&ls, hit) > score(&ls, "ok, thanks"), "a turn about a lesson scores above chat");
        let earlier = format!("{hit}\n#borg-asked lock\nok, thanks\n");
        assert_eq!(score(&ls, &earlier), score(&ls, "ok, thanks"), "earlier turns do not count");
        assert_eq!(score(&[], hit), 0.0);
        assert_eq!(score(&ls[..1], "nothing matches here"), 0.0, "a store of one has idf 0 or no match");
    }

    /// Why the held-out result was fixed in advance (reports/stop-prefilter.md): query words are a set
    /// and every BM25 term is positive, so adding words to a turn never lowers its score. A checkpoint
    /// whose brief alone reaches the threshold passes whatever the drone adds to it.
    #[test]
    fn adding_words_to_a_turn_never_lowers_its_score() {
        let ls: Vec<Challenge> = (0..20)
            .map(|i| lesson(&format!("L{i}"), &format!("question {i} about retries, timeouts and module{i} wiring")))
            .chain([lesson("lock", "Did I hold the websocket send lock across the await on the delivery queue?")])
            .collect();
        let brief = "plan: hold the websocket send lock while awaiting the delivery queue";
        let base = score(&ls, brief);
        assert!(base > 0.0);
        let mut turn = brief.to_string();
        for extra in ["ok", "retries retries retries", "module3 wiring timeouts", "lock lock lock lock", "unrelated gardening words"] {
            turn.push('\n');
            turn.push_str(extra);
            let s = score(&ls, &turn);
            assert!(s >= base - 1e-12, "{extra:?} lowered the score: {s} < {base}");
        }
    }

    #[test]
    fn edit_tools_and_state_changing_commands_count_as_changes() {
        let bash = |c: &str| changes("Bash", &serde_json::json!({"command": c}));
        assert!(changes("Edit", &serde_json::json!({})) && changes("write", &serde_json::json!({})) && changes("MultiEdit", &serde_json::json!({})));
        assert!(!changes("Read", &serde_json::json!({"file_path": "/x"})) && !changes("Grep", &serde_json::json!({})));
        for read in [
            "ls -la src",
            "cat a.rs | head -20 2>&1",
            "rg -n foo crates/ > /dev/null",
            "git status && git log --oneline -5",
            "git -C /repo diff HEAD~1",
            "sed -n 1,80p main.rs",
            "find . -name '*.rs' | wc -l",
            "cd /x && grep -r foo .",
            "LANG=C sort a | uniq -c",
        ] {
            assert!(!bash(read), "{read}");
        }
        for change in [
            "cargo build",
            "rm -rf target",
            "echo x > notes.md",
            "git commit -m x",
            "git -C /repo checkout -b y",
            "git branch -D old",
            "sed -i s/a/b/ f.rs",
            "find . -name '*.tmp' -delete",
            "ls && mv a b",
            "python3 script.py",
        ] {
            assert!(bash(change), "{change}");
        }
        assert!(!bash(""));
    }

    #[test]
    fn tallies_skips_and_model_calls_across_sessions() {
        let d = std::env::temp_dir().join(format!("borg-prefilter-tally-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.ctx"), "p\n#borg-skip 1.50\nq\n#borg-predict-cost 0.0500\n#borg-asked L1\n").unwrap();
        std::fs::write(d.join("b.ctx"), "#borg-skip 0.00\n#borg-predict-cost 0.0250\n#borg-asked \n").unwrap();
        std::fs::write(d.join("b.prompts"), "#borg-skip in a prompt file does not count\n").unwrap();
        std::fs::write(d.join("a.cost"), "free event=UserPromptSubmit lessons=3 chars=900\nblock questions=7 chars=2100\nblock questions=2 chars=600\n")
            .unwrap();
        std::fs::write(d.join("b.cost"), "free event=PostToolUse lessons=1 chars=300\njudge call=2 pending=3 resolved=2 chars=4100\n").unwrap();
        let t = tally(&d);
        assert_eq!((t.skipped, t.asked), (2, 2));
        assert_eq!((t.live_checks, t.live_lessons), (0, 0));
        assert_eq!((t.rounds, t.questions, t.block_chars), (2, 9, 2700));
        assert_eq!((t.free, t.free_lessons, t.free_chars), (2, 4, 1200));
        assert_eq!((t.judge_calls, t.judged, t.judge_resolved, t.judge_chars), (1, 3, 2, 4100));
        assert!((t.cost - 0.075).abs() < 1e-9);
        // A live check logs its call like a turn end's, and a `live` cost line: it is counted apart.
        std::fs::write(d.join("c.ctx"), "p\n#borg-predict-cost 0.0400\n#borg-live-skip 1.00\n").unwrap();
        std::fs::write(d.join("c.cost"), "live usd=0.0400 lessons=8\n").unwrap();
        let t = tally(&d);
        assert_eq!((t.skipped, t.asked, t.live_checks, t.live_lessons), (2, 2, 1, 8));
        assert!((t.cost - 0.075).abs() < 1e-9 && (t.live_cost - 0.04).abs() < 1e-9);
        assert_eq!(tally(&d.join("absent")), Tally::default());
    }
}
