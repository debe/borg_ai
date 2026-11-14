# Hand-off

A session can pass its working state to another drone, the way a call is transferred. Handing off is
optional. The original session keeps working until someone takes the work, and only the drone that takes
it gets it. A hand-off carries the code and the knowledge: the working tree as a commit, and a brief the
offering agent writes. Both travel through git, so a take still works after the offering laptop is
closed.

```sh
borg handoff "parser: error paths" --brief - <<'EOF'
# Parser: error paths
## Goal
...
EOF
borg handoff                      # the hand-offs this machine knows, offered on any machine of the team
borg handoff take h3f09a1c2b7d4   # take one over; the offering engineer's take takes it back
borg handoff decline h3f09a1c2b7d4  # say no to one asked of you (`--to` on the offer)
borg handoff abort h3f09a1c2b7d4 --why "wrong base"  # close it: its owner, an admin or a steerer
borg handoff done h3f09a1c2b7d4   # finished or merged: delete its refs and the clean work tree
borg handoff off                  # an admin: hand-offs off for the collective (`on` switches them on)
```

They run inside a session, usually from its shell tool: the borg skill teaches the agent to write
the brief and run them. The session is the one whose shell runs the command, found the way `borg tell`
finds it (`--from <session>` names another). An offer from a terminal, where no session runs the
command, is refused: no session could be fenced once the work is taken.

One command, `borg handoff`, with the verbs `take`, `decline`, `abort`, `done`, `off` and `on`; without
one it lists, and with a note it offers. The words `list`, `back` and `withdraw`, and several bare words (quote
the note), are refused, so none of them is taken for an offer's note: the owner's `take` is the
take-back, and `abort` closes an offer for good. Offers, asks and outcomes reach the team's other
machines over the mesh ([Across machines](#across-machines)); the code goes through each repo's remote.
A decline, an abort or a done from a terminal is accepted: none of them needs a session to fence.

## The brief

The brief is the knowledge the taker starts from, written by the offering agent, which knows its state
best. The skill gives the outline: Goal (in the engineer's words), Where we are, the Plan (remaining
steps, checked off where done), the Next step, Decisions and why, Open questions and risks, How to
verify, and Context the taker cannot see. A plan file or todo list of the session goes in whole.

It comes from a file (`--brief <file>`) or from stdin (`--brief -`), never from the command line, which
`ps` shows and the kernel bounds. The note is one line for the listing; without one, the brief's first
line is the note. A note of several lines is refused (it belongs in the brief), and a long one is cut
as a prompt is for one row of `borg top`.

## Offer

`borg handoff ["<note>"] --brief <file>|-` checks every repo of the session, then publishes the offer.
It is all or nothing: if a check fails in any repo, nothing is left on any remote and borg says why.

0. **The repos.** The repo the command runs in comes first: its remote is the arbiter, where every
   claim is decided. Then the session's other repos: the one it started in, and those it wrote in (an
   edit's paths, or where a shell command that does not only read ran or the paths it named, as the
   hooks record after each tool call), each once, when it belongs to the same collective and holds work
   a clone would not have: changes, or commits on HEAD that no remote-tracking ref has. A repo the
   session only read stays out, whatever it holds.
1. **Each repo.** HEAD is on a branch with a commit. No rebase, merge, cherry-pick, revert or bisect is
   in progress: a half-done one cannot be handed over safely.
2. **The push remote.** The branch's `pushRemote`, `remote.pushDefault`, its upstream's remote, the only
   remote, or `origin`. Its push URL is the one every git call of the hand-off names for that repo, so a
   remote with a separate `pushurl` is written and read in the same repo. A push URL with whitespace,
   or one starting with `-`, is refused: the offer line carries it, and a listing prints it in a
   command.
3. **Snapshot.** Each working tree becomes a commit on its HEAD, built in an index file of borg's own,
   seeded from the real index (so a sparse checkout keeps the files outside its cone). The engineer's
   branch, index and stash are not touched. Tracked and untracked files go in; ignored files never do.
   An untracked file larger than git's big-file threshold (`core.bigFileThreshold`, 512 MiB unless the
   repo sets it) and larger than every file HEAD tracks is listed instead, and never read. Submodules
   go in as their commit; other files go through git's filters as a commit would store them (an LFS
   file as its pointer, a symlink as its target).
4. **Secrets.** borg reads git's diff of the snapshot against HEAD, the bytes that would be committed,
   and leaves out every change whose added lines hold a secret: an untracked file stays out, a tracked
   one goes in as HEAD has it. Both are listed. A secret counts when its shape is known (private keys,
   provider tokens, credentials in URLs and auth headers) anywhere, and, in `.env` files and dotfiles
   such as `.npmrc`, when it is assigned to a secret-named key. The guess at unlabelled random tokens
   that messages get is not used, so lock files and fixtures go in. A binary file has no lines to
   read, so a new binary (a keystore, a key in DER form, a password vault, a local database) is left
   out and listed too, unless HEAD tracks files of its extension: a repo that keeps PNGs takes a new
   PNG.
5. **The brief commit.** The brief (`HANDOFF.md`) and borg's metadata (`state.json`) become a commit on
   the first repo's snapshot. The metadata holds each repo (remote, branch, HEAD, state and tree hash,
   the files changed since HEAD, what was left out), the session,
   its harness and engineer, its task, the latest prompt, the last proposal, touched paths, test runs
   (the last one apart). borg records no model: no hook names it. The brief and every metadata text are
   redacted like messages. A take checks what it fetches against the states the metadata names.
6. **Push.** In each repo, a probe ref `refs/borg/handoff/<id>/probe` is pushed and deleted again, which
   checks that the remote takes and deletes custom refs. Then its state goes to
   `refs/borg/handoff/<id>/state`, the first repo's with the brief at `.../brief` in one
   `git push --atomic`: both or neither. Then `ls-remote` must show every one of them.
7. **The offer line** goes to `<store>/handoffs.jsonl`: each repo, its remote (credentials redacted), the
   branch, HEAD, the state commit and its tree hash; the brief commit, the note and the brief's head
   (its title and first section), the files left out, and whom it asks (`--to`). The same line goes to
   the team over the mesh ([Across machines](#across-machines)).

Pushes of the hand-off refs use `--no-verify`, so a pre-push hook that runs the tests does not block
them. Branch pushes are unchanged.

When a push fails, borg deletes what it pushed in every repo, each ref only while it still holds the
offer's commit.
If the remote refuses that delete (a hook or a hosting rule), the offer ends and borg prints the
`git push <remote> :<ref>` command to delete it by hand. If the remote cannot be reached, the next
`borg handoff` tries again.

### Refresh while it is open

The original keeps working until someone takes the work. At each stop of the offering session, while
the offer is open as far as the remote was seen saying, a `borg handoff` nobody waits for snapshots
every repo again (and so does the session's own `borg handoff`). When a tree or a HEAD moved, the new
states of the repos that moved are pushed over the ones the offer published last, each only while the
remote still holds that one (`--force-with-lease`): those repos first, then the first repo's state (when
it moved) with a new brief (the same `HANDOFF.md`, borg's metadata of now) in one atomic push. A repo
that did not move keeps its state, so the brief and its metadata name exactly what each remote holds.
Each push is read back, `taken` is asked again, and a new offer line names them. Nothing moves once
`taken` exists.

A refresh and a take can still cross. A claim pins the state of each repo it takes, and the taker
fetched every one of them before it claimed, so the taker gets what it claimed. Two orderings leave the
remote's refs away from the claim, and both are put back:

- a refresh that finds `taken` after its pushes, or that was cut off before every push was read back,
  puts each ref it moved back to what the claim names (or, with no claim, to what the offer published
  before);
- a claim that lands after a refresh published (the refresh asked for `taken` before the claim got
  there): the taker puts the `state` and `brief` refs back to what its claim names in every repo it has
  a clone of, each only while the ref holds what it read, and the offering machine does the same in
  every repo once it learns of the claim. A repo the taker has no clone of is put right once the
  offering machine asks the remote; until then its claimed state can be fetched only by its id, where
  the remote allows that.

The original is then told which of its changes came after the state taken
([below](#the-original-after-a-take)).

## Take

`borg handoff take <id>` runs in a session, in a clone of the first repo (a remote whose push URL, or
else fetch URL, names the arbiter) or with one recorded on the machine (the clones its sessions started
in).

1. **Claim.** borg fetches the first repo's state and brief and checks that the brief is this
   hand-off's, sits on that state and names it (an offer refreshed since the listing is taken as it is
   now). It fetches the state of every other repo it has a clone of and checks each is the one the brief
   names; one that is not is being refreshed, and nothing is claimed. It makes a claim commit: its parent
   is the first repo's state, and its message names the hand-off, the verb (`take` or `back`), the
   taker's node, session, engineer and harness, the brief, the state of every repo, and a nonce.
2. **The remote decides.** The claim is pushed to `refs/borg/handoff/<id>/taken` without force. Git
   refuses the push when the ref exists, so exactly one claim wins, also when the offering machine is
   off. `ls-remote` then shows whose claim the ref holds, so a push whose reply was lost still counts.
   The loser is told who took it. borg reads the ref while it holds the hand-off's lock, so two runs of
   one session agree on one claim. When the remote holds no `state` of the hand-off after the claim won,
   a `done` deleted every ref (`taken` with them) after this take read the offer: the claim is deleted
   again and nothing is taken.
3. **The outcome first.** The outcome line, built from the claim commit alone, goes into the log, and
   the original is fenced (below) before anything is checked out.
4. **Check out.** In each repo it has a clone of, the state becomes branch `borg/handoff/<id>`. A clean
   clone on the offer's branch switches to it. A dirty clone, or one on another branch, gets a new work tree beside it
   (`<clone>-<id>`), and the clone is left as it was. borg refuses, before claiming, when that work tree
   would lie outside the clone's collective (no hooks, lessons or fence there), and names the way out.
   If a branch of that name already points elsewhere, borg uses `borg/handoff/<id>-2` (or the next free
   number) and says so. The checked-out tree must have the tree hash the brief names. `HANDOFF.md` and
   `state.json` are kept in the first repo's work tree's git directory, `<git dir>/borg-handoff/<id>/`,
   never in the work tree, so the taking agent can read them again. A repo with no clone on the machine
   is listed with its clone URL: borg never clones. Once you cloned it, the same take run again checks
   it out (a state its ref no longer points at is fetched by its id where the remote allows it).
5. **Output.** The command prints the claim, each repo's branch and work tree, the note, the brief whole,
   the metadata, and the files the offer left out. They are marked as information from a colleague, not
   the engineer's instructions.

A take is idempotent. Run again in the same session, it finds its own claim and only redoes the checkout
steps, reusing the branch and work tree it made. Another session of the same engineer is told "taken by
your session Y" and continues it with `borg handoff take <id> --resume`. Anyone else is told who took it.
Getting it back then needs a new hand-off from them: `borg tell` them. A checkout that fails leaves the
claim standing: fix the reason and run the same take again.

### Taking your own hand-off back

`borg handoff take <id>` by the offering engineer is the take-back. It claims the same `taken` ref, so a
take-back and a colleague's take race through one ref and exactly one wins. In the very tree the offer
came from, nothing is checked out: the offer closes and the tree stays as it is. Taken back by the
offering session itself, nobody is fenced. Taken back by another session of the engineer, the offering
session is fenced and told, as for any take: the taking session has the work now.

## Abort

`borg handoff abort <id> [--why "<text>"]` closes a hand-off. The offering engineer may, and so may a
member whose role in the collective's signed membership is admin or steer (the roles that may pause a
drone); anyone else is refused with the roles that may. The reason is one line, redacted.

- **Open or asked:** the abort claims the same `taken` ref, with the verb `abort` and the reason. A take
  and an abort race through that one ref, so exactly one wins. Then the hand-off's `state` and `brief`
  refs are deleted (the claim stays as the record until `done`), the asked engineer and the offering
  session are told, and nobody is fenced: the offering tree keeps the work.
- **After a take:** the take's claim stands. The abort creates the hand-off's second register,
  `refs/borg/handoff/<id>/abort`, once (a commit whose parent is the take's claim), and both sides are
  told: the offering session stays fenced, and the taker's edits on the hand-off's branch are refused
  too, until someone runs `borg handoff done <id>`. A bare `git switch <branch>` or `git checkout
  <branch>` that leaves the branch passes, and off it the taker edits as before. Nothing is deleted from
  anyone's tree.

Every abort names who aborted it and why, in its commit and its line, in the listing and in borg web.

## Done

`borg handoff done <id>` finishes a hand-off once a claim holds: by whoever took it, the offering
engineer, an admin or a steerer. It deletes every `refs/borg/handoff/<id>/*` ref from each repo's remote
it has a clone of, each only while it holds what was read; a repo with no clone here is listed, its refs
stay until `done` runs in a clone of it. It writes a done line and lifts the hand-off's fences on this
machine. Other machines lift theirs once they ask the remote: a fenced session's stop asks it in the
background, and so does any `borg handoff` there. Then it removes what this machine's take left: each
work tree it made when it is clean and holds no ignored file (`git worktree remove` would delete those;
such a tree is listed with them), not the one you are in, and the local `refs/borg/handoff/<id>/*`
refs. The branch `borg/handoff/<id>` stays
and is listed: it holds the work until it is merged. An open offer is not done: take it back or abort it.

`borg handoff` lists a hand-off whose refs remain on the remote though it is aborted or done, with the
command that deletes them, and, in a repo, the hand-off refs its remote holds of hand-offs the machine
does not know, with the `git push --delete` that deletes them by hand once you know they are over (only
the refs borg makes, `state`, `brief`, `taken`, `abort` and `probe`, every value in the command quoted).
`borg doctor` names the branches and local hand-off refs a clone still holds of a hand-off that is done
or unknown on the machine.

## Switching hand-offs off

An admin switches the collective's hand-offs off with `borg handoff off`: a field of the signed
membership, `handoffs_off`, naming the admin and when, which borg-mesh signs with the node's key (so
the command needs it installed); the listing says when they are off. Every node follows it once it
holds that version. While it is off, an offer and a new claim are refused with the reason and the
admin, no notice tells a session of a hand-off waiting, and each node aborts its own open and asked
offers at its next `borg handoff` (or the background run of a stop), as their owner, naming the
switch. Taken ones stay with their takers, and a take whose claim was made before finishes.
`borg handoff on` switches them on again; nothing aborted reopens.

A node on a borg before 0.8.0 cannot verify a membership version that carries the field: it stays on
the version before it, and so follows no later version either, revocations included. `borg handoff off`
is therefore refused while a member machine seen in the last day (the team's signed presence) runs such
a borg, and names it. A machine not seen in the last day is not checked: upgrade every machine first.

## Asking an engineer: `--to`

`borg handoff "<note>" --brief <file>|- --to <engineer|node>` offers the work to someone in particular.
`<engineer>` is an email, or the name the collective's [aliases](configuration.md) give it; `<node>` is
a node key, or a prefix of it only one node has.

**Check, before anything is pushed.** borg reads the team's signed presence, which each machine sends
with its drones over the mesh: the borg it runs, the hand-off protocol it speaks, and the repos it holds
clones of, each as a hash of its normalized remote URL and the clone's name (no path, no URL). The ask
goes ahead when one of their machines was seen in the last day (presence is kept a day), speaks
hand-off, and has a clone of the repo. Otherwise borg names the gap and offers nothing: nobody of that
name seen, a borg without hand-offs (with its version), or no clone of the repo. A machine records its
clones as its sessions start in them: each session's first hook keeps the hashes of the clone's remote
URLs in `<store>/handoff/clones.json`, as git resolves them (`insteadOf` rules and the global config
applied, so a shorthand or a mirror rewrite names the same repo as the offering side's URL). It runs
`git remote -v` only when the clone's config or the global one changed since; a clone that is gone is
not told.

**Ask.** The offer line names whom it asks, and the listing shows it as `asked` with its age. An
engineer asked also gets a message (`engineer:<email>`, in `borg inbox` and on the web page), and their
sessions in a clone of the repo get the notice below, marked "asked of you". There is no timer: an ask
nobody answers stays asked, and the owner can take it back at any time. Anyone may still take it: the
check is advice, and the remote decides.

**Answer.** Their take is the yes. `borg handoff decline <id>` is the no, accepted from a line that
names whom the offer asks (by email or node), once for the hand-off, whichever of their sessions says
it: a decline line goes to the team, the offering session gets a message, and the offer stays its
owner's, open, listed as `declined` with who declined it. As in messages, the engineer a line names is
the writer's own word; the node is the one the mesh checked, and the listing shows it.

## Across machines

The code travels through the repo's remote; the news of a hand-off travels over the mesh, as lines in
the message log (`messages.jsonl`), the log every member's mesh already syncs:

- **One kind of line** (`"kind": "handoff"`), carrying one event of the hand-off log: an offer (and its
  ask), a take or take-back, a decline. Each is written by its own node only: a node announces its own
  offers, its own claims and its own declines, and a line that speaks for another node is dropped. It
  travels signed by that node's key like every message line, fresh for the messages' week by the message
  protocol's clock rule (its writer's stamp against the reader's clock: past the week, or more than a
  minute ahead, it counts not), and bounded per member in a class of its own, so no flood of them pushes
  out a message and no message pushes out one. It is never paced, shown or counted as a message.
- **An older borg drops it.** A member on a version without hand-offs reads no such line: its mesh
  neither stores nor relays it, and counts each such line it is offered as rejected, at every session
  (the peer offers it again each time). The remote stays the arbiter, so a take still works; that member
  just does not hear of it.
- **Kept alive while open.** The owner's machine announces an open offer again once its newest
  announcement is past half the week, or when announcing it failed (at `borg handoff`, and at its
  stops), so a member that syncs once in that time never loses it; a machine that declined an open offer
  does the same with its decline. An offer whose owner stays away longer drops out of other machines'
  listings; its refs stay on the remote. A machine that took or declined a hand-off keeps the offer in
  its own log, and the offering machine keeps a peer's outcome of its offer there too, so neither rests
  on the message log's week.
- **The original is fenced at once.** When the mesh stores a peer's hand-off line, the events are folded
  again and the fences follow, before anyone asks the remote: a take on another machine refuses the
  original's next edit there.
- **An original off the mesh** learns of a take at its next stop: while a session has an offer open,
  or is fenced, its stop starts a `borg handoff` in the background that nobody waits for, one at a time
  per store (a stop while one runs starts none). It asks the remote's refs, writes the outcome from the
  claim commit, fences, and announces the offer again when due. A fenced session's stop asks for the
  hand-offs that fence it even once they are decided, so a fence a made-up abort line set lifts, and a
  `done` on another machine ends it, without anyone running `borg handoff` by hand. It never prompts: git and ssh get no terminal,
  no askpass and no credential dialog, so a remote that needs a password or a passphrase fails the ask
  (the engineer's own `GIT_SSH_COMMAND` or `core.sshCommand` is used as it is).
- **A notice at a prompt.** A session in a clone of a repo with an open hand-off (not its own) is told
  once, at a prompt, as peer data: who offers it, whom it asks, the note's first line and the command
  that takes it. One notice per offering machine at a prompt; the rest wait for later prompts. It shares
  the harness's context budget with the messages and waits for a later prompt when it does not fit. The
  agent takes it only on the engineer's word.
- **borg top** lists the hand-offs not taken under each collective's drones. **borg web** has a
  Hand-offs tab (every hand-off with its state, whom it asks, who took or declined it, and the engineers
  whose machine could take it) and a "Who can take what" table (each machine seen: its engineers, its
  borg, the repos it holds clones of); the Drones tab badges a drone that offers its work, took a
  hand-off over, or is fenced.

## The original after a take

Once a claim another session made holds, the offering session is fenced:

- its edits in the handed-off repos are refused at the tool call, with who took the work and the claim.
  Every tool but the ones that only read (Read, Grep, Glob, LS, WebFetch, WebSearch, TodoWrite, Task)
  is refused when it names a path in the repo: Edit, Write, MultiEdit, NotebookEdit, a patch's files,
  an MCP tool's path arguments. A shell command is refused when it runs there (a `cd` included) or names
  a path there, unless it only reads. Paths are resolved against the call's directory and through
  symlinks, so `../app`, a relative path and a symlinked home all count;
- reads pass, and its other repos are not affected;
- it gets one message saying who took the work, at its next prompt or tool call, or in `borg inbox` if
  it ended. When the message window to it is full, the take's operation stays open and the next
  `borg handoff` (or the taking session's next prompt) sends it. Changes it made after the taken state
  stay in its tree: once its machine learns of the take from the remote, it compares each tree with the
  state the claim names, and the listing and the refusal name those changes ("not in the take").

The fence fails closed once the machine knows of a claim. A peer's outcome line fences the original as
soon as it arrives, and stays in the machine's own log, so the message log's week does not lift it. A
take-back that loses to a colleague's take learns whose claim holds from the remote and fences its own
session at once. If outcome lines cite different claims, or name different writers for one claim, the
original is fenced unless the remote, asked by `borg handoff` through a clone on the machine, says the
claim is the original's own; while the remote cannot be asked, the hand-off is listed as contested and
the fence stays. A made-up or late line lifts no fence. Until the news of a take reaches the machine
(a line over the mesh, or the remote asked at a stop), nothing fences the original. The refusal uses
the same path as `borg pause`, at the tool call on every
harness. The agent is taught to stop editing that repo and tell the engineer, never to work around the
refusal. A wake or a message still reaches a fenced session; only its edits in the handed-off repo are
refused. The fence of a session stays while the hand-off is known, also after the session ends: a
resumed session keeps its id.

## Interrupted runs

An offer and a take each take several steps against a remote, and a run can stop between any two. Before
each step acts, borg appends what it is about to do, or what it just finished, to
`<store>/handoff/<id>.wal` and syncs it to disk; a line a crash cut short never swallows the next one.
One operation per hand-off runs at a time on a machine, under a file lock. The next `borg handoff` on
the machine finishes an operation whose process ended, and so does a background run that the session's
next prompt starts:

- an offer whose state and brief were read back from the remote is published; one that was not is
  rolled back, and what it pushed is deleted;
- a take whose claim won is finished (line, fence, checkout, message, announcement); one whose claim
  was sent is decided by the remote's `taken` ref; one that stopped before its claim ends, and the offer
  stays open. A take whose announcement to the team failed stays open until it goes;
- a refresh whose every push was read back while `taken` is absent is published; any other is put back
  (above);
- an abort is decided by its ref as a take is, and finished once won; a `done` runs again (each of its
  steps is idempotent).

Once every operation in a log has ended, the log is renamed to `<id>.<n>.done`, so only open logs are
read at a prompt.

## The hand-off log

`<store>/handoffs.jsonl` holds the lines this machine wrote (one per offer, per outcome and per
decline), the offers it took or declined, and the peers' outcomes of its own offers. The lines other
machines wrote come from the message log: `<store>/handoff/mesh.jsonl` holds their events, derived again
whenever the mesh stores one, and `borg handoff` reads both. Lines are news, never the decision: the
`taken` ref on the remote decides, and what the remote was seen saying is kept in
`<store>/handoff/remote.json`: the claim it holds and who made it, as the claim commit names them (a
claim is never moved, so this decides for good), the abort its `abort` ref holds, whether it holds no
ref of the hand-off any more (done), and the claims and aborts lines cited while it held another or
none (made up: one is announced only once the remote holds it). Of the lines that cite the held
claim, only the one naming that claimer counts; before the remote is asked, lines that name different
writers for one claim name nobody, so no line another member made up picks who is shown. The
precedence is open, then asked, then declined, then taken, taken back or aborted, then done, and a
later state never gives way to an earlier one. An abort after a take counts once it cites the take's
claim (and fences on that hint until the remote is asked). A done line counts only beside the claim the
remote was seen holding, from the offering node or the claimer's (as the claim commit names it): beside
a claim only lines cite it is a hint, so two made-up lines never close an open offer, and the next
`borg handoff` asks the remote. A remote that holds nothing of the hand-off makes it done whatever the
lines say. The lines the remote refuted leave this machine's log, and of a peer's done lines only the
one citing the claim it holds is kept. Once the remote holds nothing of the hand-off, its lines leave
this machine's log; `remote.json` keeps its end. A line is its bytes: an offer line is built from its operation's
log and an outcome line from its claim commit alone, so a re-run or a recovery writes the same bytes,
and the same news written twice is one line. The log folds to the same state in any order. A hand-off
that a line marks taken, taken back or aborted stays so, and only a line that cites a claim commit counts. If
lines cite different claims, the hand-off is contested until the remote says which holds. An offer's id
is `h` and 12 hex of a hash over its node, session and nonce, so no other node or session can write an
offer with that id. Lines are checked on read as a peer's would be: names are tokens, and what a listing
prints on a line of its own has no control character. No state is decided by comparing clocks of
different machines: ages are shown from the writer's clock, and whether a peer's line counts at all
follows the message protocol's clock rule (above).

## Where it works

Only in team collectives (`share: "team"`): a hand-off pushes the working tree and the brief to the
team's remote. A `share: "local"` collective refuses it. What reaches the remote is listed in
[team sync](team-sync.md#what-a-hand-off-pushes).

## Still open

- The taker has no one-step "hand it back": getting the work back is a new hand-off from them, which a
  `borg tell` can ask for.
