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
//   agent_end, last stop aborted -> halted           (an Esc ends the turn with no session_stop: a
//                                                      listening session's wakes are held off no longer)
//   session_shutdown            -> end
//
// A session that listens (`borg listen`, docs/commands.md) is woken by the extension: it starts the
// session's waiter, `borg listen --stream`, once the model's own `borg listen` succeeds in its
// tool_result (omp gives the command PI_SESSION_ID, so it bound this session), or once a stop's answer
// carries `"listen": true` (borg says no waiter runs). The waiter waits at no token cost, holds wakes
// off while the engineer's turn runs, and says each wake as a JSON line naming its messages. The
// extension holds a wake that arrives while omp runs a turn of the session (from its prompt or tool
// call to its last agent_end or a stop that does not block) and drops it once the turn's hooks together
// handed the model every message it names, over several hooks or before the wake arrived (borg's answer
// names them, `"shown_ids"`; an older borg's names none, so the wake goes in): when that context reached
// the model, so an `input`'s, kept for the next agent start or tool result, drops nothing before;
// otherwise it sends it in with
// pi.sendMessage(..., {deliverAs: "nextTurn", triggerTurn: true}) and answers the waiter on its stdin:
// `{"sent": ids}`, or `{"failed": ids}` when omp refused it, and the waiter wakes the session again
// for them after its backoff. Its `{"end"}` line ends it until the next `"listen": true`;
// `borg listen --off` in the session, or its end, stops it. Spawned, never awaited.
//
// How long each handler waits for borg, and how much of a shell's output borg gets, are borg's own
// (`budgets`, harness/omp.rs `BUDGETS`): the extension asks for them until an answer carried them, and
// uses FALLBACK, the same values, until then.
//
// Fails open: a missing borg binary, a crash, a timeout or bad output is a no-op.
// borg is found via $BORG_BIN, else `borg` on PATH. Plain erasable TypeScript, so omp (Bun) loads
// it as an extension and `node --test` runs its contract test without a build step.

import { spawn } from "node:child_process";

type Json = Record<string, unknown>;

/** What each handler waits for borg's answer (ms) before it fails open, and how much of a shell call's
 *  output borg gets (its end, where a test run's summary is). */
export type Budgets = { prompt_ms: number; pre_ms: number; post_ms: number; stop_ms: number; end_ms: number; tail_chars: number };

// Derived: borg's budgets (harness/omp.rs `BUDGETS`, a test keeps them equal) until its first answer
// carries them: omp gives a handler 30 s (session_shutdown 2 s), which no setting changes, so the stop
// gets 28 s and the end 1.5 s; the rest are guards, so a hung borg's file appends or stat never hold the
// engineer's turn.
const FALLBACK: Budgets = { prompt_ms: 5_000, pre_ms: 2_000, post_ms: 5_000, stop_ms: 28_000, end_ms: 1_500, tail_chars: 4_000 };

/** The budgets borg's answer carries (`"budgets"`), over `current`: each a positive number, the rest kept;
 *  undefined when it carries none (an older borg, or none asked). */
export function budgetsOf(out: unknown, current: Budgets = FALLBACK): Budgets | undefined {
	const b = (out as any)?.budgets;
	if (!b || typeof b !== "object") return undefined;
	const next = { ...current };
	for (const k of Object.keys(FALLBACK) as (keyof Budgets)[]) if (Number.isFinite(b[k]) && b[k] > 0) next[k] = Number(b[k]);
	return next;
}

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
	/** Asks borg for its budgets (`budgetsOf`). */
	budgets?: boolean;
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
 *  successful shell call the last `tail` characters of its output as `tool_response` (borg tells a test
 *  run's pass by it). */
export function toolEvent(ctx: any, ev: Json, tail: number = FALLBACK.tail_chars): ["post_tool" | "post_tool_failure", Canonical] {
	const base = { ...where(ctx), tool_name: toolName(String(ev.toolName ?? "")), tool_input: toolInput(String(ev.toolName ?? ""), ev.input) };
	if (ev.isError) return ["post_tool_failure", { ...base, error: text(ev.content) }];
	const out = base.tool_name === "Bash" ? text(ev.content).slice(-tail) : "";
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

/** What a shell command does to the listening of the session it runs in: `on` for `borg listen`, `off`
 *  for `borg listen --off`. Undefined for anything else: a waiter (`--wait`, `--stream`, `--queue`) or
 *  another, named session's (`--session`). */
export function listenCommand(command: string): "on" | "off" | undefined {
	for (const part of command.split(/&&|\|\||[;|\n]/)) {
		const m = /(?:^|\s)(?:\S*\/)?borg\s+listen(?:\s+(.*))?$/.exec(part.trim());
		if (!m) continue;
		const args = (m[1] ?? "").trim().split(/\s+/).filter(Boolean);
		if (args.some((a) => ["--wait", "--stream", "--queue", "--session"].includes(a) || a.startsWith("--session="))) return undefined;
		return args.includes("--off") ? "off" : "on";
	}
	return undefined;
}

/** What `borg listen` prints once it opted the session in. */
const LISTENS = "[borg listen] This session listens";

/** Guard: how many tool calls' inputs the extension keeps for a denial (which names only the tool), so
 *  calls whose approval never resolved cannot grow the map for the life of omp. */
const CALLS_KEPT = 256;

/** The ids of the messages borg's answer `out` showed the model (its `"shown_ids"`); none from a borg that
 *  names none. Never a count: one event may show a newer message and leave a held wake's. */
export function shownIds(out: unknown): Set<string> {
	const ids = (out as any)?.shown_ids;
	return new Set(Array.isArray(ids) ? ids.filter((i: unknown): i is string => typeof i === "string") : []);
}

/** A wake of the waiter: its text and the ids of its messages (none from an older waiter: never taken
 *  as shown), for the answer and for the hooks that show them meanwhile. */
export type Wake = { wake: string; ids: string[] };

/** One line of the waiter (`borg listen --stream`): a wake to send in, a note, or its last line. */
export type StreamLine = Wake | { note: string } | { end: string };

export function streamLine(line: string): StreamLine | undefined {
	try {
		const v = JSON.parse(line);
		if (typeof v?.wake === "string" && v.wake.trim()) {
			const ids = Array.isArray(v.ids) ? v.ids.filter((i: unknown) => typeof i === "string") : [];
			return { wake: v.wake, ids };
		}
		if (typeof v?.note === "string") return { note: v.note };
		if (typeof v?.end === "string") return { end: v.end };
	} catch {}
	return undefined;
}

/** Tells the waiter `child` whether the wake for `ids` went in (`sent`) or omp refused it (`failed`): one
 *  JSON line on its stdin. Fails open: a waiter gone meanwhile hears nothing. */
export function answerWake(child: ReturnType<typeof spawn> | undefined, how: "sent" | "failed", ids: string[]): void {
	try {
		if (ids.length && child?.stdin?.writable) child.stdin.write(`${JSON.stringify({ [how]: ids })}\n`);
	} catch {}
}

/** The session's waiter, `borg listen --stream --session <id> --dir <cwd>`: each wake to `onWake`,
 *  `onEnd` once it exits; its stdin takes the answers (`answerWake`). Its stderr is dropped; a spawn
 *  failure ends it at once (fails open). */
export function startStream(session: string, cwd: string, onWake: (wake: Wake) => void, onEnd: () => void): ReturnType<typeof spawn> | undefined {
	let child: ReturnType<typeof spawn>;
	try {
		child = spawn(process.env.BORG_BIN || "borg", ["listen", "--stream", "--session", session, "--dir", cwd], { stdio: ["pipe", "pipe", "ignore"] });
	} catch {
		onEnd();
		return undefined;
	}
	child.stdin?.on("error", () => {});
	let rest = "";
	child.stdout?.on("data", (d) => {
		rest += d;
		let nl: number;
		while ((nl = rest.indexOf("\n")) >= 0) {
			const line = streamLine(rest.slice(0, nl));
			rest = rest.slice(nl + 1);
			if (line && "wake" in line) onWake(line);
		}
	});
	let ended = false;
	const end = () => {
		if (!ended) {
			ended = true;
			// Its stdin's end too, which would otherwise keep omp's event loop holding a dead pipe.
			child.stdin?.destroy();
			onEnd();
		}
	};
	child.on("error", end);
	child.on("close", end);
	return child;
}

/** Whether omp's run `messages` ended on an abort (Esc): the turn reaches no session_stop. */
export function aborted(messages: unknown): boolean {
	if (!Array.isArray(messages)) return false;
	for (let i = messages.length - 1; i >= 0; i--) {
		const m = messages[i];
		if (m?.role === "assistant") return m.stopReason === "aborted";
	}
	return false;
}

export default function borg(pi: any): void {
	// borg's budgets: FALLBACK until an answer carried them, asked for until then. At a stop borg sizes
	// its wait for a live check and its model call from BORG_STOP_BUDGET_MS = stop_ms.
	let budgets: Budgets = { ...FALLBACK };
	let learned = false;
	/** `borg hook omp <event>`, waiting `budgets[within]`; an answer's budgets are taken. */
	const call = async (event: string, payload: Canonical, within: keyof Budgets, signal?: AbortSignal) => {
		const out = await runBorg(event, learned ? payload : { ...payload, budgets: true }, budgets[within], signal);
		const b = budgetsOf(out, budgets);
		if (b) [budgets, learned] = [b, true];
		return out;
	};
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
	// borg's context from `input`s, handed over at the next before_agent_start, or at the next
	// tool_result when the input was a steer into a running turn (omp emits `input` mid-turn too, and
	// a steer starts no agent run). Only added to: borg counts a message shown once it answers. Each with
	// the messages it shows, which count as shown once it is handed over (`take`), not before.
	const pending: { content: string; ids: string[] }[] = [];
	const keep = (out: Json | null) => {
		const m = contextMessage(out);
		if (m) pending.push({ content: m.content, ids: [...shownIds(out)] });
	};
	/** The kept context, handed over to the model now, and the messages it shows: the caller tells `shown`
	 *  with them and with whatever else goes to the model along with it. */
	const take = () => {
		const kept = pending.splice(0);
		return { content: kept.map((k) => k.content).join("\n\n"), ids: kept.flatMap((k) => k.ids) };
	};

	// Each listening session's waiter (`borg listen --stream`) in this omp process.
	const streams = new Map<string, ReturnType<typeof spawn>>();
	// Each session's turn: whether omp runs one, the wakes held until it ends, and the messages its hooks
	// handed the model (`shown`), cleared at its end: a later wake names only messages not shown when the
	// waiter looked (listen.rs `news`), and the set never grows for the session's life.
	type Turn = { busy: boolean; held: Wake[]; seen: Set<string> };
	const turns = new Map<string, Turn>();
	const turn = (session: string) => {
		let t = turns.get(session);
		if (!t) turns.set(session, (t = { busy: false, held: [], seen: new Set() }));
		return t;
	};
	/** Whether the turn `t` handed the model every message the wake `w` names; never for one that names
	 *  none (an older waiter's). */
	const allShown = (t: Turn, w: Wake) => w.ids.length > 0 && w.ids.every((i) => t.seen.has(i));
	/** Sends the session's `wakes` in, as one message that starts the next turn, and tells its waiter
	 *  whether it went in; false when omp refused it (the waiter then wakes the session again). */
	const wakeIn = (session: string, wakes: Wake[]): boolean => {
		const ids = wakes.flatMap((w) => w.ids);
		try {
			pi.sendMessage({ customType: "borg-listen", content: wakes.map((w) => w.wake).join("\n\n"), display: true }, { deliverAs: "nextTurn", triggerTurn: true });
			answerWake(streams.get(session), "sent", ids);
			return true;
		} catch {
			answerWake(streams.get(session), "failed", ids);
			return false;
		}
	};
	/** The session's turn ended: its held wakes go in, and what it showed is forgotten. */
	const idle = (session: string) => {
		const t = turn(session);
		t.busy = false;
		t.seen.clear();
		const held = t.held.splice(0);
		if (held.length) t.busy = wakeIn(session, held);
	};
	/** borg's context that showed the messages `ids` reached `session`'s model: they join what the turn
	 *  showed, and each held wake whose messages the turn's hooks together showed all of is dropped, so no
	 *  turn is spent on messages the model already read; one with a message no hook showed yet, or that
	 *  names none (an older waiter's), keeps waiting. */
	const shown = (session: string, ids: Iterable<string>) => {
		const t = turns.get(session);
		if (!t) return;
		for (const i of ids) t.seen.add(i);
		t.held = t.held.filter((w) => !allShown(t, w));
	};
	/** The session's waiter, unless one runs: its wakes go in as a custom message that starts the next
	 *  turn, at once on an idle session, else once its turn ended (`idle`). */
	const arm = (session: string, cwd: string) => {
		if (streams.has(session)) return;
		// Declared first: a spawn that fails ends the stream before startStream returns.
		let child: ReturnType<typeof spawn> | undefined;
		child = startStream(
			session,
			cwd,
			(wake) => {
				const t = turn(session);
				// One whose messages the turn showed before it arrived (the waiter looked before that hook)
				// is dropped at once.
				if (t.busy) {
					if (!allShown(t, wake)) t.held.push(wake);
				} else {
					// The wake's own turn: a later wake waits for its end.
					t.busy = wakeIn(session, [wake]);
				}
			},
			() => {
				if (child && streams.get(session) === child) streams.delete(session);
			},
		);
		if (child && child.exitCode === null && child.signalCode === null) streams.set(session, child);
	};
	/** The session listens no more (`--off`, its end): its waiter is stopped. */
	const disarm = (session: string) => {
		const child = streams.get(session);
		streams.delete(session);
		turns.delete(session);
		try {
			if (child && child.exitCode === null) child.kill("SIGTERM");
		} catch {}
	};

	pi.on(
		"input",
		safe(async (ev, ctx) => {
			if (ev.source === "extension" || typeof ev.text !== "string") return;
			inputSeen = true;
			const said = ev.text.trim();
			command = isCommand(said);
			if (said && !command) {
				const at = where(ctx).session_id;
				turn(at).busy = true;
				keep(await call("prompt", promptEvent(ctx, ev.text), "prompt_ms"));
			}
		}),
	);

	pi.on(
		"before_agent_start",
		safe(async (ev, ctx) => {
			const fromCommand = command;
			command = false;
			const at = where(ctx).session_id;
			turn(at).busy = true;
			if ((!inputSeen || fromCommand) && typeof ev.prompt === "string" && ev.prompt.trim() && ev.prompt.trim() !== asked) {
				keep(await call("prompt", promptEvent(ctx, ev.prompt), "prompt_ms"));
			}
			const { content, ids } = take();
			shown(at, ids);
			return content ? { message: { customType: "borg", content, display: false } } : undefined;
		}),
	);

	pi.on(
		"tool_call",
		safe(async (ev, ctx) => {
			if (calls.size > CALLS_KEPT) calls.delete(calls.keys().next().value as string);
			calls.set(String(ev.toolCallId), ev.input);
			turn(where(ctx).session_id).busy = true;
			// A drone paused by `borg pause` has its tool calls refused.
			const out = await call("pre_tool", { ...where(ctx), tool_name: toolName(String(ev.toolName ?? "")), tool_input: toolInput(String(ev.toolName ?? ""), ev.input) }, "pre_ms");
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
			const [event, payload] = toolEvent(ctx, ev, budgets.tail_chars);
			// A live check's questions (or graded lessons) waiting since an earlier call reach the model
			// before its next step, as a hidden message; the tool output stays as it is.
			// Context kept from a mid-turn steer goes first; both reach the model in one message, so the
			// messages of both count as shown together.
			const answer = await call(event, payload, "post_ms");
			const out = (answer as any)?.hookSpecificOutput?.additionalContext;
			const kept = take();
			const c = [kept.content, typeof out === "string" ? out.trim() : ""].filter(Boolean).join("\n\n");
			shown(payload.session_id, [...kept.ids, ...shownIds(answer)]);
			// The model's own `borg listen` (PI_SESSION_ID bound this session) or `--off`.
			const command = (ev.input as any)?.command;
			const listen = event === "post_tool" && payload.tool_name === "Bash" && typeof command === "string" ? listenCommand(command) : undefined;
			if (listen === "on" && text(ev.content).includes(LISTENS)) arm(payload.session_id, payload.cwd);
			else if (listen === "off") disarm(payload.session_id);
			return c ? { additionalContext: c } : undefined;
		}),
	);

	pi.on(
		"tool_approval_resolved",
		safe(async (ev, ctx) => {
			// Only the engineer's refusal; "no interactive UI available" or an abort is not a judgement.
			if (ev.approved !== false || ev.reason !== "denied by user") return;
			const input = calls.get(String(ev.toolCallId));
			calls.delete(String(ev.toolCallId));
			await call("post_tool_failure", denialEvent(ctx, ev, input), "post_ms");
		}),
	);

	pi.on(
		"session_stop",
		safe(async (ev, ctx) => {
			const payload = stopEvent(ctx, ev);
			const out = await call("stop", payload, "stop_ms", ev.signal);
			// The session listens and no waiter runs.
			if (out?.listen === true) arm(payload.session_id, payload.cwd);
			if (out?.decision === "block" && typeof out.reason === "string" && out.reason) {
				asked = out.reason.trim();
				return { decision: "block", reason: out.reason };
			}
			idle(payload.session_id);
			return undefined;
		}),
	);

	pi.on(
		"agent_end",
		safe(async (ev, ctx) => {
			// An Esc: no stop follows, so borg hears that the engineer's turn is over (its wakes go in again).
			const at = where(ctx);
			if (ev.willContinue === true) return;
			if (streams.has(at.session_id) && aborted(ev.messages)) await call("halted", at, "post_ms");
			idle(at.session_id);
		}),
	);

	pi.on(
		"session_shutdown",
		safe(async (_ev, ctx) => {
			const at = where(ctx);
			// borg's end forgets the opt-in; the waiter is stopped here at once.
			await call("end", at, "end_ms");
			disarm(at.session_id);
		}),
	);
}
