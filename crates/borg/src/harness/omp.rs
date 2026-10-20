//! omp (oh-my-pi): the TS extension `adapters/omp/borg.ts`, compiled in and copied to
//! `~/.omp/agent/extensions/borg.ts` (`$PI_CODING_AGENT_DIR/extensions`). The adapter translates omp's
//! events into borg's canonical ones and back, sends omp's pid, and sets the stop's budget itself.

use std::path::{Path, PathBuf};

use super::{Caps, Harness, Probe, Transcript, Wiring};

pub const ADAPTER: &str = include_str!("../../assets/adapters/omp/borg.ts");

pub struct Omp;

impl Harness for Omp {
    fn name(&self) -> &'static str {
        "omp"
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
        var("PI_CODING_AGENT_DIR").unwrap_or_else(|| home.join(".omp/agent")).join("extensions/borg.ts")
    }

    fn probe(&self) -> Probe {
        Probe { cli: "omp", tested: " (adapter tested with 18.8.5 and 18.10.4; live mode needs 18.4.4 or newer)" }
    }
}
