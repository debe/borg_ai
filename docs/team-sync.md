# Team sync

[← README](../README.md)

Members of a team collective share its lessons machine to machine, with no server, across NATs (home
office, hotel, phone hotspot). Each machine runs `borg mesh serve`, a peer like every other. Hooks read
and write only local files, so agents never wait for the mesh, and Borg keeps working offline. The mesh
never calls a model.

![Machines linked peer to peer around shared lessons](img/team.svg)

- [Set up a team](#set-up-a-team)
- [How a sync works](#how-a-sync-works)
- [How peers find each other](#how-peers-find-each-other)
- [Privacy on the mesh](#privacy-on-the-mesh)
- [Troubleshooting](#troubleshooting)
- [Testing across real NATs](#testing-across-real-nats)

## Set up a team

Install `borg-mesh` next to `borg` (see the [README](../README.md#quick-start)). Private collectives and
off repos are refused before any connection opens.

**1. Admin, once.** Inside a repo of the team collective (or with `--collective NAME`):

```sh
borg mesh init      # you become the collective's admin
borg mesh serve     # keep it running; syncs every 60 s and on local change
```

**2. Admin, per new member.** In a second terminal:

```sh
borg mesh invite                                            # prints a QR code and a ticket
borg mesh invite --role steer --expires 2d --uses 1 --png invite.png
```

Send the ticket privately (chat, password manager) or let the member scan the QR code: anyone holding
an unused ticket can join with its role. Defaults: role `member`, one use, valid 7 days. Roles are
`member`, `steer` (may also steer drones) and `observe`. `borg mesh invites` lists open tickets;
`borg mesh invites --revoke <id>` withdraws one.

**3. New member.** Install Borg, then:

```sh
borg mesh join BORG1...            # dry run: shows the collectives.json change
borg mesh join BORG1... --apply    # writes it (with a backup), dials the admin, joins, syncs
```

`join` adds the collective with the placeholder path `~/CHANGE-ME/repos-of-<name>`. Replace it with your
repo paths; until then no repo routes into the team collective. Then keep `borg mesh serve` running in
one of those repos (or with `--collective <name>`).

The admin's `serve` must run while the member joins. It checks the ticket's signature, expiry and
remaining uses, adds the member's key to the signed membership and hands over the history. Other
members learn of the newcomer at their next sync. Spent, expired and withdrawn tickets fail. A revoked
key cannot rejoin with an old ticket; an admin must re-invite it by key.

**Other commands:** `borg mesh members`, `borg mesh revoke <key>` (admin), `borg mesh id`,
`borg mesh sync <key>` (once), `borg mesh peer [<key>[@<ip:port>]]`. The older manual flow still works:
`borg mesh invite <key>` and `borg mesh join membership-history.jsonl`.

## How a sync works

![One sync session between two peers in six steps](img/mesh-sync.svg)

Each peer dials the members it knows every 60 seconds, and at once after a local change. Whatever B
learned from A reaches C in B's next round.

**Membership.** Each machine holds one ed25519 key per collective; the key is also its network address,
so a connection proves who is on the other end. The member list is a JSON document signed by an admin.
A peer accepts a new version only if an admin of the version it holds signed it, so a stranger cannot
add themselves, and two admins editing at once resolve the same way everywhere.

**Reconciliation.** A blake3 hash names each lesson line, so peers compare hashes, not contents. Both
sides split the hash space into 16 ranges and swap one fingerprint per range; differing ranges split
again until a small range lists its hashes. Then only missing lines travel. Merging is a union of
append-only files, so nothing is overwritten; when two machines improved the same lesson, the higher
version wins everywhere. Cost grows with what changed: a laptop back from three weeks' vacation
downloads three weeks of lessons.

**What travels.** Lessons, each line signed by the machine that wrote it. Raw episodes stay where they
were captured, so each is learned exactly once. Reasoning excerpts go to two peers chosen by hash, so
the "why" survives its author going offline.

**Revocation.** `borg mesh revoke <key>` re-signs the member list. Every peer that has it refuses the key
and drops lines it stamped after the revocation; earlier contributions stay. Steering directives are
checked against the same list.

## How peers find each other

Peers are addressed by node id (their public key), not by IP. iroh connects them directly where it can
and punches through NAT on both sides. Where that fails (symmetric NAT, blocked UDP), traffic goes
through a relay. Pick the relay per collective (`BORG_RELAY` overrides it for all):

| `"relay"` | Reaches | Who sees metadata |
|---|---|---|
| absent or `"default"` | anywhere | n0's public relays and address lookup (DNS/pkarr) |
| `"https://relay.example.com"` | anywhere both can reach that host | your relay only |
| `"disabled"` | LAN or VPN only (direct `ip:port`) | nobody outside the network |

```json
{ "name": "platform-team", "paths": ["~/work"], "share": "team", "relay": "https://relay.example.com" }
```

A company relay is the `iroh-relay` binary (`cargo install iroh-relay --features server`) on a host with
a public DNS name and a TLS certificate. Every member must use the same setting; `join` copies the
admin's.

`<key>@<ip:port>` works as a hint for `peer` and `sync`. With `"disabled"` it is required.

## Privacy on the mesh

Traffic is end-to-end encrypted (QUIC with TLS). Each end authenticates the other by key, and Borg checks
that key against the signed membership. A relay forwards packets it cannot read, and without a member key
it can neither join nor inject data.

A **public relay** (the default) and n0's address lookup see metadata:

- your node id (a per-collective key) and public IP address;
- which node ids talk, when, and how much;
- the relay URL each node uses (published to n0's DNS under your node id; no IP addresses).

They see no lesson, member list, repo name or email. To keep the metadata in-house, run a company relay,
or use `"disabled"` on a LAN or VPN. The default and self-hosted settings ask your router to open a port
(UPnP, NAT-PMP, PCP); `"disabled"` does not.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `join` hangs, then says the admin did not answer | The admin's `borg mesh serve` is not running, or the two sides use different `relay` settings |
| `the invite was already used` / `has expired` | Ask the admin for a new ticket |
| `no relay connection yet` at `serve` start | The machine cannot reach the relay (offline, firewall, wrong URL). LAN peers still sync |
| `no direct address ... relays are disabled` | With `"relay": "disabled"`, give peers as `<key>@<ip:port>` |
| `collective "x" is not in collectives.json` | Run `join` with `--apply`, or add the entry by hand |
| Nothing syncs after joining | Replace the placeholder path; `borg collective <repo>` must name the team collective with `may sync: true` |

## Testing across real NATs

The automated tests cover tickets, a join on localhost, and a join and sync between two nodes that can
reach each other only through a local relay. Real NAT traversal needs a manual test:

1. **Machine A** (home or office network): `borg mesh init`, then `borg mesh serve`. The `reachable at`
   line should list a relay URL.
2. **Machine B** on another network (a phone hotspot is a different, often carrier-grade, NAT):
   `borg mesh join <ticket> --apply` with a ticket from A's `borg mesh invite`.
3. Expect `joined <name> ... first sync: ... received N`. Run `borg mesh serve` on B, add a lesson on
   either side, and watch for `synced` lines on both within a minute.
4. **Negative check** on B: `BORG_RELAY=disabled borg mesh sync <key of A>@<A's LAN ip:port>` must fail.
   That proves the relay or hole punching carried step 3.
