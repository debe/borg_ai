// borg adapter for opencode (written against opencode 1.18.32 / @opencode-ai/plugin 1.18.32).
//
// Translates opencode plugin hooks and bus events into borg's one canonical, Claude-shaped hook
// event and pipes it to `borg hook opencode <event>`:
//
//   chat.message (user text)                 -> prompt
//   tool.execute.before                      -> pre_tool (throws borg's reason: a paused drone's
//                                               tool call is refused)
//   tool.execute.after                       -> post_tool
//   tool part in state "error" (incl. a
//   rejected permission with feedback)       -> post_tool_failure
//   session.idle                             -> stop; a {"decision":"block","reason"} answer is sent
//                                               back into the session as a user message, at most
//                                               once per engineer prompt (the next idle carries
//                                               stop_hook_active: true, like Claude's Stop hook).
//                                               No stop when the turn was halted (Esc/abort, a
//                                               provider error, a permission rejected without
//                                               feedback): Claude's Stop hook skips interrupts too.
//   session.deleted, plugin dispose          -> end
//
// Subagent (child) sessions are ignored, like Claude's hooks ignore subagents, with one exception:
// a child's tool call still asks pre_tool, as its root session, so a paused drone cannot carry on
// through a subagent.
// Fails open: if borg is missing, slow or broken, opencode carries on untouched.
// borg is found via $BORG_BIN, else `borg` on PATH. $BORG_OPENCODE_LOG names a file that gets
// what the adapter did and every error it swallowed.
//
// The module has a single default export: opencode calls every exported function as a plugin.

import { spawn } from "node:child_process"
import { appendFileSync } from "node:fs"
import type { Plugin, PluginModule } from "@opencode-ai/plugin"

const TIMEOUT_MS = 60_000 // a stop with lessons makes one model call (the consequence model)
const PRE_MS = 2_000 // before every tool call: borg answers in a few ms (a stat when nobody steers)
// Hard cap on questions sent back into a session per engineer prompt, so nothing can make the
// plugin prompt the drone in a loop.
const MAX_ROUNDS = 1
// Marks the text part we send back, so its chat.message echo is recognised without comparing text.
const MARK = { borg: true }
// opencode's PermissionRejectedError: the engineer said no without feedback, which halts the turn.
const REJECTED = "The user rejected permission to use this specific tool call."

// opencode tool ids -> the Claude names borg matches on (borg reads Bash command, Grep/Glob pattern,
// else file_path/path from tool_input). Unknown tools keep their id.
const TOOL_NAMES: Record<string, string> = {
  bash: "Bash", read: "Read", write: "Write", edit: "Edit", multiedit: "MultiEdit", apply_patch: "Edit",
  grep: "Grep", glob: "Glob", list: "LS", webfetch: "WebFetch", websearch: "WebSearch", task: "Task",
  todowrite: "TodoWrite",
}

function toolInput(tool: string, args: any): Record<string, unknown> {
  const input: Record<string, unknown> = { ...(args ?? {}) }
  if (typeof input.filePath === "string") input.file_path = input.filePath
  // apply_patch (GPT models) names its files only inside the patch text.
  if (tool === "apply_patch" && typeof input.patchText === "string") {
    const files = [...input.patchText.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)].map((m) => m[1].trim())
    if (files.length) input.file_path = files.join(" ")
  }
  return input
}

// opencode says "The user rejected permission to use this specific tool call with the following
// feedback: X"; borg's rejection capture speaks Claude's wording, so re-phrase it.
function canonicalError(error: string): string {
  const m = /rejected permission to use this specific tool call with the following feedback:\s*([\s\S]*)/i.exec(error)
  if (!m) return error
  return `The user doesn't want to proceed with this tool use. The tool use was rejected. To tell you how to proceed, the user said:\n${m[1]}`
}

/** The engineer- or drone-visible text of message parts (no synthetic or ignored parts). */
function visibleText(parts: any[] | undefined): string {
  return (parts ?? [])
    .filter((p: any) => p?.type === "text" && !p.synthetic && !p.ignored)
    .map((p: any) => p.text)
    .join("\n")
}

/** Why the turn that ended with assistant message `msg` was halted rather than finished, or "". */
function halted(msg: any): string {
  const error = msg?.info?.error
  if (error) return String(error.name ?? "error") // MessageAbortedError (Esc, abort), APIError, ...
  const rejected = (msg?.parts ?? []).some(
    (p: any) => p?.type === "tool" && p.state?.status === "error" && String(p.state.error ?? "").trim() === REJECTED,
  )
  return rejected ? "permission rejected" : ""
}

function note(msg: string) {
  const file = process.env.BORG_OPENCODE_LOG
  if (!file) return
  try {
    appendFileSync(file, `${new Date().toISOString()} ${msg}\n`)
  } catch {}
}

/** Runs `borg hook opencode <event>` with `payload` on stdin; resolves to its JSON answer or null. */
function borg(event: string, payload: Record<string, unknown>, timeoutMs = TIMEOUT_MS): Promise<any> {
  return new Promise((resolve) => {
    let out = ""
    let done = false
    const finish = (v: any) => {
      if (done) return
      done = true
      clearTimeout(timer)
      resolve(v)
    }
    let child: ReturnType<typeof spawn>
    try {
      // BORG_STOP_BUDGET_MS: borg keeps its expert drone inside this timeout, so its answer arrives.
      const env = { ...process.env, BORG_STOP_BUDGET_MS: String(TIMEOUT_MS) }
      child = spawn(process.env.BORG_BIN || "borg", ["hook", "opencode", event], { stdio: ["pipe", "pipe", "ignore"], env })
    } catch (e) {
      note(`spawn borg failed: ${e}`)
      return resolve(null)
    }
    const timer = setTimeout(() => {
      note(`borg ${event}: timed out`)
      child.kill()
      finish(null)
    }, timeoutMs)
    child.on("error", (e) => {
      note(`borg ${event}: ${e}`)
      finish(null)
    })
    child.stdout!.on("data", (d) => (out += d))
    child.on("close", () => {
      note(`borg ${event} -> ${out.trim().slice(0, 200) || "(nothing)"}`)
      try {
        finish(out.trim() ? JSON.parse(out) : null)
      } catch {
        finish(null)
      }
    })
    child.stdin!.on("error", () => {})
    child.stdin!.end(JSON.stringify(payload))
  })
}

type Session = {
  chain: Promise<unknown> // borg calls of one session run in order
  child: boolean // a subagent session: ignored (but for pre_tool, asked as its root session)
  parent?: string // a child's parent session id
  continued: boolean // we answered the last idle with borg's questions
  gen: number // counts engineer prompts: a stop answer older than the latest prompt is dropped
  rounds: number // questions sent back since the engineer's last prompt (<= MAX_ROUNDS)
  echoes: number // messages we sent whose chat.message has not come back yet
  stopping: boolean
  used: boolean // borg has heard of this session
  ended: boolean
  args: Map<string, { tool: string; args: any }> // callID -> call, for failures reported without input
  failed: Set<string> // callIDs already reported as failures
}

const server: Plugin = async ({ client, directory }) => {
  const sessions = new Map<string, Session>()
  const get = (id: string): Session => {
    let s = sessions.get(id)
    if (!s) {
      s = { chain: Promise.resolve(), child: false, continued: false, gen: 0, rounds: 0, echoes: 0, stopping: false, used: false, ended: false, args: new Map(), failed: new Set() }
      sessions.set(id, s)
    }
    return s
  }

  // The top-level session a (possibly nested) subagent session belongs to.
  const root = (id: string): string => {
    for (let i = 0; i < 32; i++) {
      const parent = sessions.get(id)?.parent
      if (!parent || parent === id) break
      id = parent
    }
    return id
  }

  // The drone's last assistant message ({info, parts}, to tell a halted turn) and its last visible
  // text: borg's "proposal" for captured corrections and the plan the conscience reads at stop.
  const lastAssistant = async (id: string): Promise<{ last?: any; text: string }> => {
    try {
      const res: any = await client.session.messages({ path: { id } })
      const msgs: any[] = res?.data ?? []
      let last: any
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i]?.info?.role !== "assistant") continue
        last ??= msgs[i]
        const text = visibleText(msgs[i].parts).trim()
        if (text) return { last, text }
      }
      return { last, text: "" }
    } catch (e) {
      note(`session.messages failed: ${e}`)
    }
    return { text: "" }
  }

  // Queue a borg call behind the session's earlier ones. `build` runs in turn, so it sees their effects.
  const send = (id: string, event: string, build: () => Promise<Record<string, unknown>>): Promise<any> => {
    const s = get(id)
    s.used = true
    // pid: opencode's own (the plugin runs in its process), so `borg top` sees a vanished opencode.
    const run = s.chain.then(async () => borg(event, { session_id: id, cwd: directory, pid: process.pid, ...(await build()) }))
    s.chain = run.catch(() => null)
    return run.catch(() => null)
  }

  const failure = (id: string, callID: string, tool: string, args: any, error: string) => {
    const s = get(id)
    s.args.delete(callID)
    if (s.child || s.failed.has(callID)) return
    s.failed.add(callID)
    send(id, "post_tool_failure", async () => ({
      tool_name: TOOL_NAMES[tool] ?? tool,
      tool_input: toolInput(tool, args),
      error: canonicalError(error),
      last_assistant_message: (await lastAssistant(id)).text,
    }))
  }

  const stop = async (id: string) => {
    const s = get(id)
    if (s.child || s.stopping) return
    s.stopping = true
    let sent = false
    try {
      const gen = s.gen
      const { last, text } = await lastAssistant(id)
      const why = halted(last)
      if (why) {
        // The engineer (or the provider) stopped the drone: not a turn ending on its own.
        note(`${id} halted (${why}): no stop`)
        return
      }
      const active = s.continued
      s.continued = false
      const out = await send(id, "stop", async () => ({ last_assistant_message: text, stop_hook_active: active }))
      if (active || out?.decision !== "block" || typeof out.reason !== "string") return
      // The engineer spoke while borg thought: the questions are about a turn already superseded.
      if (s.gen !== gen) return note(`${id}: engineer spoke during stop, questions dropped`)
      if (s.rounds >= MAX_ROUNDS) return note(`${id}: ${s.rounds} question round(s) this prompt, no more`)
      s.continued = true
      s.rounds++
      s.echoes++
      sent = true
      const body = { parts: [{ type: "text" as const, text: out.reason, metadata: MARK }] }
      const session: any = client.session
      const res = await (session.promptAsync ?? session.prompt).call(session, { path: { id }, body })
      if (res?.error) throw new Error(JSON.stringify(res.error))
      note(`sent borg's questions into ${id}`)
    } catch (e) {
      note(`sending borg's questions into ${id} failed: ${e}`)
      if (sent) {
        s.continued = false
        s.echoes = Math.max(0, s.echoes - 1)
      }
    } finally {
      s.stopping = false
    }
  }

  const end = (id: string) => {
    const s = get(id)
    if (s.child || !s.used || s.ended) return
    s.ended = true
    return send(id, "end", async () => ({}))
  }

  return {
    "chat.message": async (input, output) => {
      const id = input.sessionID
      const s = get(id)
      if (s.child) return
      const parts: any[] = output.parts ?? []
      // Our own stop answer coming back: not the engineer speaking.
      if (parts.some((p) => p?.metadata?.borg === true)) {
        s.echoes = Math.max(0, s.echoes - 1)
        return
      }
      const prompt = visibleText(parts)
      if (!prompt.trim()) return
      if (s.echoes > 0) {
        // We sent questions and their echo has not come back, yet this one is unmarked. Take it for
        // the echo anyway: an echo mistaken for the engineer would reset the cycle and could loop.
        s.echoes--
        return note(`${id}: unmarked message while our questions were pending, taken as their echo`)
      }
      // The engineer spoke: a fresh idle cycle.
      s.gen++
      s.continued = false
      s.rounds = 0
      // Take the proposal now: by the time the queue reaches this prompt, the drone may be
      // streaming its reply to it.
      const prior = lastAssistant(id)
      send(id, "prompt", async () => ({ prompt, last_assistant_message: (await prior).text }))
    },

    "tool.execute.before": async (input, output) => {
      const s = get(input.sessionID)
      s.args.set(input.callID, { tool: input.tool, args: output.args })
      // A drone paused by signed steering (borg steer --pause): the call is refused with borg's
      // reason. Not queued behind the session's other calls, and fail open after PRE_MS. A
      // subagent's call is asked as its root session: the pause holds for the drone's subagents.
      const out = await borg("pre_tool", {
        session_id: root(input.sessionID), cwd: directory, pid: process.pid,
        tool_name: TOOL_NAMES[input.tool] ?? input.tool, tool_input: toolInput(input.tool, output.args),
      }, PRE_MS)
      const deny = out?.hookSpecificOutput
      if (deny?.permissionDecision === "deny" && typeof deny.permissionDecisionReason === "string") {
        throw new Error(deny.permissionDecisionReason)
      }
    },

    "tool.execute.after": async (input) => {
      const s = get(input.sessionID)
      s.args.delete(input.callID)
      if (s.child) return
      send(input.sessionID, "post_tool", async () => ({
        tool_name: TOOL_NAMES[input.tool] ?? input.tool,
        tool_input: toolInput(input.tool, input.args),
      }))
    },

    event: async ({ event }) => {
      const e: any = event
      const p = e?.properties ?? {}
      switch (e?.type) {
        case "session.created":
        case "session.updated":
          if (p.info?.id && p.info.parentID) {
            const c = get(p.info.id)
            c.child = true
            c.parent = p.info.parentID
          }
          break
        case "message.part.updated": {
          // A failed tool call (opencode does not run tool.execute.after for it).
          const part = p.part
          if (part?.type === "tool" && part.state?.status === "error")
            failure(part.sessionID, part.callID, part.tool, part.state.input, String(part.state.error ?? ""))
          break
        }
        case "session.next.tool.failed": {
          // The same failure on opencode's newer session event stream.
          if (!p.sessionID || !p.callID) break
          const call = get(p.sessionID).args.get(p.callID)
          failure(p.sessionID, p.callID, call?.tool ?? "", call?.args, String(p.error?.message ?? ""))
          break
        }
        case "session.idle":
          if (p.sessionID) await stop(p.sessionID)
          break
        case "session.deleted":
          if (p.info?.id) await end(p.info.id)
          break
      }
    },

    // opencode is shutting down (e.g. `opencode run` finished): end every session borg has heard of.
    dispose: async () => {
      await Promise.all([...sessions.keys()].map(end))
    },
  }
}

export default { id: "borg", server } satisfies PluginModule
