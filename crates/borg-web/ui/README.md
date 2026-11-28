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

## The Cube

The Cube tab draws on a `<canvas>`; everything with text (counters, ticker, panel, timeline) is DOM
over it. The drawing is plain Rust with no web-sys, so it is tested and benchmarked natively:

| Module | Holds |
|---|---|
| `src/model.rs` | `/api/mesh` parsed borrowed into an owned model: strings interned, events deduped by id and kept to the 7-day window |
| `src/scene.rs` | Layout (machines on a sphere, drones in orbit), camera, projection, hit tests, and the pool of effects each event plays; `needs_draw` says whether anything moved a pixel; the core's primitives ordered by counting, far to near between the shell's far and near faces |
| `src/volume.rs` | The lessons in the core: their concern areas cut by a 3D split treemap with shafts, each lesson's light and the light an event gives it |
| `src/interior.rs` | The core built as a Borg cube when the lessons change: the shell, an area of one of eight kinds per concern area, corridors, a light per lesson |
| `src/paint.rs` | Draws a scene through the `Painter` trait, batched per style; a warm frame allocates nothing (`a_warm_frame_allocates_nothing`) |
| `src/canvas.rs` | The browser's `Painter`: a 2D context, glows as pre-rendered sprites, styles set only when they change |
| `src/views/cube.rs`, `src/views/cube/` | The view: fetch and replay clock, input, counters, ticker, toasts, filters, timeline and panel |

```sh
cargo test --manifest-path crates/borg-web/ui/Cargo.toml        # natively, as scripts/check.sh does
cd crates/borg-web/ui && cargo bench --bench scene               # 600 frames: 50 machines, 300 drones, 1000 lessons, 1000 effects (DIVE=1: in a dive)
```

The bench exits with an error if a measured frame allocates; for instructions per frame run it under
`valgrind --tool=callgrind --toggle-collect='*measure*'`.
