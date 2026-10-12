//! The replay evals (`preempt`, `queen-replay`, `queen-test`) need private eval data and are optional:
//! the crate builds without their modules, and `cfg(replay_evals)` is set only when they are present.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(replay_evals)");
    println!("cargo::rerun-if-changed=src");
    let present = ["preempt.rs", "queen_replay.rs", "queen_test.rs"].iter().all(|f| std::path::Path::new("src").join(f).exists());
    if present {
        println!("cargo::rustc-cfg=replay_evals");
    }
}
