// borg adapter for opencode, one module for both plugin APIs: opencode 1.x (`server`, written
// against 1.18.32, verified on 1.19.0) and opencode 2.x (`setup`, written against 2.0.23, verified on
// 2.0.29).
//
// Translates opencode plugin hooks and events into borg's one canonical, Claude-shaped hook event
// and pipes it to `borg hook opencode <event>`:
//
//   opencode 1.x                  opencode 2.x                      borg
//   chat.message (user text)      session hook "prompt"             prompt
//   tool.execute.before           tool hook "execute.before"        pre_tool (throws borg's reason: a
//                                                                   paused drone's tool call is refused)
//   tool.execute.after            tool hook "execute.after"         post_tool
//   tool part in state "error"    "execute.after" with an error;    post_tool_failure (incl. a rejected
//                                 session.tool.failed "aborted"     permission, with or without feedback)
//   session.idle                  session.execution.succeeded       stop
//   session.deleted, dispose      session.deleted, setup's cleanup  end
//
// At stop, a {"decision":"block","reason"} answer is sent back into the session as a user message,
// at most once per engineer prompt (the next stop carries stop_hook_active: true, like Claude's Stop
// hook). No stop when the turn was halted (Esc/abort, a provider error, a permission rejected
// without feedback): Claude's Stop hook skips interrupts too. opencode 1 tells a halted turn from
// the last assistant message; opencode 2 ends it with session.execution.failed/interrupted instead.
//
// At prompt, borg's additionalContext (e.g. a queen question) reaches the drone but not the
// engineer's TUI: opencode 1 adds it to the message as a synthetic text part, opencode 2 as a
// synthetic session input without a description, just before the prompt.
//
// After a tool call, borg's additionalContext (live mode: a check's questions, graded lessons) reaches
// the model before its next step; both after-hooks are awaited. opencode 1 appends it to the tool's
// output (stored with the call: the TUI hides it for built-in tools, exports show it); opencode 2 sends
// it as a steer synthetic input and leaves the tool result alone (the TUI shows that). opencode 1 runs
// no after-hook for a failed call, so text waiting then comes with the next successful call or prompt.
// A shell call's post_tool carries the end of its output and a non-zero exit as tool_response, Codex's
// way: borg tells a test run's pass from its failure by it.
// An answer later than POST_MS (a prompt's: PROMPT_MS) is kept for the next call or prompt: borg already
// counts it delivered.
//
// Subagent (child) sessions are ignored, like Claude's hooks ignore subagents, with one exception:
// a child's tool call still asks pre_tool, as its root session, so a paused drone cannot carry on
// through a subagent.
// Fails open: if borg is missing, slow or broken, opencode carries on untouched.
// borg is found via $BORG_BIN, else `borg` on PATH. $BORG_OPENCODE_LOG names a file that gets
// what the adapter did and every error it swallowed.
//
// The module has a single default export: opencode 1 calls every exported function as a plugin.

import { spawn } from "node:child_process"
import { appendFileSync } from "node:fs"
import type { Plugin, PluginModule } from "@opencode-ai/plugin"

// A stop with lessons makes one model call (the consequence model). opencode sets no limit of its own;
// this is the stop's BORG_STOP_BUDGET_MS, which leaves room for a live check's 15 s wait, the 45 s
// call and an expert drone.
const TIMEOUT_MS = 90_000
const PRE_MS = 2_000 // before every tool call: borg answers in a few ms (a stat when nobody steers)
// How long a tool call's result waits for borg's context (file appends, no model call).
const POST_MS = 2_000
// How long the engineer's message waits for borg's prompt context (file appends, no model call); a
// later answer is kept for the next call or prompt, like a tool call's.
const PROMPT_MS = 5_000
// Hard cap on questions sent back into a session per engineer prompt, so nothing can make the
// plugin prompt the drone in a loop.
const MAX_ROUNDS = 1
// A wait's timeout, told apart from an answer without context.
const LATE = Symbol("late")
// Marks the text part we send back, so its chat.message echo is recognised without comparing text.
const MARK = { borg: true }
// opencode's PermissionRejectedError: the engineer said no without feedback, which halts the turn.
const REJECTED = "The user rejected permission to use this specific tool call."
// opencode 1's rejection with the engineer's words after it.
const FEEDBACK = /rejected permission to use this specific tool call with the following feedback:\s*([\s\S]*)/i
// opencode 2's call the engineer declined without feedback (error type "aborted").
const DECLINED = "The user declined this tool call"
// opencode 2's `opencode run` rejects every permission ask by itself, as a Permission.CorrectedError
// with this "feedback": nobody's correction.
const AUTO_REJECTED = "This non-interactive run cannot ask the user for permission, so the request was rejected. Continue without this action."
// Claude's plain rejection, which borg takes for a wordless denial: the engineer's next prompt is the reason.
const DENIED = "The user doesn't want to proceed with this tool use."
// How much of a shell call's output borg gets (its end, where a test run's summary is).
const OUTPUT_TAIL = 4_000

// opencode tool ids -> the Claude names borg matches on (borg reads Bash command, Grep/Glob pattern,
// else file_path/path from tool_input). Unknown tools keep their id.
const TOOL_NAMES: Record<string, string> = {
  bash: "Bash", read: "Read", write: "Write", edit: "Edit", multiedit: "MultiEdit", apply_patch: "Edit",
  grep: "Grep", glob: "Glob", list: "LS", webfetch: "WebFetch", websearch: "WebSearch", task: "Task",
  todowrite: "TodoWrite",
  // opencode 2 ids (its read/write/edit/grep/glob keep their names)
  shell: "Bash", patch: "Edit", subagent: "Task",
}

// Tools whose `path` (opencode 2) is the file they touch, borg's file_path.
const FILE_TOOLS = new Set(["read", "write", "edit"])

function toolInput(tool: string, args: any): Record<string, unknown> {
  const input: Record<string, unknown> = { ...(args ?? {}) }
  if (typeof input.filePath === "string") input.file_path = input.filePath
  else if (FILE_TOOLS.has(tool) && typeof input.path === "string") input.file_path = input.path
  // An edit's new text, as Claude's Edit names it (borg keeps it with the change, .edits).
  if (typeof input.newString === "string") input.new_string = input.newString
  // apply_patch (opencode 1, GPT models) and patch (opencode 2) name their files only inside the patch text.
  if ((tool === "apply_patch" || tool === "patch") && typeof input.patchText === "string") {
    const files = [...input.patchText.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)].map((m) => m[1].trim())
    if (files.length) input.file_path = files.join(" ")
  }
  return input
}

/** borg's wording for a permission rejected with feedback (opencode 1's "...with the following
 *  feedback: X", opencode 2's Permission.CorrectedError). */
const corrected = (feedback: string) =>
  `The user doesn't want to proceed with this tool use. The tool use was rejected. To tell you how to proceed, the user said:\n${feedback}`

// borg's rejection capture speaks Claude's wording, so opencode's is re-phrased: a rejection with
// feedback carries the engineer's words; one without (opencode 1's REJECTED, opencode 2's DECLINED)
// becomes Claude's plain rejection, a wordless denial. Every other error goes as it is.
function canonicalError(error: string): string {
  const m = FEEDBACK.exec(error)
  if (m) return corrected(m[1])
  const bare = error.trim()
  return bare === REJECTED || bare === DECLINED ? DENIED : error
}

/** A shell call's tool_response for borg, which tells a test run's pass from its failure by it
 *  (hook.rs `capture_test`): the end of its output, after Codex's "Exit code: N" line when it exited
 *  non-zero (opencode's shell keeps such a call out of the error state). Undefined without output:
 *  borg records no result it cannot see. */
function shellResponse(output: string | undefined, exit: unknown): string | undefined {
  const out = (output ?? "").slice(-OUTPUT_TAIL)
  if (!out.trim()) return undefined
  return typeof exit === "number" && exit !== 0 ? `Exit code: ${exit}\nOutput:\n${out}` : out
}

/** The text of an opencode 2 tool result: its `output`, or its content's text blocks. */
function resultText(result: any): string {
  if (typeof result === "string") return result
  if (typeof result?.output === "string") return result.output
  const content = Array.isArray(result?.content) ? result.content : []
  return content.map((b: any) => (b?.type === "text" ? String(b.text ?? "") : "")).filter((t: string) => t).join("\n")
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

/** borg's additionalContext in an answer, if it has any text. */
function contextOf(out: any): string | undefined {
  const t = out?.hookSpecificOutput?.additionalContext
  return typeof t === "string" && t.trim() ? t : undefined
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
      // BORG_STOP_BUDGET_MS: at a stop borg keeps its wait for a live check, its model call and its expert
      // drone inside this timeout, so its answer arrives.
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
  echoes: number // messages we sent whose prompt echo has not come back yet
  stopping: boolean
  used: boolean // borg has heard of this session
  ended: boolean
  args: Map<string, { tool: string; args: any }> // callID -> call, for failures reported without input
  failed: Set<string> // callIDs already reported as failures
  worded: boolean // opencode 1: a call was rejected with the engineer's words since their last prompt
  late: string[] // context that came after POST_MS or PROMPT_MS: delivered with the next call or prompt
}

/** What the session state machine needs from one opencode version. */
type Io = {
  directory: string
  /** The drone's last assistant message: its last visible text, and why its turn was halted rather
   *  than finished ("" when it finished). */
  lastAssistant: (id: string) => Promise<{ text: string; halted: string }>
  /** Sends borg's questions into the session as a user message that carries MARK. */
  ask: (id: string, text: string) => Promise<void>
}

/** The per-session state machine both opencode versions drive: they differ only in how their hooks
 *  and events arrive and in `io`. */
function drones(io: Io) {
  const sessions = new Map<string, Session>()
  const get = (id: string): Session => {
    let s = sessions.get(id)
    if (!s) {
      s = { chain: Promise.resolve(), child: false, continued: false, gen: 0, rounds: 0, echoes: 0, stopping: false, used: false, ended: false, args: new Map(), failed: new Set(), worded: false, late: [] }
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

  const history = async (id: string): Promise<{ text: string; halted: string }> => {
    try {
      return await io.lastAssistant(id)
    } catch (e) {
      note(`reading ${id}'s messages failed: ${e}`)
      return { text: "", halted: "" }
    }
  }
  const lastText = async (id: string) => (await history(id)).text

  // Queue a borg call behind the session's earlier ones. `build` runs in turn, so it sees their effects.
  const send = (id: string, event: string, build: () => Promise<Record<string, unknown>>): Promise<any> => {
    const s = get(id)
    s.used = true
    // pid: opencode's own (the plugin runs in its process), so `borg top` sees a vanished opencode.
    const run = s.chain.then(async () => borg(event, { session_id: id, cwd: io.directory, pid: process.pid, ...(await build()) }))
    s.chain = run.catch(() => null)
    return run.catch(() => null)
  }

  /** A subagent session, from its creation event. */
  const child = (id: string, parent: string) => {
    const c = get(id)
    c.child = true
    c.parent = parent
  }

  /** A user message: the engineer's prompt, or the echo of our questions (`marked`). Resolves to
   *  borg's context for the prompt (e.g. a queen question), if it came within PROMPT_MS. */
  const prompt = async (id: string, text: string, marked: boolean): Promise<string | undefined> => {
    const s = get(id)
    if (s.child) return
    // Our own stop answer coming back: not the engineer speaking.
    if (marked) {
      s.echoes = Math.max(0, s.echoes - 1)
      return
    }
    if (!text.trim()) return
    if (s.echoes > 0) {
      // We sent questions and their echo has not come back, yet this one is unmarked. Take it for
      // the echo anyway: an echo mistaken for the engineer would reset the cycle and could loop.
      s.echoes--
      note(`${id}: unmarked message while our questions were pending, taken as their echo`)
      return
    }
    // The engineer spoke: a fresh idle cycle.
    s.gen++
    s.continued = false
    s.rounds = 0
    s.worded = false
    // Take the proposal now: by the time the queue reaches this prompt, the drone may be
    // streaming its reply to it.
    const prior = lastText(id)
    // Behind a stop still thinking (a model call), the message does not wait for borg's context, so
    // borg is told to hand none over (it would count as delivered, e.g. a queen question, unseen).
    const busy = s.stopping
    const answer = send(id, "prompt", async () => ({ prompt: text, last_assistant_message: await prior, ...(busy ? { context: false } : {}) }))
    if (busy) return
    return withLate(s, await within(id, answer.then(contextOf), PROMPT_MS, "prompt"))
  }

  /** borg's context if it comes within `ms`; a later answer waits in `late` for the next call or
   *  prompt (borg already counts it delivered). */
  const within = async (id: string, answer: Promise<string | undefined>, ms: number, what: string) => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const late = new Promise<typeof LATE>((r) => (timer = setTimeout(() => r(LATE), ms)))
    const t = await Promise.race([answer, late])
    clearTimeout(timer)
    if (t !== LATE) return t
    void answer.then((text) => {
      if (!text) return
      get(id).late.push(text)
      note(`${id}: borg's ${what} context came late, kept for the next call or prompt`)
    })
  }

  /** Context that came late before, then `t`. */
  const withLate = (s: Session, t: string | undefined) => {
    const parts = [...s.late.splice(0), t].filter((x): x is string => !!x)
    return parts.length ? parts.join("\n\n") : undefined
  }

  /** Before a tool call: throws borg's reason when the drone is paused by signed steering
   *  (borg steer --pause). Not queued behind the session's other calls, and fail open after PRE_MS.
   *  A subagent's call is asked as its root session: the pause holds for the drone's subagents. */
  const preTool = async (id: string, callID: string, tool: string, args: any) => {
    get(id).args.set(callID, { tool, args })
    const out = await borg("pre_tool", {
      session_id: root(id), cwd: io.directory, pid: process.pid,
      tool_name: TOOL_NAMES[tool] ?? tool, tool_input: toolInput(tool, args),
    }, PRE_MS)
    const deny = out?.hookSpecificOutput
    if (deny?.permissionDecision === "deny" && typeof deny.permissionDecisionReason === "string") {
      throw new Error(deny.permissionDecisionReason)
    }
  }

  /** A finished tool call. Resolves to the text for the model's next step: context that came late
   *  before, then borg's answer, if it came within POST_MS; a later answer waits in `late`. A shell
   *  call's `output` and `exit` go along as its tool_response (`shellResponse`). */
  const postTool = async (id: string, callID: string, tool: string, args: any, output?: string, exit?: unknown): Promise<string | undefined> => {
    const s = get(id)
    s.args.delete(callID)
    if (s.child) return
    const name = TOOL_NAMES[tool] ?? tool
    const response = name === "Bash" ? shellResponse(output, exit) : undefined
    const answer = send(id, "post_tool", async () => ({ tool_name: name, tool_input: toolInput(tool, args), ...(response ? { tool_response: response } : {}) })).then(contextOf)
    // Behind a stop still thinking (a model call) the answer can only come late: the call does not wait.
    return withLate(s, await within(id, answer, s.stopping ? 0 : POST_MS, "tool"))
  }

  /** A failed tool call; `args` undefined takes the input seen before the call. */
  const failure = (id: string, callID: string, tool: string | undefined, args: any, error: string) => {
    const s = get(id)
    const call = s.args.get(callID)
    s.args.delete(callID)
    if (s.child || s.failed.has(callID)) return
    s.failed.add(callID)
    // When the engineer rejects a call with words, opencode 1 rejects every other pending call with
    // the plain REJECTED: that cascade is nobody's denial (borg assimilate drops it too).
    if (FEEDBACK.test(error)) s.worded = true
    else if (s.worded && error.trim() === REJECTED) return
    const name = tool ?? call?.tool ?? ""
    send(id, "post_tool_failure", async () => ({
      tool_name: TOOL_NAMES[name] ?? name,
      tool_input: toolInput(name, args ?? call?.args),
      error: canonicalError(error),
      last_assistant_message: await lastText(id),
    }))
  }

  /** The drone's turn ended: borg's conscience may send questions back, at most MAX_ROUNDS per prompt. */
  const stop = async (id: string) => {
    const s = get(id)
    if (s.child || s.stopping) return
    s.stopping = true
    let sent = false
    try {
      const gen = s.gen
      const { text, halted } = await history(id)
      if (halted) {
        // The engineer (or the provider) stopped the drone: not a turn ending on its own.
        return note(`${id} halted (${halted}): no stop`)
      }
      const active = s.continued
      s.continued = false
      const out = await send(id, "stop", async () => ({ last_assistant_message: text, stop_hook_active: active }))
      if (out?.decision !== "block" || typeof out.reason !== "string") return
      if (active) return
      // The engineer spoke while borg thought: the questions are about a turn already superseded.
      if (s.gen !== gen) return note(`${id}: engineer spoke during stop, questions dropped`)
      if (s.rounds >= MAX_ROUNDS) return note(`${id}: ${s.rounds} question round(s) this prompt, no more`)
      s.rounds++
      s.continued = true
      s.echoes++
      sent = true
      await io.ask(id, out.reason)
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

  /** opencode is shutting down (e.g. `opencode run` finished): end every session borg has heard of. */
  const endAll = async () => {
    await Promise.all([...sessions.keys()].map(end))
  }

  return { get, child, prompt, preTool, postTool, failure, stop, end, endAll }
}

// ---- opencode 1.x: a `server` function that returns hooks; bus events through `event`.

const server: Plugin = async ({ client, directory }) => {
  const d = drones({
    directory,
    // The last assistant message ({info, parts}) tells a halted turn; its last visible text is borg's
    // "proposal" for captured corrections and the plan the conscience reads at stop.
    lastAssistant: async (id) => {
      const res: any = await client.session.messages({ path: { id } })
      const msgs: any[] = res?.data ?? []
      let last: any
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i]?.info?.role !== "assistant") continue
        last ??= msgs[i]
        const text = visibleText(msgs[i].parts).trim()
        if (text) return { text, halted: halted(last) }
      }
      return { text: "", halted: halted(last) }
    },
    ask: async (id, text) => {
      const body = { parts: [{ type: "text" as const, text, metadata: MARK }] }
      const session: any = client.session
      const res = await (session.promptAsync ?? session.prompt).call(session, { path: { id }, body })
      if (res?.error) throw new Error(JSON.stringify(res.error))
    },
  })

  return {
    "chat.message": async (input, output) => {
      const parts: any[] = output.parts ?? []
      const id = input.sessionID
      const context = await d.prompt(id, visibleText(parts), parts.some((p) => p?.metadata?.borg === true))
      if (!context) return
      // opencode gave the parts their ids already; ours sorts right after the last one.
      const last = parts[parts.length - 1]
      parts.push({
        ...(last?.id ? { id: `${last.id}0` } : {}),
        sessionID: last?.sessionID ?? id,
        messageID: last?.messageID ?? (input as any).messageID,
        type: "text",
        text: context,
        synthetic: true,
      })
      note(`${id}: borg's prompt context added (${context.length} chars)`)
    },

    "tool.execute.before": async (input, output) => {
      await d.preTool(input.sessionID, input.callID, input.tool, output.args)
    },

    // Awaited before the next step; the output is what the model reads (and what opencode stores).
    "tool.execute.after": async (input, output) => {
      const t = await d.postTool(input.sessionID, input.callID, input.tool, input.args, output.output, output.metadata?.exit)
      if (t) output.output = `${output.output}\n\n${t}`
    },

    event: async ({ event }) => {
      const e: any = event
      const p = e?.properties ?? {}
      switch (e?.type) {
        case "session.created":
        case "session.updated":
          if (p.info?.id && p.info.parentID) d.child(p.info.id, p.info.parentID)
          break
        case "message.part.updated": {
          // A failed tool call (opencode does not run tool.execute.after for it).
          const part = p.part
          if (part?.type === "tool" && part.state?.status === "error")
            d.failure(part.sessionID, part.callID, part.tool, part.state.input, String(part.state.error ?? ""))
          break
        }
        case "session.next.tool.failed":
          // The same failure on opencode's newer session event stream.
          if (p.sessionID && p.callID) d.failure(p.sessionID, p.callID, undefined, undefined, String(p.error?.message ?? ""))
          break
        case "session.idle":
          // Also after a halted turn: stop tells it from the last assistant message.
          if (p.sessionID) await d.stop(p.sessionID)
          break
        case "session.deleted":
          if (p.info?.id) await d.end(p.info.id)
          break
      }
    },

    dispose: d.endAll,
  }
}

// ---- opencode 2.x: `setup(ctx)` registers hooks and reads the event stream.

async function setup(ctx: any) {
  const d = drones({
    directory: ctx.location?.directory ?? process.cwd(),
    // A stop only follows session.execution.succeeded, so the turn finished: nothing to tell apart.
    lastAssistant: async (id) => {
      const res: any = await ctx.session.context({ sessionID: id })
      const msgs: any[] = Array.isArray(res) ? res : (res?.data ?? [])
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i]?.type !== "assistant") continue
        const text = visibleText(msgs[i].content).trim()
        if (text) return { text, halted: "" }
      }
      return { text: "", halted: "" }
    },
    // "queue": should the engineer have typed meanwhile, the questions wait for that turn to end.
    ask: async (id, text) => {
      await ctx.session.prompt({ sessionID: id, text, metadata: MARK, delivery: "queue" })
    },
  })

  // Every prompt into a session: the engineer's, a subagent's task, or the echo of our questions.
  // The hook is awaited before the prompt is stored. Text added to the prompt would show in the
  // engineer's message, so borg's context goes in as a synthetic input without a description: the
  // drone reads it, the TUI hides it, and it lands just before the prompt it belongs to.
  await ctx.session.hook("prompt", async (e: any) => {
    const id = e.sessionID
    const context = await d.prompt(id, String(e.prompt?.text ?? ""), e.metadata?.borg === true)
    if (!context) return
    try {
      await ctx.session.synthetic({ sessionID: id, text: context, delivery: e.delivery, resume: false })
      note(`${id}: borg's prompt context added (${context.length} chars)`)
    } catch (err) {
      // borg already counts it shown (a message's seen claim): the next call or prompt carries it.
      d.get(id).late.unshift(context)
      note(`adding borg's prompt context to ${id} failed: ${err}`)
    }
  })

  await ctx.tool.hook("execute.before", async (e: any) => {
    await d.preTool(e.sessionID, e.id, e.tool, e.input)
  })

  await ctx.tool.hook("execute.after", async (e: any) => {
    if (e.status !== "error") {
      // Awaited before the next step. A steer, not the tool result: the TUI shows the result.
      const t = await d.postTool(e.sessionID, e.id, e.tool, e.input, resultText(e.result), e.result?.metadata?.exit ?? e.metadata?.exit)
      if (!t) return
      try {
        await ctx.session.synthetic({ sessionID: e.sessionID, text: t, delivery: "steer", resume: false, metadata: MARK })
        note(`${e.sessionID}: borg's tool context steered in (${t.length} chars)`)
      } catch (err) {
        // borg already counts it delivered: the next call or prompt carries it.
        d.get(e.sessionID).late.unshift(t)
        note(`steering borg's tool context into ${e.sessionID} failed: ${err}`)
      }
      return
    }
    const cause = e.error?.error
    // `opencode run`'s own auto-reject arrives as the same error: no feedback, an ordinary failure.
    const feedback =
      cause?._tag === "Permission.CorrectedError" && typeof cause.feedback === "string" && cause.feedback.trim() !== AUTO_REJECTED ? cause.feedback : undefined
    d.failure(e.sessionID, e.id, e.tool, e.input, feedback !== undefined ? corrected(feedback) : String(e.error?.message ?? ""))
  })

  const events = new AbortController()
  void (async () => {
    try {
      for await (const event of ctx.event.subscribe({ signal: events.signal })) {
        const e: any = event
        const p = e?.data ?? {}
        switch (e?.type) {
          case "session.created":
            if (p.sessionID && p.parentID) d.child(p.sessionID, p.parentID)
            break
          case "session.tool.failed":
            // A call the engineer declined without feedback: no execute.after, the turn is interrupted.
            // Its DECLINED reaches borg as Claude's plain rejection (canonicalError).
            if (p.sessionID && p.id && p.error?.type === "aborted") d.failure(p.sessionID, p.id, undefined, undefined, String(p.error.message ?? ""))
            break
          // Each busy period ends with exactly one of these; only a finished turn gets a stop
          // (Esc, a provider error or a declined call halt it: Claude's Stop hook skips interrupts too).
          case "session.execution.succeeded":
            if (p.sessionID) await d.stop(p.sessionID)
            break
          case "session.execution.failed":
          case "session.execution.interrupted":
            if (p.sessionID) note(`${p.sessionID} halted (${e.type.slice("session.execution.".length)} ${p.reason ?? p.error?.type ?? ""}): no stop`)
            break
          case "session.deleted":
            if (p.sessionID) await d.end(p.sessionID)
            break
        }
      }
    } catch (e) {
      if (!events.signal.aborted) note(`event stream failed: ${e}`)
    }
  })()

  // The plugin is unloaded (reload, location closed, shutdown): end every session borg has heard of.
  return async () => {
    events.abort()
    await d.endAll()
  }
}

// One module for both: opencode 2 reads `id` and `setup`, opencode 1 (>= 1.18.29) `id` and `server`.
export default { id: "borg", setup, server } satisfies PluginModule & { setup: typeof setup }
