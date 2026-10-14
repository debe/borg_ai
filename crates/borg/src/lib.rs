//! borg as a library: the conscience, its learning loop and the expert index, shared by the
//! `borg` CLI (main.rs) and the eval harness.

pub mod assimilate;
pub mod assimilate_dir;
pub mod collective;
pub mod completions;
pub mod concepts;
pub mod conscience;
pub mod expert;
pub mod expert_route;
pub mod facets;
pub mod install;
pub mod learn;
pub mod live;
pub mod memory;
pub mod notes;
pub mod notify;
pub mod outcome;
pub mod predict;
pub mod prefilter;
pub mod presence;
pub mod queen;
pub mod reach;
pub mod runner;
pub mod stats;
pub mod steer;
pub mod systems;
pub mod top;
pub mod trust;
pub mod vectors;
pub mod whole;
pub mod why;
