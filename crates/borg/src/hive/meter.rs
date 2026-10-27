//! What an interactive drone's claim cost (phase g G5, docs/design/hive.md "Phase g", correction 13): one
//! line of the goal's `spend.jsonl` (run.rs `Spend`, which syncs) a claim, written when the claim ends
//! (its done or verdict at the stop, a release, the session's end), so the budget gate (mod.rs
//! `spent_out`) and monitoring read every node's interactive spend beside the runners'.
//!
//! - **Claude Code** (its hooks name the session's `transcript_path`): the usage of every assistant
//!   message of the transcript stamped at or after the claim, each message once (Claude Code repeats a
//!   message's usage on each of its content blocks), with the transcripts of its subagents beside it
//!   (`<transcript>/subagents/*.jsonl`): priced by its model ([`price`]), `exit: "interactive"`. A message
//!   of a model the table does not price makes the line unmetered rather than wrong.
//! - **Every other harness** (omp, opencode, codex: no hook event of theirs carries usage): `usd` None and
//!   `exit: "unmetered"`, which charges nothing (`Spend::charged`) and monitoring shows as unmetered.
//!
//! Never per drone: the gate reads the goal's whole ledger. Borg's own model calls are not the
//! session's usage and are never counted here (hook.rs `log_cost` is a different file).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::run::{Spend, spend_path};

/// Dollars per million tokens of a model: (input, output, cache write, cache read).
type Price = (f64, f64, f64, f64);

/// The first-party list prices per million tokens, by model id prefix, most specific first (the
/// claude-api reference's table, cached 2026-10-06). A cache write is 1.25 times the input price (the
/// 5-minute cache; a 1-hour write costs twice the input price, [`claude_usage`]); a cache read as
/// listed, else a tenth of the input price. Opus 4.5 and later cost a third of Opus 4.0 and 4.1, so
/// each Opus 4 minor version is listed by itself and one the table does not name stays unpriced
/// (unmetered) rather than priced wrong.
const PRICES: &[(&str, Price)] = &[
    ("claude-fable-5", (10.0, 50.0, 12.5, 0.25)),
    ("claude-mythos-5", (10.0, 50.0, 12.5, 0.25)),
    ("claude-opus-5-5", (4.0, 20.0, 5.0, 0.20)),
    ("claude-opus-5", (5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-8", (5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-7", (5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-6", (5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-5", (5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-1", (15.0, 75.0, 18.75, 1.50)),
    ("claude-opus-4-0", (15.0, 75.0, 18.75, 1.50)),
    ("claude-opus-4-2025", (15.0, 75.0, 18.75, 1.50)),
    ("claude-sonnet-5-5", (2.0, 10.0, 2.5, 0.20)),
    ("claude-sonnet-5", (2.0, 10.0, 2.5, 0.20)),
    ("claude-sonnet-4", (3.0, 15.0, 3.75, 0.30)),
    ("claude-haiku-5-5", (0.10, 0.50, 0.125, 0.01)),
    ("claude-haiku-4", (1.0, 5.0, 1.25, 0.10)),
];

/// The price of `model` (`PRICES`; a platform's prefix, `us.anthropic.`, left aside), None for a model it
/// does not list.
pub fn price(model: &str) -> Option<Price> {
    let m = model.trim().to_lowercase();
    let m = &m[m.find("claude-")?..];
    PRICES.iter().find(|(p, _)| m.starts_with(p)).map(|(_, p)| *p)
}

/// The tokens and dollars of the assistant messages of some transcripts since a time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    /// None when a message's model has no price (`price`): the claim is then unmetered.
    pub usd: Option<f64>,
}

/// The usage of Claude Code transcript `path` (and its subagents' beside it) stamped at or after
/// `since` (epoch ms), each assistant message once by its id. None when the transcript cannot be read.
pub fn claude_usage(path: &Path, since: i64) -> Option<Usage> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut by_id: BTreeMap<String, (String, Value)> = BTreeMap::new();
    let mut anon: Vec<(String, Value)> = Vec::new();
    let mut take = |text: &str| {
        for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
            if v["type"] != "assistant" || borg_trace::parse_ts(v["timestamp"].as_str().unwrap_or_default()) < since {
                continue;
            }
            let (model, usage) = (v["message"]["model"].as_str().unwrap_or_default().to_string(), v["message"]["usage"].clone());
            if !usage.is_object() {
                continue;
            }
            match v["message"]["id"].as_str() {
                Some(id) => {
                    by_id.insert(id.to_string(), (model, usage));
                }
                None => anon.push((model, usage)),
            }
        }
    };
    take(&text);
    for sub in subagents(path) {
        take(&std::fs::read_to_string(sub).unwrap_or_default());
    }
    let mut u = Usage { usd: Some(0.0), ..Usage::default() };
    for (model, usage) in by_id.into_values().chain(anon) {
        let n = |k: &str| usage[k].as_u64().unwrap_or(0);
        let (i, o, w, r) = (n("input_tokens"), n("output_tokens"), n("cache_creation_input_tokens"), n("cache_read_input_tokens"));
        if i + o + w + r == 0 {
            continue;
        }
        // The 1-hour writes (Claude Code's own cache) at twice the input price, the rest at the 5-minute price.
        let hour = usage["cache_creation"]["ephemeral_1h_input_tokens"].as_u64().unwrap_or(0).min(w);
        (u.input, u.output, u.cache_write, u.cache_read) = (u.input + i, u.output + o, u.cache_write + w, u.cache_read + r);
        u.usd = match (u.usd, price(&model)) {
            (Some(sum), Some((pi, po, pw, pr))) => {
                Some(sum + (i as f64 * pi + o as f64 * po + (w - hour) as f64 * pw + hour as f64 * 2.0 * pi + r as f64 * pr) / 1e6)
            }
            _ => None,
        };
    }
    Some(u)
}

/// The transcripts of a Claude Code session's subagents: `<transcript without .jsonl>/subagents/*.jsonl`.
fn subagents(path: &Path) -> Vec<PathBuf> {
    let dir = path.with_extension("").join("subagents");
    let mut out: Vec<PathBuf> =
        std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect();
    out.sort();
    out
}

/// One claim of an interactive drone ends: what it cost goes into `goal`'s spend.jsonl once (`claim`,
/// `task@stamp` of the claim, keys it), metered from Claude Code's `transcript` since the claim's stamp
/// `since` (a harness whose `Caps::transcript` is Claude Code's), else unmetered. Whether it wrote a line.
#[allow(clippy::too_many_arguments)]
pub fn record(home: &Path, goal: &str, session: &str, drone: &str, harness: &str, claim: &str, since: i64, transcript: Option<&str>, now: i64) -> bool {
    let path = spend_path(home, goal);
    let here = super::node(home);
    if super::read_lines::<Spend>(&path).iter().any(|s| s.claim == claim && s.session == session && s.owner(&here) == here) {
        return false;
    }
    let claude_transcript = crate::harness::by_name(harness).is_some_and(|h| h.caps().transcript == crate::harness::Transcript::Claude);
    let used = claude_transcript.then(|| transcript.filter(|t| !t.is_empty()).and_then(|t| claude_usage(Path::new(t), since))).flatten();
    let usd = used.and_then(|u| u.usd);
    let line = Spend {
        v: 1,
        ms: now,
        drone: drone.into(),
        node: here,
        iter: 0,
        harness: harness.into(),
        usd,
        est: None,
        session: session.into(),
        exit: if usd.is_some() { "interactive" } else { "unmetered" }.into(),
        claim: claim.into(),
    };
    super::append_line(&path, &line).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_priced_by_its_family_and_an_unknown_one_not_at_all() {
        assert_eq!(price("claude-opus-5-5"), Some((4.0, 20.0, 5.0, 0.20)));
        assert_eq!(price("claude-opus-5-20260101").map(|p| p.0), Some(5.0), "the 5 family, not 5.5");
        assert_eq!(price("us.anthropic.claude-sonnet-4-6").map(|p| p.0), Some(3.0), "a platform's prefix is left aside");
        assert_eq!(price("claude-opus-4-5-20251101").map(|p| p.0), Some(5.0));
        assert_eq!(price("claude-opus-4-1-20250805").map(|p| p.0), Some(15.0), "Opus 4.1 at its own price, not 4.5's");
        assert_eq!(price("claude-opus-4-20250514").map(|p| p.1), Some(75.0));
        assert_eq!(price("claude-opus-4-0").map(|p| p.0), Some(15.0));
        assert_eq!(price("claude-opus-4-9"), None, "an Opus 4 version the table does not name: unmetered, never wrong");
        assert_eq!(price("<synthetic>"), None);
        assert_eq!(price("gpt-5"), None);
    }

    /// The recorded transcript shape (tests/fixtures/claude/transcript-usage.jsonl): a message before
    /// the claim counts nothing, a message listed once per content block counts once, a synthetic
    /// message with no usage nothing, a subagent's transcript beside it counts; a model the table does
    /// not price makes the whole unmetered.
    #[test]
    fn a_transcripts_usage_since_the_claim_counts_each_message_once() {
        let dir = std::env::temp_dir().join(format!("borg-hive-meter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("t/subagents")).unwrap();
        let fix = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude");
        let fill = |name: &str| {
            std::fs::read_to_string(fix.join(name)).unwrap().replace("BEFORE", "2026-10-27T10:00:00.000Z").replace("AFTER", "2026-10-27T10:05:00.000Z")
        };
        let since = borg_trace::parse_ts("2026-10-27T10:01:00.000Z");
        std::fs::write(dir.join("t.jsonl"), fill("transcript-usage.jsonl")).unwrap();
        let main = claude_usage(&dir.join("t.jsonl"), since).unwrap();
        assert_eq!((main.input, main.output, main.cache_write, main.cache_read), (1000, 2000, 10000, 100000));
        assert!((main.usd.unwrap() - 0.114).abs() < 1e-9, "{main:?}");
        std::fs::write(dir.join("t/subagents/agent-1.jsonl"), fill("transcript-subagent.jsonl")).unwrap();
        assert!((claude_usage(&dir.join("t.jsonl"), since).unwrap().usd.unwrap() - 0.1158).abs() < 1e-9);
        std::fs::write(
            dir.join("t/subagents/agent-2.jsonl"),
            fill("transcript-subagent.jsonl").replace("claude-sonnet-4-6", "some-other-model").replace("msg_sub", "msg_other"),
        )
        .unwrap();
        assert_eq!(claude_usage(&dir.join("t.jsonl"), since).unwrap().usd, None, "unpriced: unmetered, never wrong");
        assert_eq!(claude_usage(&dir.join("none.jsonl"), since), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A write to the 1-hour cache (`usage.cache_creation.ephemeral_1h_input_tokens`, Claude Code's own)
    /// costs twice the input price, the rest of the writes 1.25 times.
    #[test]
    fn a_one_hour_cache_write_costs_twice_the_input_price() {
        let dir = std::env::temp_dir().join(format!("borg-hive-meter-1h-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let line = r#"{"type":"assistant","timestamp":"2026-10-27T10:05:00.000Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":1000000,"cache_creation":{"ephemeral_5m_input_tokens":250000,"ephemeral_1h_input_tokens":750000}}}}"#;
        std::fs::write(dir.join("t.jsonl"), line).unwrap();
        let u = claude_usage(&dir.join("t.jsonl"), 0).unwrap();
        assert!((u.usd.unwrap() - (0.25 * 5.0 + 0.75 * 8.0)).abs() < 1e-9, "{u:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
