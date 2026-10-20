# Contributing

Bug reports, measurements and patches are welcome. Open an issue first for anything larger than a
small fix, so we can agree on the shape before you write it.

## Build and test

You need Rust 1.91 or newer, and Node 23.6+ or bun for the adapter tests. On macOS, if `esaxx-rs`
cannot find the C++ headers, set `CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"`.

```sh
scripts/check.sh      # what CI runs, plus the tool-backed gates; must pass before a change lands
```

It stops at the first failure and runs, in order:

1. `cargo fmt --all --check` (`max_width = 160`);
2. `scripts/ui.sh --check`: `crates/borg-web/dist` matches the Leptos sources in `crates/borg-web/ui`
   (rebuild with `scripts/ui.sh` after a UI change; it needs trunk);
3. `cargo clippy --workspace --all-targets -- -D warnings`, and the same for
   `-p borg-ai --no-default-features`;
4. `cargo test --workspace`;
5. `scripts/arch.sh`, the architecture gates below;
6. `cargo test -p borg-ai --no-default-features`;
7. the adapter tests and the harness conformance suite (`node --test adapters/...`).

CI runs `scripts/check.sh` on Linux and macOS for every push to main. It does not run on pull
requests, so run it yourself and say in the pull request that it passed.

## Architecture gates

`scripts/arch.sh` measures the harness abstraction and fails when a gate fails. It needs lizard,
cargo-coupling and rust-code-analysis-cli; a missing tool skips its rows with a note. CI installs none
of them and clones without history, so there only M1 and M2 are enforced (and `harness_arch.rs` checks
them again); run the others locally, with the base commit in your clone for the before column.

| Gate | Limit |
|---|---|
| M1: harness-name branches outside `src/harness/` | 0 |
| M2: places listing harness names outside `src/harness/` | 0 |
| M5: dependency cycles through `harness::*` | 0 |
| M7: CCN of any function in `src/harness/` | ≤ 10 |
| M9: CCN of the hook's per-event functions (`hook.rs`) | ≤ 15 |
| M10: functions in `install.rs`, tests aside | < 40 |

[Harnesses](docs/harnesses.md) explains them and how to add an agent.

## The metric bar

Every new or changed function meets these limits, measured, not guessed:

- **lizard** (`lizard -l rust`): cyclomatic complexity ≤ 10, ≤ 60 lines of code, ≤ 5 parameters, and
  no new `lizard -C 15` warnings.
- **rust-code-analysis-cli:** cognitive complexity ≤ 15 per function. The maintainability index of a
  file you touch does not drop.
- **clippy pedantic** (`cargo clippy -- -W clippy::pedantic`): no new warnings in the files you change.
- **cargo-coupling:** no new dependency cycles.

Compare against `main` before and after your change, and put the numbers in the pull request.

## Style

- **Hooks fail open.** A missing binary, a bad config, an unreachable model or a locked file lets the
  agent continue untouched. A hook never blocks on the network.
- **Dry run before writes.** A command that changes files outside borg's stores prints the diff and
  writes only with `--apply`, with a backup.
- **Hand-rolled argument parsing**, as in the existing commands; no CLI framework.
- **Terse module docs:** a `//!` paragraph that says what the module does and why, then the code.
- **Tests next to the code** (`#[cfg(test)]`) and CLI tests in `crates/*/tests/`. A change of
  behaviour comes with a test that fails without it.
- **Defaults change only with evidence.** A new default needs a rule written before the measurement
  and the result; see the reports in `reports/`.
- **The skill stays in step with the CLI.** `crates/borg/tests/skill_cli.rs` checks that every command
  and flag the `borg` skill names exists; update `crates/borg/assets/skills/borg` with the CLI.

## License

By contributing you agree that your contribution is licensed under MIT OR Apache-2.0, like the rest
of the project.
