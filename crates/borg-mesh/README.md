# borg-mesh

The team mind without a server: the `borg-mesh` binary syncs a collective's lessons peer to peer between
its signed members, over iroh QUIC, through NAT or a relay. `borg mesh ...` forwards to it; it is a
separate binary so the networking stack stays out of `borg hook`, which runs on every tool call.

Install it next to [borg](https://github.com/debe/borg_ai) for a team:

```sh
cargo install borg-ai borg-mesh --locked
borg setup --team      # or --join <ticket>
```

Setup, relays and corporate networks: [Team sync](https://github.com/debe/borg_ai/blob/main/docs/team-sync.md).
The library is internal (no semver); depend on the binary.

License: MIT OR Apache-2.0.
