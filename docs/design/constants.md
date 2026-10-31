# Numeric constants by kind

Every numeric constant's doc comment says its kind and why it has its value. This file lists them by
kind, as the code labels them in 0.7.0. Paths are relative to `crates/`; the adapters are under
`crates/borg/assets/adapters/`. Rows name a constant or a function, never a line, because lines move.
When a doc comment and this file disagree, the doc comment is right and this file is stale.

There is no cost number among these. Quality (mistakes avoided) is the objective. Spend is measured and
shown, never capped by default. Only an engineer who sets the optional per-collective `budget` in
`collectives.json` puts a price on spend, and that budget is the engineer's number, not borg's.

## Kinds

| Kind | What it is | What its doc comment says |
|---|---|---|
| protocol | A format or version that other members, older lines or caches depend on, or a limit another program sets | The format; changing it needs a new version |
| guard | A rail against a runaway | The runaway it stops. Its hit rate is monitored where it can bind often: a rail that binds often is a knob in disguise |
| derived | Computed from another constant or from a harness's `Caps` | Its source |
| learned | Estimated from outcomes, either online from the store or offline in a dated evaluation, or the dated prior of a value learned per store | The estimate, its prior and its date |
| method | The precision of an estimate, a grid, a seed, a pre-registered bar, or a literature constant of a model | Why the value is enough, or the citation |
| baseline | Defines a fixed reference rule, which a learned value is compared against, or a design choice nothing has evaluated yet | That it is the reference, not a tuning knob |
| display | What a screen, label or excerpt shows | What is shown |
| experiment | An evaluation's parameter (borg-eval, the borg-search binaries, `eval/`) | The evaluation |

## Protocol

### Messages and the mesh

| File | Name | Value | Meaning |
|---|---|---|---|
| `borg/src/msg.rs` | `WIRE_TEXT` | 16,384 chars | A v2 line's text, kept whole up to it, plus an ellipsis past it; part of `well_formed`. Also a guard: about 4,000 tokens, more than any harness takes into one event, so the delivery budget decides what shows |
| `borg/src/msg.rs` | `V1_TEXT` | 1,000 chars | A v1 line's text, 0.5.0's bound. A text that fits is written as v1, so 0.5.0 readers and `borg/mesh/13` peers read it |
| `borg/src/msg.rs` | `NAME`, `TOKEN` | 64, 128 chars | The longest sender name, and the longest session id, node or address, a line holds |
| `borg/src/msg.rs` | `TTL_MS` | 7 days (`7 × clock::DAY_MS`) | A message is shown and kept this long; every member drops a line older than it |
| `borg/src/msg.rs` | `SKEW_MS` | 60 s | The one clock skew between members; steer.rs `SLACK_MS` and borg-mesh live.rs's presence freshness derive from it |
| `borg/src/msg.rs` | `SEEN_IDS` | 256 ids | The most ids one `seen` line lists (protocol and guard: a line stays under about 4 KB, a forged one cannot list millions) |
| `borg/src/msg.rs` | `window_for` | 1 at first, doubled per acknowledgement up to `WINDOW_CAP`, halved per expiry, at least 1 | The unacknowledged tells and asks one sender may have to one recipient; `send` decides by it, under the log's lock. Where no acknowledgement may come (`Pace`) `V1_RATE_MS` also admits (a broadcast, the engineer) or alone admits (a session of a node that acknowledged none of the pair's messages) |
| `borg/src/msg.rs` | `within_windows` | acknowledged, or at most `WINDOW_CAP` unread of a pair within `V1_RATE_MS` | What the mesh carries (borg-mesh sync.rs `window`, on offer and on receipt): the bound every window keeps, reading no earlier line, so the TTL and the bound never take a held line out of it |
| `borg/src/msg.rs` | `V1_RATE_MS` | 10 min | 0.5.0's pacing, one tell or ask per sender and recipient: all a `/13` node takes (borg-mesh `v1_window`, which imports it), the pace of a node that acknowledges nothing (`Pace::Rate`), the floor where no acknowledgement may come (`Pace::Floor`), and the span `within_windows` bounds |
| `borg-mesh/src/sync.rs` | `ALPN` | `borg/mesh/15` | The session protocol: `borg/mesh/14`'s session with `absorbed.jsonl` as its last log |
| `borg-mesh/src/sync.rs` | `V14_ALPN` | `borg/mesh/14` | 0.6.x's protocol, still spoken whole except for the absorbs: messages v2, `seen` lines, the acknowledgement window |
| `borg-mesh/src/sync.rs` | `V13_ALPN` | `borg/mesh/13` | 0.4.0's and 0.5.x's protocol, still spoken; such a peer is offered only what 0.5.0 takes (`v1_window`) |
| `borg-mesh/src/sync.rs` | `V1_KEEP` | 500 lines | 0.5.0's line bound, applied with msg.rs `V1_RATE_MS` to what a `/13` peer is offered |
| `borg-mesh/src/sync.rs` | `LEGACY_ALPN` | `borg/mesh/12` | 0.3.0's protocol, still spoken whole except for the messages |
| `borg-mesh/src/sync.rs` | `LEGACY_GOAL_LOGS` | 7 | Logs per goal a 0.3.0 peer reconciles |
| `borg-mesh/src/sync.rs` | `JOIN_ALPN` | `borg/join/1` | The join handshake |
| `borg-mesh/src/sync.rs` | `older` | `/11` down to `/1` | Offered only so a mismatch is named, not silent |
| `borg-mesh/src/sync.rs` | `STEPS` | 7 logs | The order a session steps through the logs |
| `borg/src/absorb.rs` | `NOTE_CHARS` | 1,000 chars | The longest note an absorb carries: `borg memory absorb` refuses a longer one, and borg-mesh lines.rs `travels` refuses a longer line from a peer (absorb.rs `written`) |
| `borg/src/memory_export.rs` | `SCHEMA` | 1 | The export's schema version: a changed meaning or a removed field raises it, an added field does not |
| `borg/src/protocol.rs` | `NOTES` | one line per spoken protocol, `/15` down to `/12` | What a member on an older protocol misses, the one statement doctor, status, top, web, the drone notice and the announcement quote; each protocol bump adds its line (borg-mesh sync.rs test `every_spoken_protocol_has_a_note`) |
| `borg/src/protocol.rs` | `ANNOUNCE_PREFIX`, `announcement_head` | `borg/mesh/`, `borg/mesh/<n> is in use:` | The protocols' common prefix, and the announcement's first words, by which every node tells that it was sent (`announced`) |

### Releases

| File | Name | Value | Meaning |
|---|---|---|---|
| `borg/src/update.rs` | `RELEASES_URL` | `https://api.github.com/repos/debe/borg_ai/releases/latest` | The release feed whose `tag_name` the check reads, as scripts/install.sh does; `BORG_RELEASES_URL` names another |
| `borg/src/update.rs` | `DOWNLOAD_URL` | `https://github.com/debe/borg_ai/releases/download` | Where release tarballs are, in install.sh's layout (`<base>/v<X>/borg-v<X>-<target>.tar.gz` and `.sha256`); `BORG_DOWNLOAD_URL`, install.sh's mirror variable, names another |
| `borg/src/update.rs` | `EXIT_NEWER` | 10 | `borg update --check`'s exit code when a newer release exists (0: current, 1: the check failed) |

### Formats and caches

| File | Name | Value | Meaning |
|---|---|---|---|
| `borg/src/select.rs` | `CHECK_HEX` | 12 hex digits (48 bits) | A graded check's id, the ` @<check>` tag on its record and on every delivery marker |
| `borg/src/select/summary.rs` | `SV` | 8 | The selection summary cache's format; a summary of another version is rebuilt, never read (2: `spend`; 3: the stops' latency per harness, the monitor's weights; 4: `Past`, "left for later" counted at live checks, tallies at a session's end; 5: the live question's tally, what a session had, tail yields and counts; 6: the learned decisions that fell back to the fixed rule, per reason; 7: the question-count test's checks by k and s; 8: a live check's hand-over a stop asked is its free apply's delivery and verdict, and the live question's tally counts only checks with a 3) |
| `borg/src/select/summary.rs` | `STOP`, `LIVE`; `FREE_STOP`, `FREE_LIVE`, `LATER`, `WAIT`, `ASK_LIVE`; `TALLIES` | 0, 1; 0 to 4; 5 | Indices of `Summary::spend` and of a session's delivery tallies, and the tallies' width (checked at compile time): part of the cache's layout |
| `borg/src/select/summary.rs` | `FELL_BACK` | `lock_busy` 0, `lock_failed` 1, `unrecorded` 2 | Indices of `Summary::fell_back`, by the graded record's `reason` (outcome.rs `DECIDED_BUSY`, `DECIDED_NO_LOCK`, `DECIDED_NO_RECORD`): part of the cache's layout |
| `borg/src/select.rs` | `ARMS`, `Route::arm` | 3: ask 0, free 1, not now and placebo 2 | The length and order of every per-arm array (select/policy.rs `monitor`, select/replay.rs) |
| `borg/src/select/policy.rs` | `P`, the coefficient indices, `SESSION`, `LESSON`, `JUDGE`, `FAMILIES` | 15 coefficients, 3 families | The layout of the value model (`Model::beta`, `NAMES`); a cached model of another layout is not read |
| `borg/src/select/replay.rs` | `ASK`, `FREE`, `NOT`, `ROUTES`; `GRADES` | select.rs `Route::arm` and `ARMS`; 4 | Route and grade indices of the replay's tables |
| `borg/src/select/replay.rs` | `RULES` | 2026-10-28 | The pre-registration the checks implement; printed with every report, changed only by a dated amendment |
| `borg/src/select/replay.rs` | `AMENDMENT` | 2026-10-29 | The latest dated amendment of the rules the checks implement; printed beside `RULES` |
| `borg/src/select/replay.rs` | `RULES_V2` | 2026-10-29 | The rules v2 the checks implement (P4a–P4c, the question-count test in P1 and P3, its readout, the default rule); printed beside `RULES` |
| `borg/src/outcome.rs` | `DECIDED_BUSY`, `DECIDED_NO_LOCK`, `DECIDED_NO_RECORD` | `lock_busy`, `lock_failed`, `unrecorded` | A graded record's `reason` when its learned check fell back to the fixed rule: another check held the store's decision lock past hook.rs `DECIDE_WAIT`, the lock could not be taken, or the learned decision's record cannot land in the log its gate is folded from |
| `borg/src/msg.rs` | `STAMP` | `v3` | The format of a session's `.scanned` stamp, this node's alone: a stamp of another format (0.5.0's, an earlier 0.6.0 build's) makes the session read the log once |
| `borg/src/listen.rs` | `WAKE` | `w` | The first word of a wake's turn mark, `w <ms> [<tool start>]`, this node's alone: every reader takes it as a running turn, and a waiter undoes the mark of a refused wake |
| `borg/src/predict.rs` | `JUDGE_VERSION` | 2 | The judge template's version, logged on every verdict (0: before versions were logged) |
| `borg-trace/src/dialogue.rs` | `TOUCH_CHARS` | 200 chars | A command's head a touch keeps; the live hook records the same head (hook.rs `COMMAND_CHARS`), and a replay must see what it saw |
| `borg-trace/src/fingerprint.rs` | `NORMAL_CHARS` | 160 chars | The normalised error line a scar's fingerprint is made of: a change splits every stored scar |
| `borg/src/vectors/unigram.rs` | `UNK_PENALTY` | 10.0 | `tokenizers`' `K_UNK_PENALTY`: must equal the library's, or text splits differently from the model's training |
| `borg/src/vectors/unigram.rs` | `FNV` | `0xcbf29ce484222325` | The FNV-1a 64-bit offset basis |
| `borg/src/notes/regress_diff.rs` | `BACKSLASH` | `0x5c` | git's escape in a quoted diff path |
| `borg/src/install/change.rs` | `NEW_FILE_MODE` | `0o600` | Files borg creates are private: `settings.json` often holds secrets |
| `borg-web/src/main.rs` | `PORT` | 7979 | The page's port unless `--port` says otherwise |

### Limits other programs set

| File | Name | Value | Meaning |
|---|---|---|---|
| `borg/src/harness/claude.rs` | `CONTEXT_CHARS` | 10,000 chars | Claude Code saves a longer `additionalContext` to a file and gives the model a preview (2.1.312, read 2026-10-28) |
| `borg/src/harness/codex.rs` | `CONTEXT_CHARS` | 10,000 chars | Codex's default `additionalContextLimit`: a longer one is cut to its head and tail (measured on 0.153.2, 2026-10-28) |
| `borg/src/harness/omp.rs` | `HANDLER_LIMIT`, `SHUTDOWN_LIMIT` | 30 s, 2 s | How long omp waits for an extension's handler, and for `session_shutdown`'s (18.13.1); no setting changes them |

## Guard

### Lesson delivery and selection

| File | Name | Value | Runaway it stops |
|---|---|---|---|
| `borg/src/attention.rs` | `RAIL` | 20 | The most lessons one round asks under the learned policy (its solver's limit) and the attention cut; the fixed rule asks at most `CAP`. Stops one question per lesson of a large pool |
| `borg/src/conscience.rs` | `EARLY_CHARS` | 2,000 chars (about 500 tokens) | The context one early or free delivery adds, less the lines handed over in place of questions (the question-count test's `instead` and the evidence rule's `instead-next` hand-overs of lessons the fixed rule asks, `render_owing`), which it never cuts; a free lesson it cuts is logged `dropped_chars`, so its hit rate is measured |
| `borg/src/hook.rs` | `MAX_EARLY_DELIVERIES` | 3 per session | A long session naming many systems getting a delivery at every touch (standing lessons are exempt) |
| `borg/src/hook.rs` | `MAX_PENDING` | conscience.rs `CAP` (10) | Pending lessons one judge call sees: an unbounded judge section. What it leaves out is logged (`#borg-unjudged … why=slots`) |
| `borg/src/hook.rs` | `MAX_JUDGE_TRIES` | 2 | A lesson the judge never settles riding on every later call; what stays unjudged is censored with its reason |
| `borg/src/hook.rs` | `EVIDENCE_CHARS` | 4,000 chars | One evidence line of the `.edits` file every judge call reads (at least three times `predict::JUDGE_PART`) |
| `borg/src/hook.rs` | `EDIT_CHARS` | 600 chars | One edit's line in the `.edits` and turn files: a whole written file |
| `borg/src/hook.rs` | `COMMAND_CHARS` | borg-trace `TOUCH_CHARS` (200) | A shell command kept as a touch: a heredoc going in whole |
| `borg/src/select.rs` | `PLACEBO_SLOTS` | 2 per check | The judge's extra work for the placebo arm (the fixed rule and the learned policy): of the applies a check delivers neither way, never one its session had, each lesson once; the chance ψ = min(1, 2 / candidates) is logged |
| `borg/src/whole.rs` | `TAIL_SAMPLE` | 2 per check | The grader's extra lessons past the pool's budget, under every policy, and so what the sample adds to the prompt past the stable lesson block; their chance k / n is logged (`pt`) |
| `borg/src/hook.rs` | `DECIDE_WAIT` | 1 s (live.rs `AFTER_CALL` / 3) | A learned check holding the store's decision lock long (a summary rebuilt from a long log, a stopped process) holding up every other learned check's answer; past it the check takes the fixed rule (`reason`). Also derived: a third of what the stop's call leaves for the hook's records |
| `borg/src/select/policy.rs` | `SOLVE_MAX` | 16 applies | The exact route solver's time per check (O(n³ log n) per draw); a larger check takes the fixed rule (source `size`), so its hit rate shows |
| `borg/src/select/policy.rs` | `FIT_SESSIONS` | 256 sessions | The fit's dense block and its time; the labels it leaves out are counted |
| `borg/src/select/policy.rs` | `MAX_IPW` | 10 | A route the judge rarely reaches letting a handful of its labels outweigh the rest |
| `borg/src/select/policy.rs` | `MAX_NEWTON`, `MAX_HALVINGS`, `MAX_EB`, `PREQ_SESSIONS` | 50, 30, 50, 16 | A fit that does not converge, a step that cannot improve, EM's crawl toward a spread of 0, one fit per session and half-life |
| `borg/src/select/summary.rs` | `MAX_APPLIES`, `LATENCY_KEEP` | 50,000 applies, 2,000 calls | The cache a hook reads |
| `borg/src/select/summary.rs` | `MAX_SESSIONS` | 4,096 sessions | The session table, which `MAX_APPLIES` does not bound when sessions grade few applies; a compaction keeps at least `policy::FIT_SESSIONS` (checked at compile time) |
| `borg/src/select/summary.rs` | `MAX_FRESH` | 1,024 sessions | Sessions shown lessons before any graded check (an early delivery at a prompt) that never reach one; past it the oldest quarter go |
| `borg/src/select/replay.rs` | `CLAMP` | 10⁻⁹ | A model's 0 or 1 making one unit's log-loss infinite |
| `borg/src/select/replay.rs` | `OPEN` | 10¹² | The allowance of the gate P4c's test arm decides against: above any check's charged risk, so every draw is admitted, and finite, so the gate's arithmetic stays exact |
| `borg/src/whole.rs` | `QUESTION_CHARS`, `CONSEQUENCE_CHARS` | 240, 160 chars | One long lesson taking the share of the pool many short ones would fill |
| `borg/src/vectors.rs` | `OVER_FETCH` | 30 | A vector search coming back short once done, out-of-scope and listed hits are dropped |
| `borg/src/predict.rs` | `timeout` | 45 s (`BORG_PREDICT_TIMEOUT_S`) | One consequence-model call, under Claude Code's former 60 s hook limit |
| `borg/src/predict.rs` | `TAIL` | 24,000 chars | A huge transcript filling the call: the model reads the end of the work |
| `borg/src/predict.rs` | `JUDGE_PART` | 1,200 chars | Judge evidence per delivery: `before` this, `after` three times as much, the same for every route |
| `borg/src/live.rs` | `WAIT_SHARE` | 4 | A slow live check starving the stop's own model call: the stop waits at most a quarter of its budget |
| `borg/src/live.rs` | `AFTER_CALL` | 3 s | The stop's model call leaving no time for the hook's records and answer |
| `borg/src/live.rs` | `STALE_SLACK_MS` | 30 s | A crashed check's lock held forever (`stale_ms`) |
| `borg/src/expert_route.rs` | `STOP_MARGIN` | 5 s | A stop killed by its harness mid-write |
| `borg/src/expert_route.rs` | `DRONE_MIN` | 10 s | A drone started only to be killed |
| `borg/src/expert_route.rs` | `DRONE_TIMEOUT` | 120 s | A drone that never finishes or loops on tool calls; its cost is measured, never capped |
| `borg/src/expert_route.rs` | `ANSWER_CHARS` | 1,500 chars | A long drone answer flooding the context |
| `borg/src/expert.rs` | `INDEX_WORDS` | 160 words | A long requirement's BM25 match outweighing a short one's |

### Messages, steering, presence

| File | Name | Value | Runaway it stops |
|---|---|---|---|
| `borg/src/msg.rs` | `MEMBER_BYTES` | 1 MiB per member and class | One member's flood growing the file every hook and `borg listen` read; classes (conversation, steers, seen) apart, so tells never push out a pause or the acknowledgements. A cut is counted (`msg_dropped`, `why: bounds`) |
| `borg/src/steer.rs` | `TTL_MS` | 24 h (`clock::DAY_MS`) | A forgotten pause holding a session forever (fail open) |
| `borg/src/steer.rs` | `KEEP` | 500 entries | The admissions file every tool call reads while anyone steers |
| `borg/src/steer.rs` | `DROPPED_BYTES` | 128 KiB | A flood of refused steers filling the disk (past twice it, the older half goes) |
| `borg/src/presence.rs` | `PRUNE_MS` | 24 h (`clock::DAY_MS`) | Presence files of every session ever run |
| `borg/src/outcome.rs` | `IDLE_MS` | 24 h (`clock::DAY_MS`) | A session whose `end` hook never ran never settling |
| `borg/src/veto.rs` | `REASON_CHARS` | learn.rs `EXCERPT_CHARS` (1,500) | A veto's episode, as a correction's is bounded |
| `borg/src/veto.rs` | `REASON_MIN` | 4 chars | A stray "borg no" becoming a veto |
| `borg/src/memory.rs` | `regret_cap` | the deciding session's corrections, at least 1 | One session whose work later took many fixes outweighing every other session of a lesson |

### Hooks, harnesses and adapters

| File | Name | Value | Runaway it stops |
|---|---|---|---|
| `borg/src/harness/mod.rs` | `STOP_TIMEOUT_S` | 90 s | A stuck stop holding the engineer's turn: Claude Code's and Codex's Stop hook `timeout`, opencode's stop budget |
| `borg/src/harness/mod.rs` | `DEFAULT_STOP_BUDGET` | 60 s | A hook installed with no `timeout` on an older Claude Code being cut off (Claude Code's old default) |
| `borg/src/harness/mod.rs` | `CONTEXT_GUARD` | the stricter of claude.rs and codex.rs `CONTEXT_CHARS` (10,000) | One event filling the model's context on a harness with no limit of its own (omp, opencode) |
| `borg/src/harness/mod.rs` | `OUTPUT_TAIL` | 4,000 chars | A build log of megabytes piped through every hook (the adapters' `tail_chars`) |
| `borg/src/harness/omp.rs` | `BUDGETS` | prompt 5 s, before a tool call 2 s, after it 5 s | A hung borg holding omp's turn |
| `borg/src/harness/omp.rs` | `STOP_MARGIN`, `END_MARGIN` | 2 s, 0.5 s | borg's answer arriving after omp gave up on the handler |
| `borg/src/harness/opencode.rs` | `BUDGETS` | prompt 5 s, before and after a tool call 2 s, stop and end `STOP_TIMEOUT_S` | A hung borg holding the engineer's turn (opencode limits no plugin hook) |
| `borg/src/harness/opencode.rs` | `CLI_CAP`, `CHILDREN`, `SESSION_BUDGET` | 30 s, 20, 120 s | A hung opencode CLI, and one session's exports |
| `borg/src/harness/codex.rs` | `TIMEOUTS` | prompt 30 s, pre_tool 10 s, post_tool 30 s, stop `STOP_TIMEOUT_S`, end 3 s, halted 3 s | A hung borg holding Codex's turn (Codex's own default is 600 s); also protocol once trusted (copied in `adapters/codex/smoke.sh`) |
| `borg/src/harness/codex.rs` | `LIST_TIMEOUT` | 3 s | `borg doctor` waiting for Codex to list its hooks |
| opencode adapter | `MAX_ROUNDS` | 1 | The plugin prompting the drone in a loop: questions per engineer prompt |
| opencode adapter | `LISTEN_MS` | 10 s | The plugin's own `borg listen --session` call |
| opencode adapter | `MAX_DEPTH` | 32 parents | A cycle of parent session ids, or a chain deeper than opencode builds, spinning `root` |
| omp adapter | `CALLS_KEPT` | 256 calls | Tool calls whose approval never resolved growing the extension's input map for the life of omp |
| omp and opencode adapters | `FALLBACK` | the harness's `BUDGETS` and `OUTPUT_TAIL` | Only until borg's first answer carries `"budgets"`; a test keeps the copies equal. opencode's are guards; omp's stop and end are derived from omp's handler limits, the rest guards |
| `borg/src/reexec.rs` | `BUSY_TRIES`, `PROBE_WAIT`, `ALONE_WAIT` | 20 tries 25 ms apart, 10 s, 2 s | Re-exec of a busy file, a new binary that hangs on `--help`, other threads never ending |
| `borg/src/runner.rs` | `DRAIN_GRACE` | 1 s | A detached process holding a model call's pipe past its deadline |
| `borg/src/systems.rs` | `MAX_DIRS` | 256 | A session that touched files all over the disk costing the hook a walk per directory |

### Listen

| File | Name | Value | Runaway it stops |
|---|---|---|---|
| `borg/src/listen.rs` | `QUEUE_TIMEOUT` | 30 s | A hung `codex queue` stalling the waiter |
| `borg/src/listen.rs` | `RETRY_MIN`, `RETRY_MAX` | 10 s, 300 s | A harness that refuses every wake being asked again at once; how late a wake comes once it takes them again (`backoff`, for codex and the adapters' refused wakes alike) |
| `borg/src/listen.rs` | `STOP_SLACK_MS` | 30 s | A killed stop's mark never expiring (`stop_ms`) |
| `borg/src/listen.rs` | `LOG_MAX` | 64 KiB | A detached waiter's log |
| `borg/src/listen.rs` | `LOCK_TRIES`, `LOCK_RETRY` | 10 tries, 5 ms apart | A lock held by a probe at a waiter's start |
| `borg/src/listen.rs` | `ANCESTRY` | 16 processes | The walk up `/proc` from a command to its harness |

### Mesh

| File | Name | Value | Runaway it stops |
|---|---|---|---|
| `borg-mesh/src/sync.rs` | `MAX_FRAME` | 256 MiB | A broken or hostile peer making this node allocate what it claims |
| `borg-mesh/src/sync.rs` | `MAX_GIVE` | 64 MiB | A frame the peer refuses: a large difference goes over several sessions |
| `borg-mesh/src/sync.rs` | `MAX_ASK` | 1,000 | One round of a long history making a peer read every excerpt it holds |
| `borg-mesh/src/recon.rs` | `MAX_OPEN` | 65,536 ranges | A peer that keeps claiming differences |
| `borg-mesh/src/live.rs` | `MAX_RECEIVED` | 10,000 records | A peer sending an endless presence list |
| `borg-mesh/src/daemon.rs` | `Options` | interval 60 s, backoff_max 30 min, timeout 30 s, grace 5 s | Staleness when no change triggered a dial, an unbounded backoff, a peer that never answers, a hung session at Ctrl-C. The CLI's defaults read them from `Options::default` |
| `borg-mesh/src/main.rs` | `JOIN_TIMEOUT`, `ONLINE_WAIT` | 60 s, 10 s | A join whose admin node is down hanging; an unreachable relay blocking a command |

### Release check

| File | Name | Value | Runaway it stops |
|---|---|---|---|
| `borg/src/update.rs` | `CHECK_SECS` | 10 s | A hung connection keeping the background release check alive (curl `--max-time`) |
| `borg/src/update.rs` | `DOWNLOAD_SECS` | 600 s | A release download that never ends: a tarball of three binaries on a slow link still fits |
| `borg/src/update.rs` | `CHECKING_MS` | 10 min | Every prompt starting a check: a started check (`update.checking`) holds the others off, so at most one curl per machine per 10 minutes, even when every check fails |

### Learning, notes and the queen

| File | Name | Value | Runaway it stops |
|---|---|---|---|
| `borg/src/runner.rs` | `MAX_FAILURES` | 3 | A batch (`borg learn` in a row; `borg facets`, `borg assimilate --from-dir`, `borg reach` before the first answer) making one failed call per item while the model is unavailable |
| `borg/src/runner.rs` | `JOBS` | 4 (`--jobs`, `BORG_ASSIMILATE_JOBS`) | One child CLI per item at once hitting the provider's rate limit and the machine's memory |
| `borg/src/learn.rs` | `learn_timeout` | 300 s (`BORG_LEARN_TIMEOUT_S`) | A hung runner keeping `borg learn` alive |
| `borg/src/learn.rs` | `EXCERPT_CHARS` | 1,500 chars | A pasted log going into an episode whole (corrections, rejected tool calls, the task as the learn prompt shows it) |
| `borg/src/learn.rs` | `PROPOSAL_CHARS` | 2,000 chars | One episode's `agent` text: the end of the drone's last proposal (hook.rs, harness/claude.rs, assimilate.rs, notes.rs) |
| `borg/src/learn.rs` | `TASK_CHARS` | 3,000 chars | A pasted spec going into the `.task` file and every episode |
| `borg/src/facets.rs` | `MAX_FACETS`, `FACET_CHARS`, `MIN_FACETS` | 10, 300 chars, 3 | What a model answer adds to a lesson; a refusal or truncated answer stored as facets |
| `borg/src/assimilate_dir.rs` | `CHUNK_LINES` | 150 lines | A long knowledge file overflowing, or diluting, one prompt |
| `borg/src/review_fix.rs` | `DETAIL_CHARS` | 800 chars | What one review finding adds to the episode and the prompt |
| `borg/src/notes.rs` | `MAX_COMMITS` | 20 | A scripted series of commits writing a note each |
| `borg/src/notes/regress.rs` | `SPAN_CAP`, `FILES_CAP`, `RANGES_CAP`, `REPLAY_CAP`, `TEXT_CAP` | 400 lines, 10 files, 20 ranges, 500 commits, 240 chars | Blame and link size, a replayed range, free text in a link |
| `borg/src/notes/regress.rs` | `Cfg::default` | min age 2 days, 5 links, 40 files, 2,000 lines, 10 s per git call, 60 s in all | A refactor taken for a fix; every git call fails open |
| `borg/src/queen.rs` | `BUDGET_SESSIONS`, `MAX_QUESTIONS` | 5 sessions, 2 questions | The engineer's attention, not money: at most one queen message per engineer per 5 sessions, 2 questions in it |
| `borg/src/queen.rs` | `SKIP_MS`, `REOFFER_SESSIONS` | 3 days (`3 × clock::DAY_MS`), 3 sessions | A skipped or ignored question coming back at every session |
| `borg-trace/src/dialogue.rs` | `REFUSAL_CHARS` | 2,000 chars | A refused call's error text, whatever the harness printed |
| `borg-trace/src/scars.rs` | `CMD_CHARS` | 300 chars | A memory's line holding a heredoc or a pasted script |
| `borg-web/src/lib.rs` | `PAGE_MAX` | 2,000 rows | One request making the server serialise a whole log (`/api/feed`, `/api/messages`) |
| `borg/src/sweep.rs` | `FIRST_READ` | 4 MiB | A turn-end sweep without a valid offset (the first turn after an upgrade, a rewritten file) reading a long session's whole transcript in the hook; the session's end and the idle sweep read the rest |
| `borg/src/sweep.rs` | `DOCTOR_FILES` | 200 transcripts per harness, the newest | `borg doctor` reading every transcript of a busy fortnight; a note says when there were more |
| `borg/src/assimilate.rs` | `OPEN_CALLS` | 64 calls | Tool calls left without a result by interrupted turns growing the `.swept` state written at every turn end; a result whose call fell out keeps its id |

## Derived

| File | Name | Value | Source |
|---|---|---|---|
| `borg/src/clock.rs` | `DAY_MS` | 24 h | A unit. Every "a day" or "a week" in borg is a multiple of it |
| `borg-web/ui/src/util.rs` | `DAY_MS`, `DAY_S` | 24 h | clock.rs `DAY_MS` (the page is a wasm workspace of its own) |
| `borg/src/notes/regress.rs` | `DAY` | 86,400 s | clock.rs `DAY_MS`, in git's seconds |
| `borg/src/msg.rs` | `WINDOW_CAP` | 64 | `MEMBER_BYTES / WIRE_TEXT`: as many full texts as one member's share of the log holds (it promises no room: `send` refuses a message that would push one of its sender's unread messages to the same recipient out). Also what the mesh lets through unread from one sender to one recipient within `V1_RATE_MS` (`within_windows`) |
| `borg/src/steer.rs` | `SLACK_MS` | 1 h | 60 × msg.rs `SKEW_MS`: kept past its TTL before pruning |
| `borg/src/harness/mod.rs` | `Caps::context_chars` | 10,000 chars on every harness | The harness's own limit (claude.rs, codex.rs `CONTEXT_CHARS`), else `CONTEXT_GUARD`; shared by everything one event delivers |
| `borg/src/harness/mod.rs` | `Caps::stop_budget` | Claude Code none, omp 28 s, opencode and Codex 90 s | omp's `HANDLER_LIMIT` less its `STOP_MARGIN`; `STOP_TIMEOUT_S` |
| `borg/src/harness/omp.rs` | `BUDGETS` stop, end | 28 s, 1.5 s | `HANDLER_LIMIT − STOP_MARGIN`, `SHUTDOWN_LIMIT − END_MARGIN` |
| `borg/src/harness/mod.rs` | `longest_stop` | 90 s | The largest stop budget or installed stop `timeout` of any harness |
| `borg/src/expert_route.rs` | `DEFAULT_STOP_BUDGET` | 60 s | harness/mod.rs `DEFAULT_STOP_BUDGET` |
| `borg/src/live.rs` | `STOP_WAIT`, `stop_wait` | 15 s; min(15 s, budget / 4) | `DEFAULT_STOP_BUDGET / WAIT_SHARE`; omp's 28 s gives 7 s |
| `borg/src/live.rs` | `call_limit` | budget − time spent − `AFTER_CALL`, at most predict.rs `timeout` | omp gives at most 18 s after a 7 s wait |
| `borg/src/live.rs` | `stale_ms` | 2 × predict.rs `timeout` + 30 s | A check's longest model call and `STALE_SLACK_MS` |
| `borg/src/listen.rs` | `stop_ms` | `longest_stop` + 30 s | A stop's mark outlives the longest stop by `STOP_SLACK_MS` |
| `borg/src/hook.rs` | `LIVE_WORK_CHARS` | 4,000 chars | `EVIDENCE_CHARS`: as much of the turn as the judge's evidence keeps |
| `borg/src/learn/regret.rs` | `CAP` | 1,500 chars | learn.rs `EXCERPT_CHARS` |
| `borg/src/escalation.rs` | `MISSED_W` | 3 | memory.rs `VETOED_W` |
| `borg/src/memory.rs`, `borg/src/ope.rs` | `WEIGHTS`, `CURRENT` | veto 3, regret 1, tested 0.5 | `VETOED_W`, `REGRET`, `TESTED_W`: the weights in force |
| `borg/src/ope.rs` | `RESAMPLES` | 512 | conscience.rs `P_DRAWS` |
| `borg/src/select/policy.rs` | `DRAWS` | 512 | conscience.rs `P_DRAWS`: the joint draws behind the gate's quantile and the evidence rule's (a deviation plays only when it beats the fixed rule at their `DELTA` quantile) |
| `borg/src/select/policy.rs` | `pool_grid` | 6,000 to 24,000 tokens | whole.rs `TOKENS` × 2^(i/2), i from `POOL_STEPS` |
| `borg/src/whole.rs` | `CHARS_PER_TOKEN` | 4 | borg-trace `CHARS_PER_TOKEN` (re-exported) |
| `borg-eval/src/preempt.rs` | `EXPERT_WAIT_S` | `EXPERT_TIMEOUT_S` + 60 s | The broker's answer wait |

## Learned

| File | Name | Value | Estimate, prior, date |
|---|---|---|---|
| `borg/src/prefilter.rs` | `THRESHOLD` | 8.0 (`BORG_PREFILTER`) | Pre-registered in an internal evaluation (stop-prefilter): 0.9 × the lowest score of any labelled checkpoint with a relevant lesson in its shortlist (9.28), rounded down |
| `borg/src/select/policy.rs` | `ASK_PRIOR` | N(0, 0.35²) | Ask against free: the review→fix evaluation of 2026-10 (asked +6.3, handed over +7.3 points against the plain agent), wide |
| `borg/src/select/policy.rs` | `NOT_PRIOR` | N(−0.3, 0.25²) | Not now against free: the same evaluation, a lesson not delivered changes less than one handed over |
| `borg/src/select/policy.rs` | `DILUTION_PRIOR` | N(0, 0.1²), at most 0 (a half-normal) | Per ln(m) of asks or free lessons. Dated 2026-10-29 (Rules v2): the 2026-10-22 estimate (0.04 [0, 0.12] less per extra question, log-odds) has an interval that includes 0, so no dilution is assumed (mode 0) and its width sets the scale. 0.6.0's first design centred it at −0.1. Keeps its sd: automatic relevance never shrinks a dated prior |
| `borg/src/select/policy.rs` | the value model | fitted at `borg learn` | One logistic model of "changed the work and avoids the consequence" by grade, rank, channel, a 3 in the check, a 3 at the stop, pool size, route and dilution, with session, lesson and judge effects (empirical Bayes) and automatic relevance for the terms without a dated prior (rank, pool, the interactions). Decides under the learned policy, the default (`BORG_SELECT=fixed` keeps the fixed rule) |
| `borg/src/select/policy.rs` | delivery rates | Beta(1, 1), with the forgetting | Whether a stop's free lesson arrives, a live check's free lesson arrives, and a live question reaches the agent (1 until one is tallied; a stop's question blocks, so it is certain): these weigh a route's value. Whether a lesson left for later is delivered later and whether a waiting live question blocks are shown only. Applies of a lesson the session already had count in none |
| `borg/src/select/policy.rs` | the forgetting | a half-life from `HALF_LIVES` | Chosen by prequential likelihood over whole sessions; ties go to the longer |
| `borg/src/select/policy.rs` | `pool_tokens` | whole.rs `TOKENS` until the yield by fill position has data | Under the learned policy once its gate is open or a budget is set (select.rs `pool_active`), the pool budget a session's draw values most among those whose gain over the baseline is ≥ 0 at the gate's quantile; never past the baseline without the yield of the added positions (the tail sample's) and a latency within the stop budget. The fixed rule keeps `TOKENS` |
| `borg/src/attention.rs` | `PRIOR_K` | 11 (ranks 0 to 10) | The attention cut's prior (`BORG_STOP_ATTENTION=on`, evaluation only): conformal risk control by rank, α = the fixed rule's miss rate (0.059) on the 169 logged checks of the review-fix runs, 2026-10-22 (`eval/review-fix/crc.py`) |
| `borg/src/listen.rs` | `TURN_IDLE_MS` | 5 min | Floor, dated 2026-10-28 (the fixed value before holds were learned): the least quiet a turn holds a wake off through; a session whose own quiet gaps run longer holds longer |
| `borg/src/listen.rs` | `QUEUE_IDLE_MS` | 60 s | The same floor for codex's waiter, whose wake waits in codex's queue behind a running turn |
| `borg/src/listen.rs` | `TOOL_MS` | 10 min | Floor, dated 2026-10-28: the least time a tool call that never ended holds a wake off; longer for a session whose tool calls run longer |
| `borg/src/facets.rs` | `EST_USD_PER_LESSON` | $0.012 | Measured once (internal evaluation "facets"): the mean on the eval stores, $0.0112, rounded up. Display only: a dry run's estimate; nothing is capped by it |

## Method

| File | Name | Value | Why it is enough, or its citation |
|---|---|---|---|
| `borg/src/select/policy.rs` | `ALPHA` | 0.10 | The share of the fixed rule's replayed quality (mistakes avoided) the learned policy may risk below it while it learns, at the gate and in the monitor; a quality bound, not a cost. The default of a collective's `risk` (collectives.json, select.rs `risk_of`), the engineer's tolerance: the gate's allowance in a decision is the ledger's (accrued at `ALPHA`) scaled by `risk` / `ALPHA`, for deviations on evidence and the question-count test alike; `risk` 0 turns the test off. The monitor keeps `ALPHA` |
| `borg/src/select/policy.rs` | `DELTA` | 0.05 | The gate's quantile, the evidence rule's (a deviation must beat the fixed rule at it) and the monitor's error probability: the conventional 95% |
| `borg/src/select/policy.rs` | `PRIOR_SD` | 2.5 | The weakly informative default for logistic coefficients (Gelman et al. 2008); also the ceiling of every learned spread |
| `borg/src/select/policy.rs` | `ARD_START`, `LEVEL_START`, `SD_FLOOR` | 1.0, 0.5, 0.01 | Starting sd of a term without a dated prior (rank, pool, the interactions), starting spread of the session, lesson and judge effects (EM moves both), and the floor of every spread (0.01 on the log-odds scale is no effect) |
| `borg/src/select/policy.rs` | `PSI_FLOOR` | 10⁻³ | The least chance a drawn placebo is weighed at (the monitor, the replay's P5): a logged ψ of 0 is rounding, so such a label weighs at most a thousand sure ones |
| `borg/src/select/policy.rs` | `PRUNED_KEEPS` | intercept, the two grades, ask, not now, the two dilution slopes | The terms of P2's pre-registered pruned model (`fit_pruned`); every other is held at N(0, `SD_FLOOR`²), with no session, lesson or judge effects |
| `borg/src/select/policy.rs` | `PROBIT` | π/8 | The probit approximation's scale, E[σ(η)] ≈ σ(μ / √(1 + π σ² / 8)) (MacKay 1992) |
| `borg/src/select/policy.rs` | `TOL`, `EB_TOL` | 10⁻⁸, 10⁻⁴ | Newton's and EM's convergence |
| `borg/src/select/policy.rs` | `HALF_LIVES` | none, 64, 32, 16, 8 sessions | A doubling grid; a tie goes to the longer |
| `borg/src/select/policy.rs` | `POOL_STEPS` | −2 to 2 | A √2 grid of pool budgets around the baseline |
| `borg/src/select/policy.rs` | `LATENCY_MIN` | 20 calls | With 20, the empirical 95% quantile has an observation above it |
| `borg/src/select/policy.rs` | `PRICE_STEPS` | 60 | Bisection steps of a budget's price: enough halvings for f64 precision |
| `borg/src/select/policy.rs` | `CS_SESSIONS`, `CS_MIN` | 30, 10 sessions | Where the monitor's confidence sequence is tightest (the online readout's 30 sessions; it sets the mixture, not the error rate), and the sessions before it may trip |
| `borg/src/select/replay.rs` | `CALIBRATION` | 0.005 | P2's pre-registered bar on the log-loss difference |
| `borg/src/select/replay.rs` | `RECALL_MARGIN` | 0.05 | P4's pre-registered margin: 5 points of right-lesson recall |
| `borg/src/select/replay.rs` | `READOUT_UNITS`, `READOUT_SESSIONS` | 300, 30 | The pre-registered online readout: randomised labelled units, or sessions under the learned policy |
| `borg/src/select/replay.rs` | `TEST_CHECKS`, `TEST_SESSIONS` | 300, 30 | The question-count test's pre-registered readout (rules v2): due after this many checks that drew a count (played or refused by the gate), or sessions with one, and only then estimated |
| `borg/src/select/replay.rs` | `SAME` | 10⁻⁹ | P4c's float slack: an arm's recall of a run equals the fixed rule's within the rounding of summing `P_DRAWS` shares, far below one draw's share |
| `borg/src/select/replay.rs` | `COIN` | 10⁻⁹ | A prediction this close to 1/2 is a coin's: P2 is NOT TESTED when every prediction of both models is one |
| `borg/src/select/replay.rs` | `LOOSEST` | `Some(u64::MAX)` ms | The latency limit the replay chooses pools under, as the budget's `price` replays them: P3's pool spend is the most any hook's limit allows (none at all would forbid every pool above the baseline) |
| `borg/src/hook.rs` | `DECIDE_POLL` | 5 ms | How often a check waiting for the store's decision lock tries it again |
| `borg/src/conscience.rs` | `P_DRAWS` | 512 | Draws behind a propensity: standard error at most 1 / (2√512) ≈ 0.022 |
| `borg/src/conscience.rs` | `BM25_K1`, `BM25_B` | 1.2, 0.75 | BM25's standard parameters (Robertson and Zaragoza 2009), never tuned on borg's data |
| `borg/src/ope.rs` | `AGREE` | 0.01 | A replayed p this close to the logged one is the same draw: the logged p is rounded, the estimate Monte Carlo |
| `borg/src/ope.rs` | `SEED` | `0x006f_7065` | The bootstrap's seed, fixed so a run is deterministic (select/replay.rs's too) |
| `borg/src/memory_export.rs` | `beta_cf`'s `TINY`, `EPS`, `MAX_TERMS` | 10⁻³⁰⁰, 10⁻¹⁵, 10,000 terms | The incomplete Beta's continued fraction (Lentz's method): `TINY` keeps a denominator off zero, `EPS` is double precision, and the fraction needs about √max(a, b) terms, so `MAX_TERMS` covers beliefs of millions of outcomes and is never the stop |
| `borg/src/memory_export.rs` | `beta_quantile`'s halvings | 60 | Bisection on the monotone Beta CDF over [0, 1]: 60 halvings place the quantile within 2⁻⁶⁰ of the root, finer than the 4 decimals the export shows |
| `borg/src/top.rs` | `Z95` | 1.96 | The normal quantile of a 95% interval, for the estimates the monitoring shows |
| `borg-trace/src/lib.rs` | `CHARS_PER_TOKEN` | 4 | The usual rule of thumb for English text and code; an estimate, never a bound (borg-trace, whole.rs, `borg status`) |
| `borg/src/vectors.rs` | `SEGMENT_BITS` | 11 | SeekStorm's own default for an index of this size; borg-search's seek.rs uses it |
| `borg/src/attention.rs` | `MIN_ALPHA`, `WINDOW`, `EXPLORE` | 0.001, 200 checks, 1 in 10 | The attention cut's floor (999 labels before it moves while the fixed rule misses none), drift window and exploration rate; used only under `BORG_STOP_ATTENTION=on` |
| `borg/src/listen.rs` | `HOLD_PCT`, `GAPS_KEPT` | 95th percentile, 64 gaps | A hold is the session's own gap at the 95th percentile (nearest rank) of its latest 64 of each kind, never below its floor |
| `borg/src/harness/claude.rs` | `TAIL_CHUNK` | 64 KiB | The first read from a transcript's end (`turn_so_far`), doubled at each further one: a turn's tail takes one small read |
| `borg/src/assimilate.rs` | `NOTICE_CHARS` | 200 chars | How far into a failed tool call's first line the harness's rejection notice is looked for: past it the same words are the tool's own output. Not a display length: it decides what counts as a rejection |
| `borg/src/listen.rs` | `TICK`, `LOOK` | 250 ms, 2 s | The waiter's poll: one `stat` per tick, a look at the session when the log is unchanged |
| `borg/src/live.rs` | `WAIT_POLL` | 200 ms | How often a stop waiting for a live check looks at its lock |
| `borg/src/sweep.rs` | `HASHED` | 64 bytes | The bytes before the swept offset whose hash says a transcript is still the file swept (rewritten in place, or another file at its path, differs there) |
| `borg/src/sweep.rs` | `DOCTOR_DAYS` | 14 days | How far back `borg doctor` compares transcripts with what Borg holds: a harness update a few days old and the sessions on either side of it |
| `borg/src/sweep.rs` | `CAPTURED_SHARE` | 0.5 | `borg doctor`'s "far fewer": a working capture holds about every rejection and answer a transcript names, a broken one few or none |
| `borg/src/learn.rs` | `SAME_MS` | 5 s | Two prompt episodes of one session this close are one correction (a dedupe window: the hook's capture time and the transcript's message time); a tool call's episode is matched by its id or words instead (`Known`) |
| `borg/src/queen.rs` | `DUPLICATE` | 0.6 | The Jaccard overlap that makes two questions one (a dedupe threshold) |
| `borg/src/notes/regress_cosmetic.rs` | `MOVE_MIN`, `MOVE_NEAR` | 8 chars, 3 lines | The cosmetic-change classifier's heuristics |
| `borg-mesh/src/recon.rs` | `LEAF` | 8 hashes | Trades rounds against message size; both sides must use the same value, so it is part of the protocol |
| `borg-mesh/src/daemon.rs` | `Options::poll` | 2 s | A `stat` per synced file; a local change is dialled within it |
| `borg-mesh/src/main.rs` | `STOP_TICK`, `IMAGE_EVERY` | 100 ms, 10 ticks | How soon `serve` sees Ctrl-C, and a reinstalled binary |

## Baseline

The fixed selection rule, the default (and the learned policy's wherever it has no evidence to deviate and its gate admits no test).
For a session that had none of the lessons already, these values decide exactly as 0.5.0 did. They are
the reference the learned policy is measured against, not knobs to tune.

| File | Name | Value | Meaning |
|---|---|---|---|
| `borg/src/conscience.rs` | `CAP` | 10 | Lessons per stop block, and per free hand-over (`graded_capped`); also bounds a block's length |
| `borg/src/conscience.rs` | `BLOCK`, `LOW` | 0.8, 0.3 | The word-matching rule (`BORG_MATCH=bm25`): the stop blocks when a lesson not done scores this share of the best match, and then asks every lesson at the lower share |
| `borg/src/conscience.rs` | `GENERIC_CAP` | 3 | Unscoped lessons per block |
| `borg/src/conscience.rs` | `RELATIVE`, `EARLY_CAP` | 0.5, 5 | Early delivery: lessons at this share of the best on-topic match, at most 5; kept until a learned early rule has data (each early record logs its lessons' share, `rel`) |
| `borg/src/conscience.rs` | `STRONG` | 0.8 | A dormant lesson needs this share of the best match to be recalled |
| `borg/src/whole.rs` | `TOKENS` | 12,000 (`BORG_STOP_POOL_TOKENS`) | The grading pool's budget the internal evaluation "whole-store" passed with; the fixed rule keeps it, and the learned pool starts from it |
| `borg/src/vectors.rs` | `SHORTLIST`, `DEFAULT_K` | 30, 10 (`BORG_VECTOR_K`) | BM25 lessons and vector additions before the pool (internal evaluation "vectors"); with the whole-store pool, the order the pool overflows in |
| `borg/src/live.rs` | `DEBOUNCE_S` | 45 s (`BORG_LIVE_DEBOUNCE_S`) | Least time between the starts of two live checks, the value live mode passed its evaluation with; also guards against a check per tool call |

Design choices outside selection. Nothing evaluated or learned replaces them yet; each is the reference a
later value would be compared against.

| File | Name | Value | Meaning |
|---|---|---|---|
| `borg/src/memory.rs` | `VETOED_W`, `REGRET`, `TESTED_W` | 3, 1.0, 0.5 | What a veto, a regret and a `tested` result add to a lesson's belief. `borg debug ope` estimates alternatives offline and never applies them |
| `borg/src/outcome.rs` | `CORRECTED_W` | 2 | A correction counts twice an ignore (outcome.rs `weight` and memory.rs alike) |
| `borg/src/memory.rs` | `D` | 0.5 | ACT-R's base-level decay, the literature's value, not fitted |
| `borg/src/memory.rs` | `D_CONSOLIDATED`, `CONSOLIDATE_SESSIONS`, `DORMANT_BELOW` | 0.3, 3 sessions, −2.0 | Slower decay once consolidated; dormant after about 54 sessions without a use |
| `borg/src/memory.rs` | `NOVEL_WORDS` | 3 | Without a declaration, a change needs this many of the lesson's distinctive words new in the work |
| `borg/src/trust.rs` | `NEUTRAL` | 0.5 | Trust without evidence |
| `borg/src/escalation.rs` | `STAMP_W` | 1.0 | The loss of a rubber stamp against a missed escalation (`MISSED_W`), not money |
| `borg/src/queen.rs` | `STATED` | 0.6 | The share of a decision's words already in the prompts that means the inputs state it (the same 0.6 as `DUPLICATE` by coincidence) |
| `borg/src/expert.rs` | `MAX_CLAIMS`, `NEWER_MIN_SHARE` | 3, 0.25 | Claims per expert section; the share of the best claim a newer-release change needs for the reserved slot |
| `borg/src/learn.rs` | `MERGE_CANDIDATES` | 5 | The most similar lessons offered to the learn model for a merge (`similar`), the count since 0.1.0 |
| `borg-mesh/src/sync.rs` | `REPLICAS` | 2 | Members besides the author holding each reasoning excerpt, so it outlives one member going away |
| `borg/src/concepts.rs` | `BETA` | 0.5 (`BORG_CONCEPTS_BETA`) | The concept boost's untuned start; an experiment parameter while `BORG_CONCEPTS` is off |
| `borg/src/update.rs` | `STALE_MS` | 24 h (`clock::DAY_MS`) | How old `update.json` may be before a prompt starts another release check: a release a day late is news enough, and an idle prompt reads one small file |
| `borg/src/update.rs` | `FAILING_WARN_MS` | 3 days (`3 × clock::DAY_MS`) | How long the release check may keep failing before `borg doctor` warns: an offline day is no news, a check that never works is |

## Display

| File | Name | Value | What is shown |
|---|---|---|---|
| `borg/src/presence.rs` | `IDLE_MS` | 30 min | A session without a hook this long shows as idle; nothing paces, prunes or decides by it |
| `borg/src/presence.rs` | `FILES`, `PROMPT_CHARS` | 3 files, 100 chars | A presence record's recent files and prompt |
| `borg/src/top.rs` | `RECENT` | 5 | Messages `borg top` lists |
| `borg/src/top.rs` | `MESSAGE_CHARS` | 120 chars | One message line on `borg top`'s printed screen; `borg inbox --show` prints the whole text |
| `borg/src/top.rs` | `COLUMN_NODE_CHARS` | 6 chars | A node key in the drone table's column (top.rs and top_live.rs) |
| `borg/src/top.rs` | `QUANTILES` | 0.50, 0.95 | The stop latency quantiles the selection line and the page show, the median and the tail a stop budget must hold |
| `borg/src/msg.rs` | `NODE_CHARS`, `SESSION_CHARS` | 12, 8 chars | A node key in every label of a node (`short_node`: a delivered line, steer.rs, presence.rs, top.rs, borg-mesh main.rs), and the session id of a sender that gave no name (`sender_label`) |
| `borg/src/clock.rs` | `ago` | `45s`, `12m`, `3h05m`, `2d` | A relative time in at most 6 characters (msg.rs, top.rs, top_live.rs); borg-web's ui has its own copy |
| `borg/src/stats.rs` | `FEED_CHARS` | 160 chars | An event's text in the feed and borg web |
| `borg/src/update.rs` | `INSTALL_SH` | install.sh's one-line `curl … \| sh` | The install for a machine without `borg update`, in the commands `borg update` prints when it cannot install |
| `borg/src/protocol.rs` | `UPDATE_COMMANDS` | `borg update`, else install.sh or `cargo install … --force` | The update commands the announcement offers, for an install of 0.7 or newer and for an older one |
| `borg-web/src/lib.rs` | `PAGE` | 200 rows | A listing without `limit` (`/api/feed`, `/api/messages`) |
| `borg/src/listen.rs` | `NAMES` | 3 | Senders a wake names |
| `borg/src/notify.rs` | `BODY_CHARS` | 120 chars | A notification's question |
| `borg/src/why.rs` | `MAX_COMMITS`, `MAX_LESSONS`, `TASK_CAP`, `PROPOSAL_CAP`, `LESSON_CAP` | 8, 3, 240, 400, 240 chars | `borg why`'s compact form |
| `borg/src/why/linked.rs` | `MAX_LINKS` | 3 | Linked decisions per direction |
| `borg/src/notes/links.rs` | `MAX_LISTED` | 10 | Commits per note section |
| `borg/src/expert.rs` | `CLAIM_CHARS` | 210 chars | One claim line; three with their citations stay near 200 tokens |
| `borg/src/select/replay.rs` | `SHOW` | 5 | Differences `borg debug select` lists per check |
| `borg-trace/src/dialogue.rs` | `TOOL_CHARS` | 300 chars | A refused call's input |
| `borg-trace/src/fingerprint.rs` | `LINE_CHARS` | 200 chars | A scar's error line |
| `borg-trace/src/scars.rs` | `FIX_EDITS`, `RECALL_CHARS` | 5 files, 160 chars | A scar's fix and its recall line |
| `borg/src/queen.rs` | `QUOTE_CHARS` | 300 chars | The text a queen question quotes: a plan's sentence (queen.rs), a lesson's question (reach.rs), an invariant (veto.rs) |
| `borg/src/veto.rs` | `EVIDENCE_CHARS` | 200 chars | A veto's words in a constraint's evidence |
| `borg/src/escalation.rs` | `ANSWER_CHARS` | 120 chars | An escalation's answer in the feed |
| `borg/src/reach.rs` | `PLAN_CHARS` | 110 chars | A line of `borg reach`'s plan |
| `borg/src/conscience.rs` | `EARLY_QUESTION_CHARS`, `EARLY_CONSEQUENCE_CHARS`, `EARLY_OVERRIDES_CHARS` | 300, 200, 150 chars | An early lesson's question, its consequence and the team rule it replaces (`render_early`) |
| `borg-web/ui/src/views/messages.rs` | `SHOWN` | 60 messages | The conversation the Messages view lists, newest first |
| opencode adapter | `LOG_CHARS`, `LOG_ARGS` | 200 chars, 2 arguments | What a line of `$BORG_OPENCODE_LOG` keeps of borg's answer and of the command it ran |

## Experiment

| File | Name | Value | Evaluation |
|---|---|---|---|
| `borg-eval/src/preempt.rs` | `EXPERT_TIMEOUT_S`, `PLAN_CHARS` | 300 s, 24,000 chars | The preempt eval's expert broker and the plan it hands over |
| `borg-search/src/bin/hs.rs`, `hs-bm25.rs`, `hs-facets.rs`, `hs-concepts.rs` | `TOP` | 20 | Hybrid-search runs |
| `borg-search/src/bin/hs-vectors.rs` | `SHORT`, `WIDE`, `VECTOR_TOP` | 20, 30, 60 | Vector shortlist arms |
| `borg-search/src/bin/hs-whole.rs` | `BM25_N` | 30 | Whole-pool arms |
| `borg-search/src/seek.rs` | `CHUNK` | 1,000 | Model2Vec's chunk size, SeekStorm's README example |
| eval switches | `BORG_STOP_ASK_CAP`, `BORG_STOP_ATTENTION=on` | | Eval arms; `BORG_SELECT=fixed` wins over the cap, which wins over attention, and both over the learned policy |
| `eval/` | each script | | Each evaluation's parameters stay with its scripts and README |

## Values that share a number

Each is defined once; where two constants share a value, they mean different things and each is
labelled.

| Value | Where | Meaning |
|---|---|---|
| 10 | conscience.rs `CAP`; hook.rs `MAX_PENDING` (derived from it) | One block; one judge call's lessons, so a block's lessons share their evidence |
| 4,000 chars | hook.rs `EVIDENCE_CHARS` (and `LIVE_WORK_CHARS` from it); harness/mod.rs `OUTPUT_TAIL` | Judge evidence; a shell call's output tail |
| 500 | steer.rs `KEEP`; borg-mesh `V1_KEEP` | Steering admissions; 0.5.0's message bound for `/13` peers |
| 24 h | clock.rs `DAY_MS`, and from it presence.rs `PRUNE_MS`, steer.rs `TTL_MS`, outcome.rs `IDLE_MS` (guards), update.rs `STALE_MS` (baseline) | One unit, three guards and a release check's age |
| "Idle" | presence.rs `IDLE_MS` 30 min (display), listen.rs `TURN_IDLE_MS` 5 min (a learned hold's prior), outcome.rs `IDLE_MS` 24 h (a session's end) | Three meanings, three names |
| 2,000 | conscience.rs `EARLY_CHARS`, learn.rs `PROPOSAL_CHARS`, borg-trace `REFUSAL_CHARS` (chars); notes/regress.rs `Cfg::default` `max_lines` (lines); borg-web `PAGE_MAX` (rows) | Different bounds, each labelled |
| 160 / 240 / 300 chars | stats.rs `FEED_CHARS`, whole.rs's line, why.rs caps, facets, queen.rs `QUOTE_CHARS`, conscience.rs `EARLY_QUESTION_CHARS` | Each labelled |
| 120 / 200 chars | top.rs `MESSAGE_CHARS`, escalation.rs `ANSWER_CHARS`, notify.rs `BODY_CHARS`; veto.rs `EVIDENCE_CHARS`, conscience.rs `EARLY_CONSEQUENCE_CHARS`, the opencode adapter's `LOG_CHARS` (display), assimilate.rs `NOTICE_CHARS` (method) | Each labelled |
| 2 per check | select.rs `PLACEBO_SLOTS`, whole.rs `TAIL_SAMPLE` | The judge's extra lessons; the grader's |
| 64 | msg.rs `WINDOW_CAP` (derived), listen.rs `GAPS_KEPT` (method), sweep.rs `HASHED` (method, bytes), assimilate.rs `OPEN_CALLS` (guard, calls) | A window's ceiling; the gaps a hold reads; the bytes whose hash says a transcript is unchanged; the open tool calls kept between sweeps |
| 10 min | msg.rs `V1_RATE_MS` (protocol), listen.rs `TOOL_MS` (a learned hold's floor), update.rs `CHECKING_MS` (guard) | 0.5.0's message rate; a tool call's hold; one release check at a time |
| 10⁻⁹ | select/replay.rs `CLAMP` (guard), `COIN` and `SAME` (method) | A probability kept off 0 and 1; a prediction that is 1/2; two recalls that are equal |
| 300 and 30 | select/replay.rs `READOUT_UNITS`, `READOUT_SESSIONS`; `TEST_CHECKS`, `TEST_SESSIONS` (method) | The online readout's labelled units and sessions; the question-count test's checks and sessions |
| 0.6 and 0.8 | queen.rs `STATED` and `DUPLICATE`; conscience.rs `STRONG` and `BLOCK` | Same value, different bars |
| Shortlist sizes | vectors.rs `SHORTLIST` 30; borg-search `SHORT` 20, `TOP` 20, `BM25_N` 30 | Production is baseline, the binaries are experiment |
| codex hook timeouts | harness/codex.rs `TIMEOUTS`, `adapters/codex/smoke.sh` | The smoke test checks what install writes |
| Budgets | harness/omp.rs and opencode.rs `BUDGETS`, the adapters' `FALLBACK` | The adapters' copy is a fallback; a test keeps them equal |
| A day in the page | clock.rs `DAY_MS`, borg-web ui `DAY_MS` | The ui is a wasm workspace of its own |

## What 0.6.0 changed

- **Removed:** msg.rs `RATE_MS` (a display threshold, presence `IDLE_MS` / 3, reused as a send rate),
  `TEXT` (1,000 chars, now `V1_TEXT` beside `WIRE_TEXT`), `KEEP` (500 lines, now `MEMBER_BYTES`),
  `PER_EVENT` and `EVENT_CHARS` (10 messages and 4,000 chars per event, now `Caps::context_chars`);
  expert_route.rs `DRONE_BUDGET_USD` (the drone's $0.50 spend limit; its time limit bounds it); the
  opencode adapter's `WAKE_RETRY_MS` and `WAKE_TRIES` (the retry is `borg listen --stream`'s `backoff`)
  and the omp adapter's `FAST_MS`, `PRE_MS`, `STOP_MS`, `END_MS` (now borg's `BUDGETS`).
- **Learned now:** listen.rs's holds (`TURN_IDLE_MS`, `QUEUE_IDLE_MS`, `TOOL_MS` are floors, learned
  upward); under the learned policy, the default since Rules v2 passed, which graded lessons are
  asked, free or not now (the value model under the gate: a deviation only where it beats the fixed
  rule at `DELTA`'s quantile) and the pool size, and on live checks the question count, by a randomised test the gate rations by
  the collective's `risk` (default `ALPHA`). Until the gate admits a deviation the fixed rule's `CAP`,
  `TOKENS` and the rest of the baseline decide, and `BORG_SELECT=fixed` keeps them; the placebo and the
  tail sample measure what the learned values need under it.
- **Changed prior:** select/policy.rs `DILUTION_PRIOR` from N(−0.1, 0.1²) to N(0, 0.1²), both at most
  0 (Rules v2, 2026-10-29: the dated estimate's interval includes 0).
- **Derived now:** steer.rs `SLACK_MS` and borg-mesh live.rs's presence freshness from `SKEW_MS`; every
  day from clock.rs `DAY_MS`; the stop budgets of omp and opencode in `Caps`; listen.rs `stop_ms` from
  the longest stop; hook.rs `MAX_PENDING` from `CAP`; the excerpt bounds from learn.rs's.
- **Defined once:** clock skew, the day, BM25's parameters, characters per token, the failed-call limit
  (runner.rs `MAX_FAILURES`, which `borg reach` now honours too), the job count (runner.rs `JOBS`), the
  correction weight (outcome.rs `CORRECTED_W`), SeekStorm's segment bits, the feed excerpt, the stop
  timeout (harness/mod.rs `STOP_TIMEOUT_S`), the `ago` and calendar routines (clock.rs; borg-web's ui,
  a wasm workspace of its own, keeps a copy), 0.5.0's message rate (msg.rs `V1_RATE_MS`, which the mesh
  imports), the label lengths of a node and a session (msg.rs `NODE_CHARS`, `SESSION_CHARS`), the
  lesson quote (queen.rs `QUOTE_CHARS`), the per-arm order (select.rs `ARMS`).
