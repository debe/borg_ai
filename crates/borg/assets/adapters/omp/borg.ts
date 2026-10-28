// borg adapter for omp (oh-my-pi): translates omp extension events into the canonical,
// Claude-shaped events of `borg hook omp <event>` and borg's answer back into omp.
//
//   input / before_agent_start  -> prompt             (the engineer's prompt; not /commands or !shell;
//                                                      borg's additionalContext, e.g. a queen question,
//                                                      joins the turn as a hidden custom message)
//   tool_call                   -> pre_tool          (a paused drone's call is blocked with borg's reason)
//   tool_result                 -> post_tool | post_tool_failure (error = the result text; borg's
//                                                      context for a successful call, e.g. a live check's
//                                                      questions, is returned as additionalContext: omp
//                                                      >= 18.4.4 shows it to the model as a hidden message
//                                                      before its next step and leaves the tool output as is)
//   tool_approval_resolved deny -> post_tool_failure  (error = omp's "Tool call denied by user: <tool>")
//   session_stop                -> stop               ({"decision":"block","reason"} is returned to omp,
//                                                      which injects it as a continuation and sets
//                                                      stop_hook_active for the next stop: one round per turn)
//   session_shutdown            -> end
//
// Fails open: a missing borg binary, a crash, a timeout or bad output is a no-op.
// borg is found via $BORG_BIN, else `borg` on PATH. Plain erasable TypeScript, so omp (Bun) loads
// it as an extension and `node --test` runs its contract test without a build step.

import { spawn } from "node:child_process";

type Json = Record<string, unknown>;

// How much of a shell call's output borg gets (its end, where a test run's summary is).
const OUTPUT_TAIL = 4_000;

/** The canonical event `borg hook` reads on stdin. */
export interface Canonical {
	session_id: string;
	cwd: string;
	pid?: number;
	prompt?: string;
	tool_name?: string;
	tool_input?: unknown;
	tool_response?: string;
	error?: string;
	last_assistant_message?: string;
	stop_hook_active?: boolean;
}

/** omp tool names -> the Claude names borg's matching knows (Bash commands are parsed for paths). */
const TOOL_NAMES: Record<string, string> = {
	bash: "Bash",
	read: "Read",
	edit: "Edit",
	write: "Write",
	grep: "Grep",
	find: "Glob",
	glob: "Glob",
};

export function toolName(omp: string): string {
	return TOOL_NAMES[omp] ?? omp;
}

/** File headers inside omp's edit patch: `[src/x.rs#06FE]` or `*** Update File: src/x.rs`. */
const PATCH_FILE = /^(?:\[([^\]#\n]+)(?:#[^\]\n]*)?\]|\*\*\* (?:Update|Add|Delete) File: (.+))$/gm;

/** omp's tool input in Claude's keys, which borg reads: a read's, write's or edit's `path` becomes
 *  `file_path`, an edit's patch (`input`) its `new_string`, and an edit without a path names its files
 *  from the patch's headers. Everything else stays as omp sent it. */
export function toolInput(omp: string, input: unknown): unknown {
	if (!input || typeof input !== "object" || Array.isArray(input)) return input ?? {};
	const { path, input: patch, _input, ...rest } = input as Json;
	const out: Json = { ...rest };
	const local = (p: string) => p.trim().replace(/^local:\/\//, "");
	const file = ["read", "write", "edit"].includes(omp);
	if (typeof path === "string") out[file ? "file_path" : "path"] = file ? local(path) : path;
	const text = patch ?? _input;
	if (omp === "edit" && typeof text === "string") {
		const files = [...new Set([...text.matchAll(PATCH_FILE)].map((m) => local(m[1] ?? m[2])))];
		if (out.file_path === undefined && files.length) out.file_path = files.join(" ");
		out.new_string = text;
	} else {
		if (patch !== undefined) out.input = patch;
		if (_input !== undefined) out._input = _input;
	}
	return out;
}

/** The text of a message, content array or string (omp messages and tool results). */
export function text(v: unknown): string {
	if (typeof v === "string") return v;
	if (Array.isArray(v)) {
		return v
			.map((b) => (b && typeof b === "object" && (b as Json).type === "text" ? String((b as Json).text ?? "") : ""))
			.filter((t) => t.length > 0)
			.join("\n");
	}
	if (v && typeof v === "object" && "content" in v) return text((v as Json).content);
	return "";
}

/** Session id and cwd from the handler context (omp's ExtensionContext), and omp's pid (the
 *  extension runs in omp's process): `borg top` shows a session whose omp is gone as ended. */
function where(ctx: any, ev?: Json): { session_id: string; cwd: string; pid: number } {
	let session_id = "";
	try {
		session_id = ctx?.sessionManager?.getSessionId?.() ?? "";
	} catch {}
	return {
		session_id: session_id || String(ev?.session_id ?? "omp"),
		cwd: String(ctx?.cwd ?? process.cwd()),
		pid: process.pid,
	};
}

/** A line omp handles itself, not a prompt to the drone: a `/command` or a `!shell` line. */
export function isCommand(said: string): boolean {
	return said.startsWith("/") || said.startsWith("!");
}

export function promptEvent(ctx: any, prompt: string): Canonical {
	return { ...where(ctx), prompt };
}

/** A finished tool call: failures carry the result text as `error` (borg looks for rejections in it), a
 *  successful shell call the end of its output as `tool_response` (borg tells a test run's pass by it). */
export function toolEvent(ctx: any, ev: Json): ["post_tool" | "post_tool_failure", Canonical] {
	const base = { ...where(ctx), tool_name: toolName(String(ev.toolName ?? "")), tool_input: toolInput(String(ev.toolName ?? ""), ev.input) };
	if (ev.isError) return ["post_tool_failure", { ...base, error: text(ev.content) }];
	const out = base.tool_name === "Bash" ? text(ev.content).slice(-OUTPUT_TAIL) : "";
	return ["post_tool", out.trim() ? { ...base, tool_response: out } : base];
}

/** omp denies at its approval prompt before running the tool, so no tool_result follows; this is the
 *  text omp itself throws back to the model. */
export function denialEvent(ctx: any, ev: Json, input: unknown): Canonical {
	return {
		...where(ctx),
		tool_name: toolName(String(ev.toolName ?? "")),
		tool_input: toolInput(String(ev.toolName ?? ""), input),
		error: `Tool call denied by user: ${ev.toolName}`,
	};
}

/** borg's `additionalContext` for a prompt as the custom message `before_agent_start` adds to the
 *  turn (hidden from the engineer, read by the drone), or undefined. */
export function contextMessage(out: Json | null): { customType: string; content: string; display: boolean } | undefined {
	const context = (out as any)?.hookSpecificOutput?.additionalContext;
	return typeof context === "string" && context.trim() ? { customType: "borg", content: context, display: false } : undefined;
}

export function stopEvent(ctx: any, ev: Json): Canonical {
	return {
		...where(ctx, ev),
		last_assistant_message: text(ev.last_assistant_message),
		stop_hook_active: ev.stop_hook_active === true,
	};
}

/** Run `borg hook omp <event>`; resolves to borg's JSON answer, or null (fail open). An aborted
 *  `signal` (omp gave up on the handler) kills borg, so it stops writing state for an answer nobody reads. */
export function runBorg(event: string, payload: Canonical, timeoutMs: number, signal?: AbortSignal): Promise<Json | null> {
	return new Promise((resolve) => {
		let out = "";
		let done = false;
		const finish = (v: Json | null) => {
			if (!done) {
				done = true;
				clearTimeout(timer);
				signal?.removeEventListener("abort", kill);
				resolve(v);
			}
		};
		// Before spawning: a spawn failure (ENOENT) is emitted later, with no listener to take it.
		if (signal?.aborted) return resolve(null);
		let child: ReturnType<typeof spawn>;
		try {
			// BORG_STOP_BUDGET_MS: at a stop borg keeps its wait for a live check, its model call and its
			// expert drone inside this timeout, so its answer arrives.
			const env = { ...process.env, BORG_STOP_BUDGET_MS: String(timeoutMs) };
			child = spawn(process.env.BORG_BIN || "borg", ["hook", "omp", event], { stdio: ["pipe", "pipe", "ignore"], env });
		} catch {
			return resolve(null);
		}
		const kill = () => {
			try {
				child.kill();
			} catch {}
			finish(null);
		};
		const timer = setTimeout(kill, timeoutMs);
		signal?.addEventListener("abort", kill, { once: true });
		child.on("error", () => finish(null)); // e.g. ENOENT: borg not installed
		child.stdout?.on("data", (d) => (out += d));
		child.on("close", () => {
			try {
				finish(out.trim() ? JSON.parse(out) : null);
			} catch {
				finish(null);
			}
		});
		child.stdin?.on("error", () => {});
		child.stdin?.end(JSON.stringify(payload));
	});
}

// Per-event budgets: capture is file appends (ms); stop may call a model (the consequence model) but omp
// gives handlers a fixed 30 s (no setting changes it for session_stop), so the stop sizes its wait for a
// live check and its model call from BORG_STOP_BUDGET_MS = STOP_MS; omp waits only 2 s on
// session_shutdown, and borg detaches `borg learn` anyway.
const FAST_MS = 5_000;
// Before every tool call: borg answers in a few ms (a stat when nobody steers); fail open after 2 s.
const PRE_MS = 2_000;
const STOP_MS = 28_000;
const END_MS = 1_500;

export default function borg(pi: any): void {
	// Interactive and RPC prompts arrive as `input` (with the engineer's raw text). `omp -p` skips
	// `input`, so until one is seen, the turn's prompt is taken from `before_agent_start`.
	let inputSeen = false;
	// borg's own questions come back through before_agent_start as the continuation's prompt; they
	// are not the engineer's words (Claude never reports a Stop hook's reason as a prompt either).
	let asked = "";
	// tool_call input by id: a denial (tool_approval_resolved) carries only the tool's name.
	const calls = new Map<string, unknown>();
	const safe = (fn: (ev: any, ctx: any) => Promise<unknown>) => async (ev: any, ctx: any) => {
		try {
			return await fn(ev, ctx);
		} catch {
			return undefined;
		}
	};

	// omp emits `input` before it handles `/commands` and `!shell` lines, so `/model x` or `!ls` would
	// otherwise become the session's task, or the "reason" for a pending denial. They are not prompts;
	// if one does start an agent run (a prompt template, a skill), its expansion arrives through
	// before_agent_start instead.
	let command = false;
	// borg's context for the prompt seen at `input`, handed to the turn at its before_agent_start.
	let context: ReturnType<typeof contextMessage>;

	pi.on(
		"input",
		safe(async (ev, ctx) => {
			if (ev.source === "extension" || typeof ev.text !== "string") return;
			inputSeen = true;
			const said = ev.text.trim();
			command = isCommand(said);
			context = said && !command ? contextMessage(await runBorg("prompt", promptEvent(ctx, ev.text), FAST_MS)) : undefined;
		}),
	);

	pi.on(
		"before_agent_start",
		safe(async (ev, ctx) => {
			const fromCommand = command;
			command = false;
			if ((!inputSeen || fromCommand) && typeof ev.prompt === "string" && ev.prompt.trim() && ev.prompt.trim() !== asked) {
				context = contextMessage(await runBorg("prompt", promptEvent(ctx, ev.prompt), FAST_MS)) ?? context;
			}
			const message = context;
			context = undefined;
			return message ? { message } : undefined;
		}),
	);

	pi.on(
		"tool_call",
		safe(async (ev, ctx) => {
			if (calls.size > 256) calls.delete(calls.keys().next().value as string);
			calls.set(String(ev.toolCallId), ev.input);
			// A drone paused by signed steering (borg steer --pause) has its tool calls refused.
			const out = await runBorg("pre_tool", { ...where(ctx), tool_name: toolName(String(ev.toolName ?? "")), tool_input: toolInput(String(ev.toolName ?? ""), ev.input) }, PRE_MS);
			const deny = (out as any)?.hookSpecificOutput;
			if (deny?.permissionDecision === "deny" && typeof deny.permissionDecisionReason === "string") {
				return { block: true, reason: deny.permissionDecisionReason };
			}
			return undefined;
		}),
	);

	pi.on(
		"tool_result",
		safe(async (ev, ctx) => {
			calls.delete(String(ev.toolCallId));
			const [event, payload] = toolEvent(ctx, ev);
			// A live check's questions (or graded lessons) waiting since an earlier call reach the model
			// before its next step, as a hidden message; the tool output stays as it is.
			const c = ((await runBorg(event, payload, FAST_MS)) as any)?.hookSpecificOutput?.additionalContext;
			return typeof c === "string" && c.trim() ? { additionalContext: c } : undefined;
		}),
	);

	pi.on(
		"tool_approval_resolved",
		safe(async (ev, ctx) => {
			// Only the engineer's refusal; "no interactive UI available" or an abort is not a judgement.
			if (ev.approved !== false || ev.reason !== "denied by user") return;
			const input = calls.get(String(ev.toolCallId));
			calls.delete(String(ev.toolCallId));
			await runBorg("post_tool_failure", denialEvent(ctx, ev, input), FAST_MS);
		}),
	);

	pi.on(
		"session_stop",
		safe(async (ev, ctx) => {
			const payload = stopEvent(ctx, ev);
			const out = await runBorg("stop", payload, STOP_MS, ev.signal);
			if (out?.decision === "block" && typeof out.reason === "string" && out.reason) {
				asked = out.reason.trim();
				return { decision: "block", reason: out.reason };
			}
			return undefined;
		}),
	);

	pi.on(
		"session_shutdown",
		safe(async (_ev, ctx) => {
			await runBorg("end", where(ctx), END_MS);
		}),
	);
}
