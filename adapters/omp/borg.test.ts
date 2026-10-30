// Contract test: recorded-shape omp extension events -> adapter -> the real `borg hook omp`.
// Run: cargo build -p borg-ai && node --test adapters/omp/borg.test.ts
// Event shapes are those omp 18.8.5 emits (packages/coding-agent/src/extensibility/extensions/wrapper.ts,
// runner.ts, session/agent-session.ts in the omp bundle).

import { strict as assert } from "node:assert";
import { execFileSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";

import borg, { aborted, budgetsOf, listenCommand, runBorg, shownIds, streamLine, text, toolEvent, toolInput } from "./borg.ts";

const root = resolve(import.meta.dirname, "../..");
const BORG = process.env.BORG_BIN || join(root, "target/debug/borg");
const SESSION = "0199c0de-omp-contract";

/** Load the extension into a fake ExtensionAPI and return its handlers; `send` gets its
 *  pi.sendMessage calls (message, options). */
function load(send: (m: any, o: any) => void = () => {}): Record<string, (ev: unknown, ctx: unknown) => Promise<any>> {
	const handlers: Record<string, any> = {};
	borg({ on: (name: string, fn: any) => (handlers[name] = fn), sendMessage: send });
	return handlers;
}

function env() {
	const dir = mkdtempSync(join(tmpdir(), "borg-omp-"));
	process.env.BORG_BIN = BORG;
	process.env.BORG_STATE = join(dir, "state");
	process.env.BORG_HOME = join(dir, "home");
	// Never the engineer's real collectives config, and no model call at Stop.
	process.env.BORG_CONFIG = join(dir, "no-collectives.json");
	process.env.BORG_MATCH = "bm25";
	process.env.BORG_LEARN = "on";
	// No release check from a test: it would ask GitHub (borg update.rs).
	process.env.BORG_UPDATE_CHECK = "off";
	delete process.env.BORG_CONSCIENCE;
	const ctx = { cwd: join(dir, "relay"), sessionManager: { getSessionId: () => SESSION } };
	const ctxFile = () => readFileSync(join(dir, "state", `${SESSION}.ctx`), "utf8");
	return { dir, ctx, ctxFile };
}

test("a paused drone's tool call is blocked with borg's reason; anything else lets it run", async () => {
	const dir = mkdtempSync(join(tmpdir(), "borg-omp-pause-"));
	const bin = join(dir, "borg-pause");
	writeFileSync(bin, `#!/bin/sh\ncat >/dev/null\n[ "$3" = pre_tool ] && [ -n "$DENY" ] && echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"[borg] paused"}}'\nexit 0\n`);
	chmodSync(bin, 0o755);
	process.env.BORG_BIN = bin;
	const h = load();
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const call = () => h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "c1", input: { command: "ls" } }, ctx);
	assert.equal(await call(), undefined);
	process.env.DENY = "1";
	assert.deepEqual(await call(), { block: true, reason: "[borg] paused" });
	delete process.env.DENY;
	process.env.BORG_BIN = join(dir, "missing");
	assert.equal(await call(), undefined, "fails open");
});

test("maps tool names, texts and failures", () => {
	assert.equal(text([{ type: "text", text: "a" }, { type: "image" }, { type: "text", text: "b" }]), "a\nb");
	assert.equal(text({ role: "assistant", content: [{ type: "thinking", thinking: "x" }, { type: "text", text: "done" }] }), "done");
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const [ev, p] = toolEvent(ctx, { toolName: "bash", input: { command: "ls" }, content: [{ type: "text", text: "boom" }], isError: true });
	assert.deepEqual([ev, p], ["post_tool_failure", { session_id: "s", cwd: "/r", pid: process.pid, tool_name: "Bash", tool_input: { command: "ls" }, error: "boom" }]);
	// A successful shell call carries the end of its output (a test run's summary); other tools nothing.
	const long = "x".repeat(5_000) + "\ntest result: ok.";
	const [ok, q] = toolEvent(ctx, { toolName: "bash", input: { command: "cargo test" }, content: [{ type: "text", text: long }] });
	assert.equal(ok, "post_tool");
	assert.equal(q.tool_response, long.slice(-4_000));
	assert.equal(toolEvent(ctx, { toolName: "bash", input: { command: "true" }, content: [] })[1].tool_response, undefined);
	assert.equal(toolEvent(ctx, { toolName: "read", input: { path: "a" }, content: [{ type: "text", text: "a" }] })[1].tool_response, undefined);
});

test("edit and write name their file as file_path, an edit's patch is its new_string", () => {
	const patch = "*** Begin Patch\n[src/net.rs#06FE]\nPUT 1:\n+drain it\n[local://plan.md#01]\n*** Update File: src/net.rs\n";
	assert.deepEqual(toolInput("edit", { input: patch }), { file_path: "src/net.rs plan.md", new_string: patch });
	assert.deepEqual(toolInput("write", { path: "/r/a.rs", content: "x" }), { file_path: "/r/a.rs", content: "x" });
	assert.deepEqual(toolInput("read", { path: "local://plan.md" }), { file_path: "plan.md" });
	assert.deepEqual(toolInput("bash", { command: "ls" }), { command: "ls" });
	assert.deepEqual(toolInput("grep", { pattern: "x", path: "src" }), { pattern: "x", path: "src" }, "a search's path is not a file it touched");
	assert.deepEqual(toolInput("edit", undefined), {});
});

test("prompt and tool touches reach borg's session context", async () => {
	const { ctx, ctxFile } = env();
	const h = load();
	await h.input({ type: "input", text: "make the relay delivery lock-free", source: "interactive" }, ctx);
	// Once `input` was seen, before_agent_start must not record the prompt twice.
	await h.before_agent_start({ type: "before_agent_start", prompt: "make the relay delivery lock-free", systemPrompt: [] }, ctx);
	await h.tool_call({ type: "tool_call", toolName: "read", toolCallId: "call_1", input: { path: "src/relay.rs" } }, ctx);
	await h.tool_result(
		{ type: "tool_result", toolName: "read", toolCallId: "call_1", input: { path: "src/relay.rs" }, content: [{ type: "text", text: "fn deliver() {}" }], isError: false },
		ctx,
	);
	await h.tool_result(
		{ type: "tool_result", toolName: "bash", toolCallId: "call_2", input: { command: "cat src/queue.rs" }, content: [{ type: "text", text: "..." }], isError: false },
		ctx,
	);
	assert.equal(ctxFile(), "make the relay delivery lock-free\nsrc/relay.rs\nsrc/queue.rs\n");
});

test("borg's prompt context (a queen question) joins the turn once, as a hidden custom message", async () => {
	const { dir, ctx } = env();
	const queue = join(dir, "home", "queen");
	mkdirSync(queue, { recursive: true });
	const entry = {
		id: "qrelay", decision_key: "open_decision:relay:x", kind: "open_decision", question: "Is the relay queue bounded?",
		options: ["bound it at 1024", "leave it unbounded"], default: "bound it at 1024", evidence: [], session: "seed",
		cwd: ctx.cwd, engineer: "e@x", ms: 1,
	};
	writeFileSync(join(queue, "queue.jsonl"), JSON.stringify(entry) + "\n");
	const h = load();
	await h.input({ type: "input", text: "make the relay delivery lock-free", source: "interactive" }, ctx);
	const out = await h.before_agent_start({ type: "before_agent_start", prompt: "make the relay delivery lock-free", systemPrompt: [] }, ctx);
	assert.equal(out?.message?.customType, "borg");
	assert.equal(out.message.display, false);
	assert.match(out.message.content, /Start your reply with this one question/);
	assert.match(out.message.content, /borg' queen answer 'qrelay' '<answer>' --dir '/);
	// Once per session: the next turn gets nothing.
	await h.input({ type: "input", text: "now the queue", source: "interactive" }, ctx);
	assert.equal(await h.before_agent_start({ type: "before_agent_start", prompt: "now the queue", systemPrompt: [] }, ctx), undefined);
});

test("/commands and !shell lines are not prompts: no context, no task, no denial reason", async () => {
	const { dir, ctx, ctxFile } = env();
	const h = load();
	await h.input({ type: "input", text: "make the relay delivery lock-free", source: "interactive" }, ctx);
	await h.input({ type: "input", text: "/model x", source: "interactive" }, ctx);
	await h.input({ type: "input", text: "  !ls src", source: "interactive" }, ctx);
	assert.equal(ctxFile(), "make the relay delivery lock-free\n");
	assert.equal(readFileSync(join(dir, "state", `${SESSION}.task`), "utf8"), "make the relay delivery lock-free");
	// Right after a denial, `/approval yolo` is not the engineer's reason; their next words are.
	await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "c5", input: { command: "rm -rf target" } }, ctx);
	await h.tool_approval_resolved(
		{ type: "tool_approval_resolved", sessionId: SESSION, toolName: "bash", toolCallId: "c5", approved: false, reason: "denied by user" },
		ctx,
	);
	await h.input({ type: "input", text: "/approval yolo", source: "interactive" }, ctx);
	assert.equal(existsSync(join(dir, "home", "episodes.jsonl")), false);
	await h.input({ type: "input", text: "the benchmarks cache lives in target, keep it", source: "interactive" }, ctx);
	const ep = JSON.parse(readFileSync(join(dir, "home", "episodes.jsonl"), "utf8").trim());
	assert.equal(ep.correction, "the benchmarks cache lives in target, keep it");
});

test("a /template that starts an agent run is recorded from its expansion", async () => {
	const { ctx, ctxFile } = env();
	const h = load();
	await h.input({ type: "input", text: "/review src/relay.rs", source: "interactive" }, ctx);
	await h.before_agent_start({ type: "before_agent_start", prompt: "Review src/relay.rs for races.", systemPrompt: [] }, ctx);
	// The next typed prompt comes through `input` again, not twice.
	await h.input({ type: "input", text: "now fix the race", source: "interactive" }, ctx);
	await h.before_agent_start({ type: "before_agent_start", prompt: "now fix the race", systemPrompt: [] }, ctx);
	assert.equal(ctxFile(), "Review src/relay.rs for races.\nnow fix the race\n");
});

test("omp -p: the prompt comes from before_agent_start", async () => {
	const { ctx, ctxFile } = env();
	const h = load();
	await h.before_agent_start({ type: "before_agent_start", prompt: "add a retry budget", systemPrompt: [] }, ctx);
	assert.equal(ctxFile(), "add a retry budget\n");
});

test("a denial at omp's approval prompt plus the next prompt becomes an episode", async () => {
	const { dir, ctx } = env();
	const h = load();
	await h.input({ type: "input", text: "make the relay delivery lock-free", source: "interactive" }, ctx);
	await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "call_3", input: { command: "rm -rf target" } }, ctx);
	await h.tool_approval_resolved(
		{ type: "tool_approval_resolved", sessionId: SESSION, toolName: "bash", toolCallId: "call_3", approved: false, reason: "denied by user" },
		ctx,
	);
	await h.input({ type: "input", text: "the benchmarks cache lives in target, keep it", source: "interactive" }, ctx);
	const eps = readFileSync(join(dir, "home", "episodes.jsonl"), "utf8").trim().split("\n").map((l) => JSON.parse(l));
	assert.equal(eps.length, 1);
	assert.equal(eps[0].task, "make the relay delivery lock-free");
	assert.equal(eps[0].rejected_tool, 'Bash {"command":"rm -rf target"}');
	assert.equal(eps[0].correction, "the benchmarks cache lives in target, keep it");
});

test("omp carries on after a denial: a later successful tool call ends the wait for a reason", async () => {
	const { dir, ctx } = env();
	const h = load();
	await h.input({ type: "input", text: "make the relay delivery lock-free", source: "interactive" }, ctx);
	await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "d1", input: { command: "rm -rf target" } }, ctx);
	await h.tool_approval_resolved(
		{ type: "tool_approval_resolved", sessionId: SESSION, toolName: "bash", toolCallId: "d1", approved: false, reason: "denied by user" },
		ctx,
	);
	// The model tries an alternative and finishes the turn.
	await h.tool_result(
		{ type: "tool_result", toolName: "bash", toolCallId: "d2", input: { command: "cargo clean -p relay" }, content: [{ type: "text", text: "ok" }], isError: false },
		ctx,
	);
	const last = { role: "assistant", content: [{ type: "text", text: "Cleaned only the relay crate." }] };
	await h.session_stop({ type: "session_stop", last_assistant_message: last, session_id: SESSION, stop_hook_active: false }, ctx);
	await h.input({ type: "input", text: "great, now add tests for the relay", source: "interactive" }, ctx);
	assert.equal(existsSync(join(dir, "home", "episodes.jsonl")), false);
});

test("a denial does not outlive the session: a resumed session's first prompt is not a correction", async () => {
	const { dir, ctx } = env();
	const h = load();
	await h.input({ type: "input", text: "make the relay delivery lock-free", source: "interactive" }, ctx);
	await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "d3", input: { command: "rm -rf target" } }, ctx);
	await h.tool_approval_resolved(
		{ type: "tool_approval_resolved", sessionId: SESSION, toolName: "bash", toolCallId: "d3", approved: false, reason: "denied by user" },
		ctx,
	);
	process.env.BORG_LEARN = "off"; // `end` would otherwise start `borg learn` (a model call)
	await h.session_shutdown({ type: "session_shutdown" }, ctx);
	process.env.BORG_LEARN = "on";
	await load().input({ type: "input", text: "let's document the relay protocol", source: "interactive" }, ctx);
	assert.equal(existsSync(join(dir, "home", "episodes.jsonl")), false);
});

test("a headless denial (no UI) is not the engineer's judgement", async () => {
	const { dir, ctx } = env();
	const h = load();
	await h.tool_approval_resolved(
		{ type: "tool_approval_resolved", sessionId: SESSION, toolName: "bash", toolCallId: "c", approved: false, reason: "no interactive UI available" },
		ctx,
	);
	await h.input({ type: "input", text: "okay, carry on with the docs", source: "interactive" }, ctx);
	assert.equal(existsSync(join(dir, "home", "episodes.jsonl")), false);
});

test("an ACP rejection result is a wordless denial too", async () => {
	const { dir, ctx } = env();
	const h = load();
	await h.input({ type: "input", text: "refactor the parser", source: "rpc" }, ctx);
	await h.tool_result(
		{ type: "tool_result", toolName: "edit", toolCallId: "c4", input: { path: "src/parse.rs" }, content: [{ type: "text", text: "Tool call rejected by user (edit)" }], isError: true },
		ctx,
	);
	await h.input({ type: "input", text: "keep the old entry point, callers depend on it", source: "rpc" }, ctx);
	const ep = JSON.parse(readFileSync(join(dir, "home", "episodes.jsonl"), "utf8").trim());
	assert.equal(ep.rejected_tool, 'Edit {"file_path":"src/parse.rs"}');
});

test("session_stop: borg's questions block once, then stop_hook_active lets the drone stop", async () => {
	const { dir, ctx, ctxFile } = env();
	const lessons = join(dir, "lessons.jsonl");
	writeFileSync(
		lessons,
		[
			{ id: "L1", origin_ms: 1, question: "Is the lock-free relay delivery free of ABA on the queue head?" },
			{ id: "L2", origin_ms: 1, question: "Does the changelog mention the new CLI flag?" },
		]
			.map((l) => JSON.stringify(l))
			.join("\n"),
	);
	process.env.BORG_CONSCIENCE = lessons;
	const h = load();
	// As in `omp -p`: prompts arrive only via before_agent_start.
	await h.before_agent_start({ type: "before_agent_start", prompt: "make the relay delivery lock-free", systemPrompt: [] }, ctx);
	const last = { role: "assistant", content: [{ type: "text", text: "Done: delivery now uses a lock-free queue head." }], stopReason: "stop" };
	const stop = { type: "session_stop", messages: [], turn_id: 0, last_assistant_message: last, session_id: SESSION, session_file: "/x.jsonl", stop_hook_active: false, signal: new AbortController().signal };
	const out = await h.session_stop(stop, ctx);
	assert.equal(out?.decision, "block");
	assert.match(out.reason, /ABA/);
	// omp continues the turn with the questions as its prompt: not the engineer's words.
	await h.before_agent_start({ type: "before_agent_start", prompt: out.reason, systemPrompt: [] }, ctx);
	assert.equal(await h.session_stop({ ...stop, stop_hook_active: true }, ctx), undefined);
	// L2 shares no word with the work, so word matching (BORG_MATCH=bm25) does not ask it.
	// The ask carries its propensity (#borg-p, L1) and, from this Thompson draw, the pool
	// it was over (#borg-pool), each tagged with the check that drew it, as is the ask.
	assert.match(ctxFile(), /^make the relay delivery lock-free\n#borg-p L1=1 @([0-9a-f]{12})\n#borg-pool L1=[0-9.]+ @\1\n#borg-asked L1 @\1\n$/);
});

test("tool_result hands borg's context (a live check's questions) to omp as additionalContext; nothing else", async () => {
	const dir = mkdtempSync(join(tmpdir(), "borg-omp-live-"));
	const bin = join(dir, "borg-live");
	writeFileSync(bin, `#!/bin/sh\ncat >/dev/null\n[ "$3" = post_tool ] && [ -n "$ASK" ] && echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"[borg] borg check of your work so far: Q?"}}'\nexit 0\n`);
	chmodSync(bin, 0o755);
	process.env.BORG_BIN = bin;
	const h = load();
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const result = (isError = false) =>
		h.tool_result({ type: "tool_result", toolName: "edit", toolCallId: "c1", input: { path: "a.rs" }, content: [{ type: "text", text: "ok" }], isError }, ctx);
	assert.equal(await result(), undefined, "no answer: nothing");
	process.env.ASK = "1";
	assert.deepEqual(await result(), { additionalContext: "[borg] borg check of your work so far: Q?" });
	assert.equal(await result(true), undefined, "a failure is post_tool_failure: borg answers nothing");
	delete process.env.ASK;
});

test("context from mid-turn inputs is kept: a later input does not drop it; the next tool_result or turn carries it", async () => {
	const dir = mkdtempSync(join(tmpdir(), "borg-omp-pending-"));
	const bin = join(dir, "borg-msg");
	// Answers a prompt that names a message with it (as a shown-once session message would be).
	writeFileSync(bin, `#!/bin/sh\nin=$(cat)\ncase "$in" in *msg-*) m=$(printf %s "$in" | grep -o 'msg-[a-z]*');; *) exit 0;; esac\n[ "$3" = prompt ] && echo "{\\"hookSpecificOutput\\":{\\"hookEventName\\":\\"UserPromptSubmit\\",\\"additionalContext\\":\\"[borg] $m\\"}}"\nexit 0\n`);
	chmodSync(bin, 0o755);
	process.env.BORG_BIN = bin;
	const h = load();
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const input = (t: string) => h.input({ type: "input", text: t, source: "interactive" }, ctx);
	const result = () => h.tool_result({ type: "tool_result", toolName: "read", toolCallId: "c1", input: { path: "a.rs" }, content: [{ type: "text", text: "ok" }], isError: false }, ctx);
	const turn = (p: string) => h.before_agent_start({ type: "before_agent_start", prompt: p, systemPrompt: [] }, ctx);
	// Two steers and a /command while the agent streams (no before_agent_start between them).
	await input("steer msg-one");
	await input("/model x");
	await input("steer msg-two");
	assert.deepEqual(await result(), { additionalContext: "[borg] msg-one\n\n[borg] msg-two" });
	assert.equal(await result(), undefined, "handed over once");
	await input("steer msg-three");
	await input("plain words");
	assert.deepEqual(await turn("plain words"), { message: { customType: "borg", content: "[borg] msg-three", display: false } });
	assert.equal(await turn("again"), undefined);
});

test("session_stop: when omp gives up on the handler (its signal aborts), borg is killed and nothing is returned", async () => {
	const dir = mkdtempSync(join(tmpdir(), "borg-omp-abort-"));
	const bin = join(dir, "borg-slow");
	const pidFile = join(dir, "pid");
	writeFileSync(bin, `#!/bin/sh\ncat >/dev/null\necho $$ > "${pidFile}"\nexec sleep 30\n`);
	chmodSync(bin, 0o755);
	process.env.BORG_BIN = bin;
	const h = load();
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const abort = new AbortController();
	const started = Date.now();
	const out = h.session_stop({ type: "session_stop", last_assistant_message: "done", signal: abort.signal }, ctx);
	while (!existsSync(pidFile) || !readFileSync(pidFile, "utf8").trim()) await new Promise((r) => setTimeout(r, 20));
	abort.abort();
	assert.equal(await out, undefined);
	assert.ok(Date.now() - started < 5_000, "resolved at the abort, not after borg's 30 s");
	const pid = Number(readFileSync(pidFile, "utf8").trim());
	await new Promise((r) => setTimeout(r, 100));
	assert.throws(() => process.kill(pid, 0), "borg is gone");
});

test("session_shutdown reaches borg end", async () => {
	const { ctx, ctxFile } = env();
	process.env.BORG_LEARN = "off"; // `end` would otherwise start `borg learn` (a model call)
	const h = load();
	await h.input({ type: "input", text: "hello", source: "interactive" }, ctx);
	assert.equal(await h.session_shutdown({ type: "session_shutdown" }, ctx), undefined);
	assert.equal(ctxFile(), "hello\n");
});

test("fails open: missing borg binary, bad output", async () => {
	process.env.BORG_BIN = "/nonexistent/borg";
	assert.equal(await runBorg("stop", { session_id: "s", cwd: "/" }, 1000), null);
	const h = load();
	const ctx = { cwd: "/", sessionManager: { getSessionId: () => "s" } };
	assert.equal(await h.session_stop({ type: "session_stop", last_assistant_message: null }, ctx), undefined);
	await h.input({ type: "input", text: "x", source: "interactive" }, ctx);
	process.env.BORG_BIN = "/bin/echo"; // prints non-JSON
	assert.equal(await runBorg("stop", { session_id: "s", cwd: "/" }, 1000), null);
	assert.equal(await h.session_stop({}, {}), undefined, "even a malformed event/context is a no-op");
});

// ---- borg listen: the extension keeps a listening session's waiter and sends its wakes in.

test("a shell command opts its own session in or out only as `borg listen [--off]`; waiter lines parse", () => {
	assert.equal(listenCommand("borg listen"), "on");
	assert.equal(listenCommand("cd /r && /usr/local/bin/borg listen --dir /r"), "on");
	assert.equal(listenCommand("borg listen --off"), "off");
	assert.equal(listenCommand("borg listen --wait --session s"), undefined, "Claude's waiter");
	assert.equal(listenCommand("borg listen --stream"), undefined);
	assert.equal(listenCommand("borg listen --session s2"), undefined, "another session's");
	assert.equal(listenCommand("echo borg listens; ls"), undefined);
	assert.equal(listenCommand("borg tell all hi"), undefined);
	assert.deepEqual(streamLine('{"wake":"[borg listen] 1 message","n":1,"ids":["m000000000001"]}'), { wake: "[borg listen] 1 message", ids: ["m000000000001"] });
	assert.deepEqual(streamLine('{"wake":"[borg listen] 2 messages"}'), { wake: "[borg listen] 2 messages", ids: [] }, "an older waiter's: never taken as shown");
	assert.deepEqual([...shownIds({ shown_ids: ["m000000000001", 7, "m000000000002"] })], ["m000000000001", "m000000000002"], "only ids");
	assert.equal(shownIds({ shown: 2, hookSpecificOutput: { additionalContext: "[borg] Message from x" } }).size, 0, "borg's ids, never a count or the prose");
	assert.deepEqual(streamLine('{"note":"n"}'), { note: "n" });
	assert.deepEqual(streamLine('{"end":"stopped"}'), { end: "stopped" });
	assert.equal(streamLine('{"wake":"  "}'), undefined);
	assert.equal(streamLine("not json"), undefined);
	assert.equal(aborted([{ role: "user" }, { role: "assistant", stopReason: "aborted" }]), true);
	assert.equal(aborted([{ role: "assistant", stopReason: "aborted" }, { role: "assistant", stopReason: "stop" }]), false);
	assert.equal(aborted(undefined), false);
});

/** Until `done()`, checked every 20 ms; fails after 15 s (a bound on a hang, not a timing margin). */
async function until(done: () => boolean | Promise<boolean>) {
	const t0 = Date.now();
	while (!(await done())) {
		assert.ok(Date.now() - t0 < 15_000, "the awaited event never came");
		await new Promise((r) => setTimeout(r, 20));
	}
}

/** A fake borg: `listen --stream` records `stream\t<args>`, prints what is written to $WAKE (each
 *  line once), keeps the adapter's answers on its stdin in $ANSWERS, ends with `{"end"}` when $WAKE
 *  holds `END`, and records `stopped` on SIGTERM; a hook records `<event>`, answers a stop with
 *  $STOP_ANSWER, a post_tool with $POST_ANSWER and a prompt with $PROMPT_ANSWER. Returns [binary, record
 *  file, wake file, answers file]. */
function listenFake(): [string, string, string, string] {
	const dir = mkdtempSync(join(tmpdir(), "borg-omp-listen-"));
	const [bin, rec, wake, answers] = [join(dir, "borg"), join(dir, "rec"), join(dir, "wake"), join(dir, "answers")];
	writeFileSync(
		bin,
		`#!/bin/sh
if [ "$1 $2" = "listen --stream" ]; then
  printf 'stream\\t%s\\n' "$*" >> "${rec}"
  # A background job of a script reads /dev/null unless given the script's own stdin explicitly.
  exec 3<&0
  cat <&3 >> "${answers}" &
  reader=$!
  trap 'kill $reader 2>/dev/null; printf "stopped\\n" >> "${rec}"; exit 143' TERM
  while :; do
    if [ -s "${wake}" ]; then
      if grep -q '^END$' "${wake}"; then : > "${wake}"; echo '{"end":"listens no more"}'; kill $reader 2>/dev/null; exit 0; fi
      cat "${wake}"; : > "${wake}"; printf 'printed\n' >> "${rec}"
    fi
    sleep 0.05
  done
fi
cat > /dev/null
printf '%s\\n' "$3" >> "${rec}"
[ "$3" = stop ] && [ -n "$STOP_ANSWER" ] && echo "$STOP_ANSWER"
[ "$3" = post_tool ] && [ -n "$POST_ANSWER" ] && echo "$POST_ANSWER"
[ "$3" = prompt ] && [ -n "$PROMPT_ANSWER" ] && echo "$PROMPT_ANSWER"
exit 0
`,
	);
	chmodSync(bin, 0o755);
	return [bin, rec, wake, answers];
}

const records = (rec: string) => (existsSync(rec) ? readFileSync(rec, "utf8").trim().split("\n").filter(Boolean) : []);

test("a listening omp session: its waiter starts at its `borg listen`, wakes it by pi.sendMessage(nextTurn, triggerTurn), an Esc reaches borg as halted, --off stops it, a stop's listen restarts it, its end stops it", { timeout: 20_000 }, async () => {
	const [bin, rec, wake] = listenFake();
	process.env.BORG_BIN = bin;
	delete process.env.STOP_ANSWER;
	const sent: [any, any][] = [];
	const h = load((m, o) => sent.push([m, o]));
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const listen = (command: string, said: string) =>
		h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "l1", input: { command }, content: [{ type: "text", text: said }], isError: false }, ctx);
	const streams = () => records(rec).filter((r) => r.startsWith("stream\t"));
	try {
		// A `borg listen` that did not opt in (outside a collective): no waiter.
		await listen("borg listen", "off: /r belongs to no collective; no messages here");
		await listen("borg listen", "[borg listen] This session listens: a message for it wakes it while it is idle.");
		await until(() => streams().length === 1);
		assert.deepEqual(streams(), ["stream\tlisten --stream --session s --dir /r"]);
		writeFileSync(wake, '{"note":"ignored"}\n{"wake":"[borg listen] 1 message from other sessions waits for you.","n":1,"ids":["m000000000001"]}\n');
		await until(() => sent.length === 1);
		assert.deepEqual(sent[0], [{ customType: "borg-listen", content: "[borg listen] 1 message from other sessions waits for you.", display: true }, { deliverAs: "nextTurn", triggerTurn: true }]);
		// A finished run is no halt; an Esc is.
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "stop" }] }, ctx);
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "aborted" }], willContinue: true }, ctx);
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "aborted" }] }, ctx);
		assert.equal(records(rec).filter((r) => r === "halted").length, 1);
		await listen("borg listen --off", "[borg listen] this session listens no more");
		await until(() => records(rec).includes("stopped"));
		// The stop says the session listens with no waiter: it starts again, and its `end` line ends it.
		process.env.STOP_ANSWER = '{"listen":true}';
		assert.equal(await h.session_stop({ type: "session_stop", last_assistant_message: "ok", session_id: "s" }, ctx), undefined);
		await until(() => streams().length === 2);
		// Its `end` line: it ends, and only a later stop's listen starts it again (one per stop that says so).
		writeFileSync(wake, "END\n");
		await until(async () => (await h.session_stop({ type: "session_stop", last_assistant_message: "ok", session_id: "s" }, ctx), streams().length === 3));
		await h.session_shutdown({ type: "session_shutdown" }, ctx);
		await until(() => records(rec).filter((r) => r === "stopped").length === 2);
		assert.ok(records(rec).includes("end"));
		assert.equal(sent.length, 1);
	} finally {
		delete process.env.STOP_ANSWER;
		await h.session_shutdown({ type: "session_shutdown" }, ctx);
	}
});

test("a wake that arrives while omp runs the session's turn waits for its end, and is dropped only if a hook of the turn showed all its messages", { timeout: 20_000 }, async () => {
	const [bin, rec, wake] = listenFake();
	process.env.BORG_BIN = bin;
	delete process.env.STOP_ANSWER;
	delete process.env.POST_ANSWER;
	const sent: [any, any][] = [];
	const h = load((m, o) => sent.push([m, o]));
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const call = (id: string) => h.tool_call({ type: "tool_call", toolName: "read", toolCallId: id, input: { path: "/r/a" } }, ctx);
	const result = (id: string, command = "cat a", said = "x") =>
		h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: id, input: { command }, content: [{ type: "text", text: said }], isError: false }, ctx);
	// The fake prints each wake before it records `printed`; the next hook's borg run is the sync point
	// by which the adapter has read it.
	const wakeDuring = async (n: number) => {
		await call(`c${n}`);
		writeFileSync(wake, `{"wake":"[borg listen] 1 message from other sessions waits for you (${n}).","n":1,"ids":["m00000000000${n}"]}\n`);
		await until(() => records(rec).filter((r) => r === "printed").length === n);
	};
	try {
		await result("l1", "borg listen", "[borg listen] This session listens: a message for it wakes it while it is idle.");
		await until(() => records(rec).some((r) => r.startsWith("stream\t")));
		// Held while the turn runs; a hook that shows no message keeps it; the turn's end sends it in.
		await wakeDuring(1);
		await result("c1");
		assert.equal(sent.length, 0, "held while the turn runs");
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "stop" }], willContinue: true }, ctx);
		assert.equal(sent.length, 0, "a run that continues is no end");
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "stop" }] }, ctx);
		assert.equal(sent.length, 1);
		assert.match(sent[0][0].content, /waits for you \(1\)/);
		// The wake's own turn ends; then a turn whose tool call shows the message drops its wake.
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "stop" }] }, ctx);
		await wakeDuring(2);
		const showed = (...ids: string[]) =>
			(process.env.POST_ANSWER = JSON.stringify({ hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: "[borg] Message from Two of Two (tell): rebase" }, shown_ids: ids }));
		showed("m000000000002");
		await result("c2");
		delete process.env.POST_ANSWER;
		assert.equal(await h.session_stop({ type: "session_stop", last_assistant_message: "ok", session_id: "s" }, ctx), undefined);
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "stop" }] }, ctx);
		assert.equal(sent.length, 1, "the turn showed its message: no wake");
		// A hook that showed as many messages, but another one (a newer message that took the budget), or
		// that names none (an older borg's), keeps the wake: its message waits unread, and the turn's end
		// sends it in.
		await wakeDuring(3);
		showed("m000000000009");
		await result("c3");
		process.env.POST_ANSWER = JSON.stringify({ hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: "[borg] Message from Two of Two (tell): rebase" } });
		await result("c4");
		delete process.env.POST_ANSWER;
		assert.equal(sent.length, 1, "held while the turn runs");
		assert.equal(await h.session_stop({ type: "session_stop", last_assistant_message: "ok", session_id: "s" }, ctx), undefined);
		assert.equal(sent.length, 2, "its message was not shown: the wake goes in");
		assert.match(sent[1][0].content, /waits for you \(3\)/);
	} finally {
		delete process.env.POST_ANSWER;
		await h.session_shutdown({ type: "session_shutdown" }, ctx);
	}
});

test("a steer's context shows its messages only once handed over: its wake goes in when the turn ends first, and goes when a tool result carries it", { timeout: 20_000 }, async () => {
	const [bin, rec, wake] = listenFake();
	process.env.BORG_BIN = bin;
	delete process.env.STOP_ANSWER;
	delete process.env.POST_ANSWER;
	const sent: [any, any][] = [];
	const h = load((m, o) => sent.push([m, o]));
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const result = (id: string, command = "cat a", said = "x") =>
		h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: id, input: { command }, content: [{ type: "text", text: said }], isError: false }, ctx);
	const end = () => h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "stop" }] }, ctx);
	const MESSAGE = (n: number) => `[borg] Message from Two of Two (tell): rebase ${n}`;
	// The engineer types into the running turn: omp emits `input` and starts no agent run.
	const steer = async (n: number) => {
		process.env.PROMPT_ANSWER = JSON.stringify({ hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: MESSAGE(n) }, shown_ids: [`m00000000000${n}`] });
		await h.input({ type: "input", text: "also rebase", source: "interactive" }, ctx);
		delete process.env.PROMPT_ANSWER;
	};
	const wakeDuring = async (n: number) => {
		await h.tool_call({ type: "tool_call", toolName: "read", toolCallId: `c${n}`, input: { path: "/r/a" } }, ctx);
		writeFileSync(wake, `{"wake":"[borg listen] 1 message from other sessions waits for you (${n}).","n":1,"ids":["m00000000000${n}"]}\n`);
		await until(() => records(rec).filter((r) => r === "printed").length === n);
	};
	try {
		await result("l1", "borg listen", "[borg listen] This session listens: a message for it wakes it while it is idle.");
		await until(() => records(rec).some((r) => r.startsWith("stream\t")));
		// borg answered the steer with the message, but no agent start or tool result handed it over yet.
		await wakeDuring(1);
		await steer(1);
		await end();
		assert.equal(sent.length, 1, "the model has not read it: the wake goes in");
		// The wake's turn starts: its agent start hands the kept context over.
		const started = await h.before_agent_start({ type: "before_agent_start", prompt: sent[0][0].content, systemPrompt: [] }, ctx);
		assert.equal(started?.message?.content, MESSAGE(1));
		await end();
		// Kept from a steer and handed over by the turn's next tool result: its wake goes.
		await wakeDuring(2);
		await steer(2);
		const out = await result("c2");
		assert.equal(out?.additionalContext, MESSAGE(2));
		await end();
		assert.equal(sent.length, 1, "the tool result showed its message: no wake");
	} finally {
		delete process.env.PROMPT_ANSWER;
		await h.session_shutdown({ type: "session_shutdown" }, ctx);
	}
});

test("a held wake that names several messages is dropped once the turn's hooks together showed them all: over two tool results, a steer's kept context with the tool answer, or before it arrived; an earlier turn's hooks drop none", { timeout: 20_000 }, async () => {
	const [bin, rec, wake] = listenFake();
	process.env.BORG_BIN = bin;
	delete process.env.STOP_ANSWER;
	delete process.env.POST_ANSWER;
	delete process.env.PROMPT_ANSWER;
	const sent: [any, any][] = [];
	const h = load((m, o) => sent.push([m, o]));
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const id = (n: number) => `m00000000000${n}`;
	const MESSAGE = (n: number) => `[borg] Message from Two of Two (tell): rebase ${n}`;
	const call = (c: string) => h.tool_call({ type: "tool_call", toolName: "read", toolCallId: c, input: { path: "/r/a" } }, ctx);
	// A tool result whose post_tool answer shows the messages `ns` (msg.rs `capped`: the rest wait).
	const result = async (c: string, ...ns: number[]) => {
		if (ns.length) process.env.POST_ANSWER = JSON.stringify({ hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: ns.map(MESSAGE).join("\n\n") }, shown_ids: ns.map(id) });
		try {
			return await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: c, input: { command: "cat a" }, content: [{ type: "text", text: "x" }], isError: false }, ctx);
		} finally {
			delete process.env.POST_ANSWER;
		}
	};
	// The engineer types into the running turn; borg's answer shows message `n`, kept until handed over.
	const steer = async (n: number) => {
		process.env.PROMPT_ANSWER = JSON.stringify({ hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: MESSAGE(n) }, shown_ids: [id(n)] });
		try {
			await h.input({ type: "input", text: "also rebase", source: "interactive" }, ctx);
		} finally {
			delete process.env.PROMPT_ANSWER;
		}
	};
	// The waiter's wake for the messages `ns`, its `k`th line; the next hook's borg run is the sync point by
	// which the adapter has read it.
	const wakeFor = async (k: number, ...ns: number[]) => {
		writeFileSync(wake, `${JSON.stringify({ wake: `[borg listen] ${ns.length} messages from other sessions wait for you (${ns.join(",")}).`, n: ns.length, ids: ns.map(id) })}\n`);
		await until(() => records(rec).filter((r) => r === "printed").length === k);
	};
	const stop = async () => {
		assert.equal(await h.session_stop({ type: "session_stop", last_assistant_message: "ok", session_id: "s" }, ctx), undefined);
		await h.agent_end({ type: "agent_end", messages: [{ role: "assistant", stopReason: "stop" }] }, ctx);
	};
	try {
		await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "l1", input: { command: "borg listen" }, content: [{ type: "text", text: "[borg listen] This session listens: a message for it wakes it while it is idle." }], isError: false }, ctx);
		await until(() => records(rec).some((r) => r.startsWith("stream\t")));
		// One tool result shows the newest, the next the other.
		await call("c1");
		await wakeFor(1, 1, 2);
		await result("c1", 2);
		await call("c2");
		await result("c2", 1);
		await stop();
		assert.equal(sent.length, 0, "the turn showed both: no wake");
		// A steer's answer shows one and is kept; the next tool result hands it over with an answer that
		// shows the other, in one message to the model.
		await call("c3");
		await wakeFor(2, 3, 4);
		await steer(4);
		assert.equal((await result("c3", 3))?.additionalContext, `${MESSAGE(4)}\n\n${MESSAGE(3)}`);
		await stop();
		assert.equal(sent.length, 0, "the steer and the tool answer showed both: no wake");
		// The waiter looked before the hook that showed the message, and its wake arrives after it.
		await call("c4");
		await result("c4", 5);
		await wakeFor(3, 5);
		await call("c5");
		await stop();
		assert.equal(sent.length, 0, "shown before it arrived: no wake");
		// What a turn showed is its own: a later turn's wake whose messages its own hooks did not all show
		// goes in at its end.
		await call("c6");
		await result("c6", 6);
		await stop();
		await call("c7");
		await wakeFor(4, 6, 7);
		await result("c7", 7);
		await stop();
		assert.equal(sent.length, 1, "this turn showed only one of its messages: the wake goes in");
		assert.match(sent[0][0].content, /wait for you \(6,7\)/);
	} finally {
		await h.session_shutdown({ type: "session_shutdown" }, ctx);
	}
});

test("each wake answers its waiter: `sent` once omp took it, `failed` when omp refused it, so the waiter wakes the session again", { timeout: 20_000 }, async () => {
	const [bin, rec, wake, answers] = listenFake();
	process.env.BORG_BIN = bin;
	delete process.env.STOP_ANSWER;
	let refuse = true;
	const sent: [any, any][] = [];
	const h = load((m, o) => {
		if (refuse) throw new Error("omp is shutting down");
		sent.push([m, o]);
	});
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	const answered = () => (existsSync(answers) ? readFileSync(answers, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
	try {
		await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "l1", input: { command: "borg listen" }, content: [{ type: "text", text: "[borg listen] This session listens." }], isError: false }, ctx);
		await until(() => records(rec).some((r) => r.startsWith("stream\t")));
		writeFileSync(wake, '{"wake":"[borg listen] 1 message from other sessions waits for you.","n":1,"ids":["m000000000001"]}\n');
		await until(() => answered().length === 1);
		assert.deepEqual(answered(), [{ failed: ["m000000000001"] }], "refused: the waiter tries it again");
		assert.equal(sent.length, 0);
		// The waiter's next wake for it (after its backoff) goes in.
		refuse = false;
		writeFileSync(wake, '{"wake":"[borg listen] 1 message from other sessions waits for you.","n":1,"ids":["m000000000001"]}\n');
		await until(() => answered().length === 2);
		assert.deepEqual(answered()[1], { sent: ["m000000000001"] });
		assert.equal(sent.length, 1);
	} finally {
		await h.session_shutdown({ type: "session_shutdown" }, ctx);
	}
});

test("borg's answer sets the budgets: asked for until one carries them; the stop's and the output tail then follow it", async () => {
	const dir = mkdtempSync(join(tmpdir(), "borg-omp-budgets-"));
	const bin = join(dir, "borg");
	const rec = join(dir, "rec");
	// Records each call's event, its BORG_STOP_BUDGET_MS and its input; answers an ask for budgets with $BUDGETS.
	writeFileSync(bin, `#!/bin/sh\nin=$(cat)\nprintf '%s %s %s\\n' "$3" "$BORG_STOP_BUDGET_MS" "$in" >> "${rec}"\ncase "$in" in *'"budgets":true'*) [ -n "$BUDGETS" ] && echo "{\\"budgets\\":$BUDGETS}";; esac\nexit 0\n`);
	chmodSync(bin, 0o755);
	process.env.BORG_BIN = bin;
	const calls = () => readFileSync(rec, "utf8").trim().split("\n").map((l) => {
		const [event, budget, ...input] = l.split(" ");
		return { event, budget: Number(budget), input: JSON.parse(input.join(" ")) };
	});
	const ctx = { cwd: "/r", sessionManager: { getSessionId: () => "s" } };
	try {
		// An older borg answers none: the fallbacks stay, and each call asks again.
		delete process.env.BUDGETS;
		const old = load();
		await old.input({ type: "input", text: "hi", source: "interactive" }, ctx);
		await old.session_stop({ type: "session_stop", last_assistant_message: "ok" }, ctx);
		assert.deepEqual(calls().map((c) => [c.event, c.budget, c.input.budgets]), [["prompt", 5_000, true], ["stop", 28_000, true]]);

		process.env.BUDGETS = JSON.stringify({ prompt_ms: 4_000, pre_ms: 1_000, post_ms: 3_000, stop_ms: 20_000, end_ms: 1_000, tail_chars: 10 });
		const h = load();
		await h.input({ type: "input", text: "hi", source: "interactive" }, ctx);
		await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "c1", input: { command: "cargo test" }, content: [{ type: "text", text: "x".repeat(50) + "test result: ok" }], isError: false }, ctx);
		await h.session_stop({ type: "session_stop", last_assistant_message: "ok" }, ctx);
		const [prompt, post, stop] = calls().slice(2);
		assert.equal(prompt.input.budgets, true, "the first call asks");
		assert.equal(post.input.budgets, undefined, "answered: asked no more");
		assert.equal(post.input.tool_response, "result: ok", "the output's last tail_chars");
		assert.equal(stop.budget, 20_000, "the stop's budget borg named");
		assert.deepEqual(budgetsOf({ budgets: { stop_ms: 9, tail_chars: -1 } }), { prompt_ms: 5_000, pre_ms: 2_000, post_ms: 5_000, stop_ms: 9, end_ms: 1_500, tail_chars: 4_000 }, "each valid number taken, the rest kept");
		assert.equal(budgetsOf({ hookSpecificOutput: {} }), undefined);
	} finally {
		delete process.env.BUDGETS;
	}
});

/** Whether process `pid` runs. */
function alive(pid: number): boolean {
	try {
		process.kill(pid, 0);
		return true;
	} catch {
		return false;
	}
}

test(
	"real borg: an omp session that runs `borg listen` is woken by a tell once its turn ended, and its end stops the waiter",
	{ skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)`, timeout: 30_000 },
	async () => {
		const saved = { ...process.env };
		const root = mkdtempSync(join(tmpdir(), "borg-omp-listen-real-"));
		const work = join(root, "harbor_orion");
		for (const d of [work, join(root, "store/collectives/corp"), join(root, "home")]) mkdirSync(d, { recursive: true });
		writeFileSync(join(root, "collectives.json"), JSON.stringify({ default: "off", collectives: [{ name: "corp", paths: [work], share: "local" }] }));
		for (const k of ["BORG_STATE", "BORG_CONSCIENCE", "PI_SESSION_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "OPENCODE_PID", "STOP_ANSWER"]) delete process.env[k];
		Object.assign(process.env, {
			BORG_BIN: BORG, BORG_HOME: join(root, "store"), BORG_CONFIG: join(root, "collectives.json"), HOME: join(root, "home"),
			BORG_QUEEN: "off", BORG_LEARN: "off", BORG_VECTORS: "off", BORG_STOP_ATTENTION: "off", BORG_MODE: "hook", BORG_MATCH: "bm25", BORG_PREDICT_MODEL: "off",
		});
		const S = "omp-listen-1";
		const sent: [any, any][] = [];
		const h = load((m, o) => sent.push([m, o]));
		const ctx = { cwd: work, sessionManager: { getSessionId: () => S } };
		const lock = join(root, "store/collectives/corp/listen", `${S}.lock`);
		try {
			await h.input({ type: "input", text: "listen for the others", source: "interactive" }, ctx);
			// The model's bash runs it: omp gives it PI_SESSION_ID.
			const said = execFileSync(BORG, ["listen"], { cwd: work, env: { ...process.env, PI_SESSION_ID: S }, encoding: "utf8" });
			assert.match(said, /^\[borg listen\] This session listens/);
			await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "l1", input: { command: "borg listen" } }, ctx);
			await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "l1", input: { command: "borg listen" }, content: [{ type: "text", text: said }], isError: false }, ctx);
			await until(() => existsSync(lock) && readFileSync(lock, "utf8").trim() !== "");
			const waiter = Number(readFileSync(lock, "utf8").trim());
			// The turn ends; the waiter runs, so the stop says nothing.
			assert.equal(await h.session_stop({ type: "session_stop", last_assistant_message: "Listening.", session_id: S }, ctx), undefined);
			execFileSync(BORG, ["tell", S, "the parser is merged"], { cwd: work, env: process.env, encoding: "utf8" });
			await until(() => sent.length === 1);
			const [message, options] = sent[0];
			assert.equal(message.customType, "borg-listen");
			assert.match(message.content, /^\[borg listen\] 1 message from other sessions waits for you \(engineer tells\)/);
			assert.doesNotMatch(message.content, /parser/, "the wake never carries a body");
			assert.deepEqual(options, { deliverAs: "nextTurn", triggerTurn: true });
			await h.session_shutdown({ type: "session_shutdown" }, ctx);
			await until(() => !alive(waiter));
		} finally {
			await h.session_shutdown({ type: "session_shutdown" }, ctx);
			for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k];
			Object.assign(process.env, saved);
		}
	},
);
