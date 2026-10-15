// Stand-ins for opencode's plugin hosts, shared by the contract test (borg.test.ts) and the harness
// conformance suite (adapters/conformance/conformance.test.ts).

/** A stand-in for opencode's SDK client: a session history and a record of prompts sent back.
 *  `last` adds to the assistant message (e.g. `info.error`, more parts). */
export function fakeClient(assistantText: () => string, last: () => { info?: object; parts?: object[] } = () => ({})) {
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

/** A stand-in for opencode 2's plugin context: registered hooks, an event stream the test feeds,
 *  the session's active context and a record of prompts sent back. `emit` resolves once the plugin
 *  has handled the event (it asks the stream for the next one). */
export function fakeCtx(assistantText: () => string, directory = "/work/repo") {
  const hooks: Record<string, (e: any) => any> = {}
  const prompts: any[] = []
  const synthetics: any[] = []
  const queue: any[] = []
  let deliver: ((r: IteratorResult<any>) => void) | null = null
  let handled: (() => void) | null = null
  let closed = false
  const register = (domain: string) => async (name: string, cb: (e: any) => any) => {
    hooks[`${domain}.${name}`] = cb
    return { dispose: async () => {} }
  }
  const ctx = {
    location: { directory },
    session: {
      hook: register("session"),
      context: async () => [
        { type: "user", text: "fix it" },
        { type: "assistant", content: [{ type: "reasoning", text: "thinking" }, { type: "text", text: assistantText() }, { type: "tool" }] },
      ],
      prompt: async (input: any) => {
        prompts.push(input)
      },
      synthetic: async (input: any) => {
        synthetics.push(input)
      },
    },
    tool: { hook: register("tool") },
    event: {
      subscribe: ({ signal }: { signal: AbortSignal }) => {
        signal.addEventListener("abort", () => {
          closed = true
          deliver?.({ value: undefined, done: true })
        })
        return {
          [Symbol.asyncIterator]() {
            return this
          },
          next(): Promise<IteratorResult<any>> {
            handled?.()
            handled = null
            if (queue.length) return Promise.resolve({ value: queue.shift(), done: false })
            if (closed) return Promise.resolve({ value: undefined, done: true })
            return new Promise((r) => (deliver = r))
          },
        }
      },
    },
  }
  const emit = (type: string, data: object) =>
    new Promise<void>((resolve) => {
      handled = resolve
      const event = { id: "evt_1", created: 0, type, data }
      if (deliver) {
        const d = deliver
        deliver = null
        d({ value: event, done: false })
      } else queue.push(event)
    })
  return { ctx, hooks, prompts, synthetics, emit, closed: () => closed }
}
