//! Claude Code: native command hooks in `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR`), each running
//! `<borg> hook claude <event>`; its events are borg's canonical shape already. The one harness with a
//! transcript (the readers below), early delivery, steering in context and the question dialog.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Caps, Harness, Probe, Transcript, Wiring};

/// Claude Code hook events and the canonical borg events they become.
pub const EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "prompt"),
    // Only a paused drone's tool calls are refused here (steer.rs); otherwise a stat and out.
    ("PreToolUse", "pre_tool"),
    ("PostToolUse", "post_tool"),
    ("PostToolUseFailure", "post_tool_failure"),
    ("Stop", "stop"),
    ("SessionEnd", "end"),
];

/// Seconds Claude Code gives borg's Stop hook (its own default for a command hook is 60 s). The
/// expert drone keeps inside it (expert_route.rs `drone_cap`), so a slow drone never costs the
/// conscience's answer.
pub const STOP_TIMEOUT_S: u64 = 90;

pub struct Claude;

impl Harness for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn caps(&self) -> Caps {
        Caps {
            prompt_context: true,
            tool_context: true,
            early_delivery: true,
            steer_in_context: true,
            question_dialog: true,
            transcript: Transcript::Claude,
            deny_capture: true,
            sends_pid: false,
            // Not the 90 s install gives: the expert drone keeps inside Claude Code's 60 s default
            // (`DEFAULT_STOP_BUDGET`), so an install that predates that timeout keeps a margin too.
            stop_budget: None,
        }
    }

    fn wiring(&self) -> Wiring {
        Wiring::CommandHooks { events: &EVENTS, timeouts: &[("stop", STOP_TIMEOUT_S)], through_links: true }
    }

    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        var("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")).join("settings.json")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "claude", tested: "" }
    }
}

// ---------------------------------------------------------------- the transcript (JSONL)

/// The drone's last visible text before now, from the transcript (its proposal), its last 2000
/// characters.
pub fn last_assistant_text(transcript: &Path) -> String {
    let t = last_assistant_full(transcript);
    let chars: Vec<char> = t.chars().collect();
    chars[chars.len().saturating_sub(2000)..].iter().collect()
}

/// `last_assistant_text`, whole.
pub fn last_assistant_full(transcript: &Path) -> String {
    let text = std::fs::read_to_string(transcript).unwrap_or_default();
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        let t: String = v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !t.trim().is_empty() {
            return t;
        }
    }
    String::new()
}

/// This turn's work so far, from a Claude transcript (live.rs): the drone's text and its tool calls (name
/// and a short input: the path, or the command) since the engineer's last prompt, one per line, its last
/// `cap` characters. Mid-turn the last text block may be an earlier turn's (`last_assistant_full`), so
/// this reads the turn itself. Empty without a transcript.
pub fn turn_so_far(transcript: &Path, cap: usize) -> String {
    let text = std::fs::read_to_string(transcript).unwrap_or_default();
    let entries: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
    // The engineer's prompt: a user entry with text of its own, not a tool result or an injected note.
    let prompt = |v: &Value| {
        v["type"] == "user"
            && v["isMeta"] != true
            && match &v["message"]["content"] {
                Value::String(t) => !t.trim().is_empty(),
                Value::Array(bs) => bs.iter().any(|b| b["type"] == "text") && !bs.iter().any(|b| b["type"] == "tool_result"),
                _ => false,
            }
    };
    let start = entries.iter().rposition(prompt).map_or(0, |i| i + 1);
    let mut out: Vec<String> = Vec::new();
    for v in entries[start..].iter().filter(|v| v["type"] == "assistant") {
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => out.extend(b["text"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string)),
                Some("tool_use") => {
                    let input = &b["input"];
                    let short = ["file_path", "notebook_path", "command", "pattern", "path"]
                        .iter()
                        .find_map(|k| input[k].as_str())
                        .map_or_else(|| input.to_string(), str::to_string);
                    let short: String = short.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
                    out.push(format!("[{}] {short}", b["name"].as_str().unwrap_or("tool")));
                }
                _ => {}
            }
        }
    }
    let all = out.join("\n");
    let n = all.chars().count();
    all.chars().skip(n.saturating_sub(cap)).collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_turn_so_far_is_this_turns_text_and_tool_calls_since_the_last_prompt() {
        let t = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude/live-turn.jsonl"));
        assert_eq!(turn_so_far(t, 4000), "I will replace the reset arc.\n[Read] /x/net.rs\n[Edit] /x/net.rs\n[Bash] cargo test -p net");
        assert_eq!(turn_so_far(t, 20), "[Bash] cargo test -p net".chars().skip(4).collect::<String>(), "the newest work is kept");
        assert_eq!(turn_so_far(Path::new("/nonexistent/t.jsonl"), 4000), "");
    }

    #[test]
    fn its_config_dir_variable_wins_unless_isolated() {
        let none = |_: &str| None;
        assert_eq!(Claude.target(Path::new("/h"), &none), Path::new("/h/.claude/settings.json"));
        let set = |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| PathBuf::from("/c"));
        assert_eq!(Claude.target(Path::new("/h"), &set), Path::new("/c/settings.json"));
    }
}
