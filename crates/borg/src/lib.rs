//! borg as a library: the conscience, its learning loop and the expert index, shared by the
//! `borg` CLI (main.rs) and the eval harness.
//!
//! **Internal API, no semver.** It exists for borg's own crates (borg-mesh, borg-web, the evals),
//! which pin it `=` to the same version. Any release may change or remove any item; depend on the
//! `borg` binary and its documented files instead.

#[cfg(not(unix))]
compile_error!("borg supports Unix only (Linux, macOS): it relies on process groups, signals and Unix file modes");

pub mod assimilate;
pub mod assimilate_dir;
pub(crate) mod attention;
pub mod collective;
pub mod completions;
#[doc(hidden)]
pub mod concepts;
pub mod conscience;
pub mod escalation;
pub mod expert;
pub(crate) mod expert_route;
pub mod facets;
pub mod harness;
pub mod hook;
pub mod install;
pub(crate) mod jsonl;
pub mod learn;
pub mod listen;
pub mod live;
pub mod memory;
pub mod msg;
pub mod notes;
pub(crate) mod notify;
pub mod ope;
pub mod outcome;
pub mod playbook;
#[doc(hidden)]
pub mod predict;
pub mod prefilter;
pub mod presence;
pub mod queen;
pub mod reach;
pub mod reexec;
pub mod review_fix;
#[doc(hidden)]
pub mod runner;
pub mod stats;
pub mod steer;
#[doc(hidden)]
pub mod systems;
pub mod top;
pub(crate) mod top_live;
pub mod trust;
pub mod vectors;
pub mod veto;
#[doc(hidden)]
pub mod whole;
pub mod why;
