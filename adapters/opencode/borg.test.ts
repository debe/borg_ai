// Contract test: opencode-shaped hooks/events go into the plugin, the plugin's canonical JSON goes
// into `borg hook opencode`, and borg's answer comes back into the session.
//
//   cargo build -p borg-ai && node --test adapters/opencode/
//
// Node >= 23.6 runs the TypeScript directly. BORG_TEST_BIN overrides the borg binary under test.

import { test } from "node:test"
import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { mkdirSync, mkdtempSync, writeFileSync, readFileSync, existsSync, chmodSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import plugin from "./borg.ts"
import { fakeClient, fakeCtx } from "./fakes.ts"

const BORG = process.env.BORG_TEST_BIN ?? resolve(import.meta.dirname, "../../target/debug/borg")

const SESSION = "ses_contract1"
const PLAN = "I check closed.get() on the AtomicBoolean before deliver(); reader and drainer threads race."

async function load(client: any, dir: string) {
  return plugin.server({ client, directory: dir } as any)
}

const userMessage = (text: string, extra: object = {}) => ({
  message: {} as any,
  parts: [{ type: "text", text, ...extra }] as any,
})

const idleEvent = { event: { type: "session.idle", properties: { sessionID: SESSION } } as any }
const marked = { metadata: { borg: true } }

/** A fake borg that records `<event>\t<stdin>` per call and answers a stop with a block, like borg
 *  would; $STOP_SLEEP makes that stop slow. Returns [binary, record file]. */
function recorder(): [string, string] {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-"))
  const rec = join(dir, "rec.jsonl")
  const bin = join(dir, "borg-recorder")
  writeFileSync(
    bin,
    `#!/bin/sh\nin=$(cat)\n[ "$3" = stop ] && [ -n "$STOP_SLEEP" ] && sleep "$STOP_SLEEP"\nprintf '%s\\t%s\\n' "$3" "$in" >> "${rec}"\n[ "$3" = stop ] && echo '{"decision":"block","reason":"Q?"}'\nexit 0\n`,
  )
  chmodSync(bin, 0o755)
  return [bin, rec]
}

function recorded(rec: string): [string, any][] {
  if (!existsSync(rec)) return []
  return readFileSync(rec, "utf8").trim().split("\n").map((l) => {
    const [event, json] = l.split("\t")
    return [event, JSON.parse(json)]
  })
}

const toolError = (callID: string, tool: string, input: object, error: string) => ({
  event: {
    type: "message.part.updated",
    properties: { part: { type: "tool", sessionID: SESSION, messageID: "m1", callID, tool, state: { status: "error", input, error } } },
  } as any,
})

test("the plugin speaks borg's canonical event", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")

  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
  await hooks["tool.execute.before"]!({ tool: "read", sessionID: SESSION, callID: "c1" }, { args: { filePath: "/work/repo/relay.rs" } })
  await hooks["tool.execute.after"]!(
    { tool: "read", sessionID: SESSION, callID: "c1", args: { filePath: "/work/repo/relay.rs" } },
    { title: "", output: "", metadata: {} },
  )
  await hooks.event!(toolError("c2", "bash", { command: "rm -rf x" }, "The user rejected permission to use this specific tool call with the following feedback: use git clean instead"))
  await hooks.event!({ event: { type: "session.idle", properties: { sessionID: SESSION } } as any })
  await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })

  const calls = recorded(rec)
  // pre_tool is not queued behind the session's other calls (it must answer before the tool runs).
  const pre = calls.find((c) => c[0] === "pre_tool")![1]
  const queued = calls.filter((c) => c[0] !== "pre_tool")
  assert.deepEqual(queued.map((c) => c[0]), ["prompt", "post_tool", "post_tool_failure", "stop", "end"])
  const [prompt, post, fail, stop, end] = queued.map((c) => c[1])
  const pid = process.pid
  assert.deepEqual(prompt, { session_id: SESSION, cwd: "/work/repo", pid, prompt: "Fix the relay race", last_assistant_message: PLAN })
  assert.deepEqual(pre, { session_id: SESSION, cwd: "/work/repo", pid, tool_name: "Read", tool_input: { filePath: "/work/repo/relay.rs", file_path: "/work/repo/relay.rs" } })
  assert.equal(post.tool_name, "Read")
  assert.equal(post.tool_input.file_path, "/work/repo/relay.rs")
  assert.equal(fail.tool_name, "Bash")
  assert.equal(fail.tool_input.command, "rm -rf x")
  assert.match(fail.error, /want to proceed with this tool use.*the user said:\nuse git clean instead/s)
  assert.deepEqual(stop, { session_id: SESSION, cwd: "/work/repo", pid, last_assistant_message: PLAN, stop_hook_active: false })
  assert.deepEqual(end, { session_id: SESSION, cwd: "/work/repo", pid })
  assert.deepEqual(prompts, [{ path: { id: SESSION }, body: { parts: [{ type: "text", text: "Q?", ...marked } ] } }])
})

test("a paused drone's tool call is refused with borg's reason; anything else lets it run", async () => {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-"))
  const bin = join(dir, "borg-pause")
  writeFileSync(bin, `#!/bin/sh\ncat >/dev/null\n[ "$3" = pre_tool ] && [ -n "$DENY" ] && echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"[borg] paused"}}'\nexit 0\n`)
  chmodSync(bin, 0o755)
  process.env.BORG_BIN = bin
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const call = () => hooks["tool.execute.before"]!({ tool: "bash", sessionID: SESSION, callID: "c9" }, { args: { command: "ls" } })
  await call()
  process.env.DENY = "1"
  await assert.rejects(call, /\[borg\] paused/)
  delete process.env.DENY
  process.env.BORG_BIN = join(dir, "missing")
  await call()
})

test("a paused drone's subagents are refused too: their pre_tool asks as the root session", async () => {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-"))
  const rec = join(dir, "rec.jsonl")
  const bin = join(dir, "borg-pause-root")
  // Denies a tool call when the session asked about is the paused root.
  writeFileSync(bin, `#!/bin/sh\nin=$(cat)\nprintf '%s\\t%s\\n' "$3" "$in" >> "${rec}"\n[ "$3" = pre_tool ] && case "$in" in *'"session_id":"${SESSION}"'*) echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"[borg] paused"}}';; esac\nexit 0\n`)
  chmodSync(bin, 0o755)
  process.env.BORG_BIN = bin
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const created = (id: string, parentID: string) => ({ event: { type: "session.created", properties: { info: { id, parentID } } } as any })
  await hooks.event!(created("ses_child", SESSION))
  await hooks.event!(created("ses_grandchild", "ses_child"))
  for (const id of ["ses_child", "ses_grandchild"]) {
    await assert.rejects(hooks["tool.execute.before"]!({ tool: "edit", sessionID: id, callID: `c-${id}` }, { args: { filePath: "/work/repo/x.rs" } }), /\[borg\] paused/)
  }
  // Another top-level session runs on, and child sessions still send nothing else.
  await hooks["tool.execute.before"]!({ tool: "edit", sessionID: "ses_other", callID: "c3" }, { args: { filePath: "/work/repo/x.rs" } })
  await hooks["tool.execute.after"]!({ tool: "edit", sessionID: "ses_child", callID: "c4", args: {} }, { title: "", output: "", metadata: {} })
  const calls = recorded(rec)
  assert.deepEqual(calls.map((c) => [c[0], c[1].session_id]), [["pre_tool", SESSION], ["pre_tool", SESSION], ["pre_tool", "ses_other"]])
})

test("no stop when the turn was halted: abort, provider error, permission rejected without feedback", async () => {
  const halts = [
    { info: { error: { name: "MessageAbortedError", data: { message: "Aborted" } } } },
    { info: { error: { name: "APIError", data: { message: "overloaded", isRetryable: true } } } },
    { parts: [{ type: "tool", tool: "bash", state: { status: "error", input: {}, error: "The user rejected permission to use this specific tool call." } }] },
  ]
  for (const halt of halts) {
    const [bin, rec] = recorder()
    process.env.BORG_BIN = bin
    const { client, prompts } = fakeClient(() => PLAN, () => halt)
    const hooks = await load(client, "/work/repo")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
    await hooks.event!(idleEvent)
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
    assert.deepEqual(prompts, [], JSON.stringify(halt))
    assert.deepEqual(recorded(rec).map((c) => c[0]), ["prompt", "end"], JSON.stringify(halt))
  }
})

test("an engineer prompt during a slow stop drops the stale questions", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  process.env.STOP_SLEEP = "0.5"
  try {
    let said = PLAN
    const { client, prompts } = fakeClient(() => said)
    const hooks = await load(client, "/work/repo")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
    const stopping = hooks.event!(idleEvent)
    await new Promise((r) => setTimeout(r, 100)) // borg is still thinking about the stop
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("no, use a channel"))
    said = "streaming the reply to the new prompt"
    await stopping
    assert.deepEqual(prompts, [], "questions about the superseded turn are not sent")
    delete process.env.STOP_SLEEP
    await hooks.event!(idleEvent) // the engineer's new task gets its own conscience
    assert.equal(prompts.length, 1)
    const calls = recorded(rec)
    assert.deepEqual(calls.map((c) => c[0]), ["prompt", "stop", "prompt", "stop"])
    assert.equal(calls[2][1].last_assistant_message, PLAN, "the correction is against the proposal the engineer saw")
    assert.equal(calls[0][1].context, undefined)
    assert.equal(calls[2][1].context, false, "a prompt that will not wait for borg's context asks for none")
    assert.equal(calls[3][1].stop_hook_active, false)
  } finally {
    delete process.env.STOP_SLEEP
  }
})

test("an echo that differs from what we sent cannot make the plugin loop", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
  for (let i = 0; i < 4; i++) {
    await hooks.event!(idleEvent)
    // opencode echoes our message, but changed and without the marker.
    if (prompts.length > i) await hooks["chat.message"]!({ sessionID: SESSION }, userMessage(" Q? \n"))
  }
  assert.equal(prompts.length, 1, "one round of questions per engineer prompt")
  assert.deepEqual(recorded(rec).filter((c) => c[0] === "prompt").map((c) => c[1].prompt), ["Fix the relay race"])

  // A marked echo is recognised even when no echo is pending.
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("anything", marked))
  assert.equal(recorded(rec).filter((c) => c[0] === "prompt").length, 1)
})

test("borg's prompt context (a queen question) joins the engineer's message as a synthetic part, once", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` }, async () => {
  const home = mkdtempSync(join(tmpdir(), "borg-home-"))
  process.env.BORG_BIN = BORG
  process.env.BORG_HOME = home
  process.env.BORG_STATE = join(home, "state")
  process.env.BORG_CONFIG = join(home, "no-collectives.json")
  process.env.BORG_MATCH = "bm25"
  mkdirSync(join(home, "queen"))
  const entry = {
    id: "qrepo", decision_key: "open_decision:repo:x", kind: "open_decision", question: "Is the relay queue bounded?",
    options: ["bound it at 1024", "leave it unbounded"], default: "bound it at 1024", evidence: [], session: "seed",
    cwd: "/work/repo", engineer: "e@x", ms: 1,
  }
  writeFileSync(join(home, "queen", "queue.jsonl"), JSON.stringify(entry) + "\n")
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const first = { message: {} as any, parts: [{ id: "prt_a1", sessionID: SESSION, messageID: "msg_1", type: "text", text: "Fix the relay race" }] as any[] }
  await hooks["chat.message"]!({ sessionID: SESSION }, first as any)
  assert.equal(first.parts.length, 2)
  const added = first.parts[1]
  assert.deepEqual([added.id, added.sessionID, added.messageID, added.type, added.synthetic], ["prt_a10", SESSION, "msg_1", "text", true])
  assert.match(added.text, /Start your reply with this one question/)
  assert.match(added.text, /borg' queen answer 'qrepo' '<answer>' --dir '\/work\/repo'/)
  const second = userMessage("now the queue")
  await hooks["chat.message"]!({ sessionID: SESSION }, second)
  assert.equal(second.parts.length, 1, "one question per session")
})

test("fails open when borg is missing", async () => {
  process.env.BORG_BIN = "/nonexistent/borg"
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("hello"))
  await hooks.event!({ event: { type: "session.idle", properties: { sessionID: SESSION } } as any })
  assert.equal(prompts.length, 0)
})

test("real borg: conscience asks once per session, corrections and rejections become episodes", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` }, async () => {
  const home = mkdtempSync(join(tmpdir(), "borg-home-"))
  process.env.BORG_BIN = BORG
  process.env.BORG_HOME = home
  process.env.BORG_STATE = join(home, "state")
  delete process.env.BORG_LEARN
  process.env.BORG_LEARN_MODEL = "off" // episodes are recorded; the `borg learn` they start never calls a model
  // Never the engineer's real collectives config, and no model call at Stop.
  process.env.BORG_CONFIG = join(home, "no-collectives.json")
  process.env.BORG_MATCH = "bm25"
  writeFileSync(
    join(home, "lessons.jsonl"),
    JSON.stringify({ id: "toctou", origin_ms: 1, question: "Is any check-then-act on an AtomicBoolean flag TOCTOU-safe across threads?" }) + "\n" +
      JSON.stringify({ id: "css", origin_ms: 1, question: "Does the stylesheet follow the design tokens?" }) + "\n",
  )
  let said = PLAN
  const { client, prompts } = fakeClient(() => said)
  const hooks = await load(client, "/work/repo")
  const idle = () => hooks.event!({ event: { type: "session.idle", properties: { sessionID: SESSION } } as any })

  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race in relay.rs"))
  await idle()
  assert.equal(prompts.length, 1, "borg's questions go back into the session")
  const reason: string = prompts[0].body.parts[0].text
  assert.match(reason, /\n1\. \[toctou\] Is any check-then-act on an AtomicBoolean flag TOCTOU-safe/)

  // opencode echoes our message through chat.message; it is not the engineer's prompt.
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage(reason, prompts[0].body.parts[0]))
  await idle() // the drone answered the questions: stop_hook_active, no second round
  assert.equal(prompts.length, 1)

  // The engineer pushes back: a correction episode with the drone's proposal.
  said = "I will add a second AtomicBoolean to guard the first."
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("no, don't add another flag, use a channel"))
  // A rejected edit with feedback: a rejection episode.
  await hooks.event!(toolError("c9", "edit", { filePath: "/work/repo/relay.rs", oldString: "a", newString: "b" }, "The user rejected permission to use this specific tool call with the following feedback: keep the public API unchanged"))
  await idle() // a fresh cycle after the engineer spoke, but the only matching lesson was asked already this session
  assert.equal(prompts.length, 1, "a lesson is asked at most once per session")

  const episodes = readFileSync(join(home, "episodes.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l))
  assert.equal(episodes.length, 2)
  assert.equal(episodes[0].task, "Fix the relay race in relay.rs")
  assert.equal(episodes[0].agent, "I will add a second AtomicBoolean to guard the first.")
  assert.equal(episodes[0].correction, "no, don't add another flag, use a channel")
  assert.equal(episodes[1].correction, "keep the public API unchanged")
  assert.match(episodes[1].rejected_tool, /^Edit .*relay\.rs/)
})

// ---- opencode 2.x: the same contract through `setup(ctx)`.

test("a shell call's post_tool carries its output's end and a non-zero exit, other tools none", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const after = (callID: string, tool: string, args: object, output: string, metadata: object) =>
    hooks["tool.execute.after"]!({ tool, sessionID: SESSION, callID, args }, { title: "", output, metadata })
  await after("c1", "bash", { command: "cargo test" }, "test result: FAILED. 1 failed", { exit: 101 })
  await after("c2", "bash", { command: "cargo test" }, "y".repeat(5_000) + "test result: ok.", { exit: 0 })
  await after("c3", "bash", { command: "cargo test" }, "", { exit: 0 })
  await after("c4", "read", { filePath: "/work/repo/a.rs" }, "fn a() {}", {})
  const posts = recorded(rec).filter((c) => c[0] === "post_tool").map((c) => c[1].tool_response)
  assert.deepEqual(posts, ["Exit code: 101\nOutput:\ntest result: FAILED. 1 failed", ("y".repeat(5_000) + "test result: ok.").slice(-4_000), undefined, undefined])
})

test("opencode 2: a shell call's post_tool carries its result's text and a non-zero exit", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { hooks, cleanup } = await setupV2()
  const after = (id: string, result: object) =>
    hooks["tool.execute.after"]({ tool: "shell", sessionID: SESSION, agent: "build", messageID: "msg_2", id, input: { command: "cargo test" }, status: "completed", result })
  await after("c1", { content: [{ type: "text", text: "test result: FAILED." }], metadata: { exit: 101 } })
  await after("c2", { output: "test result: ok." })
  await settle()
  const posts = recorded(rec).filter((c) => c[0] === "post_tool").map((c) => c[1].tool_response)
  assert.deepEqual(posts, ["Exit code: 101\nOutput:\ntest result: FAILED.", "test result: ok."])
  await cleanup()
})

test("one module serves both plugin APIs: opencode 2 reads id + setup, opencode 1 id + server", () => {
  assert.equal(plugin.id, "borg")
  assert.equal(typeof plugin.setup, "function")
  assert.equal(typeof plugin.server, "function")
})

async function setupV2(assistantText: () => string = () => PLAN) {
  const f = fakeCtx(assistantText)
  const cleanup = await plugin.setup(f.ctx)
  // A user prompt as opencode 2 hands it to the prompt hook.
  const say = (text: string, metadata?: object, sessionID = SESSION) =>
    f.hooks["session.prompt"]({ sessionID, messageID: "msg_1", prompt: { text }, metadata, delivery: "steer" })
  return { ...f, cleanup, say }
}

// Long enough for the recorder's spawn: a sandboxed macOS takes ~300 ms to exec a shell script.
const settle = () => new Promise((r) => setTimeout(r, 500))

test("opencode 2: the plugin speaks borg's canonical event", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { hooks, prompts, emit, cleanup, say } = await setupV2()

  say("Fix the relay race")
  await hooks["tool.execute.before"]({ tool: "read", sessionID: SESSION, agent: "build", messageID: "msg_2", id: "c1", input: { path: "/work/repo/relay.rs" } })
  await hooks["tool.execute.after"]({ tool: "read", sessionID: SESSION, agent: "build", messageID: "msg_2", id: "c1", input: { path: "/work/repo/relay.rs" }, status: "completed", result: { content: [] } })
  await hooks["tool.execute.before"]({ tool: "shell", sessionID: SESSION, agent: "build", messageID: "msg_2", id: "c2", input: { command: "rm -rf x" } })
  await hooks["tool.execute.after"]({
    tool: "shell", sessionID: SESSION, agent: "build", messageID: "msg_2", id: "c2", input: { command: "rm -rf x" }, status: "error",
    error: { _tag: "Tool.Error", message: "Unable to run command", error: { _tag: "Permission.CorrectedError", feedback: "use git clean instead" } },
  })
  await emit("session.execution.succeeded", { sessionID: SESSION })
  await emit("session.deleted", { sessionID: SESSION })
  await settle()

  const calls = recorded(rec)
  const queued = calls.filter((c) => c[0] !== "pre_tool")
  assert.deepEqual(queued.map((c) => c[0]), ["prompt", "post_tool", "post_tool_failure", "stop", "end"])
  const [prompt, post, fail, stop, end] = queued.map((c) => c[1])
  const pid = process.pid
  assert.deepEqual(prompt, { session_id: SESSION, cwd: "/work/repo", pid, prompt: "Fix the relay race", last_assistant_message: PLAN })
  assert.deepEqual(calls.find((c) => c[0] === "pre_tool")![1], { session_id: SESSION, cwd: "/work/repo", pid, tool_name: "Read", tool_input: { path: "/work/repo/relay.rs", file_path: "/work/repo/relay.rs" } })
  assert.equal(post.tool_name, "Read")
  assert.equal(post.tool_input.file_path, "/work/repo/relay.rs")
  assert.equal(fail.tool_name, "Bash")
  assert.equal(fail.tool_input.command, "rm -rf x")
  assert.match(fail.error, /want to proceed with this tool use.*the user said:\nuse git clean instead/s)
  assert.deepEqual(stop, { session_id: SESSION, cwd: "/work/repo", pid, last_assistant_message: PLAN, stop_hook_active: false })
  assert.deepEqual(end, { session_id: SESSION, cwd: "/work/repo", pid })
  assert.deepEqual(prompts, [{ sessionID: SESSION, text: "Q?", metadata: { borg: true }, delivery: "queue" }])
  await cleanup()
})

test("opencode 2: a paused drone's tool call and its subagents' are refused", async () => {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-"))
  const rec = join(dir, "rec.jsonl")
  const bin = join(dir, "borg-pause-root")
  writeFileSync(bin, `#!/bin/sh\nin=$(cat)\nprintf '%s\\t%s\\n' "$3" "$in" >> "${rec}"\n[ "$3" = pre_tool ] && case "$in" in *'"session_id":"${SESSION}"'*) echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"[borg] paused"}}';; esac\nexit 0\n`)
  chmodSync(bin, 0o755)
  process.env.BORG_BIN = bin
  const { hooks, emit, cleanup, say } = await setupV2()
  await emit("session.created", { sessionID: "ses_child", parentID: SESSION })
  const call = (sessionID: string) => hooks["tool.execute.before"]({ tool: "edit", sessionID, id: `c-${sessionID}`, input: { path: "/work/repo/x.rs" } })
  await assert.rejects(call(SESSION), /\[borg\] paused/)
  await assert.rejects(call("ses_child"), /\[borg\] paused/)
  await call("ses_other")
  // The child's own prompt (the subagent's task) and tool results send nothing.
  say("explore the repo", undefined, "ses_child")
  await hooks["tool.execute.after"]({ tool: "edit", sessionID: "ses_child", id: "c4", input: {}, status: "completed", result: {} })
  await emit("session.execution.succeeded", { sessionID: "ses_child" })
  await settle()
  assert.deepEqual(recorded(rec).map((c) => [c[0], c[1].session_id]), [["pre_tool", SESSION], ["pre_tool", SESSION], ["pre_tool", "ses_other"]])
  await cleanup()
})

test("opencode 2: no stop when the turn was halted; a declined call is still a failure", async () => {
  for (const [type, data] of [
    ["session.execution.interrupted", { reason: "user" }],
    ["session.execution.failed", { error: { type: "provider.rate-limit", message: "slow down" } }],
  ] as const) {
    const [bin, rec] = recorder()
    process.env.BORG_BIN = bin
    const { prompts, emit, cleanup, say } = await setupV2()
    say("Fix the relay race")
    await emit(type, { sessionID: SESSION, ...data })
    await settle()
    assert.deepEqual(prompts, [], type)
    assert.deepEqual(recorded(rec).map((c) => c[0]), ["prompt"], type)
    await cleanup()
  }

  // Declined without feedback: no execute.after, a session.tool.failed, then the turn is interrupted.
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { hooks, prompts, emit, cleanup, say } = await setupV2()
  say("Fix the relay race")
  await hooks["tool.execute.before"]({ tool: "write", sessionID: SESSION, id: "c7", input: { path: "/work/repo/relay.rs", content: "x" } })
  await emit("session.tool.failed", { sessionID: SESSION, assistantMessageID: "msg_2", id: "c7", error: { type: "aborted", message: "The user declined this tool call" }, executed: false })
  await emit("session.execution.interrupted", { sessionID: SESSION, reason: "shutdown" })
  await settle()
  assert.deepEqual(prompts, [])
  const calls = recorded(rec).filter((c) => c[0] !== "pre_tool")
  assert.deepEqual(calls.map((c) => c[0]), ["prompt", "post_tool_failure"])
  assert.equal(calls[1][1].tool_name, "Write")
  assert.equal(calls[1][1].tool_input.file_path, "/work/repo/relay.rs")
  assert.equal(calls[1][1].error, "The user doesn't want to proceed with this tool use.", "a wordless denial in Claude's wording")
  await cleanup()
})

test("opencode 1: a rejection without feedback reaches borg as Claude's plain rejection; other errors as they are", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  await hooks.event!(toolError("c1", "bash", { command: "rm -rf x" }, "The user rejected permission to use this specific tool call."))
  await hooks.event!(toolError("c2", "write", { filePath: "/work/repo/x" }, "The user has specified a rule which prevents you from using this specific tool call. Here are some of the relevant rules []"))
  await hooks.event!(toolError("c3", "question", {}, "The user dismissed this question"))
  await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  const errors = recorded(rec).filter((c) => c[0] === "post_tool_failure").map((c) => c[1].error)
  assert.deepEqual(errors, [
    "The user doesn't want to proceed with this tool use.",
    "The user has specified a rule which prevents you from using this specific tool call. Here are some of the relevant rules []",
    "The user dismissed this question",
  ])
})

test("opencode 1: the plain rejections that follow a rejection with words are its cascade, not denials", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
  await hooks.event!(toolError("c1", "read", { filePath: "/work/repo/relay.rs" }, "The user rejected permission to use this specific tool call with the following feedback: use compareAndSet"))
  await hooks.event!(toolError("c2", "read", { filePath: "/work/repo/queue.rs" }, "The user rejected permission to use this specific tool call."))
  // After the engineer's next prompt a plain rejection is theirs again.
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("now add a test for it"))
  await hooks.event!(toolError("c3", "bash", { command: "rm -rf x" }, "The user rejected permission to use this specific tool call."))
  await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  const fails = recorded(rec).filter((c) => c[0] === "post_tool_failure").map((c) => [c[1].tool_name, c[1].error])
  assert.deepEqual(fails.map((f) => f[0]), ["Read", "Bash"])
  assert.match(fails[0][1], /the user said:\nuse compareAndSet$/)
  assert.equal(fails[1][1], "The user doesn't want to proceed with this tool use.")
})

test("opencode 2: opencode run's own auto-reject is no feedback from the engineer", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { hooks, cleanup, say } = await setupV2()
  say("remove the old file")
  await hooks["tool.execute.after"]({
    tool: "shell", sessionID: SESSION, id: "c5", input: { command: "rm -f relay.rs" }, status: "error",
    error: {
      _tag: "Tool.Error", message: "Unable to execute command: rm -f relay.rs",
      error: { _tag: "Permission.CorrectedError", feedback: "This non-interactive run cannot ask the user for permission, so the request was rejected. Continue without this action." },
    },
  })
  await settle()
  const fail = recorded(rec).find((c) => c[0] === "post_tool_failure")![1]
  assert.equal(fail.error, "Unable to execute command: rm -f relay.rs")
  await cleanup()
})

test("opencode 2: questions once per engineer prompt; the marked echo is not the engineer", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { prompts, emit, cleanup, say } = await setupV2()
  say("Fix the relay race")
  for (let i = 0; i < 3; i++) {
    await emit("session.execution.succeeded", { sessionID: SESSION })
    await settle()
    // opencode runs our prompt through the prompt hook with the metadata we gave it.
    if (prompts.length > i) say(prompts[i].text, prompts[i].metadata)
  }
  assert.equal(prompts.length, 1)
  const calls = recorded(rec)
  assert.deepEqual(calls.filter((c) => c[0] === "prompt").map((c) => c[1].prompt), ["Fix the relay race"])
  assert.deepEqual(calls.filter((c) => c[0] === "stop").map((c) => c[1].stop_hook_active), [false, true, false])
  await cleanup()
})

test("opencode 2: cleanup ends every session and closes the event stream", async () => {
  const [bin, rec] = recorder()
  process.env.BORG_BIN = bin
  const { closed, cleanup, say } = await setupV2()
  say("Fix the relay race")
  say("look at the docs", undefined, "ses_two")
  await cleanup()
  assert.ok(closed())
  assert.deepEqual(recorded(rec).filter((c) => c[0] === "end").map((c) => c[1].session_id).sort(), [SESSION, "ses_two"])
})

test("opencode 2: fails open when borg is missing", async () => {
  process.env.BORG_BIN = "/nonexistent/borg"
  const { hooks, prompts, emit, cleanup, say } = await setupV2()
  say("hello")
  await hooks["tool.execute.before"]({ tool: "shell", sessionID: SESSION, id: "c1", input: { command: "ls" } })
  await emit("session.execution.succeeded", { sessionID: SESSION })
  await settle()
  assert.equal(prompts.length, 0)
  await cleanup()
})

test("opencode 2: borg's prompt context (a queen question) reaches the drone as a hidden synthetic input, once", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` }, async () => {
  const home = mkdtempSync(join(tmpdir(), "borg-home-"))
  process.env.BORG_BIN = BORG
  process.env.BORG_HOME = home
  process.env.BORG_STATE = join(home, "state")
  process.env.BORG_CONFIG = join(home, "no-collectives.json")
  process.env.BORG_MATCH = "bm25"
  mkdirSync(join(home, "queen"))
  const entry = {
    id: "qrepo", decision_key: "open_decision:repo:x", kind: "open_decision", question: "Is the relay queue bounded?",
    options: ["bound it at 1024", "leave it unbounded"], default: "bound it at 1024", evidence: [], session: "seed",
    cwd: "/work/repo", engineer: "e@x", ms: 1,
  }
  writeFileSync(join(home, "queen", "queue.jsonl"), JSON.stringify(entry) + "\n")
  const { synthetics, cleanup, say } = await setupV2()
  // The hook is awaited: the synthetic input is in before opencode stores the prompt.
  await say("Fix the relay race")
  assert.equal(synthetics.length, 1)
  const [added] = synthetics
  assert.deepEqual([added.sessionID, added.delivery, added.resume, added.description], [SESSION, "steer", false, undefined])
  assert.match(added.text, /Start your reply with this one question/)
  assert.match(added.text, /borg' queen answer 'qrepo' '<answer>' --dir '\/work\/repo'/)
  await say("now the queue")
  assert.equal(synthetics.length, 1, "one question per session")
  await cleanup()
})

test("opencode 2, real borg: conscience asks once, a rejection with feedback becomes an episode", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` }, async () => {
  const home = mkdtempSync(join(tmpdir(), "borg-home-"))
  process.env.BORG_BIN = BORG
  process.env.BORG_HOME = home
  process.env.BORG_STATE = join(home, "state")
  delete process.env.BORG_LEARN
  process.env.BORG_LEARN_MODEL = "off" // episodes are recorded; the `borg learn` they start never calls a model
  // Never the engineer's real collectives config, and no model call at Stop.
  process.env.BORG_CONFIG = join(home, "no-collectives.json")
  process.env.BORG_MATCH = "bm25"
  writeFileSync(join(home, "lessons.jsonl"), JSON.stringify({ id: "toctou", origin_ms: 1, question: "Is any check-then-act on an AtomicBoolean flag TOCTOU-safe across threads?" }) + "\n")
  let said = PLAN
  const { hooks, prompts, emit, cleanup, say } = await setupV2(() => said)
  const done = async () => {
    await emit("session.execution.succeeded", { sessionID: SESSION })
    await settle()
  }

  say("Fix the relay race in relay.rs")
  await done()
  assert.equal(prompts.length, 1, "borg's questions go back into the session")
  assert.match(prompts[0].text, /\n1\. \[toctou\] Is any check-then-act on an AtomicBoolean flag TOCTOU-safe/)
  say(prompts[0].text, prompts[0].metadata)
  await done()
  assert.equal(prompts.length, 1)

  said = "I will add a second AtomicBoolean to guard the first."
  say("no, don't add another flag, use a channel")
  await hooks["tool.execute.before"]({ tool: "edit", sessionID: SESSION, id: "c9", input: { path: "/work/repo/relay.rs", oldString: "a", newString: "b" } })
  await hooks["tool.execute.after"]({
    tool: "edit", sessionID: SESSION, id: "c9", input: { path: "/work/repo/relay.rs", oldString: "a", newString: "b" }, status: "error",
    error: { _tag: "Tool.Error", message: "Unable to edit /work/repo/relay.rs", error: { _tag: "Permission.CorrectedError", feedback: "keep the public API unchanged" } },
  })
  await done()
  await cleanup()

  const episodes = readFileSync(join(home, "episodes.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l))
  assert.equal(episodes.length, 2)
  assert.equal(episodes[0].correction, "no, don't add another flag, use a channel")
  assert.equal(episodes[0].agent, "I will add a second AtomicBoolean to guard the first.")
  assert.equal(episodes[1].correction, "keep the public API unchanged")
  assert.match(episodes[1].rejected_tool, /^Edit .*relay\.rs/)
})

test("real borg: a call declined without words makes the engineer's next prompt the reason, in both versions", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` }, async () => {
  const fresh = () => {
    const home = mkdtempSync(join(tmpdir(), "borg-home-"))
    process.env.BORG_BIN = BORG
    process.env.BORG_HOME = home
    process.env.BORG_STATE = join(home, "state")
    delete process.env.BORG_LEARN
    process.env.BORG_LEARN_MODEL = "off" // episodes are recorded; the `borg learn` they start never calls a model
    process.env.BORG_CONFIG = join(home, "no-collectives.json")
    process.env.BORG_MATCH = "bm25"
    return () => readFileSync(join(home, "episodes.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l))
  }

  // opencode 1: the bare REJECTED on a tool part.
  let episodes = fresh()
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race in relay.rs"))
  await hooks.event!(toolError("c1", "bash", { command: "rm -rf build" }, "The user rejected permission to use this specific tool call."))
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("keep the build cache, clean only target"))
  await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  let got = episodes()
  assert.equal(got.length, 1)
  assert.equal(got[0].correction, "keep the build cache, clean only target")
  assert.match(got[0].rejected_tool, /^Bash .*rm -rf build/)

  // opencode 2: a declined call (session.tool.failed "aborted"), then the engineer's reason.
  episodes = fresh()
  const v2 = await setupV2()
  v2.say("Fix the relay race in relay.rs")
  await v2.hooks["tool.execute.before"]({ tool: "shell", sessionID: SESSION, id: "c7", input: { command: "rm -rf build" } })
  await v2.emit("session.tool.failed", { sessionID: SESSION, assistantMessageID: "msg_2", id: "c7", error: { type: "aborted", message: "The user declined this tool call" }, executed: false })
  await v2.emit("session.execution.interrupted", { sessionID: SESSION, reason: "shutdown" })
  await settle()
  await v2.say("keep the build cache, clean only target")
  await v2.cleanup()
  got = episodes()
  assert.equal(got.length, 1)
  assert.equal(got[0].correction, "keep the build cache, clean only target")
  assert.match(got[0].rejected_tool, /^Bash .*rm -rf build/)
})

/** A fake borg that answers post_tool with $CTX as additionalContext (after $POST_SLEEP seconds) and
 *  prompt with $PROMPT_CTX (after $PROMPT_SLEEP seconds). */
function contextBorg(): string {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-live-"))
  const bin = join(dir, "borg-context")
  writeFileSync(
    bin,
    `#!/bin/sh\ncat >/dev/null\nanswer() { printf '{"hookSpecificOutput":{"hookEventName":"%s","additionalContext":"%s"}}\\n' "$1" "$2"; }\n` +
      `[ "$3" = post_tool ] && [ -n "$POST_SLEEP" ] && sleep "$POST_SLEEP"\n` +
      `[ "$3" = post_tool ] && [ -n "$CTX" ] && answer PostToolUse "$CTX"\n` +
      `[ "$3" = prompt ] && [ -n "$PROMPT_SLEEP" ] && sleep "$PROMPT_SLEEP"\n` +
      `[ "$3" = prompt ] && [ -n "$PROMPT_CTX" ] && answer UserPromptSubmit "$PROMPT_CTX"\nexit 0\n`,
  )
  chmodSync(bin, 0o755)
  return bin
}

const after1 = (hooks: any, callID: string) => {
  const output = { title: "", output: "fn deliver() {}", metadata: {} }
  return hooks["tool.execute.after"]!({ tool: "edit", sessionID: SESSION, callID, args: { filePath: "/work/repo/relay.rs" } }, output).then(() => output.output)
}

test("opencode 1: borg's tool context (a live check's questions) is appended to the tool output the model reads next", async () => {
  process.env.BORG_BIN = contextBorg()
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
  assert.equal(await after1(hooks, "c1"), "fn deliver() {}", "no context: the output as it was")
  process.env.CTX = "[borg] borg check of your work so far: Q?"
  try {
    assert.equal(await after1(hooks, "c2"), "fn deliver() {}\n\n[borg] borg check of your work so far: Q?")
  } finally {
    delete process.env.CTX
  }
})

test("opencode 2: borg's tool context is steered into the session; the tool result stays as it is", async () => {
  process.env.BORG_BIN = contextBorg()
  const { hooks, synthetics, cleanup, say } = await setupV2()
  await say("Fix the relay race")
  const result = { content: [{ type: "text", text: "fn deliver() {}" }] }
  const after = (id: string) =>
    hooks["tool.execute.after"]({ tool: "edit", sessionID: SESSION, agent: "build", messageID: "msg_2", id, input: { path: "/work/repo/relay.rs" }, status: "completed", result })
  assert.equal(await after("c1"), undefined)
  assert.equal(synthetics.length, 0, "no context: nothing sent")
  process.env.CTX = "[borg] borg check of your work so far: Q?"
  try {
    const e = { tool: "edit", sessionID: SESSION, agent: "build", messageID: "msg_2", id: "c2", input: { path: "/work/repo/relay.rs" }, status: "completed", result }
    assert.equal(await hooks["tool.execute.after"](e), undefined, "the hook returns nothing")
    assert.equal(e.result, result)
    assert.deepEqual(result.content, [{ type: "text", text: "fn deliver() {}" }])
    assert.deepEqual(synthetics, [{ sessionID: SESSION, text: "[borg] borg check of your work so far: Q?", delivery: "steer", resume: false, metadata: { borg: true } }])
  } finally {
    delete process.env.CTX
  }
  await cleanup()
})

test("a tool context slower than POST_MS is never lost: it comes with the next tool call or the next prompt", async () => {
  process.env.BORG_BIN = contextBorg()
  const wait = (ms: number) => new Promise((r) => setTimeout(r, ms))
  try {
    // opencode 1: the next tool call.
    const { client } = fakeClient(() => PLAN)
    const hooks = await load(client, "/work/repo")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
    process.env.CTX = "LATE-1"
    process.env.POST_SLEEP = "3"
    assert.equal(await after1(hooks, "c1"), "fn deliver() {}", "the tool call does not wait for it")
    delete process.env.CTX
    delete process.env.POST_SLEEP
    assert.equal(await after1(hooks, "c2"), "fn deliver() {}\n\nLATE-1")
    assert.equal(await after1(hooks, "c3"), "fn deliver() {}", "delivered once")

    // opencode 2: the next prompt, before borg's own prompt context.
    const v2 = await setupV2()
    await v2.say("Fix the relay race")
    process.env.CTX = "LATE-2"
    process.env.POST_SLEEP = "3"
    await v2.hooks["tool.execute.after"]({ tool: "edit", sessionID: SESSION, id: "c1", input: { path: "/work/repo/relay.rs" }, status: "completed", result: { content: [] } })
    assert.equal(v2.synthetics.length, 0)
    delete process.env.CTX
    delete process.env.POST_SLEEP
    await wait(1500)
    process.env.PROMPT_CTX = "PROMPT-CONTEXT"
    await v2.say("now the queue")
    assert.deepEqual(v2.synthetics.map((s: any) => s.text), ["LATE-2\n\nPROMPT-CONTEXT"])
    await v2.cleanup()
  } finally {
    delete process.env.CTX
    delete process.env.POST_SLEEP
    delete process.env.PROMPT_CTX
  }
})

test("a prompt context slower than PROMPT_MS is never lost: it comes with the next tool call", async () => {
  process.env.BORG_BIN = contextBorg()
  try {
    const { client } = fakeClient(() => PLAN)
    const hooks = await load(client, "/work/repo")
    process.env.PROMPT_CTX = "LATE-PROMPT"
    process.env.PROMPT_SLEEP = "6"
    const msg = userMessage("Fix the relay race")
    await hooks["chat.message"]!({ sessionID: SESSION }, msg)
    assert.equal(msg.parts.length, 1, "the message does not wait for it")
    delete process.env.PROMPT_CTX
    delete process.env.PROMPT_SLEEP
    await new Promise((r) => setTimeout(r, 1500))
    assert.equal(await after1(hooks, "c1"), "fn deliver() {}\n\nLATE-PROMPT")
    assert.equal(await after1(hooks, "c2"), "fn deliver() {}", "delivered once")
  } finally {
    delete process.env.PROMPT_CTX
    delete process.env.PROMPT_SLEEP
  }
})

test("opencode 2: a tool context whose steer fails is kept for the next call or prompt", async () => {
  process.env.BORG_BIN = contextBorg()
  const v2 = await setupV2()
  await v2.say("Fix the relay race")
  const synthetic = v2.ctx.session.synthetic
  v2.ctx.session.synthetic = async () => {
    throw new Error("session busy")
  }
  process.env.CTX = "STEER-FAILED"
  try {
    await v2.hooks["tool.execute.after"]({ tool: "edit", sessionID: SESSION, id: "c1", input: { path: "/work/repo/relay.rs" }, status: "completed", result: { content: [] } })
  } finally {
    delete process.env.CTX
    v2.ctx.session.synthetic = synthetic
  }
  await v2.say("go on")
  assert.deepEqual(v2.synthetics.map((s: any) => s.text), ["STEER-FAILED"])
  await v2.cleanup()
})

// ---- Interactive hive drones (phase g): the plugin binds the session, keeps its waiter and wakes it.

/** A fake borg for hive drones: `hive join … --bind` answers a join (its args and BORG_HARNESS
 *  recorded), `hive wait --stream` prints each line written to $WAKE until it is stopped, and a hook
 *  records `<event>\t<stdin>`; its stop blocks with a hive handover (`hive: true`) while $HANDOVER is set,
 *  with a conscience's questions alone (a drone's too: no mark) while $QUESTIONS is.
 *  Returns [binary, record file]. */
function hiveFake(): [string, string] {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-hive-"))
  const rec = join(dir, "rec.jsonl")
  const bin = join(dir, "borg-hive")
  writeFileSync(
    bin,
    `#!/bin/sh
case "$1 $2" in
"hive join") printf 'join\\t%s\\t%s\\n' "$BORG_HARNESS" "$*" >> "${rec}"; echo "S joined hive goal g1 as drone i-abc123."; exit 0;;
"hive wait") printf 'wait\\t%s\\n' "$*" >> "${rec}"; trap 'exit 143' TERM INT HUP
  while :; do if [ -n "$WAKE" ] && [ -s "$WAKE" ]; then cat "$WAKE"; : > "$WAKE"; fi; sleep 0.05; done;;
esac
in=$(cat)
printf '%s\\t%s\\n' "$3" "$in" >> "${rec}"
[ "$3" = stop ] && [ -n "$HANDOVER" ] && echo '{"decision":"block","hive":true,"reason":"[borg hive] t1 done: its checks passed (ok). Next: t2."}'
[ "$3" = stop ] && [ -n "$QUESTIONS" ] && echo '{"decision":"block","reason":"1. [G1] Did I check the relay race? [borg hive] Nothing is open to you now."}'
exit 0
`,
  )
  chmodSync(bin, 0o755)
  return [bin, rec]
}

function lines(rec: string): string[][] {
  return existsSync(rec) ? readFileSync(rec, "utf8").trim().split("\n").map((l) => l.split("\t")) : []
}

async function until(ms: number, what: string, done: () => boolean) {
  const t0 = Date.now()
  while (!done()) {
    assert.ok(Date.now() - t0 < ms, `${what} within ${ms} ms`)
    await new Promise((r) => setTimeout(r, 50))
  }
}

const wakeLine = (text: string) => JSON.stringify({ wake: text, left: false }) + "\n"

test("opencode 1: a session's `borg hive join` is bound by the plugin, its waiter starts at the bind, and a wake goes in by promptAsync only while it is idle", async () => {
  const [bin, rec] = hiveFake()
  const wake = join(mkdtempSync(join(tmpdir(), "borg-oc-wake-")), "wake")
  process.env.BORG_BIN = bin
  process.env.WAKE = wake
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  try {
    assert.equal(process.env.BORG_HIVE_BINDER, "opencode", "every command the model runs knows the plugin binds")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("join the hive"))
    await hooks["tool.execute.before"]!({ tool: "bash", sessionID: SESSION, callID: "j1" }, { args: { command: "borg hive join g1" } })
    const out = { title: "", output: "[borg hive] this session's harness plugin binds it", metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "j1", args: { command: "borg hive join g1" } }, out)
    assert.match(out.output, /binds it\n\nS joined hive goal g1 as drone i-abc123\./, "the model reads the bind's answer")
    const join_ = lines(rec).find((l) => l[0] === "join")!
    assert.deepEqual(join_.slice(1), ["opencode", `hive join g1 --bind --session ${SESSION} --dir /work/repo`])
    await until(5_000, "the waiter", () => lines(rec).some((l) => l[0] === "wait"))
    assert.deepEqual(lines(rec).find((l) => l[0] === "wait")!.slice(1), [`hive wait --stream --session ${SESSION} --dir /work/repo`])
    // Work arrives while the join's turn runs, with a long tool call in it: held.
    await hooks["tool.execute.before"]!({ tool: "bash", sessionID: SESSION, callID: "b1" }, { args: { command: "cargo build" } })
    writeFileSync(wake, wakeLine("[borg hive g1] Work arrived. Your claim: t1"))
    await new Promise((r) => setTimeout(r, 800))
    assert.deepEqual(prompts, [], "no wake inside the turn")
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "b1", args: { command: "cargo build" } }, { title: "", output: "", metadata: {} })
    await hooks.event!(idleEvent)
    await until(5_000, "the wake", () => prompts.length === 1)
    assert.deepEqual(prompts[0], { path: { id: SESSION }, body: { parts: [{ type: "text", text: "[borg hive g1] Work arrived. Your claim: t1", ...marked }] } })
    // Its echo is ours, not the engineer's: no prompt reaches borg.
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("[borg hive g1] Work arrived. Your claim: t1", marked))
    assert.equal(lines(rec).filter((l) => l[0] === "prompt").length, 1)
    // A plain session's join of a directory, or another session's: nothing bound.
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: "ses_plain", callID: "p1", args: { command: "borg hive join g1 --here" } }, { title: "", output: "", metadata: {} })
    assert.equal(lines(rec).filter((l) => l[0] === "join").length, 1)
  } finally {
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
    delete process.env.WAKE
  }
})

test("opencode 1: a drone's hive handover is not dropped by an engineer prompt during its stop nor counted by MAX_ROUNDS; a plain session's still is", async () => {
  const [bin, rec] = hiveFake()
  process.env.BORG_BIN = bin
  process.env.HANDOVER = "1"
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const idle = (id: string) => hooks.event!({ event: { type: "session.idle", properties: { sessionID: id } } as any })
  try {
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("join the hive"))
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "j1", args: { command: "borg hive join" } }, { title: "", output: "", metadata: {} })
    await idle(SESSION)
    assert.equal(prompts.length, 1, "the handover went in")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("x", marked))
    // The next stop (stop_hook_active): a handover again, sent.
    await idle(SESSION)
    assert.equal(prompts.length, 2, "not counted by MAX_ROUNDS")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("x", marked))
    // The engineer types while the stop runs: the handover holds a claim, so it is queued, not dropped.
    const stopping = idle(SESSION)
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("what are you on?"))
    await stopping
    assert.equal(prompts.length, 3, "not dropped by the engineer's prompt")
    for (const p of prompts) assert.match(p.body.parts[0].text, /^\[borg hive\] t1 done/)
    const stops = lines(rec).filter((l) => l[0] === "stop").map((l) => JSON.parse(l[1]).stop_hook_active)
    // The engineer's prompt starts a fresh cycle: that stop is a first round again.
    assert.deepEqual(stops, [false, true, false])
    // A plain session with the same answers: one round per engineer prompt, as before.
    const plain = "ses_plain_rounds"
    await hooks["chat.message"]!({ sessionID: plain }, userMessage("fix the race"))
    await idle(plain)
    await hooks["chat.message"]!({ sessionID: plain }, userMessage("x", marked))
    await idle(plain)
    assert.equal(prompts.filter((p) => p.path.id === plain).length, 1)
  } finally {
    delete process.env.HANDOVER
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  }
})

test("opencode 2: the plugin binds a hive drone's session from its execute.after, wakes it by session.prompt with delivery queue once idle, also after an interrupted turn", async () => {
  const [bin, rec] = hiveFake()
  const wake = join(mkdtempSync(join(tmpdir(), "borg-oc-wake-")), "wake")
  process.env.BORG_BIN = bin
  process.env.WAKE = wake
  const v2 = await setupV2()
  try {
    await v2.say("join the hive")
    await v2.hooks["tool.execute.before"]({ tool: "shell", sessionID: SESSION, id: "j1", input: { command: "borg hive join" } })
    await v2.hooks["tool.execute.after"]({ tool: "shell", sessionID: SESSION, id: "j1", input: { command: "borg hive join" }, status: "completed", result: { output: "binds it" } })
    assert.deepEqual(v2.synthetics.map((s: any) => [s.text, s.delivery]), [["S joined hive goal g1 as drone i-abc123.", "steer"]], "the model reads the bind's answer")
    assert.equal(lines(rec).find((l) => l[0] === "join")![2], `hive join --bind --session ${SESSION} --dir /work/repo`)
    await until(5_000, "the waiter", () => lines(rec).some((l) => l[0] === "wait"))
    writeFileSync(wake, wakeLine("[borg hive] Work arrived. Your claim: t1"))
    await new Promise((r) => setTimeout(r, 800))
    assert.deepEqual(v2.prompts, [], "no wake inside the turn")
    // Esc: the turn is interrupted, no stop; the session is idle, so the wake goes in.
    await v2.emit("session.execution.interrupted", { sessionID: SESSION, reason: "user" })
    await until(5_000, "the wake", () => v2.prompts.length === 1)
    assert.deepEqual(v2.prompts[0], { sessionID: SESSION, text: "[borg hive] Work arrived. Your claim: t1", metadata: { borg: true }, delivery: "queue" })
    assert.equal(lines(rec).filter((l) => l[0] === "stop").length, 0, "no stop for the interrupted turn")
    // borg hears the halt, so its waiter's busy flag ends with the turn (not 5 minutes later).
    await until(5_000, "the halted event", () => lines(rec).some((l) => l[0] === "halted"))
    assert.deepEqual(JSON.parse(lines(rec).find((l) => l[0] === "halted")![1]), { session_id: SESSION, cwd: "/work/repo", pid: process.pid })
    // A plain session's halt: nothing sent.
    await v2.emit("session.execution.interrupted", { sessionID: "ses_plain_halt", reason: "user" })
    await settle()
    assert.equal(lines(rec).filter((l) => l[0] === "halted").length, 1)
  } finally {
    delete process.env.WAKE
    await v2.cleanup()
  }
  await settle()
  assert.ok(lines(rec).some((l) => l[0] === "end"), "the cleanup ends the session")
})

/** A store, a collective over a committed git repo `harbor_orion` and goal g1 (its oracle passes once
 *  `ok` exists in the drone's tree), with this process's env pointed at them; `restore` puts it back. */
function hiveWorld() {
  const saved = { ...process.env }
  const root = mkdtempSync(join(tmpdir(), "borg-oc-hive-real-"))
  const work = join(root, "harbor_orion")
  const store = join(root, "store/collectives/corp")
  for (const d of [work, store, join(root, "home")]) mkdirSync(d, { recursive: true })
  writeFileSync(join(root, "collectives.json"), JSON.stringify({ default: "off", collectives: [{ name: "corp", paths: [work], share: "local" }] }))
  for (const k of ["BORG_STATE", "BORG_CONSCIENCE", "PI_SESSION_ID", "CLAUDE_CODE_SESSION_ID", "BORG_HIVE", "BORG_HIVE_DRONE", "WAKE", "HANDOVER"]) delete process.env[k]
  Object.assign(process.env, {
    BORG_BIN: BORG, BORG_HOME: join(root, "store"), BORG_CONFIG: join(root, "collectives.json"), HOME: join(root, "home"), GIT_CONFIG_NOSYSTEM: "1",
    BORG_QUEEN: "off", BORG_LEARN: "off", BORG_VECTORS: "off", BORG_STOP_ATTENTION: "off", BORG_MODE: "hook", BORG_MATCH: "bm25", BORG_PREDICT_MODEL: "off",
    BORG_HIVE_TAKEUP_MS: "20000",
  })
  const git = (...args: string[]) => execFileSync("git", ["-C", work, "-c", "user.email=e@x", "-c", "user.name=E", ...args], { stdio: "pipe" })
  git("init", "-q", "-b", "main")
  writeFileSync(join(work, "README"), "orion\n")
  git("add", ".")
  git("commit", "-q", "-m", "start")
  const hive = (args: string[]) => execFileSync(BORG, ["hive", ...args, "--dir", work], { env: process.env, encoding: "utf8" })
  hive(["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=test -f ok", "--budget", "5"])
  const registry = () =>
    existsSync(join(store, "hive/drones.local")) ? readFileSync(join(store, "hive/drones.local"), "utf8").trim().split("\n").map((l) => JSON.parse(l)) : []
  // borg's scratch state names a session by its letters, digits and dashes (hook.rs `state_file`).
  const state = (s: string, ext: string) => join(store, `state/${s.replace(/[^A-Za-z0-9-]/g, "")}.${ext}`)
  const hs = (s: string) => (existsSync(state(s, "hive")) ? JSON.parse(readFileSync(state(s, "hive"), "utf8")) : null)
  const restore = () => {
    for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k]
    Object.assign(process.env, saved)
  }
  return { work, store, hive, registry, hs, state, restore }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

test("real borg, opencode 1: a bound drone of one opencode process waits from its bind, is woken only once idle, says its done first through the hive path and leaves at its end; another session's join binds only it", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` }, async () => {
  const w = hiveWorld()
  const B = "ses_hive_other"
  const { client, prompts } = fakeClient(() => "I made the change.")
  const hooks = await load(client, w.work)
  const idle = (id: string) => hooks.event!({ event: { type: "session.idle", properties: { sessionID: id } } as any })
  const after = (id: string, callID: string, command: string, output: string) =>
    hooks["tool.execute.after"]!({ tool: "bash", sessionID: id, callID, args: { command } }, { title: "", output, metadata: { exit: 0 } })
  try {
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("join the hive"))
    // The model's own join: its shell has no session id, so borg leaves the bind to the plugin.
    const said = w.hive(["join"])
    assert.match(said, /harness plugin binds it/)
    assert.equal(w.registry().length, 0, "the command itself binds nobody")
    await hooks["tool.execute.before"]!({ tool: "bash", sessionID: SESSION, callID: "j1" }, { args: { command: "borg hive join" } })
    const out = { title: "", output: said, metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "j1", args: { command: "borg hive join" } }, out)
    assert.match(out.output, /joined every open hive goal of this repo \(g1\) as drone i-[0-9a-f]{6}/)
    const [entry] = w.registry()
    assert.deepEqual([entry.session, entry.harness, entry.pid], [SESSION, "opencode", process.pid], "the session that ran it, under opencode's own process")
    const drone = w.hs(SESSION).drone
    const lock = join(w.store, `state/${drone}.hive-waiter`)
    await until(5_000, "the waiter", () => existsSync(lock) && readFileSync(lock, "utf8").trim() !== "")
    const waiter = Number(readFileSync(lock, "utf8").trim())
    // Another session of the same opencode process joins: it is its own drone, the first stays.
    await after(B, "j2", "borg hive join", w.hive(["join"]))
    assert.deepEqual(w.registry().map((e) => [e.session, e.by]), [[SESSION, "join"], [B, "join"]])
    assert.notEqual(w.hs(B).drone, drone)
    // Still the join's turn, a long tool call in it: the task that opens is not handed.
    await hooks["tool.execute.before"]!({ tool: "bash", sessionID: SESSION, callID: "b1" }, { args: { command: "cargo build" } })
    w.hive(["open", "g1", "--id", "t1", "--title", "do t1"])
    // The engineer approves the plan from their own terminal (G5), outside the plugin's process env.
    execFileSync(BORG, ["hive", "ratify", "g1", "--dir", w.work], { env: { ...process.env, BORG_HIVE_BINDER: "" }, encoding: "utf8" })
    await new Promise((r) => setTimeout(r, 1_500))
    assert.equal(prompts.filter((p) => p.path.id === SESSION).length, 0, "nothing in the engineer's turn")
    await after(SESSION, "b1", "cargo build", "")
    // B's turn is over first: t1 may go to either drone; end B so it goes to this session.
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: B } } } as any })
    await idle(SESSION)
    await until(10_000, "the wake", () => prompts.some((p) => p.path.id === SESSION))
    const woke = prompts.find((p) => p.path.id === SESSION)!
    assert.match(woke.body.parts[0].text, /^\[borg hive g1\] Work arrived\..*Your claim: t1 \(work\) do t1/s)
    assert.deepEqual(woke.body.parts[0].metadata, { borg: true })
    assert.ok(existsSync(w.state(SESSION, "hive-takeup")), "handed, not taken up yet")
    // The woken turn: its echo (ours), then its first tool call takes the claim up.
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage(woke.body.parts[0].text, marked))
    await hooks["tool.execute.before"]!({ tool: "read", sessionID: SESSION, callID: "r1" }, { args: { filePath: join(w.work, "README") } })
    assert.ok(!existsSync(w.state(SESSION, "hive-takeup")), "taken up")
    writeFileSync(join(w.hs(SESSION).worktree, "ok"), "")
    await idle(SESSION)
    const handover = prompts.filter((p) => p.path.id === SESSION)
    assert.equal(handover.length, 2, "the stop's handover went in")
    assert.match(handover[1].body.parts[0].text, /^\[borg hive\] t1 done: its checks passed \(ok\); verify task t1\.v1 is open for another drone\./)
    assert.match(handover[1].body.parts[0].text, /Drone idle: waiting for hive work/)
    assert.doesNotMatch(handover[1].body.parts[0].text, /run_in_background/)
    assert.ok(alive(waiter), "one waiter for the session's life")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage(handover[1].body.parts[0].text, marked))
    await idle(SESSION)
    // The session's end: the drone leaves, its waiter stops.
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
    assert.equal(w.hs(SESSION), null)
    assert.equal(w.registry().filter((e) => e.by !== "leave").length, 2, "two joins")
    assert.deepEqual(w.registry().filter((e) => e.by === "leave").map((e) => e.session).sort(), [B, SESSION].sort())
    await until(10_000, "the waiter's end", () => !alive(waiter))
  } finally {
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: B } } } as any })
    w.restore()
  }
})

test("opencode 1: a drone's block of questions alone keeps the rules for questions: dropped when the engineer spoke during its stop", async () => {
  const [bin, rec] = hiveFake()
  process.env.BORG_BIN = bin
  process.env.QUESTIONS = "1"
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const idle = (id: string) => hooks.event!({ event: { type: "session.idle", properties: { sessionID: id } } as any })
  try {
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("join the hive"))
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "j1", args: { command: "borg hive join" } }, { title: "", output: "", metadata: {} })
    const stopping = idle(SESSION)
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("what are you on?"))
    await stopping
    assert.deepEqual(prompts, [], "questions about a superseded turn are dropped, though their text names the hive")
    // The next stop of the engineer's new turn: one round of questions, then no more.
    await idle(SESSION)
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("x", marked))
    await idle(SESSION)
    assert.equal(prompts.length, 1, "counted by MAX_ROUNDS")
    assert.ok(lines(rec).filter((l) => l[0] === "stop").length >= 2)
  } finally {
    delete process.env.QUESTIONS
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  }
})

test("opencode 2: a wake that fails to go in is kept and sent again, never dropped", async () => {
  const [bin] = hiveFake()
  const wake = join(mkdtempSync(join(tmpdir(), "borg-oc-wake-")), "wake")
  process.env.BORG_BIN = bin
  process.env.WAKE = wake
  const v2 = await setupV2()
  const send = v2.ctx.session.prompt
  let failed = 0
  v2.ctx.session.prompt = async (input: any) => {
    if (failed++ === 0) throw new Error("server busy")
    return send(input)
  }
  try {
    await v2.hooks["tool.execute.after"]({ tool: "shell", sessionID: SESSION, id: "j1", input: { command: "borg hive join" }, status: "completed", result: { output: "" } })
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    writeFileSync(wake, wakeLine("[borg hive] Work arrived. Your claim: t1"))
    await until(15_000, "the wake, sent again", () => v2.prompts.length === 1)
    assert.equal(failed, 2, "the first send failed, the second went in")
    assert.equal(v2.prompts[0].text, "[borg hive] Work arrived. Your claim: t1")
  } finally {
    delete process.env.WAKE
    await v2.cleanup()
  }
})
