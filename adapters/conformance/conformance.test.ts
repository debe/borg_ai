// The harness conformance suite for the adapter harnesses (docs/harnesses.md): omp, opencode 1.x and
// opencode 2.x. `borg install <harness> --apply` writes the adapter into a temp HOME, and the *installed*
// file is loaded into a fake host (the omp `pi.on` stand-in, opencode's fakes.ts), which finds borg on
// PATH as a real host would. One scenario, the same for every host and the same as the command-hook
// harnesses' (crates/borg/tests/conformance_cli.rs): install, the hooks, live mode, and what
// `borg top --json` (the web page's drones) shows. The model is a fake `claude` on PATH: 0 tokens.
//
//   cargo build -p borg-ai && node --test adapters/conformance/conformance.test.ts
//
// A missing binary fails every test (BORG_TEST_BIN names another; scripts/check.sh passes cargo's).
// An assertion that depends on a capability reads the flag from the harness's module
// (crates/borg/src/harness/<name>.rs) and asserts the side it is on; all three hosts have the same
// flags today (context at a prompt and after a tool call, denials, their pid; no early delivery,
// steering or dialog, which this suite does not exercise).

import { test } from "node:test"
import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { pathToFileURL } from "node:url"

import { fakeClient, fakeCtx } from "../opencode/fakes.ts"

const BORG = process.env.BORG_TEST_BIN ?? resolve(import.meta.dirname, "../../target/debug/borg")
const REPO = resolve(import.meta.dirname, "../..")

const PROMPT = "Tidy the budget handling of the Orion net"
const PLAN = "I replaced the drain with a reset arc on the Orion budget place."
const NEW_TEXT = "use a reset arc on the budget place"
const CORRECTION = "no, keep the budget token: do not use a reset arc, drain it with an explicit consumer"

/** The harness's capability flags, from its module: the one table of them (`Caps`). */
function caps(harness: string): Record<string, boolean> {
  const src = readFileSync(join(REPO, `crates/borg/src/harness/${harness}.rs`), "utf8")
  return Object.fromEntries([...src.matchAll(/^\s*(\w+): (true|false),/gm)].map((m) => [m[1], m[2] === "true"]))
}

/** One of the adapter's budgets before borg's answer names them (`const FALLBACK = { stop_ms: 28_000, … }`,
 *  harness/<name>.rs `BUDGETS`, which a Rust test keeps equal): what the host itself is given. */
function adapterBudget(harness: string, name: string): number {
  const line = /^const FALLBACK\b.*$/m.exec(readFileSync(join(REPO, `adapters/${harness}/borg.ts`), "utf8"))?.[0] ?? ""
  const m = new RegExp(`\\b${name}: ([\\d_]+)`).exec(line)
  assert.ok(m, `adapters/${harness}/borg.ts: FALLBACK.${name}`)
  return Number(m[1].replaceAll("_", ""))
}

/** A Orion repo routed to the collective `corp`, its Orion-scoped lesson, and a fake `claude` that saves
 *  each prompt and grades `Mdrain` 3 (as crates/borg/tests/common). `borg` is on PATH as itself. */
function world(tag: string) {
  assert.ok(existsSync(BORG), `no borg binary at ${BORG} (cargo build -p borg-ai)`)
  const root = mkdtempSync(join(tmpdir(), `borg-conf-${tag}-`))
  for (const d of ["harbor_orion/src", "home", "bin", "prompts", "store/collectives/corp"]) mkdirSync(join(root, d), { recursive: true })
  const lesson = { id: "Mdrain", origin_ms: 1, scope: ["Orion"], question: "Does the Orion net drain the budget place with an explicit consumer instead of a reset arc?", consequence: "a reset arc drops the budget token" }
  writeFileSync(join(root, "store/collectives/corp/lessons.jsonl"), JSON.stringify(lesson) + "\n")
  writeFileSync(join(root, "collectives.json"), JSON.stringify({ default: "off", collectives: [{ name: "corp", paths: [join(root, "harbor_orion")], share: "local" }] }))
  const prompts = join(root, "prompts")
  writeFileSync(
    join(root, "bin/claude"),
    `#!/bin/sh\nn=$(( $(ls ${prompts} | wc -l) ))\nprintf '%s' "$2" > ${prompts}/$n.txt\necho '{"structured_output":{"moves":[],"applies":[{"id":"Mdrain","why":"w","grade":3}]},"total_cost_usd":0.05}'\n`,
  )
  chmodSync(join(root, "bin/claude"), 0o755)
  symlinkSync(BORG, join(root, "bin/borg"))
  // Nothing of the engineer's own borg setup: only this world.
  for (const k of Object.keys(process.env)) if (k.startsWith("BORG_")) delete process.env[k]
  Object.assign(process.env, {
    PATH: `${join(root, "bin")}:${process.env.PATH}`,
    HOME: join(root, "home"),
    GIT_CONFIG_NOSYSTEM: "1",
    BORG_HOME: join(root, "store"),
    BORG_CONFIG: join(root, "collectives.json"),
    BORG_QUEEN: "off",
    BORG_LEARN: "off",
    BORG_VECTORS: "off",
    BORG_PREFILTER: "off",
    BORG_EARLY: "off",
    BORG_NOTIFY: "off",
    BORG_LIVE_DEBOUNCE_S: "0",
    // Pinned to the fake `claude`: the harness's own CLI would otherwise answer.
    BORG_PREDICT_MODEL: "claude:sonnet",
    BORG_MODE: "hook",
  })
  const cli = (...args: string[]) => execFileSync("borg", args, { encoding: "utf8", env: process.env })
  const state = (session: string, ext: string) => {
    try {
      return readFileSync(join(root, `store/collectives/corp/state/${session}.${ext}`), "utf8")
    } catch {
      return ""
    }
  }
  const asked = () => readdirSync(prompts).length
  return { root, home: join(root, "home"), cwd: join(root, "harbor_orion"), cli, state, asked }
}

/** A tool call, in borg's terms; each host spells it natively. */
type Call = { kind: "read" | "edit" | "bash"; path?: string; command?: string }

/** What a scenario does to a host, and what the host's model then sees. */
type Host = {
  /** The engineer's prompt: borg's context delivered with it ("" for none). */
  prompt(text: string): Promise<string>
  /** A tool call: borg's context the model reads before its next step ("" for none). */
  tool(call: Call, id: string): Promise<string>
  /** Before a tool call: borg's reason when it refuses it, else "". */
  refused(call: Call, id: string): Promise<string>
  /** The engineer refuses the call at the host's approval prompt, without a word. */
  deny(call: Call, id: string): Promise<void>
  /** The turn ends with `last`: the continuation the host injects ("" for none). */
  stop(last: string): Promise<string>
  /** The model runs `borg listen` in the session's shell, as the host gives it one; the adapter binds
   *  the session where the host's shell cannot, and starts its waiter. */
  listen(): Promise<void>
  /** The waiter's wakes the adapter sent into the session, in order. */
  wakes(): string[]
  end(): Promise<void>
  close(): Promise<void>
}

type Profile = {
  name: string
  harness: "omp" | "opencode"
  /** The stop's time the adapter gives borg, in ms (its fallback budget). */
  stopMs: number
  installed: (home: string) => string; host: (mod: any, cwd: string, session: string) => Promise<Host> }

const ompCall = (c: Call): [string, object] =>
  c.kind === "read" ? ["read", { path: c.path }] : c.kind === "bash" ? ["bash", { command: c.command }]
  // omp's edit: a patch in its own format, the file in a `[path#hash]` header.
  : ["edit", { input: `*** Begin Patch\n[${c.path}#06FE]\nPUT 1:\n+${NEW_TEXT}\n` }]

const oc1Call = (c: Call): [string, object] =>
  c.kind === "read" ? ["read", { filePath: c.path }] : c.kind === "bash" ? ["bash", { command: c.command }]
  : ["edit", { filePath: c.path, oldString: "drain the budget", newString: NEW_TEXT }]

const oc2Call = (c: Call): [string, object] =>
  c.kind === "read" ? ["read", { path: c.path }] : c.kind === "bash" ? ["shell", { command: c.command }]
  : ["edit", { path: c.path, oldString: "drain the budget", newString: NEW_TEXT }]

const settle = () => new Promise((r) => setTimeout(r, 50))

const PROFILES: Profile[] = [
  {
    name: "omp",
    harness: "omp",
    stopMs: adapterBudget("omp", "stop_ms"),
    installed: (home) => join(home, ".omp/agent/extensions/borg.ts"),
    host: async (mod, cwd, session) => {
      const h: Record<string, (ev: any, ctx: any) => Promise<any>> = {}
      const sent: any[] = []
      mod.default({ on: (name: string, fn: any) => (h[name] = fn), sendMessage: (m: any) => sent.push(m) })
      const ctx = { cwd, sessionManager: { getSessionId: () => session } }
      // omp sets stop_hook_active on the stop after a continuation it injected.
      let active = false
      return {
        prompt: async (text) => {
          await h.input({ type: "input", text, source: "interactive" }, ctx)
          return (await h.before_agent_start({ type: "before_agent_start", prompt: text, systemPrompt: [] }, ctx))?.message?.content ?? ""
        },
        tool: async (c, id) => {
          const [toolName, input] = ompCall(c)
          await h.tool_call({ type: "tool_call", toolName, toolCallId: id, input }, ctx)
          const out = await h.tool_result({ type: "tool_result", toolName, toolCallId: id, input, content: [{ type: "text", text: "ok" }], isError: false }, ctx)
          return out?.additionalContext ?? ""
        },
        refused: async (c, id) => {
          const [toolName, input] = ompCall(c)
          return (await h.tool_call({ type: "tool_call", toolName, toolCallId: id, input }, ctx))?.reason ?? ""
        },
        deny: async (c, id) => {
          const [toolName, input] = ompCall(c)
          await h.tool_call({ type: "tool_call", toolName, toolCallId: id, input }, ctx)
          await h.tool_approval_resolved({ type: "tool_approval_resolved", toolName, toolCallId: id, approved: false, reason: "denied by user" }, ctx)
        },
        stop: async (last) => {
          const out = await h.session_stop({ type: "session_stop", last_assistant_message: last, stop_hook_active: active }, ctx)
          active = out?.decision === "block"
          return active ? out.reason : ""
        },
        listen: async () => {
          // omp gives its shell PI_SESSION_ID: the model's own `borg listen` binds the session.
          const said = execFileSync("borg", ["listen"], { cwd, encoding: "utf8", env: { ...process.env, PI_SESSION_ID: session } })
          const input = { command: "borg listen" }
          await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "listen", input }, ctx)
          await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "listen", input, content: [{ type: "text", text: said }], isError: false }, ctx)
        },
        wakes: () => sent.filter((m) => m.customType === "borg-listen").map((m) => m.content),
        end: async () => void (await h.session_shutdown({ type: "session_shutdown" }, ctx)),
        close: async () => {},
      }
    },
  },
  {
    name: "opencode 1.x",
    harness: "opencode",
    stopMs: adapterBudget("opencode", "stop_ms"),
    installed: (home) => join(home, ".config/opencode/plugins/borg.ts"),
    host: async (mod, cwd, session) => {
      let last = ""
      const { client, prompts } = fakeClient(() => last)
      const hooks: any = await mod.default.server({ client, directory: cwd })
      const say = async (text: string, metadata?: object) => {
        const parts: any[] = [{ type: "text", text, ...(metadata ? { metadata } : {}) }]
        await hooks["chat.message"]({ sessionID: session }, { message: {}, parts })
        return parts.slice(1).map((p) => p.text).join("\n\n")
      }
      return {
        prompt: (text) => say(text),
        tool: async (c, id) => {
          const [tool, args] = oc1Call(c)
          await hooks["tool.execute.before"]({ tool, sessionID: session, callID: id }, { args })
          const output = { title: "", output: "ok", metadata: {} }
          await hooks["tool.execute.after"]({ tool, sessionID: session, callID: id, args }, output)
          return output.output.replace(/^ok(\n\n)?/, "")
        },
        refused: async (c, id) => {
          const [tool, args] = oc1Call(c)
          return hooks["tool.execute.before"]({ tool, sessionID: session, callID: id }, { args }).then(() => "", (e: Error) => e.message)
        },
        deny: async (c, id) => {
          const [tool, args] = oc1Call(c)
          await hooks["tool.execute.before"]({ tool, sessionID: session, callID: id }, { args })
          // opencode's PermissionRejectedError, on the tool part (no execute.after follows).
          const state = { status: "error", input: args, error: "The user rejected permission to use this specific tool call." }
          await hooks.event({ event: { type: "message.part.updated", properties: { part: { type: "tool", sessionID: session, callID: id, tool, state } } } })
        },
        stop: async (text) => {
          last = text
          const n = prompts.length
          await hooks.event({ event: { type: "session.idle", properties: { sessionID: session } } })
          const reason = prompts.length > n ? prompts[prompts.length - 1].body.parts[0].text : ""
          // The continuation comes back as a marked user message, as opencode echoes it.
          if (reason) await say(reason, { borg: true })
          return reason
        },
        listen: async () => {
          const args = { command: "borg listen" }
          await hooks["tool.execute.before"]({ tool: "bash", sessionID: session, callID: "listen" }, { args })
          // opencode's shell names no session, only opencode's pid; the plugin binds (BORG_LISTEN_BINDER).
          const said = execFileSync("borg", ["listen"], { cwd, encoding: "utf8", env: { ...process.env, OPENCODE_PID: String(process.pid) } })
          await hooks["tool.execute.after"]({ tool: "bash", sessionID: session, callID: "listen", args }, { title: "", output: said, metadata: { exit: 0 } })
        },
        wakes: () => prompts.map((p) => p.body.parts[0].text).filter((t: string) => t.startsWith("[borg listen]")),
        end: async () => void (await hooks.event({ event: { type: "session.deleted", properties: { info: { id: session } } } })),
        close: async () => void (await hooks.dispose?.()),
      }
    },
  },
  {
    name: "opencode 2.x",
    harness: "opencode",
    stopMs: adapterBudget("opencode", "stop_ms"),
    installed: (home) => join(home, ".config/opencode/plugins/borg.ts"),
    host: async (mod, cwd, session) => {
      let last = ""
      const f = fakeCtx(() => last, cwd)
      const cleanup = await mod.default.setup(f.ctx)
      const say = async (text: string, metadata?: object) => {
        const n = f.synthetics.length
        await f.hooks["session.prompt"]({ sessionID: session, messageID: "msg_1", prompt: { text }, metadata, delivery: "steer" })
        return f.synthetics.slice(n).map((s: any) => s.text).join("\n\n")
      }
      return {
        prompt: (text) => say(text),
        tool: async (c, id) => {
          const [tool, input] = oc2Call(c)
          const n = f.synthetics.length
          await f.hooks["tool.execute.before"]({ tool, sessionID: session, id, input })
          await f.hooks["tool.execute.after"]({ tool, sessionID: session, id, input, status: "completed", result: { content: [] } })
          return f.synthetics.slice(n).map((s: any) => s.text).join("\n\n")
        },
        refused: async (c, id) => {
          const [tool, input] = oc2Call(c)
          return f.hooks["tool.execute.before"]({ tool, sessionID: session, id, input }).then(() => "", (e: Error) => e.message)
        },
        deny: async (c, id) => {
          const [tool, input] = oc2Call(c)
          await f.hooks["tool.execute.before"]({ tool, sessionID: session, id, input })
          // Declined without feedback: no execute.after, the turn is interrupted.
          await f.emit("session.tool.failed", { sessionID: session, id, error: { type: "aborted", message: "The user declined this tool call" } })
        },
        stop: async (text) => {
          last = text
          const n = f.prompts.length
          await f.emit("session.execution.succeeded", { sessionID: session })
          const reason = f.prompts.length > n ? f.prompts[f.prompts.length - 1].text : ""
          if (reason) await say(reason, { borg: true })
          return reason
        },
        listen: async () => {
          const input = { command: "borg listen" }
          await f.hooks["tool.execute.before"]({ tool: "shell", sessionID: session, id: "listen", input })
          const said = execFileSync("borg", ["listen"], { cwd, encoding: "utf8", env: { ...process.env, OPENCODE_PID: String(process.pid) } })
          await f.hooks["tool.execute.after"]({ tool: "shell", sessionID: session, id: "listen", input, status: "completed", result: { output: said, metadata: { exit: 0 } } })
        },
        wakes: () => f.prompts.map((p: any) => p.text).filter((t: string) => t.startsWith("[borg listen]")),
        end: async () => {
          await f.emit("session.deleted", { sessionID: session })
          await settle()
        },
        close: () => cleanup(),
      }
    },
  },
]

/** Installs `p`'s adapter into the world's HOME and starts a host on the *installed* file. */
async function drone(w: ReturnType<typeof world>, p: Profile, session: string, cwd = w.cwd): Promise<Host> {
  w.cli("install", p.harness, "--apply", "--home", w.home)
  // A fresh module per host: the query keeps node's module cache from handing back another world's.
  const mod = await import(pathToFileURL(p.installed(w.home)).href + `?${session}`)
  return p.host(mod, cwd, session)
}

/** This session in `borg top --json` (what the web page's drones panel shows). */
function top(w: ReturnType<typeof world>, session: string): any {
  const all: any[] = JSON.parse(w.cli("top", "--json"))
  return all.flatMap((c) => c.drones ?? []).find((d) => d.session === session) ?? {}
}

async function until(done: () => boolean, ms = 20_000) {
  const end = Date.now() + ms
  while (!done() && Date.now() < end) await new Promise((r) => setTimeout(r, 50))
}

/** borg on the world's PATH, but for the session's waiter (`borg listen --stream`), which prints what is
 *  written to `wake`, each line once, and notes each print: the test says when a wake comes. borg's own
 *  waiter holds its wakes off while a turn runs; the adapter's hold is for one that comes anyway. All
 *  else, the hooks and their answers above all, is borg itself. */
function fakeWaiter(w: ReturnType<typeof world>) {
  const [wake, printed, streams] = ["wake", "printed", "streams"].map((f) => join(w.root, f))
  const bin = join(w.root, "bin/borg")
  rmSync(bin)
  writeFileSync(
    bin,
    `#!/bin/sh
if [ "$1 $2" = "listen --stream" ]; then
  printf '%s\\n' "$*" >> "${streams}"
  # Until the adapter stops it (SIGTERM), or its host process is gone.
  while kill -0 "$PPID" 2>/dev/null; do
    if [ -s "${wake}" ]; then cat "${wake}"; : > "${wake}"; printf 'printed\\n' >> "${printed}"; fi
    sleep 0.05
  done
fi
exec "${BORG}" "$@"
`,
  )
  chmodSync(bin, 0o755)
  const count = (f: string) => (existsSync(f) ? readFileSync(f, "utf8").split("\n").filter(Boolean).length : 0)
  return { wake, prints: () => count(printed), streams: () => count(streams) }
}

/** An adapter's code as it runs: comments, strings and regex literals blanked (a template's `${…}`
 *  kept), each line where it was. */
function codeOf(src: string): string[] {
  let out = ""
  // What is open: "`" a template's text, "${" an expression in it, "{" a block or an object.
  const open: string[] = []
  // The last code character: a `/` after an operand divides, anywhere else starts a regex literal.
  let last = ""
  for (let i = 0; i < src.length; i++) {
    const c = src[i]
    const blank = (to: number) => {
      out += src.slice(i, to).replace(/[^\n]/g, " ")
      i = to - 1
    }
    if (open.at(-1) === "`") {
      if (c === "\\") blank(i + 2)
      else if (c === "`") (open.pop(), (out += c), (last = c))
      else if (c === "$" && src[i + 1] === "{") (open.push("${"), (out += "${"), i++, (last = "{"))
      else out += c === "\n" ? c : " "
      continue
    }
    const eol = src.indexOf("\n", i) < 0 ? src.length : src.indexOf("\n", i)
    if (c === "/" && src[i + 1] === "/") blank(eol)
    else if (c === "/" && src[i + 1] === "*") blank(src.indexOf("*/", i) + 2)
    else if (c === '"' || c === "'") {
      let j = i + 1
      while (j < eol && src[j] !== c) j += src[j] === "\\" ? 2 : 1
      blank(j + 1)
      last = c
    } else if (c === "/" && (last === "" || "(,=:[!&|?{};+-*%<>~^".includes(last) || /\b(return|typeof)\s*$/.test(out))) {
      let j = i + 1
      for (let cls = false; j < eol && (cls || src[j] !== "/"); j++) {
        if (src[j] === "\\") j++
        else if (src[j] === "[") cls = true
        else if (src[j] === "]") cls = false
      }
      while (/[a-z]/.test(src[j + 1] ?? "")) j++
      blank(j + 1)
      last = "/"
    } else {
      if (c === "`" || c === "{") open.push(c)
      else if (c === "}") open.pop()
      out += c
      if (!/\s/.test(c)) last = c
    }
  }
  return out.split("\n")
}

test("every adapter's numbers are named constants, each with its kind in the comment above it", () => {
  // The kinds of docs/design/constants.md; a guard's comment also says what runaway it stops.
  const KIND = /\b(protocol|guard|derived|learned|method|baseline|display):/i
  for (const harness of ["omp", "opencode"]) {
    const src = readFileSync(join(REPO, `adapters/${harness}/borg.ts`), "utf8")
    const lines = src.split("\n")
    const unlabelled: string[] = []
    const bare: string[] = []
    codeOf(src).forEach((line, i) => {
      const named = /^(?:export )?const ([A-Z][A-Z0-9_]*)\b/.exec(line)
      // An index (`m[2]`, a capture group's) counts nothing; 0 and 1 start, step and test.
      const numbers = line.replace(/(?<=[\w\])])\[\d+\]/g, "").match(/(?<![\w$.])\d[\d_]*(?:\.\d+)?/g)?.filter((n) => n !== "0" && n !== "1") ?? []
      if (!numbers.length) return
      if (!named) return void bare.push(`${i + 1}: ${lines[i].trim()}`)
      const comment: string[] = []
      for (let j = i - 1; j >= 0 && /^\s*(\/\/|\/\*\*|\*)/.test(lines[j]); j--) comment.unshift(lines[j])
      if (!KIND.test(comment.join(" "))) unlabelled.push(named[1])
    })
    assert.deepEqual(unlabelled, [], `${harness}: a comment above each starts its reason with its kind (Guard:, Baseline:, ...)`)
    assert.deepEqual(bare, [], `${harness}: a number in code is a named constant`)
  }
})

for (const p of PROFILES) {
  test(`${p.name}: install is a dry run first, then idempotent and reversible`, () => {
    const w = world("install")
    const file = p.installed(w.home)
    assert.match(w.cli("install", p.harness, "--home", w.home), /dry run/)
    assert.equal(existsSync(file), false, "a dry run writes nothing")
    assert.match(w.cli("install", p.harness, "--apply", "--home", w.home), /wrote/)
    assert.equal(readFileSync(file, "utf8"), readFileSync(resolve(import.meta.dirname, `../${p.harness}/borg.ts`), "utf8"), "the adapter, whole")
    assert.match(w.cli("install", p.harness, "--home", w.home), /nothing to change/)
    assert.match(w.cli("status", "--home", w.home), new RegExp(`${p.harness}\\s+wired: `))
    assert.match(w.cli("doctor", "--home", w.home), new RegExp(`ok   ${p.harness}: adapter installed at`))
    w.cli("uninstall", p.harness, "--apply", "--home", w.home)
    assert.equal(existsSync(file), false, "uninstall removes borg's own adapter")
    rmSync(w.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 })
  })

  test(`${p.name}: the hooks record the turn, block the stop once and end the drone`, async () => {
    const w = world("hooks")
    const s = `conf-hooks-${p.harness}`
    const net = join(w.cwd, "src/net.rs")

    // Off-repo: no state, no presence.
    const off = await drone(w, p, "conf-off", "/elsewhere/off")
    await off.prompt(PROMPT)
    await off.close()
    assert.equal(existsSync(join(w.root, "store/collectives/corp/state")), false, "off-repo writes nothing")

    const h = await drone(w, p, s)
    assert.equal(await h.prompt(PROMPT), "")
    assert.equal(w.state(s, "lastprompt"), PROMPT)
    let d = top(w, s)
    assert.deepEqual([d.harness, d.state, d.prompt], [p.harness, "working", PROMPT])

    assert.equal(await h.refused({ kind: "read", path: net }, "c0"), "", "nobody steers: the call runs")
    await h.tool({ kind: "read", path: net }, "c1")
    assert.match(w.state(s, "ctx"), new RegExp(net), "the read's path is a touch")
    await h.tool({ kind: "edit", path: net }, "c2")
    await h.tool({ kind: "bash", command: "cat src/budget.rs" }, "c3")
    const edits = w.state(s, "edits")
    assert.ok(edits.includes(net) && edits.includes(NEW_TEXT), `edits land in .edits with path: ${edits}`)
    const turn = w.state(s, "turn")
    assert.ok(turn.includes(`[Edit] ${net}`) && turn.includes("[Bash] src/budget.rs"), turn)

    const started = Date.now()
    const reason = await h.stop(PLAN)
    assert.ok(Date.now() - started < p.stopMs, "inside the stop's budget")
    assert.match(reason, /\[Mdrain\]/, "one block with the lesson")
    assert.equal(top(w, s).state, "waiting")
    assert.equal(await h.stop("Answered."), "", "one round per stop")

    await h.end()
    d = top(w, s)
    assert.equal(d.state, "ended")
    await h.close()
    rmSync(w.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 })
  })

  test(`${p.name}: a wordless denial and the next prompt become an episode`, async () => {
    const w = world("deny")
    Object.assign(process.env, { BORG_LEARN: "on", BORG_PREDICT_MODEL: "off" })
    const s = `conf-deny-${p.harness}`
    const h = await drone(w, p, s)
    await h.prompt(PROMPT)
    await h.deny({ kind: "bash", command: "rm -rf target" }, "c1")
    await h.prompt(CORRECTION)
    await h.close()
    let episodes = ""
    try {
      episodes = readFileSync(join(w.root, "store/collectives/corp/episodes.jsonl"), "utf8")
    } catch {}
    if (caps(p.harness).deny_capture) assert.ok(episodes.includes("drain it with an explicit consumer") && episodes.includes("rm -rf target"), episodes)
    else assert.ok(!episodes.includes("rm -rf target"), `the host's denials are ignored: ${episodes}`)
    rmSync(w.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 })
  })

  test(`${p.name}: a gone host process reads as ended`, async () => {
    const w = world("pid")
    const s = `conf-pid-${p.harness}`
    const h = await drone(w, p, s)
    // The host gone: the adapter sends a pid that runs nowhere (above Linux's default pid_max).
    const own = Object.getOwnPropertyDescriptor(process, "pid")!
    Object.defineProperty(process, "pid", { ...own, value: 4_194_303 })
    try {
      await h.prompt(PROMPT)
    } finally {
      Object.defineProperty(process, "pid", own)
    }
    assert.equal(top(w, s).state, caps(p.harness).sends_pid ? "ended" : "working")
    await h.close()
    rmSync(w.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 })
  })

  test(`${p.name}: a paused drone has its tools refused`, async () => {
    const w = world("pause")
    const s = `conf-pause-${p.harness}`
    const h = await drone(w, p, s)
    await h.prompt(PROMPT)
    // The engineer's `borg pause` from this machine: a pause message, admitted locally (steer.rs).
    assert.match(w.cli("pause", s, "--dir", w.cwd, "use the mock db"), /^paused /)
    assert.match(await h.refused({ kind: "bash", command: "ls" }, "c1"), /paused by/)
    w.cli("resume", s, "--dir", w.cwd)
    assert.equal(await h.refused({ kind: "bash", command: "ls" }, "c2"), "", "resumed")
    await h.close()
    rmSync(w.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 })
  })

  test(`${p.name}: a held wake whose message the turn's hooks showed is dropped by borg's ids; one they did not show goes in`, async () => {
    const w = world("wake")
    const waiter = fakeWaiter(w)
    const s = `conf-wake-${p.harness}`
    const h = await drone(w, p, s)
    try {
      await h.prompt("Look at the parser")
      await h.listen()
      await until(() => waiter.streams() === 1)
      assert.equal(waiter.streams(), 1, "the adapter started the session's waiter")
      // A message for the session while its turn runs, its wake, and a wake for a message no hook shows.
      const told = JSON.parse(execFileSync("borg", ["tell", "--json", s, "the parser is merged"], { cwd: w.cwd, encoding: "utf8", env: process.env }))
      const wake = (who: string) => `[borg listen] 1 message from other sessions waits for you (${who}).`
      const line = (id: string, who: string) => JSON.stringify({ wake: wake(who), n: 1, ids: [id] })
      writeFileSync(waiter.wake, `${line(told.id, "engineer tells")}\n${line("m00000000beef", "nobody shows it")}\n`)
      await until(() => waiter.prints() === 1)
      // The next tool call (by which the adapter has read the waiter's lines) shows the message.
      assert.match(await h.tool({ kind: "read", path: join(w.cwd, "src/net.rs") }, "c1"), new RegExp(`\\[${told.id}\\]: the parser is merged`))
      // The turn ends (a stop that blocks once keeps it running): what is held goes in.
      for (let i = 0; i < 2 && !h.wakes().length; i++) await h.stop(PLAN)
      assert.deepEqual(h.wakes(), [wake("nobody shows it")], "only the wake whose message the model has not read (borg's answer names it, `shown_ids`)")
    } finally {
      // The session's end stops its waiter.
      await h.end()
      await h.close()
    }
    rmSync(w.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 })
  })

  test(`${p.name}: a change is checked live and its ask arrives with the next tool call`, async () => {
    const w = world("live")
    process.env.BORG_MODE = "live"
    const s = `conf-live-${p.harness}`
    const net = join(w.cwd, "src/net.rs")
    const h = await drone(w, p, s)
    await h.prompt(PROMPT)
    await h.tool({ kind: "read", path: net }, "c1")
    assert.equal(w.state(s, "live.n"), "", "a read changes nothing")
    // The edit starts a detached check; the call returns at once, nothing to say yet.
    const started = Date.now()
    assert.equal(await h.tool({ kind: "edit", path: net }, "c2"), "")
    assert.ok(Date.now() - started < 5_000)
    await until(() => w.state(s, "queued").includes("live ") && w.state(s, "live.mark") !== "")
    assert.match(w.state(s, "queued"), /^live Mdrain @[0-9a-f]{12}\n$/, "the ask is queued, with the check that asked it")
    const checked = readFileSync(join(w.root, "prompts/0.txt"), "utf8")
    assert.ok(checked.includes("[Edit] ") && checked.includes(NEW_TEXT), `the check read the turn's tool calls: ${checked}`)
    const next = await h.tool({ kind: "read", path: join(w.cwd, "src/budget.rs") }, "c3")
    assert.ok(next.includes("borg check of your work so far") && next.includes("[Mdrain]"), next)
    // The check covered the turn: no model call, no block (the ask was delivered).
    assert.equal(await h.stop(PLAN), "")
    assert.equal(w.asked(), 1, "a covered stop costs no model call")
    assert.match(w.state(s, "ctx"), /#borg-skip live_covered/)
    await h.end()
    await h.close()
    rmSync(w.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 })
  })
}
