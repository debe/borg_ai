# Collectives

A collective is one separate memory. Every repo path routes to exactly one collective or to none
(`off`). A drone reads and writes only its own collective's store, so nothing crosses between
collectives. The mapping lives only on this machine and is never synced.

## The file

`$BORG_CONFIG`, default `~/.config/borg/collectives.json`. Without the file, every repo belongs to one
implicit collective called `default` (share: team), stored at `$BORG_HOME`. Write the file before
wiring borg if some repos must stay private.

```json
{
  "default": "off",
  "off": ["~/work/payroll-secrets", "~/personal/journal"],
  "collectives": [
    { "name": "platform-team", "paths": ["~/work"], "share": "team", "mode": "live" },
    { "name": "private", "paths": ["~/personal"], "share": "local" }
  ],
  "personal": "private",
  "engineers": [
    { "name": "alex", "emails": ["alex.doe@example.com", "alex@home.example"] }
  ]
}
```

| Key | Where | Meaning |
|---|---|---|
| `default` | top | `"off"` (the default when the file exists) or a collective's name: where repos no rule matches go |
| `off` | top | Paths no collective may ever see: the hooks do nothing there and leave no trace |
| `collectives` | top | The list of collectives |
| `personal` | top | The `local` collective that receives personal lessons; needed only with several `local` ones |
| `engineers` | top | One person's several git emails counted as one engineer (trust, status, top). Never synced |
| `name` | entry | The collective's name; its store is `$BORG_HOME/collectives/<name>/` |
| `paths` | entry | Directories whose repos belong to it; `~/` is expanded |
| `share` | entry | `"team"` (default): may sync over the mesh. `"local"`: never leaves this machine |
| `mode` | entry | `"live"` (default): checks during the turn and at its end. `"hook"`: at the turn end only, about an eighth of live's cost. `BORG_MODE` overrides it |
| `relay` | entry | Mesh only: absent or `"default"` (n0's public relays), `"disabled"` (LAN/VPN only) or a self-hosted `iroh-relay` URL. `BORG_RELAY` overrides it. See [team](team.md) |
| `experts` | entry | Local checkouts of dependencies the turn-end check may cite: `{"name", "repo", "match": [...], "drone": false}`. `"drone": true` also asks a read-only, time-bounded agent in that checkout when its index has nothing |
| `budget` | entry | Optional, absent by default: the pool tokens this collective's checks may read per session, on average. Without it lesson selection's spend is measured and shown, never priced; with it the learned policy (`BORG_SELECT=learned`) prices a larger pool to stay within it, and the fixed rule, the default, reads none. A value that is not a positive number counts as none. Set it only when the engineer asks for a spend limit: quality, not spend, is what selection optimises |

## Routing rules

In this order:

1. A path under any `off` entry wins over everything, also over a collective path above it.
2. Otherwise the collective with the **longest** matching path.
3. Otherwise `default`.

Paths match whole components: `~/work` does not match `~/work-old`. Check any repo with
`borg collective <dir>`; it prints the collective, its share, whether it may sync and its store.

## What makes the file invalid

Bad JSON, two collectives whose names resolve to one store (also differing only by case), an
`engineers` entry without a name or emails, a name listed twice, one email under two engineers, or
`personal` naming a collective that is not `"share": "local"`.
An invalid file routes **every repo off**: borg captures and asks nothing until it is fixed.
`borg doctor` and `borg status` say so. `default` naming no collective is only a warning: unmatched
repos then route off.

## Editing it for the engineer

1. Read the current file (it may not exist).
2. Write the new version to a temporary file, show the engineer the full file and a diff, and get a yes.
3. Keep every key you were not asked to change. Back up the old file (`cp collectives.json
   collectives.json.bak-<date>`), then write the new one.
4. Run `borg doctor` (the collectives lines) and `borg collective <dir>` for each repo you moved.

Renaming a collective starts an empty store under the new name; the old store stays where it was.
Moving a repo from a `team` to a `local` collective does not withdraw lessons already synced.

## Worked examples

**Solo, private by default.** Everything the engineer does stays on this machine; unmatched repos
are off.

```json
{
  "default": "off",
  "collectives": [{ "name": "private", "paths": ["~/src"], "share": "local" }]
}
```

Use `"default": "private"` instead to cover repos outside `~/src` as well.

**A team plus a private collective.** Work repos share lessons with the team; personal projects and
personal lessons (commit style, tone) stay local.

```json
{
  "default": "off",
  "off": ["~/work/client-nda"],
  "collectives": [
    { "name": "platform-team", "paths": ["~/work"], "share": "team" },
    { "name": "private", "paths": ["~/personal"], "share": "local" }
  ],
  "personal": "private"
}
```

**A company collective.** There is no separate company type: it is a `share: "team"` collective
synced over the mesh, usually with the company's own relay and confidential repos kept off. Experts
let the turn-end check cite an internal library's checkout.

```json
{
  "default": "off",
  "off": ["~/work/acme/payroll", "~/work/acme/legal"],
  "collectives": [
    {
      "name": "acme",
      "paths": ["~/work/acme"],
      "share": "team",
      "relay": "https://relay.acme.example",
      "mode": "live",
      "experts": [
        { "name": "acme-core", "repo": "~/work/acme/core", "match": ["acme-core", "com.acme.core"] }
      ]
    },
    { "name": "private", "paths": ["~/personal"], "share": "local" }
  ],
  "personal": "private"
}
```

Every member of a mesh must use the same `relay`; `borg mesh join` copies the admin's. An expert repo
under an `off` path or inside another `local` collective is never consulted.

**Joined a team with a ticket.** `borg mesh join <ticket> --paths DIR... --apply` adds the collective
with those repo directories. Without `--paths` it writes the placeholder path
`~/CHANGE-ME/repos-of-<name>`; replace it with the real repo directories (or join again with
`--paths`), since until then no repo routes into the team collective.

## Decision → fix links

At session end borg links each commit the session landed to the earlier decisions whose lines it
removed or changed: origins with borg reasoning, from another session, at least two days older
(`BORG_REGRESS_MIN_AGE`). What the share allows:

| Repo | Links in both commits' git notes | Link record synced over the mesh | Regret lesson (`BORG_REGRESS=learn`) |
|---|---|---|---|
| `"share": "team"` | yes | yes | yes |
| `"share": "local"` | yes | no | yes, kept on this machine |
| `off` | no | no | no |

`BORG_REGRESS` is `links` by default (record, learn nothing), `learn` or `off`. Show the engineer
`borg regress --replay <A..B> --explain` on their own history, and `borg regress --session ID --explain`
on a few real sessions, before they turn on `learn`: both are dry runs that list the candidates left out
and why. A replay has no test runs or prompts, so a live run can make fix links the replay never showed.
`borg why <path>:<line>` shows a link from either end.
A rebase drops notes unless the repo has `git config notes.rewriteRef refs/notes/borg`; suggest it,
and let the engineer run it.
