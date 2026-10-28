# borg-web

A local, live page of a collective's knowledge and usage statistics on 127.0.0.1: lessons and their
belief, checks and questions, drones, playbooks and cost. `borg web` forwards to this binary; it is
separate so the web stack stays out of `borg hook`. The page is built and embedded, so installing it
needs no wasm toolchain.

```sh
cargo install borg-ai borg-web --locked
borg web        # http://127.0.0.1:7979/
```

What each tab shows: [Statistics and the page](https://github.com/debe/borg_ai/blob/main/docs/statistics.md).
The library is internal (no semver); depend on the binary.

License: MIT OR Apache-2.0.
