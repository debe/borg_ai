# SWE-chat engineering corrections: test-set candidates

Purpose: a test set of real engineering corrections (design, concurrency, data model, API,
logic, security, error handling, performance), kept apart from agent steering. Nothing here
goes into Borg's product.

Scripts: `eval/swe-chat/engineering/`. Data: `~/.local/share/borg/eval/swe-chat/engineering/`.
Spend: $4.37 on `claude -p --model haiku` (claude-haiku-5-5), under the $5 cap. Every call is logged in `calls.jsonl`, including a $0.024 estimate for four in-flight calls killed at a restart.

## Method

1. **Prompts.** I took all `correction` and `rejection` user prompts from `conversations.parquet`, read column by column with only nine columns. That gives 66,855 prompts across 10,963 sessions and 641 repos. Each prompt keeps the tail of the previous agent message and the files the agent edited in the turns before and after it. I deleted the temporary local copy of the file afterwards.
2. **Authorship signal** (0 tokens). Entire's `prompt_attributions` is keyed by checkpoint, not by prompt. Mapping checkpoint k to the k-th editing turn only works where the session's entry count equals its count of editing turns, which is true for about 30% of sessions (1,182 prompts). In those sessions, 505 prompts have the human editing lines at the correction's checkpoint or the next one (`s1>=2`). Separately, the agent re-edited the same files right after the correction for 5,949 prompts. That is weak evidence and does not count as corroboration.
3. **Notre Dame labels** (0 tokens). All 5,785 SWE-chat sessions in `session_mapping.json` map to SWE-chat `session_id`s through `session_sha`. Labels are per session, so I matched each episode's quoted USER evidence back to the prompt text. That matched 2,818 prompts. Of these, 1,764 are engineering-like (S1 wrong diagnosis, S3 constraint violation, S5 faulty implementation) and 1,051 are steering-like only (S2, S4, S6, S7).
4. **Keyword prefilter** (0 tokens). English and German engineering vocabulary plus code-shaped tokens let 23,500 prompts through. Together with the strong signals from steps 2 and 3, that makes 24,733 candidates.
5. **Haiku** labelled the top 16,923 candidates by priority: 6,356 engineering, 6,427 steering, 4,140 unclear. Each label comes with a category, a repo-agnostic concept and a one-line paraphrase. A second Haiku pass, run once per category, merged the 6.2k free-text concepts into canonical principles.
6. **Combine.** An engineering correction needs a Haiku `engineering` label, an available transcript, and at least one of: `s1>=2`, an ND engineering-like label, or long specific text. That gives 5,680 cases.
   - **Quality tier:** `s1>=2`, an ND label, or at least 40 words with at least 2 code tokens and at least 2 keyword hits.
   - **Pasted text dropped:** pasted review reports, findings tables, logs and CodeRabbit output are removed from the tier. They were 2,512 of 3,803 cases.

## Counts

| stage | n |
|---|---|
| correction + rejection prompts | 66,855 |
| keyword pass / candidates | 23,500 / 24,733 |
| Haiku labelled | 16,923 (6,356 eng / 6,427 steer / 4,140 unclear) |
| engineering + corroborated + transcript | 5,680 (318 repos, 323 users; 4,252 with linked commit) |
| **quality tier** (no pastes) | **1,291** (419 strong signal; 186 repos, 194 users; 998 with commit; 277 compact transcript) |

- **Agents** (5,680 set): Claude Code 4,705, Codex 559, Pi 188, OpenCode 94, Cursor 57, other 77.
- **Repo skew:** `entireio/cli` has 1,523 cases in the 5,680 set; the next largest repo has 311.

## Cross-repo (quality tier)

- **Concepts:** 982 canonical concepts, of which 123 appear in at least 2 repos.
- **Transfer cases:** 203 cases have a concept that appeared earlier, in time, in a different repo's quality-tier correction.
  - 149 of the 203 have a linked commit.
  - 158 of the 203 are outside `entireio/cli`.
- **Undercount:** concepts were merged within each category only, so the same idea filed under two categories is not joined.

| concept | repos | cases |
|---|---|---|
| extract duplicated logic into shared helper | 7 | 18 |
| keep one source of truth per value | 7 | 8 |
| include all discriminating fields in identity keys | 7 | 7 |
| replace silent fallbacks with loud failures | 5 | 11 |
| clean up resources on every exit path | 5 | 8 |
| tests must exercise real production path | 5 | 7 |
| apply the same fix to every parallel path | 4 | 4 |
| replace hardcoded values with configuration | 4 | 4 |
| replace stringly-typed values with enums | 4 | 4 |

The test needs about 150 cases. There are 203 cross-repo cases, 149 of them with a commit, which is enough.

## Caveats

- **Pasted reviews.** Many "corrections" are review output from bots or other agents that the user pasted in. The tier filters them out with a regex, so a few remain, for example #8, #12 and #18 below.
- **Unchecked authorship.** The authorship signal covers only sessions whose checkpoint mapping is exact, and nobody has verified it by hand.
- **Unchecked labels.** Haiku's labels have not been checked against a human sample. This file is that sample.

## Sample for review

The 25 engineering cases come from the quality tier, with each concept also present in another repo, one case per repo and at most 4 per category.

| # | repo | category / concept | signals | correction (trimmed) | agent had |
|---|---|---|---|---|---|
| 1 | entireio/cli | logic / run required side effects on every execution path | human-edit,long | Cursor said: "Empty skip hides suspension hint Medium Severity When created.Empty is true, mirror create returns immediately and never runs waitForMirrorClone. For an existing placement (created false), that bypasses the repo-scoped token exchange that trigger ... | Committed the repo mirror create change with an early return for empty upstreams and offered to push or open a PR. |
| 2 | oddessentials/ado-git-repo-insights | types / validate external data into typed models | ND:constraint_violation,long | Let's keep the scope controlled but I will not stand for this current slop. Fix it now. * Pre-push hook execution is not reliably enforced; add a hard-fail mechanism with guaranteed visible output to ensure preflight cannot be silently bypassed. * CI is runnin ... | Planned a pre-commit tsc hook and fixes for Object.keys narrowing, to be pushed as one commit. |
| 3 | dayhaysoos/nimbus | data_model / include all discriminating fields in identity keys | ND:constraint_violation,inaccurate_self_report,long | Update the Phase 1 spec with these three changes: 1. **D1 cache composite key** — change `review_cochange_cache` to use a composite primary key of `(file_path, repo)` instead of `file_path` alone. Add `repo TEXT NOT NULL` column (value is the owner/name slug, ... | Said the spec file exists locally and offered to commit and push it. |
| 4 | osabiohq/osabio | data_model / replace legacy fields fully and update references | ND:wrong_project_diagnosis,long | "I see the user wrapped the migration back into a single transaction. That will fail for the same reason — DEFINE FIELD + UPDATE in the same transaction on SCHEMAFULL" this works fine, i just executed it ... why is agent_type still on authority_scope ? | Claimed the single-transaction migration would fail on SCHEMAFULL DEFINE FIELD plus UPDATE. |
| 5 | CPS-IT/quality-tools | api_design / replace boolean flags with named modes or options | ND:constraint_violation,overreach,long | 1. naming: use "configuration_file". Reason: clarity. 2. Boolean Disable: rejected. Reason: unnecessary complexity. custom config is enabled either explicitly by providing a custom path or implicitly by putting a file in place. the "(auto-discovered)" clarifie ... | Proposed a transparent config design with auto-discovery and a config:show view. |
| 6 | lassejohannis/TechEurope | architecture / replace hardcoded values with configuration | long | 4. Autonome Ontologie-Evolution (Paul) Migration 008: type_config-Tabellen erweitern (auto_proposed, approval_status, proposed_by_source_id, similarity_to_nearest, from_type, to_type) propose.classify_or_propose_type(payload, kind) Funktion Embedding-Similarit ... | Proposed hardcoded-style improvements and asked whether to implement them. |
| 7 | ejpir/serval | types / align literal values with declared types or spec | rework,long | • 1. High: Task 4 has an internal type mismatch that will produce incorrect implementation. consecutive_mutex_timeouts is later defined as std.atomic.Value(u32), but the lock code snippet still uses plain integer ops/assignment (= 0, += 1, compare directly). T ... | Documented a fix that uses tee and PIPESTATUS for the zig exit code, then asked whether to execute subagent-driven or in ... |
| 8 | malpercio-dev/ezpds | architecture / extract duplicated logic into shared helper | long | `decode_canonical_base64url` (and the `SIGNATURE_BYTES`/`NONCE_BYTES` constants beside it) is now duplicated verbatim in three PDS route files: - crates/pds/src/routes/sovereign_session.rs - crates/pds/src/routes/oauth_consent.rs - crates/pds/src/routes/delete ... | Agent had no prior text; user pasted a review noting the duplicated security check and proposing a shared module. |
| 9 | dandi/pydantic2linkml | function_impl / extract duplicated logic into shared helper | long | let's take a look at the PR at https://github.com/dandi/pydantic2linkml/pull/59. Let me know what you think. The branch of the PR is already checked out locally. In the docstring of the constructor of `SlotUsageGenerationError`, I think there need to be a ment ... | Reviewed PR 59 and was asked to update the constructor docstring, deduplicate the sorted-attribute logic, and address te ... |
| 10 | stale2000/git-sync | security / bind credentials to exact origin | rework,long | 1. Medium: credential-helper lookups are keyed to the original remote, not the host that actually returned 401, so redirected smart-HTTP setups can miss or poison credentials. In internal/gitproto/smarthttp.go:191, the retry path calls Lookup/Approve/Reject wi ... | Reported a clean cleanup commit and efficiency sign-off without addressing redirect host keying or POST auth fallback. |
| 11 | Shadylukin/Neumann | architecture / route data access through one service layer | long | Enter plan mode: Server surfaces bypass the router. REST vector endpoints call VectorEngine directly through VectorApiContext at neumann_server/src/rest/mod.rs (line 37), while query gRPC goes through QueryRouter. That creates different auth, cache, durability ... | No prior agent text shown; the user asked the agent to enter plan mode for this change. |
| 12 | melagiri/code-insights | architecture / share one implementation instead of duplicating logic | long | **🟡 SUGGESTION — Duplicate Ollama model discovery logic** `ollama-detect.ts:queryOllamaModels()` reimplements the same `fetch /api/tags` + 3s timeout + silent catch pattern that already exists in `server/src/llm/providers/ollama.ts:discoverOllamaModels()` (lin ... | No agent response shown; the review flags duplicated Ollama discovery code in the CLI. |
| 13 | mseele/sve-backend | types / replace stringly-typed values with enums | long | let's improve Primitive Obsession. sepa_lead_days(seq_tp: &str) matches on raw strings for a SEPA domain concept (sequence type). A small enum would be more honest: fn sepa_lead_days(seq_tp: SepaSequenceType) -> u32 Mysterious Name. The closure in test_is_sepa ... | Reported a standards review with one hard violation and three smells, with no spec. |
| 14 | chikachow/cyspbot | security / bind identity to immutable identifiers | long | [74] tool exec call: const patch = "*** Begin Patch\n*** Update File: /Users/cysp/src/cyspbot/docs/service-contract.md\n@@\n-- `sub` is either the expected legacy repository/ref form or the immutable form constructed from the signed repository and owner ID cla ... | Agent's contract defined sub as constructed from the signed repository and owner ID claims. |
| 15 | entireio/entire-graph | data_model / namespace or scope identifiers to avoid collisions | long | Another entire-brain run reports this: Validated with fresh local builds of both repos. Result: - The original `entire-sem` fixes are partially validated: - `doctor --json` now reports `no_egress: true`. - `snapshot --repo ... --format ndjson --no-network` now ... | Reported that the agentviz snapshot validated, with 877 symbols and 3490 relations. |
| 16 | bgauduch/terraform-examples | logic / verify external api docs before asserting limitations | long | # Health of the deployed key itself, re-read from AWS rather than from state, so # a key disabled or scheduled for deletion out of band shows up on the next plan. check "key_operational" { data "aws_kms_key" "deployed" { key_id = aws_kms_key.app.key_id } asser ... | Added a Terraform check asserting the KMS key state is Enabled, with an error message claiming non-Enabled states break ... |
| 17 | jukellam/dispersal-draft | types / replace stringly-typed values with enums | long | **Lesson — Avoid: Hardcoded strings for enums (Fix #017)** Trading replaced hardcoded `"DISPERSAL"` / `"STARTUP"` strings with `DraftType(str, Enum)`. The simulation plan uses string literals like `"active"`, `"complete"` in comparisons (`state["draft_status"] ... | The plan used string literals for draft status comparisons. |
| 18 | bryanhiestand/ferrous-comics | security / validate endpoint urls before use | long | **4. `comic.img` URL is used directly in `<img src>` without validating scheme (minor injection risk)** The image URL comes from the xkcd API and is passed through `escape_html`, which neutralizes HTML attribute injection characters. However, `escape_html` doe ... | Previous turn had no visible output; the security finding was pasted as the correction. |
| 19 | Diogenesoftoronto/keating | security / default-deny for unclassified tools or roles | long | Wave 4 security remediation A: shared fail-closed tool authorization. Work directly in your forked workspace. You are not alone; preserve all edits. Own web/src/keating/security/**, web/src/keating/integration/voice-permissions.ts, web/src/keating/browser-tool ... | No response text was shown. |
| 20 | manderson240/cohezion | error_handling / replace silent fallbacks with loud failures | long | **4. Connection test** ``` === Connection test === ❌ Failed to connect to SurrealDB: {'code': -32002, 'details': {'details': {'kind': 'InvalidAuth'}, 'kind': 'Auth'}, 'kind': 'NotAllowed', 'message': 'There was a problem with authentication'}. Falling back to ... | Agent's previous message tail was a connection test showing an auth failure and diagnosing that credentials were not loa ... |
| 21 | spiceai/spiceai | logic / do not silently drop inputs, values, or results | long | At `crates/runtime/src/dataaccelerator/partitioned_arrow.rs:L128` The _source parameter in create_external_table is intentionally ignored, but the PARAMETERS declaration lists hash_index and sort_columns as supported parameters. In ArrowAccelerator, these para ... | Ignored the _source parameter in PartitionedArrowAccelerator.create_external_table while still listing hash_index and so ... |
| 22 | yash-learner/care_fe | performance / hoist loop-invariant work out of loops | long | Actual issues found (see inline comments): 1. Performance: O(n2) tree walks (`conditionSummary.ts:50`) — `questionsByLinkId` is built fresh on every `shortConditionSummary` and `plainWordsSummary` call. Per canvas render, per question block. The comment even w ... | Proposed to run a multi-dimension adversarial review of the full PR diff with fixes. |
| 23 | entireio/git-sync | api_design / docs must match actual runtime and validation behavior | rework,long | 1. Medium: the public/API contract for `AllRefs` still overstates what the library and unstable `fetch` path actually do. `RefScope` says `AllRefs` broadens scope “to every `refs/*` on the source” ([types.go](/Users/soph/Work/entire/devenv/git-sync/types.go:81 ... | Made --all-refs imply --tags in the CLI and documented the per-command bundling, then asked whether to push. |
| 24 | MohammedMqat/week-5-project-express | api_design / verify server contract before shipping client | long | The error is data.data is undefined — which means renderAnime received something, but it didn't have a .data array inside it. Question 1: Look at public/app.js line 4. What URL is it fetching? Question 2: Now look at src/router.js line 16. What does that route ... | Asked the user to add a console.log to see the shape of the response before assuming the cause. |
| 25 | obsessiondb/rudel | performance / move optional probes and refresh off critical path | long | the activeOrg is somehow in the criticalPath, is there a possibility to cache this value in localStorage? Also how are we determining the "activeOrg" in the backedn for the api requests? How can we do all of this more performant, right now the loading of the a ... | Gated the dashboard query on org loading, showing a spinner until the org resolves. |

The 10 rejected-as-steering cases are prompts with at least 2 engineering keywords that Haiku labelled steering.

| # | repo | correction (trimmed) | why steering (Haiku paraphrase) |
|---|---|---|---|
| 1 | zchee/agentmux | <state_management> OMX persists runtime state under `.omx/`: - `.omx/state/` — mode state - `.omx/notepad.md` — session notes - `.omx/project-memory.json` — cross-session memory - `.omx/plans/` — plans - `.omx/logs/` — l ... | Pasted OMX state-management instructions: persist mode state under .omx and follow start, update, complete, and cancel lifecycle rules. |
| 2 | basher83/tailnet-microservices | anthropic-auth/src/token.rs` contains tests that deliberately send bogus credentials to Anthropic’s real token endpoint - i feel thats a overstatement | Says calling the token.rs tests that send bogus credentials to the real endpoint is an overstatement. |
| 3 | jeffs-brain/phone | we need to now look to refactor some of these files that have gotten a bit out of hand, the index.tsx and settings.tsx for example along with lib files and components that should be created. so lets refactor, clean up, r ... | Refactor oversized files, remove dead code, fix bugs, and get to a clean production-ready base using parallel subagents. |
| 4 | zchee/zmux | <state_management> OMX persists runtime state under `.omx/`: - `.omx/state/` — mode state - `.omx/notepad.md` — session notes - `.omx/project-memory.json` — cross-session memory - `.omx/plans/` — plans - `.omx/logs/` — l ... | Pasted OMX state-management instructions: persist mode state under .omx and follow start, update, complete, and cancel lifecycle rules. |
| 5 | MohammedMqat/week-5-project-express | const container = document.getElementById("container"); fetchAnime().then(renderAnime); function fetchAnime() { return fetch(`/api/entity/top`).then((response) => response.json()); } function renderAnime(data) { data.dat ... | Clarifies the client fetches /api/entity/top, not /api/anime/top, and that they meant entity. |
| 6 | entireio/cli | I want options to return the list filtered by author and/or status. Default should be "me" and in_progress. Recommend a parameter design. | Add options to filter the trail list by author and status, defaulting to me and in_progress, and recommend a parameter design. |
| 7 | vanmarkic/AI-Boilerplate | help me design the perfect user flow for creating a scenario tin tfc from scratch. don't look at the current code. look a the specs, the md and pdf docs and the scenario data model. | Requests a designed user flow for creating a scenario from scratch, using specs and docs rather than current code. |
| 8 | lue-labs/pi-mono | then instead of activating dream-memory now, lets first fixt the system prompt and tool schemas and skill loading etc. | Don't activate dream-memory yet; first fix the system prompt, tool schemas, and skill loading. |
| 9 | JanDeDobbeleer/oh-my-posh | The invert power line bug I talked about is not a bug, that's an issue in the team configuration file. Before we continue, I want you to have Fable 5 execute an analysis on all of the code changes, how we designed, etc., ... | Says the invert powerline issue is a team config problem, not a bug, and asks for an analysis of code simplification and deduplication. |
| 10 | dylan-112/OpenACP | checkout sang nhánh này redesign/microkernel-plugin-architecture. Từ upstream của nhánh redesign/microkernel-plugin-architecture | Check out the redesign/microkernel-plugin-architecture branch tracking its upstream. |

