# Borg

Borg is a shared memory for coding-agent sessions. It learns from the corrections engineers give their
agents and brings each lesson back when a later session, yours or a teammate's, does similar work.
It runs as hooks inside Claude Code, and as extensions inside omp (oh-my-pi) and opencode. Team members
share lessons machine to machine, without a server.

Status: version 0.1.0, early. One engineer has used it on one machine so far; see
[Evidence and known limits](#evidence-and-known-limits) before relying on it.

## Contents

- [What it does in a session](#what-it-does-in-a-session)
- [Requirements](#requirements)
- [Install](#install)
- [Configure collectives](#configure-collectives)
- [Wire the hooks](#wire-the-hooks)
- [Import what you already have](#import-what-you-already-have)
- [Vectors](#vectors)
- [Team sync over the mesh](#team-sync-over-the-mesh)
- [Daily commands](#daily-commands)
- [Environment variables](#environment-variables)
- [Cost](#cost)
- [Privacy](#privacy)
- [Failure behaviour](#failure-behaviour)
- [Upgrade and uninstall](#upgrade-and-uninstall)
- [Evidence and known limits](#evidence-and-known-limits)
- [Development](#development)

## What it does in a session

1. **It learns from corrections.** Borg records an *episode* when you correct the agent. An episode is a
   prompt that pushes back ("no, don't add another flag, use a channel") or a tool call you rejected with
   a reason. It holds the task, the agent's proposal and your correction. When the session ends,
   `borg learn` makes one model call per new episode and turns it into a *lesson*:
   - a **question** the agent should ask itself ("Is any check-then-act on this flag safe across threads?");
   - the **situations** in which it applies;
   - the **consequence** of getting it wrong.

   A lesson can be scoped to a system or component. A new correction on the same point updates the
   existing lesson instead of adding a duplicate.
2. **Early delivery, free.** The first time a prompt or a touched file names a lesson's topic, Borg adds
   that topic's top lessons to the agent's context. This uses word matching only, with no model call.
   Claude Code only.
3. **One graded check at the end of a turn.** When the agent finishes a turn, a free word-matching gate
   decides whether the turn could touch any lesson. Most turns stop here and cost nothing. Otherwise Borg
   makes one model call with the turn and every eligible lesson. The model grades each lesson that
   applies: 3 = certain, 2 = likely, 1 = worth a thought.
   - **A grade-3 lesson** blocks the stop. The agent gets the questions and does one more round of work.
   - **Grades 2 and 1** do not block. They go to the agent for free with the next prompt or file touch
     (Claude Code only).
4. **Usefulness is judged by meaning.** At a later turn end of the same session, the same model call
   also judges whether the agent changed its work on the lesson's point. No extra call is made. A lesson
   that helped gains weight. A lesson that is ignored or still needs a correction loses weight.
5. **Memory strengthens and fades.** Lessons that keep helping get stronger and decay more slowly. A lesson
   unused for about 54 sessions of its collective goes dormant: it comes back only on a strong match and
   is never deleted. Time counts in sessions, not days, so a holiday erases nothing.
6. **System gate.** When the repo you work in maps to a known system, lessons scoped only to other
   systems are not delivered, even if the conversation names those systems. Put your mapping in
   `~/.config/borg/systems.json` (or `BORG_SYSTEMS`), in the format of the example
   [`crates/borg/data/systems.json`](crates/borg/data/systems.json), which applies when you have none.
7. **Collectives.** Each repo path belongs to one collective, or to none:
   - **team**: shared with the collective's members over the mesh;
   - **private** (`"share": "local"`): stays on this machine;
   - **off**: Borg does nothing there and leaves no trace.

   A session only reads its own collective's lessons. Nothing crosses between collectives.
8. **Peer mesh.** Members of a team collective sync its lessons directly, peer to peer (QUIC, signed
   membership, no central server). `borg top` shows the drones (agent sessions) running across the
   team, and `borg steer` sends them a signed instruction.

Also included:

- **Reasoning notes.** When a session commits, Borg keeps a redacted excerpt of the agent's reasoning as a
  git note (`refs/notes/borg`, never pushed). `borg why <file>:<line>` shows it on demand.
- **Expert repos.** A collective can name local checkouts of libraries it depends on. At a turn end that
  mentions such a dependency, Borg adds at most three cited claims from that checkout, at the version the
  repo pins. Configured per collective (`experts`, see `crates/borg/src/expert_route.rs`).
- **The queen.** A drone can ask the engineer a high-stakes open question. **Off by default** (it failed
  its test, see below).

## Requirements

- **Rust** 1.88 or newer (edition 2024; developed on 1.94). Install with [rustup](https://rustup.rs).
- **Linux or macOS.** The code uses Unix process APIs and has been run on Linux only.
- **At least one harness:**
  - [Claude Code](https://docs.anthropic.com/en/docs/claude-code), with every feature;
  - omp (oh-my-pi, tested with 18.8.5) and/or opencode. These get the turn-end check, learning and steering,
    but no early or free delivery. See [`adapters/omp/README.md`](adapters/omp/README.md) and
    [`adapters/opencode/README.md`](adapters/opencode/README.md).
- **The `claude` CLI on `PATH`, logged in.** Every model step (`borg learn`, the turn-end check,
  `borg assimilate`) runs `claude -p` (Sonnet by default) and is billed to that account. Without it Borg
  still records episodes and does free word-matching delivery, but learns nothing new and skips the
  turn-end check.
- **Recommended:** the vectors model (Model2Vec potion-multilingual-128M, about 490 MB on disk) for better
  recall on paraphrased and German lessons. See [Vectors](#vectors).
- **Optional:** Node 23.6+ or bun, only to run the adapter tests.

## Install

```sh
git clone https://github.com/debe/borg_ai borg && cd borg
cargo install --path crates/borg --locked                      # the borg CLI and hook binary, vectors included
cargo install --path crates/borg-mesh --locked                 # only for team sync, borg top across machines, borg steer
# a lean build without vectors (BM25 only):
cargo install --path crates/borg --locked --no-default-features
```

This puts `borg` (and `borg-mesh`) into `~/.cargo/bin`. `borg mesh ...` and `borg steer ...` run the
`borg-mesh` binary found next to `borg` or on `PATH`.

The hooks call the binary by its absolute path, so the binary must stay where it was when you ran
`borg install`.

## Configure collectives

Do this **before** wiring the hooks. Without a config file every repo belongs to one collective named
`default`, which is a team collective. A mesh would share it as soon as you set one up.

Create `~/.config/borg/collectives.json` (or point `BORG_CONFIG` at another file):

```json
{
  "default": "off",
  "off": ["~/work/payroll-secrets", "~/personal/journal"],
  "collectives": [
    { "name": "platform-team", "paths": ["~/work"], "share": "team" },
    { "name": "private", "paths": ["~/personal"], "share": "local" }
  ],
  "engineers": [
    { "name": "alex", "emails": ["alex.doe@example.com", "alex@home.example"] }
  ]
}
```

Rules, in order:

1. A path under `off` wins over everything.
2. Otherwise the collective with the longest matching path wins.
3. Otherwise `default` applies: `"off"` or the name of a collective.

Paths match whole path components, so `~/work` does not match `~/work-old`. `share` is `"team"` (the
default) or `"local"`. `engineers` is optional: it merges one person's git emails into one author for
trust, `borg status` and `borg top`. It is local and never synced. `relay` (optional) sets how the
mesh reaches the collective's members across NAT; see [How peers find each other](#how-peers-find-each-other).

An invalid config (unreadable JSON, two collectives that resolve to the same store, an email listed under
two engineers) routes **everything off**. `borg doctor` reports it.

Check where a directory goes:

```sh
borg collective ~/work/some-repo     # -> platform-team (Team, may sync: true) store ~/.local/share/borg/collectives/platform-team
borg collective ~/personal/notes     # -> private (Local, may sync: false) ...
```

Each collective's data lives in `~/.local/share/borg/collectives/<name>/` (`BORG_HOME` moves the root):
`episodes.jsonl`, `lessons.jsonl`, `outcomes.jsonl` and, if used, `vectors/`, `mesh/`, `queen/` and
`presence/`. Per-session scratch state goes to `/tmp/borg` (`BORG_STATE`).

## Wire the hooks

`borg install` edits your **user** harness config. The default is a dry run that prints the exact diff:

```sh
borg install all              # dry run: shows what would change for claude, omp and opencode
borg install claude --apply   # writes it; a timestamped backup is kept next to every changed file
borg doctor                   # checks the wiring, the config, the claude CLI and the stores
```

What it writes:

- **Claude Code:** one hook per event in `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR`):
  `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop` (90 s timeout) and
  `SessionEnd`. Each runs `<path to borg> hook claude <event>`. Your other keys and hooks are kept as they
  are.
- **omp:** the adapter is copied to `~/.omp/agent/extensions/borg.ts`.
- **opencode:** the adapter is copied to `~/.config/opencode/plugins/borg.ts`.

The adapters are compiled into the binary, so an installed `borg` needs no checkout. Running `install`
again changes nothing. `--home DIR` installs into another home directory, for testing.

## Import what you already have

**Past sessions.** Mine your existing Claude Code and omp transcripts for corrections:

```sh
borg assimilate                         # dry run: what it found, per collective; no model calls, writes nothing
borg assimilate --apply --limit 50      # store the newest 50 new episodes and learn them (model calls)
borg assimilate --apply --since 2026-09-01
```

The defaults read `~/.claude/projects` and `~/.omp/agent/sessions` (`--claude DIR`, `--omp DIR` change
that). Each episode is routed by the directory it happened in, so off repos contribute nothing. Running
it again adds nothing twice. `BORG_ASSIMILATE_SKIP=word1,word2` drops every episode that mentions one
of those words, for example an abandoned project's name.

**Team documents.** Turn a git checkout of written team knowledge into scoped lessons:

```sh
borg assimilate --from-dir ~/work/team-handbook --plan /tmp/plan.json   # model calls, writes only the plan
borg assimilate --from-dir ~/work/team-handbook --apply --plan /tmp/plan.json   # applies exactly that plan, no model calls
```

It reads tracked Markdown files. The default globs fit a team-handbook layout: `skills/*-knowledge/...`,
`docs/*retrospectives/*.md`, `docs/**/*coverage-audit*.md`, and decision markers in `docs/**/*.md`.
Change them with `--knowledge`, `--retros`, `--families` and `--decisions`. Lessons go to the collective
the directory routes to, unless you pass `--collective NAME` or `--for PATH`. Open decisions go to the
queen's queue, not into lessons. **The dry run does call the model** (one call per chunk, 4 in
parallel); review the plan, then apply it.

## Vectors

Vectors add lessons that share no rare word with the work: paraphrases, German work against English
lessons, renamed identifiers. They only add to the turn-end check. Nothing else loads them.

```sh
cargo install --path crates/borg --locked
borg vectors --fetch      # downloads the model once (~490 MB, pinned revision, sha256 checked)
borg vectors --rebuild    # builds the index of the store the current directory routes to
borg vectors              # status of the model and the index
```

The index then updates itself after `borg learn` and `borg assimilate --apply`. The model is never
downloaded inside a hook. If the model or index is missing or locked, Borg uses word matching only.

## Team sync over the mesh

Members sync peer to peer, also between laptops behind different NATs (home office, hotel, phone
hotspot). Private collectives and off repos are refused before any connection opens.

**1. Admin, once.** Inside a repo of the team collective (or with `--collective NAME`):

```sh
borg mesh init                         # you become the collective's admin
borg mesh serve                        # keep it running; syncs every 60 s and on local change
```

**2. Admin, per new member.** In a second terminal:

```sh
borg mesh invite                       # prints a QR code and a ticket string
borg mesh invite --role steer --expires 2d --uses 1 --png invite.png
```

Send the ticket privately (chat, password manager) or let the member scan the QR code. Whoever holds
an unused ticket can join with its role. Defaults: role `member`, one use, valid 7 days. Roles:
`member`, `steer` (may also steer drones), `observe`. `borg mesh invites` lists open tickets,
`borg mesh invites --revoke <id>` withdraws one.

**3. New member.** Install Borg, then:

```sh
borg mesh join BORG1...                # dry run: shows the collectives.json change, writes nothing
borg mesh join BORG1... --apply        # writes it (with a backup), dials the admin, joins, syncs
```

`join` adds the collective to your `collectives.json` with the placeholder path
`~/CHANGE-ME/repos-of-<name>`. No repo is routed into the team collective until you replace it with
your own repo paths. Then keep `borg mesh serve` running in one of those repos (or
`borg mesh serve --collective <name>`). Done: lessons now flow both ways.

The admin's `serve` must be running while the member joins. The admin's node checks the ticket's
signature, expiry and remaining uses, adds the member's key to the signed membership with the
ticket's role and hands over the history. Spent, expired and withdrawn tickets are refused. A revoked
key cannot rejoin with an old ticket; an admin must re-invite it by key. Other members learn of the
new member at their next sync.

Other commands: `borg mesh members`, `borg mesh revoke <key>` (admin), `borg mesh id`,
`borg mesh sync <key>` (once), `borg mesh peer [<key>[@<ip:port>]]`. The older manual flow still
works: `borg mesh invite <key>` and `borg mesh join membership-history.jsonl`.

### How peers find each other

Peers are addressed by node id (their public key), not by IP address. iroh connects them directly
where it can. Behind NAT it punches a hole through both routers. Where that fails (for example
symmetric NAT or blocked UDP), traffic goes through a relay. Pick the relay per collective in
`collectives.json` (`BORG_RELAY` overrides it for every collective):

| `"relay"` | Reaches | Who sees metadata |
|---|---|---|
| absent or `"default"` | anywhere | n0's public relays and n0's address lookup (DNS/pkarr) |
| `"https://relay.example.com"` | anywhere both can reach that host | your own relay only |
| `"disabled"` | LAN or VPN only (direct `ip:port`) | nobody outside the network |

```json
{ "name": "platform-team", "paths": ["~/work"], "share": "team", "relay": "https://relay.example.com" }
```

A company relay is the `iroh-relay` binary (`cargo install iroh-relay --features server`) on a
host with a public DNS name and a TLS certificate; see the iroh-relay documentation. Every member
of the collective must use the same setting. `join` copies the admin's setting into the new
member's config.

`ip:port` still works as an optional hint (`borg mesh peer <key>@<ip:port>`, `sync <key>@<ip:port>`).
With `"disabled"` it is required, because no relay or lookup can find the peer.

### Privacy on the mesh

Traffic between members is end-to-end encrypted (QUIC with TLS). Each end authenticates the other by
its key, and Borg then checks that key against the signed membership. A relay forwards packets it
cannot read. It cannot join or inject data: it has no member key.

A **public relay** (the default) and n0's address lookup do see metadata:

- your node id (a per-collective key) and your public IP address;
- which node ids exchange traffic, when, and how much (packet sizes and timing);
- the relay URL each node is reachable through (published to n0's DNS under your node id, so anyone
  who knows your node id can look up that relay; no IP addresses are published there).

They see no lesson, member list, repo name or email. To keep even this metadata in-house, run a
company relay, or set `"disabled"` on a LAN or VPN. The default and self-hosted settings also ask
your router to open a port (UPnP, NAT-PMP, PCP); `"disabled"` does not.

### Troubleshooting

- **`join` hangs, then says the admin did not answer:** the admin's `borg mesh serve` is not running,
  or both sides use different relay settings. Compare the `relay` entries in both
  `collectives.json` files.
- **`the invite was already used` / `has expired`:** ask the admin for a new ticket
  (`borg mesh invite`).
- **`no relay connection yet` at `serve` start:** the machine cannot reach the relay (offline, a
  firewall blocking outbound HTTPS, or a wrong relay URL). LAN peers still sync directly.
- **`no direct address ... relays are disabled`:** with `"relay": "disabled"`, give peers as
  `<key>@<ip:port>`.
- **`collective "x" is not in collectives.json`:** run `join` with `--apply`, or add the entry by
  hand.
- **Nothing syncs after joining:** check that you replaced the placeholder path and that
  `borg collective <repo>` names the team collective with `may sync: true`.

### Testing across real NATs (manual)

The automated tests cover tickets, the join on localhost, and a join and sync between two nodes that
have no direct address and can only use a local relay. They cannot test real NAT traversal across the
internet. To check it by hand:

1. Machine A on your home or office network: `borg mesh init`, then `borg mesh serve`. Note the
   `reachable at` line; it should list a relay URL.
2. Machine B on another network (a phone hotspot is easiest, since it is a different NAT, often
   carrier-grade): `borg mesh join <ticket> --apply` with a ticket from A's `borg mesh invite`.
3. Expect `joined <name> ... first sync: ... received N`. Run `borg mesh serve` on B, add a lesson on
   either side, and see `synced` lines on both within a minute.
4. Negative check: on B, run `BORG_RELAY=disabled borg mesh sync <key of A>@<A's LAN ip:port>`. It
   must fail, because A's private address is unreachable from the hotspot. This shows that the relay
   or hole punching made step 3 work.

## Daily commands

| Command | What it shows or does |
|---|---|
| `borg status` | Each collective's store (lessons, episodes, outcomes) and what is wired |
| `borg doctor` | What is broken: config, hooks, adapters, `claude` CLI, `borg` on `PATH` |
| `borg collective [DIR]` | Which collective a directory belongs to and whether it may sync |
| `borg learn [--session ID]` | Turn waiting episodes into lessons now (normally runs at session end) |
| `borg memory <lessons.jsonl>` | Each lesson's activation, usefulness, weight, and consolidated or dormant state |
| `borg top [--watch] [--json]` | The running drones of each collective, local and remote |
| `borg steer <drone\|session\|all> "<text>"` | Send a signed instruction to running drones; `--pause` / `--resume` |
| `borg why <path>[:<line>[-<line>]] \| <commit>` | The reasoning behind code: blame, Borg notes, linked lessons |
| `borg notes attach [<commit>]` | Write the redacted reasoning note for a session's commits (runs at session end) |
| `borg notes fetch [<commit>...]` | Get reasoning notes from team members over the mesh |
| `borg trust [--dir DIR]` | Trust per engineer and area, from what happened after their lessons were asked |
| `borg queen` / `borg queen answer <id> "<text>"` | Open questions to the engineer; an answer becomes a lesson |
| `borg vectors [--fetch\|--rebuild]` | Vector model and index |
| `borg mesh <id\|init\|invite\|invites\|join\|revoke\|members\|peer\|serve\|sync>` | Team sync, see above |
| `borg assimilate ...` | Import past sessions or team documents, see above |
| `borg install\|uninstall <claude\|omp\|opencode\|all> [--apply]` | Wire or unwire the hooks |

Commands for debugging and evaluation: `borg recall`, `borg prefilter`, `borg resolve`, `borg expert`,
`borg facets`. Their usage is in the header of [`crates/borg/src/main.rs`](crates/borg/src/main.rs).

## Environment variables

All are optional. Experimental switches are off by default. Most of them failed a pre-registered test
and are kept for further experiments.

| Variable | Default | Effect |
|---|---|---|
| `BORG_CONFIG` | `~/.config/borg/collectives.json` | The collectives config |
| `BORG_HOME` | `~/.local/share/borg` | Root of the stores |
| `BORG_STATE` | `/tmp/borg` | Per-session scratch state |
| `BORG_RELAY` | the collective's `relay`, else `default` | Mesh relay: `default` (n0), `disabled`, or relay URL(s) |
| `BORG_BIN` | `borg` on `PATH` | The binary the omp and opencode adapters run |
| `BORG_LEARN` | on | `off`: record no episodes or outcomes (for eval drones) |
| `BORG_LEARN_MODEL` | `sonnet` | Model for `borg learn` and `borg assimilate` |
| `BORG_ASSIMILATE_JOBS` | `4` | Parallel model calls in `assimilate --from-dir` |
| `BORG_ASSIMILATE_SKIP` | none | Comma-separated words; episodes and documents that mention one are not imported |
| `BORG_EARLY` | on | `off`: no early delivery |
| `BORG_SYSTEM_GATE` | on | `off`: no system gate |
| `BORG_SYSTEMS` | `systems.json` next to `BORG_CONFIG` | Your system map for the gate; the shipped example applies when the file is absent |
| `BORG_MATCH` | model | `bm25`: word matching alone at the turn end, no model call |
| `BORG_PREFILTER` | `8.0` | Threshold of the free turn-end gate; a number overrides it, `off` always calls the model |
| `BORG_STOP_GATE` | lexical gate | `edits`: call the model only for turns that edited files or ran a state-changing command |
| `BORG_STOP_POOL` | whole pool | `shortlist`: show the model only the word-matching shortlist |
| `BORG_STOP_POOL_TOKENS` | `12000` | Token budget of the lesson pool in the turn-end call |
| `BORG_STOP_FORMAT` | compact | `classic`: the older prompt layout |
| `BORG_STOP_GRADED` | on | `off`: no grades; any applying lesson blocks |
| `BORG_PREDICT_MODEL` | `sonnet` | Model for the turn-end check |
| `BORG_PREDICT_TIMEOUT_S` | `45` | Time limit of the turn-end model call |
| `BORG_STOP_BUDGET_MS` | 60 s | Stop-hook time budget when the harness gives none (the adapters set it) |
| `BORG_CONSCIENCE` | the routed store's `lessons.jsonl` | Read lessons from another file |
| `BORG_VECTORS` | on (default build) | `off`: no vector recall |
| `BORG_VECTOR_MODEL` | `~/.local/share/borg/models/potion-multilingual-128M` | Model directory |
| `BORG_VECTOR_K` | `10` | Vector hits added to the shortlist |
| `BORG_VECTOR_URL` | Hugging Face | Mirror for `borg vectors --fetch` |
| `BORG_EXPERT_MODEL` | `sonnet` | Model of the optional expert drone |
| `BORG_QUEEN` | off | **Experimental.** `on`: the turn end may ask the engineer (failed its test) |
| `BORG_CONCEPTS` | off | **Experimental.** `on`: boost lessons that share engineering concepts with the work; `BORG_CONCEPTS_BETA` (0.5) sets the strength |
| `BORG_FACETS` | off | **Experimental.** `on`: `learn` and `assimilate` write facets (failed its test) |

Evaluation only: `BORG_BEFORE`, `BORG_CRITIC`, `BORG_ONLY`, `BORG_EXPERT_CEILING`, `BORG_VECTORS_TRACE`.

## Cost

- **Most turn ends cost 0 tokens.** On the author's own sessions the free gate skipped 78% of turn ends.
  Early delivery, and the grade-2 and grade-1
  lessons that go free with the next prompt, never call a model.
- **A turn-end model call costs about $0.07** (Sonnet, mean $0.067 over 67 calls) and takes about 10 s.
- **An extra round** of agent work happens only when a grade-3 lesson applies. That round is billed by
  your harness as normal agent work.
- **Learning** is one model call per new correction, about $0.02 each (21 lessons from 10 sessions cost
  $0.66).
- `assimilate --from-dir` makes one call per document chunk, and prints each call's cost.

## Privacy

**Never leaves the machine:**

- anything from an **off** repo (Borg writes nothing there);
- anything in a **private** (`local`) collective;
- the raw episodes (your exact corrections and the session's task), session transcripts, source code,
  `collectives.json` and the per-session state.

**Sent to team members over the mesh, for team collectives only:**

- **Lessons**: question, situations, consequence, scope, the authors' git emails and repo names.
- **Reasoning excerpts** of landed commits: the task and the agent's last proposal, **redacted first**.
  Each excerpt goes to two member replicas.
- **Presence** for `borg top`: session id, harness, git email, repo name, collective, the first line of
  your latest prompt (redacted), recently touched file paths and timestamps.
- **Steering instructions** and the signed membership list.

**Seen by a mesh relay** (n0's public relays by default): node ids, IP addresses and traffic volume,
never content. See [Privacy on the mesh](#privacy-on-the-mesh).

**Redaction** (`crates/borg/src/notes.rs`, `redact`) replaces tokens, keys, passwords, private-key
blocks and `.env`-style values with a conservative regex list before an excerpt or presence line is
written. Lessons are not passed through it. A lesson is a generalised question written by the model,
but check `lessons.jsonl` before you share a collective that worked with secrets.

**Sent to the model provider:** every model step runs the `claude` CLI under your account. The turn-end
check sends the turn (prompt, touched paths, the agent's final message) and the eligible lessons.
`borg learn` sends the episode.

## Failure behaviour

Hooks **fail open**. Any error lets the agent continue untouched: no binary, a crash, an invalid config, an
unreachable model, a locked index. The turn-end model call has a 45 s limit (`BORG_PREDICT_TIMEOUT_S`),
under Claude Code's hook timeout (Borg's Stop hook gets 90 s). If the call fails or times out, the turn
ends without questions. When `borg learn` gets no answer, the episodes stay waiting for the next run. The omp
extension answers a pre-tool call within 2 s or lets it run.

## Upgrade and uninstall

**Upgrade:**

```sh
git pull
cargo install --path crates/borg --locked --force        # vectors are built in by default
cargo install --path crates/borg-mesh --locked --force   # if installed
borg install all              # dry run: refreshes omp/opencode adapter copies if they changed
borg install all --apply      # only if the dry run shows a diff
borg doctor
```

Claude Code hooks call the binary by path, so a reinstall to the same path needs no rewiring. Stores are
append-only JSONL. There is no migration step.

**Uninstall:**

```sh
borg uninstall all            # dry run
borg uninstall all --apply    # removes only what install added; a backup is kept
cargo uninstall borg borg-mesh
```

Uninstall removes an omp or opencode adapter file only when its content matches this binary's adapter.
An older or edited copy is left in place and reported; delete it by hand.

Uninstall leaves your data in place. To delete it, remove `~/.local/share/borg` (stores, vectors model,
mesh keys) and `~/.config/borg`. Reasoning notes stay in each repo under `refs/notes/borg`; delete them
with `git update-ref -d refs/notes/borg`.

## Evidence and known limits

Each change to the turn-end defaults was tested against a rule written before the paid run. Most of
those tests ran on the author's own sessions and code, so their data and reports are not part of this
repository. The main results:

- The turn-end check (whole pool, graded) gets 0.64 of relevant lessons to the agent, against 0.45 for the
  earlier shortlist (+0.17 [+0.09, +0.25]). It blocks on 3 of 16 negative cases and asks 12 false lessons
  there.
- The free gate skips 78% of real turn ends. It kept every labelled grade-2 recall, but only on first-turn,
  brief-sized checkpoints; recall on short later turns is unknown.
- Learning stores few items, and a blind judge found all of them useful: 21 lessons from 10 sessions, all
  21 useful (95% CI 84.5–100%), for $0.66. A comparison memory system stored 543 items, 31% useful.
- Against Hindsight with the same memory model, accuracy ties (no significant difference). With
  Sonnet-built lessons, Borg improved the lesson cases by +29 points (significant). Recall takes 0.16 s at
  10k lessons.
- The system gate cut early deliveries on a real cross-system session from 83 to 0 and dropped 0 relevant
  lessons from labelled queries.
- Vectors raise turn-end shortlist recall by +0.09 overall and +0.14 on German work, for about 120 ms.

On public data (SWE-chat, third-party Claude Code sessions), no memory, Borg included, measurably helped
raise the point a user was about to correct, and Borg's surfaced lessons lost to a plain log of earlier
corrections: [swe-chat.md](reports/swe-chat.md), [swe-chat-engineering.md](reports/swe-chat-engineering.md),
[swe-chat-engineering-transfer.md](reports/swe-chat-engineering-transfer.md).

An end-to-end run in a sandboxed home passed all 18 plumbing checks (install, routing, learning, delivery,
uninstall). It did **not** show that Borg improved the agent's plan on that task: the agent found the fix
without it ([dogfood-e2e.md](reports/dogfood-e2e.md)).

Known limits:

- One engineer on one machine so far. No team has used the mesh in daily work.
- Small samples: 16 negative and 51 positive cases for the turn-end rule, 10 sessions for memory quality.
  The confidence intervals are wide.
- The queen is off. It failed its pre-registered bar.
- The mesh across real NATs is untested. NAT traversal is iroh hole punching with relay fallback; the
  tests cover a relay-only path on localhost (see [Testing across real NATs](#testing-across-real-nats-manual)).
  Invite tickets and `join` have not been used by a team yet.
- omp and opencode get the turn-end check only. Early delivery and free delivery of grade-2 and grade-1
  lessons are Claude Code only.
- Lessons are learned from corrections only. Knowledge nobody corrected never becomes a lesson unless you
  import it with `assimilate --from-dir`.

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo test -p borg --no-default-features   # the lean build without vectors
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all                               # rustfmt.toml: max_width 160
node --test adapters/omp/borg.test.ts         # adapter contract tests; need target/debug/borg (Node 23.6+ or bun)
node --test adapters/opencode/borg.test.ts
scripts/check.sh                              # all of the above; run before a release
```

Workspace layout:

| Path | What |
|---|---|
| `crates/borg` | The product: CLI, hooks, learning, the turn-end check, memory, collectives, install, vectors (feature `vectors`) |
| `crates/borg-mesh` | Peer-to-peer sync, membership, presence and steering (iroh). A separate binary so the hook stays small |
| `crates/borg-trace` | Parses Claude Code and omp transcripts |
| `crates/borg-eval` | Eval harness: sandboxed drone runs, replays, dogfood (not shipped) |
| `crates/borg-search` | Eval only: retrieval benchmarks (`publish = false`) |
| `adapters/omp`, `adapters/opencode` | TypeScript adapters, compiled into `borg` by `borg install` |
| `eval/` | Eval scripts (Python, shell) |
| `reports/` | Measured results |

**Eval tooling uses local paths.** The scripts in `eval/` and `borg-eval` read and write eval data under
`~/.local/share/borg/eval/` and `target/`. They are for reproducing reports, not for daily use.

License: MIT OR Apache-2.0 ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
