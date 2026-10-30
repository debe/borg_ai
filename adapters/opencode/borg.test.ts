// Contract test: opencode-shaped hooks/events go into the plugin, the plugin's canonical JSON goes
// into `borg hook opencode`, and borg's answer comes back into the session.
//
//   cargo build -p borg-ai && node --test adapters/opencode/
//
// Node >= 23.6 runs the TypeScript directly. BORG_TEST_BIN overrides the borg binary under test.

import { test } from "node:test"
import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { mkdirSync, mkdtempSync, writeFileSync, readFileSync, existsSync, chmodSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import plugin from "./borg.ts"
import { fakeClient, fakeCtx } from "./fakes.ts"

const BORG = process.env.BORG_TEST_BIN ?? resolve(import.meta.dirname, "../../target/debug/borg")
// No release check from a test: it would ask GitHub (borg update.rs).
process.env.BORG_UPDATE_CHECK = "off"

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
// Every call asks borg for its budgets until an answer carries them; these fakes answer none.
const ASK = { budgets: true }

/** A fake borg that records `<event>\t<stdin>` per call and answers a stop with a block, like borg
 *  would; while the file $STOP_HOLD names exists, a stop waits, after it wrote `<record file>.held`.
 *  Returns [binary, record file]. */
function recorder(): [string, string] {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-"))
  const rec = join(dir, "rec.jsonl")
  const bin = join(dir, "borg-recorder")
  writeFileSync(
    bin,
    `#!/bin/sh\nin=$(cat)\n[ "$3" = stop ] && [ -n "$STOP_HOLD" ] && : > "${rec}.held" && while [ -e "$STOP_HOLD" ]; do sleep 0.05; done\nprintf '%s\\t%s\\n' "$3" "$in" >> "${rec}"\n[ "$3" = stop ] && echo '{"decision":"block","reason":"Q?"}'\nexit 0\n`,
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
  assert.deepEqual(prompt, { session_id: SESSION, cwd: "/work/repo", pid, prompt: "Fix the relay race", last_assistant_message: PLAN, ...ASK })
  assert.deepEqual(pre, { session_id: SESSION, cwd: "/work/repo", pid, tool_name: "Read", tool_input: { filePath: "/work/repo/relay.rs", file_path: "/work/repo/relay.rs" }, ...ASK })
  assert.equal(post.tool_name, "Read")
  assert.equal(post.tool_input.file_path, "/work/repo/relay.rs")
  assert.equal(fail.tool_name, "Bash")
  assert.equal(fail.tool_input.command, "rm -rf x")
  assert.match(fail.error, /want to proceed with this tool use.*the user said:\nuse git clean instead/s)
  assert.deepEqual(stop, { session_id: SESSION, cwd: "/work/repo", pid, last_assistant_message: PLAN, stop_hook_active: false, ...ASK })
  assert.deepEqual(end, { session_id: SESSION, cwd: "/work/repo", pid, ...ASK })
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
  const hold = `${rec}.hold`
  writeFileSync(hold, "")
  process.env.BORG_BIN = bin
  process.env.STOP_HOLD = hold
  try {
    let said = PLAN
    const { client, prompts } = fakeClient(() => said)
    const hooks = await load(client, "/work/repo")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
    const stopping = hooks.event!(idleEvent)
    await until(() => existsSync(`${rec}.held`)) // borg is still thinking about the stop
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("no, use a channel"))
    said = "streaming the reply to the new prompt"
    rmSync(hold)
    await stopping
    assert.deepEqual(prompts, [], "questions about the superseded turn are not sent")
    delete process.env.STOP_HOLD
    await hooks.event!(idleEvent) // the engineer's new task gets its own conscience
    assert.equal(prompts.length, 1)
    const calls = recorded(rec)
    assert.deepEqual(calls.map((c) => c[0]), ["prompt", "stop", "prompt", "stop"])
    assert.equal(calls[2][1].last_assistant_message, PLAN, "the correction is against the proposal the engineer saw")
    assert.equal(calls[0][1].context, undefined)
    assert.equal(calls[2][1].context, false, "a prompt that will not wait for borg's context asks for none")
    assert.equal(calls[3][1].stop_hook_active, false)
  } finally {
    delete process.env.STOP_HOLD
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

test("borg's answer sets the budgets: asked for until one carries them; the stop's and the output tail then follow it", async () => {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-budgets-"))
  const bin = join(dir, "borg")
  const rec = join(dir, "rec")
  // Records each call's event, its BORG_STOP_BUDGET_MS and its input; answers an ask for budgets with $BUDGETS.
  writeFileSync(bin, `#!/bin/sh\nin=$(cat)\nprintf '%s\\t%s\\t%s\\n' "$3" "$BORG_STOP_BUDGET_MS" "$in" >> "${rec}"\ncase "$in" in *'"budgets":true'*) [ -n "$BUDGETS" ] && echo "{\\"budgets\\":$BUDGETS}";; esac\nexit 0\n`)
  chmodSync(bin, 0o755)
  process.env.BORG_BIN = bin
  const calls = () => readFileSync(rec, "utf8").trim().split("\n").map((l) => {
    const [event, budget, input] = l.split("\t")
    return { event, budget: Number(budget), input: JSON.parse(input) }
  })
  process.env.BUDGETS = JSON.stringify({ prompt_ms: 4_000, pre_ms: 1_000, post_ms: 3_000, stop_ms: 20_000, end_ms: 20_000, tail_chars: 10 })
  try {
    const { client } = fakeClient(() => PLAN)
    const hooks = await load(client, "/work/repo")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
    const out = { title: "", output: "x".repeat(50) + "test result: ok", metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "c1", args: { command: "cargo test" } }, out)
    await hooks.event!(idleEvent)
    const [prompt, post, stop] = calls()
    assert.deepEqual([prompt.event, prompt.budget, prompt.input.budgets], ["prompt", 90_000, true], "the first call asks, under the fallback")
    assert.equal(post.input.budgets, undefined, "answered: asked no more")
    assert.equal(post.input.tool_response, "result: ok", "the output's last tail_chars")
    assert.deepEqual([stop.event, stop.budget], ["stop", 20_000], "the stop's budget borg named")
  } finally {
    delete process.env.BUDGETS
  }
})

test("the session's end waits the end_ms borg named, not the stop's budget", async () => {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-end-"))
  const bin = join(dir, "borg")
  const rec = join(dir, "rec")
  // Records each call's event; answers an ask for budgets with a stop and an end apart from each other.
  writeFileSync(bin, `#!/bin/sh\nin=$(cat)\necho "$3" >> "${rec}"\ncase "$in" in *'"budgets":true'*) echo '{"budgets":{"stop_ms":98765,"end_ms":4321}}';; esac\nexit 0\n`)
  chmodSync(bin, 0o755)
  process.env.BORG_BIN = bin
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
  // The delays of the timers set while the session ends: the end call's kill timer among them.
  const set = globalThis.setTimeout
  const delays: unknown[] = []
  globalThis.setTimeout = ((f: any, ms?: number, ...a: any[]) => (delays.push(ms), set(f, ms, ...a))) as typeof setTimeout
  try {
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  } finally {
    globalThis.setTimeout = set
  }
  assert.deepEqual(readFileSync(rec, "utf8").trim().split("\n"), ["prompt", "end"])
  assert.deepEqual(delays.filter((d) => d === 4321 || d === 98765), [4321], "the end is cut off after end_ms")
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
  assert.deepEqual(prompt, { session_id: SESSION, cwd: "/work/repo", pid, prompt: "Fix the relay race", last_assistant_message: PLAN, ...ASK })
  assert.deepEqual(calls.find((c) => c[0] === "pre_tool")![1], { session_id: SESSION, cwd: "/work/repo", pid, tool_name: "Read", tool_input: { path: "/work/repo/relay.rs", file_path: "/work/repo/relay.rs" }, ...ASK })
  assert.equal(post.tool_name, "Read")
  assert.equal(post.tool_input.file_path, "/work/repo/relay.rs")
  assert.equal(fail.tool_name, "Bash")
  assert.equal(fail.tool_input.command, "rm -rf x")
  assert.match(fail.error, /want to proceed with this tool use.*the user said:\nuse git clean instead/s)
  assert.deepEqual(stop, { session_id: SESSION, cwd: "/work/repo", pid, last_assistant_message: PLAN, stop_hook_active: false, ...ASK })
  assert.deepEqual(end, { session_id: SESSION, cwd: "/work/repo", pid, ...ASK })
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

/** A fake borg that answers post_tool with $CTX as additionalContext and prompt with $PROMPT_CTX, each
 *  only once the file $POST_HOLD (or $PROMPT_HOLD) names is gone. */
function contextBorg(): string {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-live-"))
  const bin = join(dir, "borg-context")
  writeFileSync(
    bin,
    `#!/bin/sh\ncat >/dev/null\nanswer() { printf '{"hookSpecificOutput":{"hookEventName":"%s","additionalContext":"%s"}}\\n' "$1" "$2"; }\n` +
      `[ "$3" = post_tool ] && [ -n "$POST_HOLD" ] && while [ -e "$POST_HOLD" ]; do sleep 0.05; done\n` +
      `[ "$3" = post_tool ] && [ -n "$CTX" ] && answer PostToolUse "$CTX"\n` +
      `[ "$3" = prompt ] && [ -n "$PROMPT_HOLD" ] && while [ -e "$PROMPT_HOLD" ]; do sleep 0.05; done\n` +
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

test("opencode 2: a prompt context whose synthetic insert fails is not lost: the next tool call carries it", async () => {
  process.env.BORG_BIN = contextBorg()
  const f = fakeCtx(() => PLAN)
  const insert = f.ctx.session.synthetic
  let fail = true
  f.ctx.session.synthetic = async (input: any) => {
    if (fail) throw new Error("synthetic refused")
    return insert(input)
  }
  const cleanup = await plugin.setup(f.ctx)
  process.env.PROMPT_CTX = "[borg] message from a peer"
  try {
    await f.hooks["session.prompt"]({ sessionID: SESSION, messageID: "msg_1", prompt: { text: "Fix the relay race" }, delivery: "steer" })
  } finally {
    delete process.env.PROMPT_CTX
  }
  assert.equal(f.synthetics.length, 0)
  fail = false
  const result = { content: [{ type: "text", text: "ok" }] }
  await f.hooks["tool.execute.after"]({ tool: "read", sessionID: SESSION, agent: "build", messageID: "msg_2", id: "c1", input: { path: "/work/repo/a.rs" }, status: "completed", result })
  assert.deepEqual(f.synthetics.map((x) => x.text), ["[borg] message from a peer"])
  await cleanup()
})

/** A hold file the fake borg waits on and the adapter's log, for an answer that comes late: `held` sets
 *  `env` and the hold, runs `call` (the adapter gives up on the answer after its budget, which it cannot
 *  have had), then lets the answer go and waits until the adapter kept it. */
function lateAnswers() {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-late-"))
  const [hold, log] = [join(dir, "hold"), join(dir, "log")]
  process.env.BORG_OPENCODE_LOG = log
  const kept = () => (existsSync(log) ? readFileSync(log, "utf8").split("\n").filter((l) => l.includes("context came late")).length : 0)
  const held = async <T,>(env: Record<string, string>, call: () => Promise<T>): Promise<T> => {
    const before = kept()
    writeFileSync(hold, "")
    Object.assign(process.env, env)
    try {
      return await call()
    } finally {
      for (const k of Object.keys(env)) delete process.env[k]
      rmSync(hold)
      await until(() => kept() === before + 1)
    }
  }
  return { hold, held }
}

test("a tool context slower than the tool call's budget (post_ms) is never lost: it comes with the next tool call or the next prompt", async () => {
  process.env.BORG_BIN = contextBorg()
  const { hold, held } = lateAnswers()
  try {
    // opencode 1: the next tool call.
    const { client } = fakeClient(() => PLAN)
    const hooks = await load(client, "/work/repo")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("Fix the relay race"))
    assert.equal(await held({ CTX: "LATE-1", POST_HOLD: hold }, () => after1(hooks, "c1")), "fn deliver() {}", "the tool call does not wait for it")
    assert.equal(await after1(hooks, "c2"), "fn deliver() {}\n\nLATE-1")
    assert.equal(await after1(hooks, "c3"), "fn deliver() {}", "delivered once")

    // opencode 2: the next prompt, before borg's own prompt context.
    const v2 = await setupV2()
    await v2.say("Fix the relay race")
    const after = () => v2.hooks["tool.execute.after"]({ tool: "edit", sessionID: SESSION, id: "c1", input: { path: "/work/repo/relay.rs" }, status: "completed", result: { content: [] } })
    await held({ CTX: "LATE-2", POST_HOLD: hold }, after)
    assert.equal(v2.synthetics.length, 0)
    process.env.PROMPT_CTX = "PROMPT-CONTEXT"
    await v2.say("now the queue")
    assert.deepEqual(v2.synthetics.map((s: any) => s.text), ["LATE-2\n\nPROMPT-CONTEXT"])
    await v2.cleanup()
  } finally {
    delete process.env.PROMPT_CTX
    delete process.env.BORG_OPENCODE_LOG
  }
})

test("a prompt context slower than the prompt's budget (prompt_ms) is never lost: it comes with the next tool call", async () => {
  process.env.BORG_BIN = contextBorg()
  const { hold, held } = lateAnswers()
  try {
    const { client } = fakeClient(() => PLAN)
    const hooks = await load(client, "/work/repo")
    const msg = userMessage("Fix the relay race")
    await held({ PROMPT_CTX: "LATE-PROMPT", PROMPT_HOLD: hold }, () => hooks["chat.message"]!({ sessionID: SESSION }, msg))
    assert.equal(msg.parts.length, 1, "the message does not wait for it")
    assert.equal(await after1(hooks, "c1"), "fn deliver() {}\n\nLATE-PROMPT")
    assert.equal(await after1(hooks, "c2"), "fn deliver() {}", "delivered once")
  } finally {
    delete process.env.BORG_OPENCODE_LOG
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

// ---- borg listen: the plugin binds a listening session, keeps its waiter and wakes it when idle.

/** A fake borg for listening sessions: `listen --stream` records `stream\t<args>`, prints what is
 *  written to the wake file (each line once), keeps the plugin's answers on its stdin in the answers
 *  file and records `stopped` on SIGTERM; any other `listen` records `listen\t<args>` and answers as
 *  borg does; a hook records `<event>\t<stdin>`, answers a stop with $STOP_ANSWER and a post_tool with
 *  $POST_ANSWER, that one only once the file $POST_HOLD names is gone. Returns [binary, record file, wake
 *  file, adapter log, answers file]. */
function listenFake(): [string, string, string, string, string] {
  const dir = mkdtempSync(join(tmpdir(), "borg-oc-listen-"))
  const [bin, rec, wake, log, answers] = [join(dir, "borg"), join(dir, "rec"), join(dir, "wake"), join(dir, "log"), join(dir, "answers")]
  writeFileSync(
    bin,
    `#!/bin/sh
case "$1 $2" in
"listen --stream") printf 'stream\\t%s\\n' "$*" >> "${rec}"
  # A background job of a script reads /dev/null unless given the script's own stdin explicitly.
  exec 3<&0; cat <&3 >> "${answers}" & reader=$!
  trap 'kill $reader 2>/dev/null; printf "stopped\\n" >> "${rec}"; exit 143' TERM
  while :; do if [ -s "${wake}" ]; then cat "${wake}"; : > "${wake}"; fi; sleep 0.05; done;;
"listen --off") printf 'listen\\t%s\\n' "$*" >> "${rec}"; echo "[borg listen] this session listens no more"; exit 0;;
"listen "*) printf 'listen\\t%s\\n' "$*" >> "${rec}"; [ -n "$LISTEN_HOLD" ] && while [ -e "$LISTEN_HOLD" ]; do sleep 0.05; done
  echo "[borg listen] This session listens: a message for it wakes it while it is idle."; exit 0;;
esac
in=$(cat)
printf '%s\\t%s\\n' "$3" "$in" >> "${rec}"
[ "$3" = stop ] && [ -n "$STOP_ANSWER" ] && echo "$STOP_ANSWER"
[ "$3" = post_tool ] && [ -n "$POST_HOLD" ] && while [ -e "$POST_HOLD" ]; do sleep 0.05; done
[ "$3" = post_tool ] && [ -n "$POST_ANSWER" ] && printf '%s\\n' "$POST_ANSWER"
exit 0
`,
  )
  chmodSync(bin, 0o755)
  return [bin, rec, wake, log, answers]
}

/** The plugin's answers to the waiter's wakes, as the fake kept them. */
const answered = (answers: string): any[] => (existsSync(answers) ? readFileSync(answers, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : [])

function lines(rec: string): string[][] {
  return existsSync(rec) ? readFileSync(rec, "utf8").trim().split("\n").filter(Boolean).map((l) => l.split("\t")) : []
}

/** Until `done()`, checked every 20 ms; fails after 15 s (a bound on a hang, not a timing margin). */
async function until(done: () => boolean | Promise<boolean>) {
  const t0 = Date.now()
  while (!(await done())) {
    assert.ok(Date.now() - t0 < 15_000, "the awaited event never came")
    await new Promise((r) => setTimeout(r, 20))
  }
}

const logged = (log: string, what: string) => existsSync(log) && readFileSync(log, "utf8").includes(what)
const WAKE = "[borg listen] 1 message from other sessions waits for you (Two of Five tells)."
/** The waiter's line for a wake of one message, `id`. */
const wakeLine = (text: string, id = "m000000000001") => JSON.stringify({ wake: text, n: 1, ids: [id] }) + "\n"

test("opencode 1: a session's `borg listen` is bound by the plugin, its waiter starts there, a wake goes in by promptAsync only once idle, --off and the session's end stop it", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  delete process.env.STOP_ANSWER
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const streams = () => lines(rec).filter((l) => l[0] === "stream")
  try {
    assert.equal(process.env.BORG_LISTEN_BINDER, "opencode", "every command the model runs knows the plugin binds")
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("listen for the others"))
    await hooks["tool.execute.before"]!({ tool: "bash", sessionID: SESSION, callID: "l1" }, { args: { command: "borg listen" } })
    // The shell's own run binds nothing under the plugin (listen.rs `BINDER_ENV`) and says so.
    const out = { title: "", output: "[borg listen] the opencode plugin binds the session that ran this; its answer follows", metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "l1", args: { command: "borg listen" } }, out)
    assert.match(out.output, /its answer follows\n\n\[borg listen\] This session listens/, "the model reads the bind's answer")
    assert.deepEqual(lines(rec).find((l) => l[0] === "listen")!.slice(1), [`listen --session ${SESSION} --dir /work/repo`])
    await until(() => streams().length === 1)
    assert.deepEqual(streams()[0].slice(1), [`listen --stream --session ${SESSION} --dir /work/repo`])
    // A message arrives while the turn runs: held.
    writeFileSync(wake, '{"note":"n"}\n' + wakeLine(WAKE))
    await until(() => logged(log, "a wake, held while the turn runs"))
    assert.deepEqual(prompts, [], "no wake inside the turn")
    await hooks.event!(idleEvent)
    await until(() => prompts.length === 1)
    assert.deepEqual(prompts[0], { path: { id: SESSION }, body: { parts: [{ type: "text", text: WAKE, ...marked }] } })
    // Its echo is ours, not the engineer's: no prompt reaches borg.
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage(WAKE, marked))
    assert.equal(lines(rec).filter((l) => l[0] === "prompt").length, 1)
    // A shell run that said it itself: the bind's answer is not repeated.
    const off = { title: "", output: "[borg listen] this session listens no more", metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "l2", args: { command: "borg listen --off" } }, off)
    assert.equal(off.output, "[borg listen] this session listens no more")
    assert.deepEqual(lines(rec).filter((l) => l[0] === "listen").at(-1)!.slice(1), [`listen --off --session ${SESSION} --dir /work/repo`])
    await until(() => lines(rec).some((l) => l[0] === "stopped"))
    // Another session's listen, or a waiter's command line: nothing bound.
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: "ses_plain", callID: "p1", args: { command: "borg listen --session other" } }, { title: "", output: "", metadata: {} })
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: "ses_plain", callID: "p2", args: { command: "borg listen --wait" } }, { title: "", output: "", metadata: {} })
    assert.equal(lines(rec).filter((l) => l[0] === "listen").length, 2)
    // A stop that says the session listens with no waiter starts it again; the session's end stops it.
    process.env.STOP_ANSWER = '{"listen":true}'
    await hooks.event!(idleEvent)
    await until(() => streams().length === 2)
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
    await until(() => lines(rec).filter((l) => l[0] === "stopped").length === 2)
    assert.ok(lines(rec).some((l) => l[0] === "end"))
    assert.equal(prompts.length, 1)
  } finally {
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  }
})

test("opencode 2: a stop's listen starts the waiter; a wake goes in by session.prompt with delivery queue once idle, also after an interrupted turn, which borg hears as halted", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const v2 = await setupV2()
  try {
    // A plain session's interruption: nothing sent.
    await v2.emit("session.execution.interrupted", { sessionID: "ses_plain_halt", reason: "user" })
    await v2.say("listen for the others")
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    assert.equal(lines(rec).filter((l) => l[0] === "listen").length, 0, "a stop's listen binds nothing")
    await v2.say("now look at the parser")
    writeFileSync(wake, wakeLine(WAKE))
    await until(() => logged(log, "a wake, held while the turn runs"))
    assert.deepEqual(v2.prompts, [], "no wake inside the turn")
    // Esc: the turn is interrupted, no stop; the session is idle, so the wake goes in.
    await v2.emit("session.execution.interrupted", { sessionID: SESSION, reason: "user" })
    await until(() => v2.prompts.length === 1)
    assert.deepEqual(v2.prompts[0], { sessionID: SESSION, text: WAKE, metadata: { borg: true }, delivery: "queue" })
    await until(() => lines(rec).some((l) => l[0] === "halted"))
    assert.deepEqual(JSON.parse(lines(rec).find((l) => l[0] === "halted")![1]), { session_id: SESSION, cwd: "/work/repo", pid: process.pid, ...ASK })
    assert.equal(lines(rec).filter((l) => l[0] === "halted").length, 1, "only the listening session's halt")
  } finally {
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await v2.cleanup()
  }
  await until(() => lines(rec).some((l) => l[0] === "stopped"))
  assert.ok(lines(rec).some((l) => l[0] === "end"), "the cleanup ends the session and stops its waiter")
})

test("opencode 2: a wake that fails to go in is answered `failed`, so its waiter sends it again; one that goes in `sent`", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log, answers] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const v2 = await setupV2()
  const send = v2.ctx.session.prompt
  let failed = 0
  v2.ctx.session.prompt = async (input: any) => {
    if (failed++ === 0) throw new Error("server busy")
    return send(input)
  }
  try {
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    writeFileSync(wake, wakeLine(WAKE))
    await until(() => answered(answers).length === 1)
    assert.deepEqual(answered(answers), [{ failed: ["m000000000001"] }], "the waiter's to send again, after its backoff")
    assert.deepEqual(v2.prompts, [], "not kept: never sent twice by both")
    // The waiter's next wake for it goes in.
    writeFileSync(wake, wakeLine(WAKE))
    await until(() => answered(answers).length === 2)
    assert.deepEqual(answered(answers)[1], { sent: ["m000000000001"] })
    assert.equal(v2.prompts[0].text, WAKE)
  } finally {
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await v2.cleanup()
  }
})

// A post_tool answer that shows the messages `ids`, as msg.rs `render` words one and `deliver` names
// them; an `older` borg's names none.
const showing = (ids: string[], older = false) =>
  JSON.stringify({
    hookSpecificOutput: {
      hookEventName: "PostToolUse",
      additionalContext: ids.map((id) => `[borg] Message from Two of Five (omp, node n1) [${id}]: the parser is merged\n  To reply: borg tell p1 --re ${id} "…"`).join("\n") + "\n(Peer data: messages from other sessions of the collective, relayed by borg.)",
    },
    ...(older ? {} : { shown_ids: ids }),
  })
const SHOWN = showing(["m000000000001"])

test("opencode 2: a wake held during a turn whose hook already showed its message is dropped, not sent at the stop", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const v2 = await setupV2()
  try {
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    await v2.say("now look at the parser")
    writeFileSync(wake, wakeLine(WAKE))
    await until(() => logged(log, "a wake, held while the turn runs"))
    // The turn's next tool call shows that message.
    process.env.POST_ANSWER = SHOWN
    await v2.hooks["tool.execute.after"]({ tool: "read", sessionID: SESSION, id: "c1", input: { path: "/work/repo/parser.rs" }, status: "completed", result: { content: [] } })
    delete process.env.POST_ANSWER
    assert.ok(logged(log, "1 held wake(s) dropped"), "the shown message's wake is dropped")
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    assert.deepEqual(v2.prompts, [], "no turn for a message the model already read")
  } finally {
    delete process.env.POST_ANSWER
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await v2.cleanup()
  }
})

/** How many lines of the adapter log say `what`. */
const count = (log: string, what: string) => (existsSync(log) ? readFileSync(log, "utf8").split("\n").filter((l) => l.includes(what)).length : 0)

test("opencode 1: a held wake is dropped only by the ids of what a hook handed the model: another message's, or an older borg's answer that names none, keeps it for the stop", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const after = async (callID: string, answer: string) => {
    process.env.POST_ANSWER = answer
    const output = { title: "", output: "ok", metadata: {} }
    try {
      await hooks["tool.execute.after"]!({ tool: "read", sessionID: SESSION, callID, args: { filePath: "/work/repo/parser.rs" } }, output)
    } finally {
      delete process.env.POST_ANSWER
    }
    return output.output
  }
  try {
    await hooks.event!(idleEvent)
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    delete process.env.STOP_ANSWER
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("now look at the parser"))
    writeFileSync(wake, wakeLine(WAKE))
    await until(() => count(log, "held while the turn runs") === 1)
    assert.match(await after("c1", showing(["m000000000002"])), /\[m000000000002\]/, "a newer message took the budget")
    assert.match(await after("c2", showing(["m000000000001"], true)), /\[m000000000001\]/, "an older borg's answer names none")
    assert.equal(count(log, "held wake(s) dropped"), 0)
    await hooks.event!(idleEvent)
    await until(() => prompts.length === 1)
    assert.equal(prompts[0].body.parts[0].text, WAKE, "not known to be read: the wake goes in")
    // The wake's turn: a hook that hands over the next wake's message drops it.
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage(WAKE, marked))
    writeFileSync(wake, wakeLine(WAKE, "m000000000003"))
    await until(() => count(log, "held while the turn runs") === 2)
    await after("c3", showing(["m000000000003"]))
    assert.equal(count(log, "1 held wake(s) dropped"), 1)
    await hooks.event!(idleEvent)
    assert.equal(prompts.length, 1, "no turn for a message the model already read")
  } finally {
    delete process.env.POST_ANSWER
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  }
})

test("opencode 2: an answer later than post_ms drops no held wake before a call hands it over: a stop first sends the wake in, the next call drops one", { timeout: 30_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  const hold = `${rec}.hold`
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const v2 = await setupV2()
  const after = (id: string) => v2.hooks["tool.execute.after"]({ tool: "read", sessionID: SESSION, id, input: { path: "/work/repo/parser.rs" }, status: "completed", result: { content: [] } })
  // The call's answer, showing `id`, is held until the call gave up on it (post_ms): it cannot come in time.
  const slow = async (call: string, id: string) => {
    const late = count(log, "context came late")
    writeFileSync(hold, "")
    Object.assign(process.env, { POST_HOLD: hold, POST_ANSWER: showing([id]) })
    try {
      await after(call)
    } finally {
      delete process.env.POST_HOLD
      delete process.env.POST_ANSWER
      rmSync(hold)
    }
    await until(() => count(log, "context came late") === late + 1)
  }
  const text = (id: string) => v2.synthetics.filter((s: any) => s.text.includes(`[${id}]`)).length
  try {
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    delete process.env.STOP_ANSWER
    await v2.say("now look at the parser")
    writeFileSync(wake, wakeLine(WAKE))
    await until(() => count(log, "held while the turn runs") === 1)
    await slow("c1", "m000000000001")
    assert.equal(text("m000000000001"), 0, "borg answered, the model has not read it")
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => v2.prompts.length === 1)
    assert.equal(v2.prompts[0].text, WAKE, "the turn ended first: the wake goes in")
    // The wake's turn: the next wake's message comes late too, and the call after hands it over.
    await v2.say(WAKE, marked.metadata)
    writeFileSync(wake, wakeLine(WAKE, "m000000000002"))
    await until(() => count(log, "held while the turn runs") === 2)
    await slow("c2", "m000000000002")
    assert.equal(text("m000000000001"), 1, "the first late answer came with this call")
    assert.equal(count(log, "held wake(s) dropped"), 0)
    await after("c3")
    assert.equal(text("m000000000002"), 1)
    assert.equal(count(log, "1 held wake(s) dropped"), 1, "handed over: its wake goes")
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    assert.equal(v2.prompts.length, 1, "no turn for a message the model already read")
  } finally {
    delete process.env.POST_HOLD
    delete process.env.POST_ANSWER
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await v2.cleanup()
  }
})

/** The waiter's line for a wake of the messages `ids`. */
const wakeOf = (ids: string[]) => JSON.stringify({ wake: `[borg listen] ${ids.length} messages from other sessions wait for you.`, n: ids.length, ids }) + "\n"

test("opencode 1: a held wake is dropped once the turn's hooks, over several, handed the model every message it names, and one that comes after they did as it comes; the next turn starts with none handed", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const { client, prompts } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  const after = async (callID: string, answer: string) => {
    process.env.POST_ANSWER = answer
    const output = { title: "", output: "ok", metadata: {} }
    try {
      await hooks["tool.execute.after"]!({ tool: "read", sessionID: SESSION, callID, args: { filePath: "/work/repo/parser.rs" } }, output)
    } finally {
      delete process.env.POST_ANSWER
    }
    return output.output
  }
  try {
    await hooks.event!(idleEvent)
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    delete process.env.STOP_ANSWER
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("now look at the parser"))
    writeFileSync(wake, wakeOf(["m000000000001", "m000000000002"]))
    await until(() => count(log, "held while the turn runs") === 1)
    // borg's context budget split the two: the newest first, the other with the next call.
    assert.match(await after("c1", showing(["m000000000002"])), /\[m000000000002\]/)
    assert.equal(count(log, "held wake(s) dropped"), 0, "one of its messages is still unread")
    assert.match(await after("c2", showing(["m000000000001"])), /\[m000000000001\]/)
    assert.equal(count(log, "1 held wake(s) dropped"), 1, "the turn handed over both")
    // The waiter took the next message before c3's claim of it landed: its wake comes after c3 showed it.
    await after("c3", showing(["m000000000003"]))
    writeFileSync(wake, wakeOf(["m000000000003"]))
    await until(() => count(log, "held while the turn runs") === 2)
    assert.equal(count(log, "1 held wake(s) dropped"), 2, "dropped as it comes")
    await hooks.event!(idleEvent)
    assert.deepEqual(prompts, [], "no turn for messages the model already read")
    // The next turn: what the last one handed over is gone (borg claimed it shown, the waiter names it no
    // more), so a wake that names it goes in at the stop.
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("and now the lexer"))
    writeFileSync(wake, wakeOf(["m000000000003"]))
    await until(() => count(log, "held while the turn runs") === 3)
    assert.equal(count(log, "held wake(s) dropped"), 2)
    await hooks.event!(idleEvent)
    await until(() => prompts.length === 1)
    assert.equal(prompts[0].body.parts[0].text, JSON.parse(wakeOf(["m000000000003"])).wake)
  } finally {
    delete process.env.POST_ANSWER
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  }
})

test("opencode 2: a held wake is dropped once two calls' steers handed over the messages it names, and one that comes after they did as it comes", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const v2 = await setupV2()
  const after = async (id: string, answer: string) => {
    process.env.POST_ANSWER = answer
    try {
      await v2.hooks["tool.execute.after"]({ tool: "read", sessionID: SESSION, id, input: { path: "/work/repo/parser.rs" }, status: "completed", result: { content: [] } })
    } finally {
      delete process.env.POST_ANSWER
    }
  }
  try {
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    delete process.env.STOP_ANSWER
    await v2.say("now look at the parser")
    writeFileSync(wake, wakeOf(["m000000000001", "m000000000002"]))
    await until(() => count(log, "held while the turn runs") === 1)
    await after("c1", showing(["m000000000001"]))
    assert.equal(count(log, "held wake(s) dropped"), 0)
    await after("c2", showing(["m000000000002"]))
    assert.equal(count(log, "1 held wake(s) dropped"), 1)
    await after("c3", showing(["m000000000003"]))
    writeFileSync(wake, wakeOf(["m000000000003"]))
    await until(() => count(log, "held while the turn runs") === 2)
    assert.equal(count(log, "1 held wake(s) dropped"), 2)
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    assert.deepEqual(v2.prompts, [], "no turn for messages the model already read")
  } finally {
    delete process.env.POST_ANSWER
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await v2.cleanup()
  }
})

// Guard, borg.ts HANDED's value: the most ids a turn keeps of those it handed over.
const HANDED = 1_024

test("opencode 2: a turn keeps the newest HANDED ids it handed over: a wake naming an older one goes in at the stop, one naming a newer is dropped", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const v2 = await setupV2()
  const ids = Array.from({ length: HANDED + 1 }, (_, i) => `m${String(i + 1).padStart(12, "0")}`)
  const [oldest, newest] = [ids[0], ids[HANDED]]
  try {
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    delete process.env.STOP_ANSWER
    await v2.say("now look at the parser")
    // One answer names them all (its text need not: the plugin keeps what `shown_ids` names).
    process.env.POST_ANSWER = JSON.stringify({ hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: "[borg] messages" }, shown_ids: ids })
    await v2.hooks["tool.execute.after"]({ tool: "read", sessionID: SESSION, id: "c1", input: { path: "/work/repo/parser.rs" }, status: "completed", result: { content: [] } })
    delete process.env.POST_ANSWER
    writeFileSync(wake, wakeOf([oldest]) + wakeOf([newest]))
    await until(() => count(log, "held while the turn runs") === 2)
    assert.equal(count(log, "1 held wake(s) dropped"), 1, "the newest's")
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => v2.prompts.length === 1)
    assert.equal(v2.prompts[0].text, JSON.parse(wakeOf([oldest])).wake, "the id let go: its wake goes in")
  } finally {
    delete process.env.POST_ANSWER
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await v2.cleanup()
  }
})

test("opencode 2: a wake send that fails after the engineer's prompt leaves the turn busy: the next wake waits for its stop", { timeout: 20_000 }, async () => {
  const [bin, rec, wake, log, answers] = listenFake()
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.STOP_ANSWER = '{"listen":true}'
  const v2 = await setupV2()
  const send = v2.ctx.session.prompt
  let fail: ((e: Error) => void) | undefined
  v2.ctx.session.prompt = (input: any) => (fail ? send(input) : new Promise<void>((_, reject) => (fail = reject)))
  try {
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    writeFileSync(wake, wakeLine(WAKE))
    await until(() => fail !== undefined)
    // The engineer types while the wake's send hangs; then the send fails.
    await v2.say("now look at the parser")
    fail!(new Error("server busy"))
    await until(() => logged(log, "failed: Error: server busy"))
    assert.deepEqual(answered(answers), [{ failed: ["m000000000001"] }], "the failed one is the waiter's again")
    writeFileSync(wake, wakeLine(WAKE, "m000000000002"))
    await until(() => readFileSync(log, "utf8").split("\n").filter((l) => l.includes(": a wake")).length === 2)
    assert.ok(readFileSync(log, "utf8").split("\n").filter((l) => l.includes(": a wake")).at(-1)!.includes("held while the turn runs"), "the turn still runs")
    assert.deepEqual(v2.prompts, [], "nothing sent into the engineer's turn")
    await v2.emit("session.execution.succeeded", { sessionID: SESSION })
    await until(() => v2.prompts.length === 1)
    assert.equal(v2.prompts[0].text, WAKE, "the held wake goes in at its stop")
    await until(() => answered(answers).length === 2)
    assert.deepEqual(answered(answers)[1], { sent: ["m000000000002"] })
  } finally {
    delete process.env.STOP_ANSWER
    delete process.env.BORG_OPENCODE_LOG
    await v2.cleanup()
  }
})

test("opencode 1: a slow bind of `borg listen` does not hold the tool result: its answer comes with the next call", { timeout: 20_000 }, async () => {
  const [bin, rec, , log] = listenFake()
  const hold = `${rec}.hold`
  writeFileSync(hold, "")
  process.env.BORG_BIN = bin
  process.env.BORG_OPENCODE_LOG = log
  process.env.LISTEN_HOLD = hold
  delete process.env.STOP_ANSWER
  const { client } = fakeClient(() => PLAN)
  const hooks = await load(client, "/work/repo")
  try {
    await hooks["chat.message"]!({ sessionID: SESSION }, userMessage("listen for the others"))
    const out = { title: "", output: "[borg listen] the opencode plugin binds the session that ran this; its answer follows", metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "l1", args: { command: "borg listen" } }, out)
    // The bind is still held (its file exists), yet the tool result is back.
    assert.ok(existsSync(hold) && lines(rec).some((l) => l[0] === "listen"), "the bind runs and cannot have finished")
    assert.doesNotMatch(out.output, /This session listens/)
    rmSync(hold)
    await until(() => lines(rec).some((l) => l[0] === "stream"))
    const next = { title: "", output: "ok", metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: SESSION, callID: "l2", args: { command: "ls" } }, next)
    assert.match(next.output, /^ok\n\n\[borg listen\] This session listens/, "the bind's answer, late")
  } finally {
    delete process.env.LISTEN_HOLD
    delete process.env.BORG_OPENCODE_LOG
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: SESSION } } } as any })
  }
})

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

test("real borg, opencode 1: the plugin binds the session that ran `borg listen`, a tell wakes it once idle, its end stops the waiter", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)`, timeout: 30_000 }, async () => {
  const saved = { ...process.env }
  const root = mkdtempSync(join(tmpdir(), "borg-oc-listen-real-"))
  const work = join(root, "harbor_orion")
  for (const d of [work, join(root, "store/collectives/corp"), join(root, "home")]) mkdirSync(d, { recursive: true })
  writeFileSync(join(root, "collectives.json"), JSON.stringify({ default: "off", collectives: [{ name: "corp", paths: [work], share: "local" }] }))
  for (const k of ["BORG_STATE", "BORG_CONSCIENCE", "PI_SESSION_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "OPENCODE_PID", "STOP_ANSWER"]) delete process.env[k]
  Object.assign(process.env, {
    BORG_BIN: BORG, BORG_HOME: join(root, "store"), BORG_CONFIG: join(root, "collectives.json"), HOME: join(root, "home"),
    BORG_QUEEN: "off", BORG_LEARN: "off", BORG_VECTORS: "off", BORG_STOP_ATTENTION: "off", BORG_MODE: "hook", BORG_MATCH: "bm25", BORG_PREDICT_MODEL: "off",
  })
  const S = "ses_listen_real1"
  const { client, prompts } = fakeClient(() => "Listening.")
  const hooks = await load(client, work)
  // borg's files name a session by its letters, digits and dashes (presence.rs `safe`).
  const S2 = "ses_listen_real2"
  const listenDir = join(root, "store/collectives/corp/listen")
  const lock = join(listenDir, `${S.replace(/[^A-Za-z0-9-]/g, "")}.lock`)
  try {
    await hooks["chat.message"]!({ sessionID: S }, userMessage("listen for the others"))
    // A second root session of this process, touched last: the pid names both.
    await hooks["chat.message"]!({ sessionID: S2 }, userMessage("carry on"))
    await hooks["tool.execute.before"]!({ tool: "bash", sessionID: S, callID: "l1" }, { args: { command: "borg listen" } })
    // S's shell runs real borg, as opencode's shell does: OPENCODE_PID set, the plugin's BORG_LISTEN_BINDER inherited.
    const shell = execFileSync(BORG, ["listen"], { cwd: work, env: { ...process.env, OPENCODE_PID: String(process.pid) }, encoding: "utf8" })
    assert.doesNotMatch(shell, /This session listens/, "the shell binds no session by the pid")
    const out = { title: "", output: shell, metadata: { exit: 0 } }
    await hooks["tool.execute.after"]!({ tool: "bash", sessionID: S, callID: "l1", args: { command: "borg listen" } }, out)
    assert.match(out.output, /\[borg listen\] This session listens/, "the plugin's bind answers")
    assert.ok(!existsSync(join(listenDir, `${S2.replace(/[^A-Za-z0-9-]/g, "")}.json`)), "the other session never asked")
    await until(() => existsSync(lock) && readFileSync(lock, "utf8").trim() !== "")
    const waiter = Number(readFileSync(lock, "utf8").trim())
    await hooks.event!({ event: { type: "session.idle", properties: { sessionID: S } } as any })
    execFileSync(BORG, ["tell", S, "the parser is merged"], { cwd: work, env: process.env, encoding: "utf8" })
    await until(() => prompts.length === 1)
    const text = prompts[0].body.parts[0].text
    assert.match(text, /^\[borg listen\] 1 message from other sessions waits for you \(engineer tells\)/)
    assert.doesNotMatch(text, /parser/, "the wake never carries a body")
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: S } } } as any })
    await until(() => !alive(waiter))
  } finally {
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: S } } } as any })
    await hooks.event!({ event: { type: "session.deleted", properties: { info: { id: S2 } } } as any })
    for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k]
    Object.assign(process.env, saved)
  }
})
