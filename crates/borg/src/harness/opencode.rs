//! opencode 1.x and 2.x: the TS plugin `adapters/opencode/borg.ts`, compiled in and copied to
//! `~/.config/opencode/plugins/borg.ts` (`$OPENCODE_CONFIG_DIR`, `$XDG_CONFIG_HOME/opencode`). The plugin
//! translates both ways, sends opencode's pid, emulates the stop's second round and sets its budget.

use std::path::{Path, PathBuf};

use super::{Caps, Harness, Probe, Transcript, Wiring};

pub const ADAPTER: &str = include_str!("../../../../adapters/opencode/borg.ts");

pub struct Opencode;

impl Harness for Opencode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn caps(&self) -> Caps {
        Caps {
            prompt_context: true,
            tool_context: true,
            // The channels exist (borg.ts); turning these on waits for an evaluation.
            early_delivery: false,
            steer_in_context: false,
            question_dialog: false,
            transcript: Transcript::None,
            deny_capture: true,
            sends_pid: true,
            // The adapter names the stop's budget itself (`BORG_STOP_BUDGET_MS`).
            stop_budget: None,
        }
    }

    fn wiring(&self) -> Wiring {
        Wiring::AdapterFile { source: ADAPTER }
    }

    fn target(&self, home: &Path, var: &dyn Fn(&str) -> Option<PathBuf>) -> PathBuf {
        let dir = var("OPENCODE_CONFIG_DIR").or_else(|| var("XDG_CONFIG_HOME").map(|x| x.join("opencode")));
        dir.unwrap_or_else(|| home.join(".config/opencode")).join("plugins/borg.ts")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "opencode", tested: " (adapter tested with 1.18.32, 1.19.0, 2.0.23 and 2.0.29)" }
    }
}
