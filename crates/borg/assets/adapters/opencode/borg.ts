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
// Interactive hive drones (phase g, docs/commands.md "Interactive drones"): opencode gives its shell no
// session id, so the plugin sets BORG_HIVE_BINDER in its own process (every command its model runs has
// it) and binds the session itself: seeing `borg hive join [<goal>]` in that session's own
// tool.execute.after, it runs `borg hive join [<goal>] --bind --session <id>` and adds borg's answer to
// what the model reads. From then on it keeps the drone's waiter, `borg hive wait --stream`, for the
// session's life (re-armed at a stop when none runs): the waiter waits at no token cost and hands
// nothing while the engineer's own turn runs; the plugin sends each wake in only while the session is
// idle (from its prompt or tool call to its stop, also a halted one, it is not), opencode 1 with
// session.promptAsync, opencode 2 with session.prompt({delivery: "queue"}); one that fails to go in is
// kept and sent again. A halted turn of a drone (no stop) is sent to borg as `halted`, which ends the
// engineer's turn there too, so the waiter hands work at once. A drone's hive handover at its stop (a
// task done, the next one; borg marks it `hive: true`) goes in by its own path: never dropped because
// the engineer spoke during the stop (it holds a claim) and never counted against MAX_ROUNDS; a drone's
// block of questions alone keeps the rules for questions. The session's end makes the drone leave,
// which stops its waiter.
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

/** Whether a shell command runs `borg hive join` for the session it runs in (not `--here`, which is for
 *  runner worktrees, nor a join of another, named session), and the goal it names, if any. */
export function hiveJoin(command: string): { goal?: string } | undefined {
  for (const part of command.split(/&&|\|\||[;|\n]/)) {
    const m = /(?:^|\s)(?:\S*\/)?borg\s+hive\s+join(?:\s+(.*))?$/.exec(part.trim())
    if (!m) continue
    const args = (m[1] ?? "").trim().split(/\s+/).filter(Boolean)
    if (args.some((a) => a === "--here" || a === "--bind" || a === "--session" || a.startsWith("--session="))) return undefined
    let goal: string | undefined
    for (let i = 0; i < args.length; i++) {
      if (args[i] === "--dir" || args[i] === "--drone") i++
      else if (!args[i].startsWith("-")) goal ??= args[i].replace(/^['"]|['"]$/g, "")
    }
    return { goal }
  }
  return undefined
}

/** Runs `borg <args>` (no stdin); resolves to its stdout and exit code, or null (fails open). */
function borgRun(args: string[], env: Record<string, string>, timeoutMs: number): Promise<{ out: string; code: number | null } | null> {
  return new Promise((resolve) => {
    let out = ""
    let child: ReturnType<typeof spawn>
    try {
      child = spawn(process.env.BORG_BIN || "borg", args, { stdio: ["ignore", "pipe", "ignore"], env: { ...process.env, ...env } })
    } catch (e) {
      note(`spawn borg ${args.join(" ")} failed: ${e}`)
      return resolve(null)
    }
    const timer = setTimeout(() => {
      note(`borg ${args.slice(0, 2).join(" ")}: timed out`)
      child.kill()
    }, timeoutMs)
    child.on("error", (e) => {
      clearTimeout(timer)
      note(`borg ${args.slice(0, 2).join(" ")}: ${e}`)
      resolve(null)
    })
    child.stdout!.on("data", (d) => (out += d))
    child.on("close", (code) => {
      clearTimeout(timer)
      resolve({ out, code })
    })
  })
}

// How long a bind (`borg hive join --bind`: the drone's worktree, its node's join) may take.
const BIND_MS = 60_000
// A wake that failed to go in is sent again this much later, at most WAKE_TRIES times in a row; then at
// the session's next stop or halt.
const WAKE_RETRY_MS = 5_000
const WAKE_TRIES = 5

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
  drone: boolean // an interactive hive drone the plugin bound (or saw join)
  waiter?: ReturnType<typeof spawn> // its `borg hive wait --stream`
  busy: boolean // a turn runs: from a prompt or tool call to its stop (also a halted one)
  wakes: string[] // the waiter's wakes not sent yet: sent once the session is idle
  retries: number // failed sends of the waiting wakes in a row (<= WAKE_TRIES)
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
      s = { chain: Promise.resolve(), child: false, continued: false, gen: 0, rounds: 0, echoes: 0, stopping: false, used: false, ended: false, args: new Map(), failed: new Set(), worded: false, late: [], drone: false, busy: false, wakes: [], retries: 0 }
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
    // Any prompt starts a turn: no wake goes in until its stop.
    s.busy = true
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
    get(root(id)).busy = true
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
    const context = withLate(s, await within(id, answer, s.stopping ? 0 : POST_MS, "tool"))
    const join = name === "Bash" && typeof args?.command === "string" ? hiveJoin(args.command) : undefined
    if (!join) return context
    const said = await bind(id, join.goal)
    return [context, said].filter((t): t is string => !!t).join("\n\n") || undefined
  }

  /** The session's model ran `borg hive join [<goal>]`: the plugin binds this session (its shell has no
   *  session id) behind its other borg calls, and starts its waiter. borg's answer, for the model. */
  const bind = async (id: string, goal?: string): Promise<string | undefined> => {
    const s = get(id)
    const args = ["hive", "join", ...(goal ? [goal] : []), "--bind", "--session", id, "--dir", io.directory]
    const run = s.chain.then(() => borgRun(args, { BORG_HARNESS: "opencode" }, BIND_MS))
    s.chain = run.catch(() => null)
    const res = await run.catch(() => null)
    note(`${id}: borg hive join --bind -> ${res?.code} ${(res?.out ?? "").trim().slice(0, 200)}`)
    if (res?.code !== 0) return res?.out.trim() || undefined
    s.drone = true
    arm(id)
    return res.out.trim() || undefined
  }

  /** The drone's waiter, unless one runs: each wake goes in once the session is idle (`wake`). */
  const arm = (id: string) => {
    const s = get(id)
    if (!s.drone || s.ended || (s.waiter && s.waiter.exitCode === null && s.waiter.signalCode === null)) return
    let child: ReturnType<typeof spawn>
    try {
      child = spawn(process.env.BORG_BIN || "borg", ["hive", "wait", "--stream", "--session", id, "--dir", io.directory], { stdio: ["ignore", "pipe", "ignore"] })
    } catch (e) {
      return note(`${id}: starting the drone's waiter failed: ${e}`)
    }
    s.waiter = child
    let left = false
    let rest = ""
    child.stdout!.on("data", (d) => {
      rest += d
      let nl: number
      while ((nl = rest.indexOf("\n")) >= 0) {
        const line = rest.slice(0, nl)
        rest = rest.slice(nl + 1)
        try {
          const v = JSON.parse(line)
          left ||= v?.left === true
          if (typeof v?.wake === "string" && v.wake.trim()) wake(id, v.wake)
        } catch {}
      }
    })
    const end = (code: number | null) => {
      if (s.waiter === child) s.waiter = undefined
      // exit 1: no drone (the bind did not take); a `left` line: a drone no more.
      if (left || code === 1) s.drone = false
      note(`${id}: the drone's waiter ended (${code})`)
    }
    child.on("error", () => end(null))
    child.on("close", end)
    note(`${id}: the drone's waiter runs`)
  }

  /** A wake from the drone's waiter (its next task, a stuck goal, its goal finished): sent into the
   *  session now when it is idle, else at its stop. */
  const wake = (id: string, text: string) => {
    const s = get(id)
    s.wakes.push(text)
    flush(id)
  }

  /** Sends the session's waiting wakes, as one message, while it is idle and no stop runs. A wake
   *  that fails to go in is kept and sent again (WAKE_RETRY_MS later, at most WAKE_TRIES times, then at
   *  the session's next stop or halt): its claim is held for the drone, so the wake is never dropped. */
  const flush = (id: string) => {
    const s = get(id)
    if (s.busy || s.stopping || s.ended || !s.wakes.length) return
    const sent = s.wakes.splice(0)
    s.busy = true
    io.ask(id, sent.join("\n\n")).then(
      () => {
        s.retries = 0
        note(`woke ${id} with hive work`)
      },
      (e) => {
        note(`waking ${id} failed: ${e}`)
        s.busy = false
        if (s.ended) return
        s.wakes.unshift(...sent)
        if (++s.retries <= WAKE_TRIES) setTimeout(() => flush(id), WAKE_RETRY_MS).unref?.()
      },
    )
  }

  /** The session's turn was halted with no stop (opencode 2's session.execution.failed or
   *  .interrupted; opencode 1's halted turn still reaches `stop`, which says so): it is idle, and a
   *  drone's waiting wakes go in, so an Esc never leaves a drone that nothing wakes. */
  const idle = (id: string) => {
    const s = get(id)
    if (s.child) return
    s.busy = false
    halt(id)
    flush(id)
  }

  /** A drone's turn was halted with no stop: borg ends the engineer's turn on its side too (`borg hook
   *  opencode halted`), so its waiter hands work again at once. Nothing for a plain session. */
  const halt = (id: string) => {
    const s = get(id)
    if (!s.drone || s.ended) return
    s.retries = 0
    send(id, "halted", async () => ({}))
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
    s.busy = false
    let sent = false
    try {
      const gen = s.gen
      const { text, halted } = await history(id)
      if (halted) {
        // The engineer (or the provider) stopped the drone: not a turn ending on its own.
        note(`${id} halted (${halted}): no stop`)
        halt(id)
        return
      }
      const active = s.continued
      s.continued = false
      const out = await send(id, "stop", async () => ({ last_assistant_message: text, stop_hook_active: active }))
      if (out?.decision !== "block" || typeof out.reason !== "string") return
      // A drone's hive handover (a task done, the next one): borg's own path, which holds the claim it
      // names, so it goes in whatever the engineer said meanwhile (queued behind their turn) and is
      // never counted as a round of questions; borg blocks again only for a handover (stop_hook_active).
      // borg marks it (`hive: true`): a drone's block of questions alone keeps the rules for questions.
      const hive = s.drone && out.hive === true
      if (!hive) {
        if (active) return
        // The engineer spoke while borg thought: the questions are about a turn already superseded.
        if (s.gen !== gen) return note(`${id}: engineer spoke during stop, questions dropped`)
        if (s.rounds >= MAX_ROUNDS) return note(`${id}: ${s.rounds} question round(s) this prompt, no more`)
        s.rounds++
      }
      s.continued = true
      s.echoes++
      s.busy = true
      sent = true
      await io.ask(id, out.reason)
      note(hive ? `sent borg's hive handover into ${id}` : `sent borg's questions into ${id}`)
    } catch (e) {
      note(`sending borg's questions into ${id} failed: ${e}`)
      if (sent) {
        s.continued = false
        s.echoes = Math.max(0, s.echoes - 1)
        s.busy = false
      }
    } finally {
      s.stopping = false
      // A drone whose waiter is not running (it ended, a crash) waits again from here; wakes that came
      // during the turn go in now that it is idle.
      arm(id)
      flush(id)
    }
  }

  const end = (id: string) => {
    const s = get(id)
    if (s.child || !s.used || s.ended) return
    s.ended = true
    s.wakes = []
    // The drone leaves at its end (its claim released, its waiter stopped by borg); a waiter still
    // running after that is stopped here.
    return send(id, "end", async () => ({})).then((out) => {
      const w = s.waiter
      s.drone = false
      try {
        if (w && w.exitCode === null && w.signalCode === null) w.kill("SIGTERM")
      } catch {}
      return out
    })
  }

  /** opencode is shutting down (e.g. `opencode run` finished): end every session borg has heard of. */
  const endAll = async () => {
    await Promise.all([...sessions.keys()].map(end))
  }

  return { get, child, prompt, preTool, postTool, failure, stop, idle, end, endAll }
}

// ---- opencode 1.x: a `server` function that returns hooks; bus events through `event`.

const server: Plugin = async ({ client, directory }) => {
  process.env.BORG_HIVE_BINDER = "opencode"
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
          // Also after a halted turn (no stop then): the session is idle, and a drone's wakes go in.
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
  process.env.BORG_HIVE_BINDER = "opencode"
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
            if (p.sessionID) {
              note(`${p.sessionID} halted (${e.type.slice("session.execution.".length)} ${p.reason ?? p.error?.type ?? ""}): no stop`)
              d.idle(p.sessionID)
            }
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
