# Security

## Reporting a vulnerability

Report it privately through GitHub: the repository's **Security** tab, then **Report a vulnerability**.
Do not open a public issue for it. If private reporting is not available, open an issue that asks for
a private contact and leave out the details.

Please include:

- the borg version (the release, or `cargo install --list`) and the operating system;
- which part is affected (the hooks, `borg-mesh`, `borg-web`, an adapter, the relay setup);
- the steps or a test case that shows it, and what an attacker gains.

We aim to answer within a week. Fixes go into the next release; the changelog names the issue once a
fixed release is out.

## Supported versions

Only the newest release gets security fixes.

## What counts

Borg handles three kinds of trust:

- **Your machine.** Hooks run as you, with your agent's input. A way to make borg run a command, write
  outside its stores, or read a repo routed `off` is a vulnerability.
- **The mesh.** Membership is a member list signed by an admin, every lesson line is signed by the
  machine that wrote it, and steering directives are checked against the member list. A way for a
  non-member or a revoked key to join or inject lines, or for a member without the `steer` role to
  steer drones, is a vulnerability. So is lesson or presence content reaching a relay or n0 in clear text.
- **What leaves the machine.** Raw episodes, transcripts, `local` collectives and `off` repos never sync.
  Anything that makes them travel is a vulnerability. A secret that survives `redact` in a reasoning
  excerpt or in presence is a bug worth reporting privately too.

Not in scope: what a relay sees by design (node ids, IP addresses, traffic volume; see
[Running your own relay](docs/relay.md#what-the-relay-sees)), what the model provider receives for a
model step ([Privacy](README.md#privacy)), and lessons that quote a secret the model wrote into them
(read `lessons.jsonl` before you share a collective that handled secrets).
