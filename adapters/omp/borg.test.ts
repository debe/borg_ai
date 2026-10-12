// Contract test: recorded-shape omp extension events -> adapter -> the real `borg hook omp`.
// Run: cargo build -p borg && node --test adapters/omp/borg.test.ts
// Event shapes are those omp 18.8.5 emits (packages/coding-agent/src/extensibility/extensions/wrapper.ts,
// runner.ts, session/agent-session.ts in the omp bundle).

import { strict as assert } from "node:assert";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";

import borg, { runBorg, text, toolEvent } from "./borg.ts";

const root = resolve(import.meta.dirname, "../..");
const BORG = process.env.BORG_BIN || join(root, "target/debug/borg");
const SESSION = "0199c0de-omp-contract";

/** Load the extension into a fake ExtensionAPI and return its handlers. */
function load(): Record<string, (ev: unknown, ctx: unknown) => Promise<any>> {
	const handlers: Record<string, any> = {};
	borg({ on: (name: string, fn: any) => (handlers[name] = fn) });
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
	assert.equal(ep.rejected_tool, 'Edit {"path":"src/parse.rs"}');
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
	assert.equal(ctxFile(), "make the relay delivery lock-free\n#borg-asked L1\n");
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
