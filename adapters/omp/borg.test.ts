// Contract test: recorded-shape omp extension events -> adapter -> the real `borg hook omp`.
// Run: cargo build -p borg-ai && node --test adapters/omp/borg.test.ts
// Event shapes are those omp 18.8.5 emits (packages/coding-agent/src/extensibility/extensions/wrapper.ts,
// runner.ts, session/agent-session.ts in the omp bundle).

import { strict as assert } from "node:assert";
import { execFileSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, join as join_, resolve } from "node:path";
import { test } from "node:test";

import borg, { hiveJoin, runBorg, text, toolEvent, toolInput, waiterLine } from "./borg.ts";

const root = resolve(import.meta.dirname, "../..");
const BORG = process.env.BORG_BIN || join(root, "target/debug/borg");
const SESSION = "0199c0de-omp-contract";

/** Load the extension into a fake ExtensionAPI and return its handlers; `sent` records its
 *  pi.sendMessage calls (message, options). */
function load(sent: [any, any][] = []): Record<string, (ev: unknown, ctx: unknown) => Promise<any>> {
	const handlers: Record<string, any> = {};
	borg({ on: (name: string, fn: any) => (handlers[name] = fn), sendMessage: (m: any, o: any) => sent.push([m, o]) });
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
	// The ask carries its propensity (#borg-p, hive.md L1) and, from this Thompson draw, the pool
	// it was over (#borg-pool), both tagged with the check that drew it.
	assert.match(ctxFile(), /^make the relay delivery lock-free\n#borg-p L1=1 @([0-9a-f]{12})\n#borg-pool L1=[0-9.]+ @\1\n#borg-asked L1\n$/);
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

// ---- Interactive hive drones (phase g): the join binds the session, the extension keeps its waiter.

test("a shell command joins the hive for its own session only when it runs `borg hive join` without --here or --session", () => {
	assert.deepEqual(hiveJoin("borg hive join"), { goal: undefined });
	assert.deepEqual(hiveJoin("cd /r && /usr/local/bin/borg hive join g1 --dir /r"), { goal: "g1" });
	assert.deepEqual(hiveJoin("borg hive join --dir /r 'g2'"), { goal: "g2" });
	assert.equal(hiveJoin("borg hive join g1 --here"), undefined, "a directory's join is a runner's");
	assert.equal(hiveJoin("borg hive join g1 --session s2"), undefined, "another session's");
	assert.equal(hiveJoin("echo borg hive joined; ls"), undefined);
	assert.equal(hiveJoin("borg hive leave"), undefined);
	assert.deepEqual(waiterLine('{"wake":"[borg hive g1] Work arrived.","left":false}'), { wake: "[borg hive g1] Work arrived.", left: false });
	assert.deepEqual(waiterLine('{"left":true}'), { wake: undefined, left: true });
	assert.equal(waiterLine("not json"), undefined);
});

/** A store, a collective over a committed git repo `harbor_orion` and goal g1 (its oracle passes once
 *  `ok` exists in the drone's tree), with this process's env pointed at them; `restore` puts the env back. */
function hiveWorld() {
	const saved = { ...process.env };
	const root = mkdtempSync(join(tmpdir(), "borg-omp-hive-"));
	const work = join(root, "harbor_orion");
	const store = join(root, "store/collectives/corp");
	for (const d of [work, store, join(root, "home")]) mkdirSync(d, { recursive: true });
	writeFileSync(join(root, "collectives.json"), JSON.stringify({ default: "off", collectives: [{ name: "corp", paths: [work], share: "local" }] }));
	for (const k of ["BORG_STATE", "BORG_CONSCIENCE", "PI_SESSION_ID", "CLAUDE_CODE_SESSION_ID", "BORG_HIVE", "BORG_HIVE_DRONE"]) delete process.env[k];
	Object.assign(process.env, {
		BORG_BIN: BORG, BORG_HOME: join(root, "store"), BORG_CONFIG: join(root, "collectives.json"), HOME: join(root, "home"), GIT_CONFIG_NOSYSTEM: "1",
		BORG_QUEEN: "off", BORG_LEARN: "off", BORG_VECTORS: "off", BORG_STOP_ATTENTION: "off", BORG_MODE: "hook", BORG_MATCH: "bm25", BORG_PREDICT_MODEL: "off",
		BORG_HIVE_TAKEUP_MS: "20000",
	});
	const git = (...args: string[]) => execFileSync("git", ["-C", work, "-c", "user.email=e@x", "-c", "user.name=E", ...args], { stdio: "pipe" });
	git("init", "-q", "-b", "main");
	writeFileSync(join(work, "README"), "orion\n");
	git("add", ".");
	git("commit", "-q", "-m", "start");
	const hive = (args: string[], env: Record<string, string> = {}) => execFileSync(BORG, ["hive", ...args, "--dir", work], { env: { ...process.env, ...env }, encoding: "utf8" });
	hive(["init", "g1", "--spec", "Ship the parser", "--oracle", "ok=test -f ok", "--budget", "5"]);
	const registry = () =>
		existsSync(join(store, "hive/drones.local")) ? readFileSync(join(store, "hive/drones.local"), "utf8").trim().split("\n").map((l) => JSON.parse(l)) : [];
	const hs = (s: string) => (existsSync(join(store, `state/${s}.hive`)) ? JSON.parse(readFileSync(join(store, `state/${s}.hive`), "utf8")) : null);
	const restore = () => {
		for (const k of Object.keys(process.env)) if (!(k in saved)) delete process.env[k];
		Object.assign(process.env, saved);
	};
	return { root, work, store, hive, registry, hs, restore };
}

async function until(ms: number, what: string, done: () => boolean) {
	const t0 = Date.now();
	while (!done()) {
		assert.ok(Date.now() - t0 < ms, `${what} within ${ms} ms`);
		await new Promise((r) => setTimeout(r, 50));
	}
}

/** Whether the process `pid` runs. */
function alive(pid: number): boolean {
	try {
		process.kill(pid, 0);
		return true;
	} catch {
		return false;
	}
}

test(
	"a joined omp session: its waiter starts at the join, never wakes it in the engineer's turn, wakes it with pi.sendMessage(nextTurn, triggerTurn), says the done first and leaves at its end",
	{ skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` },
	async () => {
		const w = hiveWorld();
		const S = "omp-hive-drone-1";
		const sent: [any, any][] = [];
		const h = load(sent);
		const ctx = { cwd: w.work, sessionManager: { getSessionId: () => S } };
		try {
			await h.input({ type: "input", text: "join the hive", source: "interactive" }, ctx);
			// The model's bash runs the join: omp gives it PI_SESSION_ID.
			const said = w.hive(["join"], { PI_SESSION_ID: S });
			assert.match(said, /joined every open hive goal of this repo \(g1\) as drone i-[0-9a-f]{6}/);
			assert.match(said, /its harness's plugin waits for work/);
			const join = { type: "tool_result", toolName: "bash", toolCallId: "j1", input: { command: "borg hive join" }, content: [{ type: "text", text: said }], isError: false };
			await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "j1", input: { command: "borg hive join" } }, ctx);
			await h.tool_result(join, ctx);
			const drone = w.hs(S).drone;
			const [entry] = w.registry();
			assert.equal(entry.harness, "omp");
			assert.equal(entry.pid, process.pid, "the harness process is omp's (the extension's process)");
			// The waiter runs from the join on (correction 12), its pid in its lock file.
			const lock = join_(w.store, `state/${drone}.hive-waiter`);
			await until(5_000, "the waiter", () => existsSync(lock) && readFileSync(lock, "utf8").trim() !== "");
			const waiter = Number(readFileSync(lock, "utf8").trim());
			// Still the engineer's turn (the join's), with a long tool call: nothing is handed.
			await h.tool_call({ type: "tool_call", toolName: "bash", toolCallId: "b1", input: { command: "cargo build" } }, ctx);
			w.hive(["open", "g1", "--id", "t1", "--title", "do t1"]);
			// The engineer approves the plan from their own terminal (G5): no drone takes an unapproved task.
			w.hive(["ratify", "g1"]);
			await new Promise((r) => setTimeout(r, 1_500));
			assert.equal(sent.length, 0, "no wake in the engineer's turn, its tool call running");
			await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "b1", input: { command: "cargo build" }, content: [{ type: "text", text: "ok" }], isError: false }, ctx);
			await new Promise((r) => setTimeout(r, 1_000));
			assert.equal(sent.length, 0, "nor before its stop");
			const stop = { type: "session_stop", last_assistant_message: "Joined.", session_id: S, stop_hook_active: false, signal: new AbortController().signal };
			const out = await h.session_stop(stop, ctx);
			assert.equal(out, undefined, "a plugin's drone is never asked to start a wait");
			await until(10_000, "the wake", () => sent.length > 0);
			const [message, options] = sent[0];
			assert.equal(message.customType, "borg-hive");
			assert.match(message.content, /^\[borg hive g1\] Work arrived\./);
			assert.match(message.content, /Your claim: t1 \(work\) do t1/);
			assert.deepEqual(options, { deliverAs: "nextTurn", triggerTurn: true });
			assert.ok(existsSync(join_(w.store, `state/${S}.hive-takeup`)), "handed, not taken up yet");
			// The woken turn's first tool call takes it up.
			await h.tool_call({ type: "tool_call", toolName: "read", toolCallId: "r1", input: { path: "README" } }, ctx);
			assert.ok(!existsSync(join_(w.store, `state/${S}.hive-takeup`)), "taken up");
			// The drone's work, in its own tree; its stop says the done first, then the idle line.
			writeFileSync(join_(w.hs(S).worktree, "ok"), "");
			const done = await h.session_stop({ ...stop, last_assistant_message: "I made the change." }, ctx);
			assert.equal(done?.decision, "block");
			assert.match(done.reason, /^\[borg hive\] t1 done: its checks passed \(ok\); verify task t1\.v1 is open for another drone\./);
			assert.match(done.reason, /Drone idle: waiting for hive work/);
			assert.doesNotMatch(done.reason, /run_in_background|hive wait/);
			assert.equal(await h.session_stop({ ...stop, stop_hook_active: true }, ctx), undefined);
			assert.ok(alive(waiter) && Number(readFileSync(lock, "utf8").trim()) === waiter, "one waiter, for the session's life");
			await new Promise((r) => setTimeout(r, 800));
			assert.equal(sent.length, 1, "its own verify task is not handed to it");
			// The session's end: the drone leaves, its waiter stops.
			await h.session_shutdown({ type: "session_shutdown" }, ctx);
			assert.equal(w.hs(S), null, "a plain session again");
			assert.equal(w.registry().at(-1).by, "leave");
			await until(10_000, "the waiter's end", () => !alive(waiter));
		} finally {
			await h.session_shutdown({ type: "session_shutdown" }, ctx);
			w.restore();
		}
	},
);

test(
	"an omp drone whose engineer types while a task opens is woken only after that turn's stop; an unjoined session starts no waiter",
	{ skip: !existsSync(BORG) && `no borg binary at ${BORG} (cargo build -p borg-ai)` },
	async () => {
		const w = hiveWorld();
		const S = "omp-hive-busy-1";
		const P = "omp-hive-plain-1";
		const sent: [any, any][] = [];
		const h = load(sent);
		const ctx = { cwd: w.work, sessionManager: { getSessionId: () => S } };
		const plain = { cwd: w.work, sessionManager: { getSessionId: () => P } };
		try {
			// A plain session's bash runs other borg commands, even a directory join's: no drone, no waiter.
			await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "p1", input: { command: "borg hive board" }, content: [], isError: false }, plain);
			await h.session_stop({ type: "session_stop", last_assistant_message: "Done.", session_id: P, stop_hook_active: false }, plain);
			assert.equal(w.registry().length, 0);
			const said = w.hive(["join", "g1"], { PI_SESSION_ID: S });
			await h.tool_result({ type: "tool_result", toolName: "bash", toolCallId: "j1", input: { command: "borg hive join g1" }, content: [{ type: "text", text: said }], isError: false }, ctx);
			const stop = { type: "session_stop", last_assistant_message: "Joined.", session_id: S, stop_hook_active: false };
			assert.equal(await h.session_stop(stop, ctx), undefined);
			// The engineer types: their turn, in which a task opens.
			await h.input({ type: "input", text: "look at the parser tests", source: "interactive" }, ctx);
			w.hive(["open", "g1", "--id", "t1", "--title", "do t1"]);
			// The engineer approves the plan from their own terminal (G5): no drone takes an unapproved task.
			w.hive(["ratify", "g1"]);
			await new Promise((r) => setTimeout(r, 1_500));
			assert.equal(sent.length, 0, "never inside the engineer's turn");
			assert.equal(await h.session_stop({ ...stop, last_assistant_message: "The tests pass." }, ctx), undefined);
			await until(10_000, "the wake after the turn", () => sent.length === 1);
			assert.match(sent[0][0].content, /Your claim: t1/);
			assert.equal(w.registry().filter((e) => e.session === P).length, 0, "the plain session is no drone");
		} finally {
			await h.session_shutdown({ type: "session_shutdown" }, ctx);
			await h.session_shutdown({ type: "session_shutdown" }, plain);
			w.restore();
		}
	},
);
