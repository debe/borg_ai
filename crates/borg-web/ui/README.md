# borg web UI

The page `borg web` serves: a Leptos client compiled to WebAssembly. `borg-web` embeds the built files
in `crates/borg-web/dist/` (committed, and outside `ui/` because cargo leaves a directory with its own
`Cargo.toml` out of a package), so installing it needs no wasm toolchain. Rebuild it after changing
anything here:

```sh
rustup target add wasm32-unknown-unknown   # once
cargo install trunk --locked               # once
scripts/ui.sh                              # trunk build --release, then records the source hash
cargo install --path crates/borg-web --locked --force
```

`scripts/check.sh` fails while `dist/` is older than the sources. `trunk serve` does not help here: the page needs borg-web's `/api` routes.
