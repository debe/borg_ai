# Team and company collectives

A team collective is a `share: "team"` entry in collectives.json whose lessons sync between members'
machines over the mesh: peer to peer, end-to-end encrypted, no server of ours. Each machine runs
`borg mesh serve`. Hooks only read and write local files, so drones never wait for the mesh and borg
keeps working offline. `borg mesh` runs the separate `borg-mesh` binary; if it is missing:
`cargo install borg-mesh --locked` (or `--path crates/borg-mesh` from a borg checkout).

What travels: lessons, daily usage counts, the team's redacted outcomes, ratified
constraints, messages between sessions, which sessions are running, and the redacted reasoning behind
commits. Each line is signed by its writer's node. Raw episodes and transcripts stay where they were
captured.
`share: "local"` collectives and `off` repos are refused before any connection opens.

## Before anything: decide the relay

Peers are addressed by public key. iroh connects them directly where it can and falls back to a relay
(symmetric NAT, blocked UDP). The relay is per collective (`"relay"` in collectives.json; `BORG_RELAY`
overrides it for all), and **every member must use the same setting**.

| Setting | Reaches | Who sees metadata |
|---|---|---|
| absent or `"default"` | anywhere | n0's public relays and address lookup: node ids, public IPs, who talks to whom and when. Never lessons, member lists, repo names or emails |
| `"https://relay.example.com"` | anywhere both sides reach that host | your relay only |
| `"disabled"` | LAN or VPN only | nobody outside the network; peers must be given as `<key>@<ip:port>` |

**Self-hosted relay (company):** the `iroh-relay` binary of the iroh version borg-mesh uses
(`cargo install iroh-relay@1.3.0 --features server --locked`) on a host with a DNS name and a TLS
certificate. Put its URL in the collective's `relay` before the admin prints invites: each ticket
carries the admin's relay setting and `join` copies it. Several URLs, comma-separated, give redundancy.
The full guide (ports 443/TCP, 7842/UDP and 80/TCP, Let's Encrypt or your own certificate, the config
file, a systemd unit, a container, load balancers, an `allowlist` of members' `borg mesh id` keys) is
docs/relay.md in the borg repository: https://github.com/debe/borg_ai/blob/main/docs/relay.md

**Corporate networks:**
- A TLS-inspecting proxy that re-signs with a company CA: the relay's and address lookup's certificates
  are checked against the system trust store, so install the company CA there. The proxy then sees the
  metadata above, never lesson content (end-to-end encrypted between member keys).
- The mesh does not use `HTTPS_PROXY`. Where the only way out is an HTTP proxy, run a company relay
  reachable inside the network, or use `"disabled"` over the VPN.
- Vectors are fetched separately, with curl: see [vectors](vectors.md) for `BORG_VECTOR_URL` and proxies.

## Admin, once

Inside a repo that routes to the team collective (check with `borg collective .`), or with
`--collective NAME`:

```sh
borg mesh init                      # you become the collective's admin
borg mesh serve                     # keep it running: syncs every 60 s and on local change
```

`borg setup --team NAME --paths DIR... [--relay URL|disabled]` does this in one pass: it writes the
team entry (asking for off paths and the mode), runs `borg mesh init` and offers a first invite (with
`--relay disabled`, an invite needs `borg mesh serve` running first, so it says that instead).

Keep `serve` running as a service. A systemd user unit, for example
`~/.config/systemd/user/borg-mesh-acme.service`:

```ini
[Unit]
Description=borg mesh for the acme collective

[Service]
ExecStart=%h/.cargo/bin/borg mesh serve --collective acme
Restart=on-failure

[Install]
WantedBy=default.target
```

then `systemctl --user enable --now borg-mesh-acme`. On macOS use a launchd agent with the same
command. Show the engineer the unit before enabling it.

## Admin, per new member

```sh
borg mesh invite                                         # QR code and a BORG1... ticket; role member, 1 use, 7 days
borg mesh invite --role steer --expires 2d --uses 1 --png invite.png
borg mesh invites                                        # open tickets
borg mesh invites --revoke <id>                          # withdraw one
borg mesh revoke <key>                                   # remove a member
```

Roles: `member`, `steer` (may also pause, resume and veto other members' sessions, as the admin may),
`observe` (may not steer). Anyone holding an unused ticket can join,
so it goes through a private channel. The admin's `serve` must be running while the member joins.

## Joiner

```sh
borg mesh join BORG1... --paths ~/work            # dry run: shows the collectives.json change
borg mesh join BORG1... --paths ~/work --apply    # writes it (with a backup), dials the admin, joins, syncs once
```

`borg setup --join BORG1... --paths DIR...` does the same. `--paths` names the engineer's repo
directories for the team collective; without it the join writes the placeholder path
`~/CHANGE-ME/repos-of-<name>`, which routes no repo: replace it (show the diff first), or join again
with `--paths`. Then check with `borg collective <repo>`: it must name the team collective with `may sync: true`.
Then keep `borg mesh serve --collective <name>` running, as a service like the admin's.

## Other commands

`borg mesh members` (the signed member list), `borg mesh id` (this node's key), `borg mesh sync <key>`
(once), `borg mesh peer [<key>[@<ip:port>]]` (peers this node dials, or add one), `borg mesh --help`.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `join` hangs, then says the admin did not answer | The admin's `borg mesh serve` is not running, or the two sides use different `relay` settings |
| `the invite was already used` / `has expired` | Ask the admin for a new ticket |
| `no relay connection yet` at `serve` start | The machine cannot reach the relay (offline, firewall, wrong URL). LAN peers still sync. Behind a re-signing proxy, its CA must be in the system trust store |
| `no direct address ... relays are disabled` | With `"relay": "disabled"`, give peers as `<key>@<ip:port>` (`borg mesh peer <key>@<ip:port>`) |
| `collective "x" is not in collectives.json` | Run `join` with `--apply`, or add the entry by hand |
| Nothing syncs after joining | The placeholder path is still there, or `serve` is not running; `borg collective <repo>` must say `may sync: true` |
| A revoked member cannot rejoin with an old ticket | By design: the admin re-invites the key (`borg mesh invite <key>`) |
