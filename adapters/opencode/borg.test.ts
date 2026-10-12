// Contract test: opencode-shaped hooks/events go into the plugin, the plugin's canonical JSON goes
// into `borg hook opencode`, and borg's answer comes back into the session.
//
//   cargo build -p borg && node --test adapters/opencode/
//
// Node >= 23.6 runs the TypeScript directly. BORG_TEST_BIN overrides the borg binary under test.

import { test } from "node:test"
import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, writeFileSync, readFileSync, existsSync, chmodSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import plugin from "./borg.ts"

const BORG = process.env.BORG_TEST_BIN ?? resolve(import.meta.dirname, "../../target/debug/borg")

const SESSION = "ses_contract1"
const PLAN = "I check closed.get() on the AtomicBoolean before deliver(); reader and drainer threads race."

/** A stand-in for opencode's SDK client: a session history and a record of prompts sent back.
 *  `last` adds to the assistant message (e.g. `info.error`, more parts). */
function fakeClient(assistantText: () => string, last: () => { info?: object; parts?: object[] } = () => ({})) {
  const prompts: any[] = []
  const client = {
    session: {
      messages: async () => ({
        data: [
          { info: { role: "user" }, parts: [{ type: "text", text: "fix it" }] },
          {
            info: { role: "assistant", ...last().info },
            parts: [{ type: "text", text: assistantText() }, { type: "tool", tool: "read" }, ...(last().parts ?? [])],
          },
        ],
      }),
      promptAsync: async (opts: any) => {
        prompts.push(opts)
        return { data: undefined }
      },
    },
  }
  return { client, prompts }
}

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

test("borg's prompt context (a queen question) joins the engineer's message as a synthetic part, once", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg)` }, async () => {
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

test("real borg: conscience asks once per session, corrections and rejections become episodes", { skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg)` }, async () => {
  const home = mkdtempSync(join(tmpdir(), "borg-home-"))
  process.env.BORG_BIN = BORG
  process.env.BORG_HOME = home
  process.env.BORG_STATE = join(home, "state")
  delete process.env.BORG_LEARN
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
