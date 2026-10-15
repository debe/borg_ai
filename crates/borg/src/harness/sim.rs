//! The simulated harness, for the conformance suites (tests/conformance_cli.rs, docs/harnesses.md):
//! command hooks in `~/.sim/hooks.json`, under native event names of its own, and capabilities from
//! `BORG_SIM_CAPS`, so every capability branch of the core is reachable from one binary without any
//! third-party agent. Hidden: never in help, completions, `all`, status or doctor, and known by name
//! only to a process with `BORG_TEST_HARNESS` set (`TEST_ENV`), as the suites run it.

use std::path::{Path, PathBuf};

use super::{Caps, Harness, Probe, Transcript, Wiring};

/// Native names unlike any real harness's: the event table, not a name, decides the canonical event.
pub const EVENTS: [(&str, &str); 6] = [
    ("SimPrompt", "prompt"),
    ("SimPreTool", "pre_tool"),
    ("SimPostTool", "post_tool"),
    ("SimPostToolFailure", "post_tool_failure"),
    ("SimStop", "stop"),
    ("SimEnd", "end"),
];

pub const CAPS_ENV: &str = "BORG_SIM_CAPS";

/// Without `BORG_SIM_CAPS`: context at the prompt and after a tool call, denials captured, nothing else.
const DEFAULT_CAPS: &str = "prompt,tool,deny";

pub struct Sim;

/// The flags in a comma list: prompt, tool, early, steer, dialog, transcript, deny, pid.
pub fn caps_of(list: &str) -> Caps {
    let on = |f: &str| list.split(',').any(|x| x.trim() == f);
    Caps {
        prompt_context: on("prompt"),
        tool_context: on("tool"),
        early_delivery: on("early"),
        steer_in_context: on("steer"),
        question_dialog: on("dialog"),
        transcript: if on("transcript") { Transcript::Claude } else { Transcript::None },
        deny_capture: on("deny"),
        sends_pid: on("pid"),
        stop_budget: None,
    }
}

impl Harness for Sim {
    fn name(&self) -> &'static str {
        "sim"
    }

    fn caps(&self) -> Caps {
        caps_of(&std::env::var(CAPS_ENV).unwrap_or_else(|_| DEFAULT_CAPS.into()))
    }

    fn wiring(&self) -> Wiring {
        Wiring::CommandHooks { events: &EVENTS, timeouts: &[("stop", 90)], through_links: false }
    }

    fn target(&self, home: &Path, _var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        home.join(".sim/hooks.json")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "sim", tested: "" }
    }

    fn hidden(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_come_from_the_list() {
        let c = caps_of("early, steer,transcript");
        assert!(c.early_delivery && c.steer_in_context && !c.question_dialog && !c.prompt_context);
        assert_eq!(c.transcript, Transcript::Claude);
        let d = caps_of(DEFAULT_CAPS);
        assert!(d.prompt_context && d.tool_context && d.deny_capture && !d.early_delivery && !d.sends_pid);
    }
}
