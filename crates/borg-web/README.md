# borg-web

A local, live page of a collective's knowledge and usage statistics on 127.0.0.1: lessons and their
belief, checks and questions, drones, messages, hand-offs and cost, and the collective live as a Borg
cube (machines, drones, and what moves between them). `borg web` forwards to this binary; it is
separate so the web stack stays out of `borg hook`. The page is built and embedded, so installing it
needs no wasm toolchain.

```sh
cargo install borg-ai borg-web --locked
borg web        # http://127.0.0.1:7979/
```

To try it without a store of your own, or to film the Cube, generate a made-up team collective (six
machines, their drones and a week of messages, lessons, hand-offs and syncs; every name invented) and
point the page at it. `--live 60` keeps writing a message about every second for a minute:

```sh
cargo run -p borg-web --example demo_store -- /tmp/borg-demo --live 60 &
BORG_HOME=/tmp/borg-demo/home BORG_CONFIG=/tmp/borg-demo/collectives.json \
    borg-web --port 7990 --dir /tmp/borg-demo/repos/shop
```

The README's video is this store, recorded from a headless Chromium with the page in a UTS namespace
of its own, so this machine shows as `unimatrix-01` too (`unshare --user --map-root-user --uts sh -c
'hostname unimatrix-01; exec borg-web …'`).

What each tab shows: [Statistics and the page](https://github.com/debe/borg_ai/blob/main/docs/statistics.md).
The library is internal (no semver); depend on the binary.

License: MIT OR Apache-2.0.
