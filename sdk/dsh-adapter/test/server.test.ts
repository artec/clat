/**
 * Protocol tests: serveClat over PassThrough stdio against a scripted MCP
 * client — initialize gating, tools face, server-initiated sampling and
 * elicitation, error mapping, EOF dispose (INV-D4/D7).
 */

import assert from 'node:assert/strict'
import test from 'node:test'
import { createInterface } from 'node:readline'
import { PassThrough } from 'node:stream'
import { serveClat } from '../src/index.js'
import { demoPlugin } from '../src/demo.js'
import type { DshContext, DshPluginLike, RunningAdapter } from '../src/index.js'

interface Frame {
  jsonrpc?: string
  id?: number | string | null
  method?: string
  params?: unknown
  result?: unknown
  error?: { code?: number; message?: string }
}

const WAIT_MS = 2000

/** Scripted MCP client over the adapter's PassThrough stdio. */
class Client {
  readonly input = new PassThrough()
  readonly output = new PassThrough()
  readonly #frames: Frame[] = []
  readonly #waiters: (() => void)[] = []
  #nextId = 1000
  #sentIds = new Set<number>()

  constructor() {
    const rl = createInterface({ input: this.output })
    rl.on('line', line => {
      const trimmed = line.trim()
      if (trimmed === '') return
      const parsed: unknown = JSON.parse(trimmed)
      this.#frames.push(parsed as Frame)
      for (const wake of this.#waiters.splice(0)) wake()
    })
  }

  async #waitFor(predicate: (frame: Frame) => boolean): Promise<Frame> {
    const found = () => this.#frames.find(predicate)
    for (;;) {
      const frame = found()
      if (frame !== undefined) {
        this.#frames.splice(this.#frames.indexOf(frame), 1)
        return frame
      }
      await new Promise<void>(resolve => {
        const timer = setTimeout(() => {
          const index = this.#waiters.indexOf(resolve)
          if (index >= 0) this.#waiters.splice(index, 1)
          assert.fail('timed out waiting for a protocol frame')
        }, WAIT_MS)
        this.#waiters.push(() => {
          clearTimeout(timer)
          resolve()
        })
      })
    }
  }

  send(frame: Frame): void {
    this.input.write(`${JSON.stringify(frame)}\n`)
  }

  private async request(method: string, params: unknown): Promise<{ result?: unknown; error?: Frame['error'] }> {
    const id = this.#nextId
    this.#nextId += 1
    this.#sentIds.add(id)
    this.send({ jsonrpc: '2.0', id, method, params })
    const frame = await this.#waitFor(candidate => candidate.id === id)
    return { result: frame.result, error: frame.error }
  }

  /** Await the response frame for an externally-written request id. */
  async awaitResponse(id: number): Promise<Frame> {
    return this.#waitFor(candidate => candidate.id === id)
  }

  async initialize(capabilities: Record<string, unknown> = { sampling: {}, elicitation: {} }, protocolVersion = '2025-06-18'): Promise<{ result?: unknown; error?: Frame['error'] }> {
    return this.request('initialize', { protocolVersion, capabilities, clientInfo: { name: 'test', version: '0' } })
  }

  async call(method: string, params?: unknown): Promise<{ result?: unknown; error?: Frame['error'] }> {
    return this.request(method, params)
  }

  /** Await the next server-initiated request and capture its params. */
  async serverRequest(): Promise<{ id: number; method: string; params: unknown }> {
    const frame = await this.#waitFor(candidate =>
      typeof candidate.method === 'string' && typeof candidate.id === 'number' && !this.#sentIds.has(candidate.id),
    )
    return { id: frame.id as number, method: frame.method as string, params: frame.params }
  }

  reply(id: number, result: unknown): void {
    this.send({ jsonrpc: '2.0', id, result })
  }

  replyError(id: number, code: number, message: string): void {
    this.send({ jsonrpc: '2.0', id, error: { code, message } })
  }

  end(): void {
    this.input.end()
  }
}

async function start(client: Client, plugin: DshPluginLike = demoPlugin, options: Parameters<typeof serveClat>[1] = {}): Promise<RunningAdapter> {
  const adapter = await serveClat(plugin, { input: client.input, output: client.output, ...options })
  return adapter
}

function textOf(result: unknown): string {
  const content = (result as { content?: Array<{ type?: string; text?: string }> }).content
  const block = content?.find(candidate => candidate.type === 'text')
  return block?.text ?? ''
}

test('initialize handshake echoes protocol version and gates on apply', async () => {
  const client = new Client()
  let applied = false
  const adapter = await start(client, {
    name: 'slow',
    async apply() {
      await new Promise(resolve => setTimeout(resolve, 30))
      applied = true
    },
  })
  const promise = client.initialize()
  await promise.then(({ result }) => {
    assert.equal(applied, true, 'initialize resolves only after apply settled')
    const info = (result as { serverInfo?: { name?: string } }).serverInfo
    assert.equal(info?.name, 'slow')
  })
  await adapter.dispose()
})

test('requests before initialize fail with -32002', async () => {
  const client = new Client()
  const adapter = await start(client)
  const { error } = await client.call('tools/list')
  assert.equal(error?.code, -32002)
  await adapter.dispose()
})

test('tools face: list, call, bad args, unknown tool, unknown method, ping', async () => {
  const client = new Client()
  const adapter = await start(client)
  await client.initialize()

  const listed = await client.call('tools/list')
  const tools = (listed.result as { tools?: Array<{ name?: string; inputSchema?: { type?: string } }> }).tools ?? []
  assert.deepEqual(tools.map(tool => tool.name).sort(), ['ask_roundtrip', 'echo', 'sample_roundtrip'])
  assert.equal(tools[0]?.inputSchema?.type, 'object')

  const echoed = await client.call('tools/call', { name: 'echo', arguments: { text: 'hi', times: 2 } })
  const value = JSON.parse(textOf(echoed.result)) as { lines?: string[] }
  assert.deepEqual(value.lines, ['hi', 'hi'])

  const bad = await client.call('tools/call', { name: 'echo', arguments: {} })
  assert.equal((bad.result as { isError?: boolean }).isError, true)
  assert.match(textOf(bad.result), /text/)

  const unknown = await client.call('tools/call', { name: 'nope', arguments: {} })
  assert.equal((unknown.result as { isError?: boolean }).isError, true)
  assert.match(textOf(unknown.result), /unknown tool/)

  const missingMethod = await client.call('resources/list')
  assert.equal(missingMethod.error?.code, -32601)

  const ping = await client.call('ping')
  assert.deepEqual(ping.result, {})
  await adapter.dispose()
})

test('DSH systemPrompt is exposed through marked MCP prompts/list + prompts/get', async () => {
  const client = new Client()
  const adapter = await start(client, {
    name: 'prompted',
    apply(ctx) {
      ctx.systemPrompt.section({ name: 'later', order: 20, text: 'cwd={{cwd}}' })
      ctx.systemPrompt.section({ name: 'first', order: 10, text: 'be precise' })
      ctx.systemPrompt.context({ name: 'runtime', order: 0, text: 'model={{model}}' })
    },
  })
  await client.initialize()
  const listed = await client.call('prompts/list')
  const prompts = (listed.result as {
    prompts?: Array<{ name?: string; _meta?: Record<string, unknown> }>
  }).prompts ?? []
  assert.equal(prompts[0]?.name, 'dsh-system-prompt')
  assert.equal(prompts[0]?._meta?.['io.artec.clat/dshSystemPrompt'], true)

  const resolved = await client.call('prompts/get', {
    name: 'dsh-system-prompt',
    arguments: { cwd: '/repo', model: 'deepseek-v4' },
  })
  const meta = (resolved.result as { _meta?: Record<string, unknown> })._meta
  assert.equal(meta?.['io.artec.clat/systemPrompt'], 'be precise\n\ncwd=/repo')
  assert.match(String(meta?.['io.artec.clat/runtimeContext']), /model=deepseek-v4/)
  await adapter.dispose()
})

test('sampling round-trip: tools/call → sampling/createMessage → result', async () => {
  const client = new Client()
  const adapter = await start(client)
  await client.initialize()
  const outcome = client.call('tools/call', { name: 'sample_roundtrip', arguments: { prompt: 'meaning of life?' } })
  const request = await client.serverRequest()
  assert.equal(request.method, 'sampling/createMessage')
  const params = request.params as { messages?: Array<{ role?: string; content?: { text?: string } }>; maxTokens?: number }
  assert.equal(params.messages?.[0]?.content?.text, 'meaning of life?')
  assert.equal(params.maxTokens, 64)
  client.reply(request.id, { role: 'assistant', content: { type: 'text', text: '42' }, model: 'fake', stopReason: 'endTurn' })
  const done = await outcome
  assert.match(textOf(done.result), /42/)
  await adapter.dispose()
})

test('sampling denied by host maps to an isError tool result', async () => {
  const client = new Client()
  const adapter = await start(client)
  await client.initialize()
  const outcome = client.call('tools/call', { name: 'sample_roundtrip', arguments: { prompt: 'q' } })
  const request = await client.serverRequest()
  client.replyError(request.id, -32000, 'sampling was not approved: no')
  const done = await outcome
  assert.equal((done.result as { isError?: boolean }).isError, true)
  assert.match(textOf(done.result), /sampling was not approved/)
  await adapter.dispose()
})

test('host without sampling capability fails ctx.llm closed', async () => {
  const client = new Client()
  const adapter = await start(client)
  await client.initialize({})
  const done = await client.call('tools/call', { name: 'sample_roundtrip', arguments: { prompt: 'q' } })
  assert.equal((done.result as { isError?: boolean }).isError, true)
  assert.match(textOf(done.result), /NO_SAMPLING/)
  await adapter.dispose()
})

test('elicitation round-trip incl. enumValues and structured answers', async () => {
  const client = new Client()
  const adapter = await start(client)
  await client.initialize()
  const outcome = client.call('tools/call', { name: 'ask_roundtrip', arguments: {} })
  const request = await client.serverRequest()
  assert.equal(request.method, 'elicitation/create')
  const params = request.params as { message?: string; requestedSchema?: { properties?: Record<string, Record<string, unknown>>; required?: string[] } }
  const flavor = params.requestedSchema?.properties?.['flavor']
  assert.deepEqual(flavor?.['enumValues'], ['vanilla', 'pistachio'])
  assert.deepEqual(flavor?.['enum'], ['vanilla', 'pistachio'], 'standard enum keeps other hosts honest')
  assert.deepEqual(params.requestedSchema?.required, ['flavor', 'toppings', 'note'])
  const toppings = params.requestedSchema?.properties?.['toppings']
  assert.equal(toppings?.['type'], 'string')
  client.reply(request.id, {
    action: 'accept',
    content: { flavor: 'pistachio', toppings: 'sprinkles, fudge, extra', note: 'no sugar' },
  })
  const done = await outcome
  const structured = (done.result as { structuredContent?: { answers?: Array<{ id?: string; selected?: string[]; custom?: string }> } }).structuredContent
  const answers = Object.fromEntries((structured?.answers ?? []).map(answer => [answer.id, answer]))
  assert.deepEqual(answers['flavor']?.selected, ['pistachio'])
  assert.deepEqual(answers['toppings']?.selected, ['sprinkles', 'fudge'])
  assert.equal(answers['toppings']?.custom, 'extra')
  assert.equal(answers['note']?.custom, 'no sugar')
  await adapter.dispose()
})

test('elicitation declined maps to USER_DECLINED', async () => {
  const client = new Client()
  const adapter = await start(client)
  await client.initialize()
  const outcome = client.call('tools/call', { name: 'ask_roundtrip', arguments: {} })
  const request = await client.serverRequest()
  client.reply(request.id, { action: 'decline' })
  const done = await outcome
  assert.equal((done.result as { isError?: boolean }).isError, true)
  assert.match(textOf(done.result), /USER_DECLINED/)
  await adapter.dispose()
})

test('frames are processed in arrival order across the initialize ready-gate', async () => {
  const client = new Client()
  const adapter = await start(client)
  // Write initialize + tools/list + ping back-to-back without awaiting the
  // first: the ready-gate must not let the followers overtake into -32002.
  client.send({ jsonrpc: '2.0', id: 9001, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {} } })
  client.send({ jsonrpc: '2.0', id: 9002, method: 'tools/list' })
  client.send({ jsonrpc: '2.0', id: 9003, method: 'ping' })
  const [init = {}, listed = {}, pong = {}] = await Promise.all([9001, 9002, 9003].map(id => client.awaitResponse(id)))
  assert.equal((init.result as { serverInfo?: { name?: string } }).serverInfo?.name, 'demo')
  assert.equal(listed.error, undefined, 'tools/list must not overtake initialize')
  assert.deepEqual(pong.result, {})
  await adapter.dispose()
})

test('built-in web_search surfaces with read-only open-world annotations', async () => {
  const client = new Client()
  const adapter = await start(client, {
    name: 'searchy',
    apply(ctx) {
      ctx.web.registerSearchProvider({
        id: 'fake',
        available: () => true,
        async search(request) {
          return { sources: [{ url: `https://x/${request.query}` }], truncated: false }
        },
      })
    },
  })
  await client.initialize()
  const listed = await client.call('tools/list')
  const tools = (listed.result as { tools?: Array<{ name?: string; annotations?: Record<string, boolean> }> }).tools ?? []
  const webSearch = tools.find(tool => tool.name === 'web_search')
  assert.ok(webSearch, 'web_search listed once a provider registered')
  // CLAT 的 effect_from_annotations：ro+ow → Network。
  assert.deepEqual(webSearch?.annotations, { readOnlyHint: true, openWorldHint: true })
  const result = await client.call('tools/call', { name: 'web_search', arguments: { queries: ['clat'] } })
  assert.match(textOf(result.result), /https:\/\/x\/clat/)
  await adapter.dispose()
})

test('notifications are ignored; garbage lines get a parse error', async () => {
  const client = new Client()
  const adapter = await start(client)
  await client.initialize()
  client.send({ jsonrpc: '2.0', method: 'notifications/initialized' })
  client.send({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 999 } })
  client.input.write('this is not json\n')
  const frame = await client.call('ping')
  assert.deepEqual(frame.result, {})
  await adapter.dispose()
})

test('INV-D4: stdin EOF disposes effects LIFO and settles pending requests', async () => {
  const client = new Client()
  const order: string[] = []
  const adapter = await start(client, {
    name: 'clean',
    apply(ctx: DshContext) {
      ctx.effect(function* () {
        order.push('setup-1')
        yield () => order.push('cleanup-1')
      })
      ctx.effect(function* () {
        order.push('setup-2')
        yield () => order.push('cleanup-2')
      })
    },
  })
  await client.initialize()
  client.end()
  await adapter.dispose()
  assert.deepEqual(order, ['setup-1', 'setup-2', 'cleanup-2', 'cleanup-1'])
})

test('serveClat: inject whitelist, Config validation, and static Service class lifecycle', async () => {
  const client = new Client()
  await assert.rejects(
    start(client, { name: 'x', inject: ['fs'], apply() {} }),
    (error: unknown) => {
      assert.match((error as Error).message, /inject \['fs'\]/)
      return true
    },
  )

  let seenConfig: unknown
  const validating = await start(client, {
    name: 'cfg',
    Config: (config: unknown) => ({ greeting: (config as { greeting?: string }).greeting ?? 'default' }),
    apply(_ctx, config) {
      seenConfig = config
    },
  })
  assert.deepEqual(seenConfig, { greeting: 'default' })
  await validating.dispose()

  const returnedLifecycle: string[] = []
  const cleanupClient = new Client()
  const cleanupAdapter = await start(cleanupClient, {
    name: 'returned-cleanup',
    apply() {
      returnedLifecycle.push('apply')
      return () => returnedLifecycle.push('cleanup')
    },
  })
  assert.deepEqual(returnedLifecycle, ['apply'])
  await cleanupAdapter.dispose()
  assert.deepEqual(returnedLifecycle, ['apply', 'cleanup'])

  await assert.rejects(
    start(client, {
      name: 'badcfg',
      Config: (config: unknown) => {
        const value = (config as { apiKey?: string }).apiKey
        if (typeof value !== 'string' || value === '') throw new Error('config validation failed: apiKey')
        return config
      },
      apply() {},
    }),
    /config validation failed/,
    'a throwing Config validator rejects serveClat before the server starts',
  )

  const lifecycle: string[] = []
  class ServicePlugin {
    static inject = ['tools']
    constructor(readonly ctx: DshContext) {
      ctx.reflect.provide('fixtureService', this)
    }

    *[Symbol.for('cordis.init')](): Generator<() => void> {
      lifecycle.push('init')
      this.ctx.tools.register({
        name: 'class_tool',
        description: 'registered by a Service class',
        parameters: { type: 'object', properties: {} },
        output: { render: () => [{ type: 'text', text: 'class ok' }] },
        execute: async () => ({ ok: true }),
      })
      yield () => lifecycle.push('cleanup')
    }
  }
  const classClient = new Client()
  const classAdapter = await start(classClient, ServicePlugin as unknown as DshPluginLike)
  await classClient.initialize()
  const listed = await classClient.call('tools/list')
  const classTools = (listed.result as { tools?: Array<{ name?: string }> }).tools ?? []
  assert(classTools.some(tool => tool.name === 'class_tool'))
  assert.deepEqual(lifecycle, ['init'])
  await classAdapter.dispose()
  assert.deepEqual(lifecycle, ['init', 'cleanup'])
})

test('apply failure rejects serveClat and disposes', async () => {
  const client = new Client()
  const cleaned: string[] = []
  await assert.rejects(
    serveClat(
      {
        name: 'boom',
        apply(ctx: DshContext) {
          ctx.effect(function* () {
            yield () => cleaned.push('cleanup')
          })
          throw new Error('apply exploded')
        },
      },
      { input: client.input, output: client.output },
    ),
    /apply exploded/,
  )
  await new Promise(resolve => setImmediate(resolve))
  assert.deepEqual(cleaned, ['cleanup'])
})

// W1-12：非可序列化工具结果（BigInt/循环引用）不得毒化写链——必须
// 返回结构化 tool error（isError），且 stdio 服务保持应答（不永久失声）。
test('non-serializable tool results fail as tool errors without muting the server', async () => {
  const client = new Client()
  const bigintPlugin: DshPluginLike = {
    name: 'bigint',
    apply(ctx: DshContext) {
      ctx.tools.register({
        name: 'bigint_out',
        description: 'Returns a BigInt in structuredContent.',
        parameters: { type: 'object', properties: {} },
        output: { render: () => [{ type: 'text', text: 'rendered' }] },
        execute: async () => ({ size: 10n }),
      })
      ctx.tools.register({
        name: 'circular_out',
        description: 'Returns a circular reference in structuredContent.',
        parameters: { type: 'object', properties: {} },
        output: { render: () => [{ type: 'text', text: 'rendered' }] },
        execute: async () => {
          const value: Record<string, unknown> = {}
          value['self'] = value
          return value
        },
      })
    },
  }
  const adapter = await start(client, bigintPlugin)
  try {
    await client.initialize()
    // BigInt 载荷：期待 isError 工具结果（模型可读），而非永不应答。
    const bigint = await client.call('tools/call', { name: 'bigint_out', arguments: {} })
    const bigintResult = bigint.result as { isError?: boolean } | undefined
    assert.equal(bigintResult?.isError, true, 'BigInt result must surface as a tool error')
    assert.match(textOf(bigint.result), /serializ/i)
    // 循环引用载荷：同款失败。
    const circular = await client.call('tools/call', { name: 'circular_out', arguments: {} })
    const circularResult = circular.result as { isError?: boolean } | undefined
    assert.equal(circularResult?.isError, true, 'circular result must surface as a tool error')
    // 服务未失声：后续 ping 仍应答（写链未死）。
    const ping = await client.call('ping')
    assert.equal(ping.error, undefined)
    assert.deepEqual(ping.result, {})
  } finally {
    await adapter.dispose()
  }
})

// W1-18（A3）：取消传播——notifications/cancelled 必须触发在途 tools/call
// 的 exec.signal，插件 execute 以 ABORTED 错误收束（而非挂到 EOF）。
test('notifications/cancelled aborts the in-flight tool signal and settles the call', async () => {
  const gate = { resolve: (_: unknown) => {} } as { resolve: (v: unknown) => void }
  const gatePromise = new Promise(resolve => { gate.resolve = resolve })
  let observedSignal: AbortSignal | undefined
  const cancelPlugin: DshPluginLike = {
    name: 'cancel-me',
    apply(ctx: DshContext) {
      ctx.tools.register({
        name: 'hang',
        description: 'Hangs until the signal aborts.',
        parameters: { type: 'object', properties: {} },
        output: { render: () => [{ type: 'text', text: 'unreachable' }] },
        execute: async (_args: unknown, exec: { signal: AbortSignal }) => {
          observedSignal = exec.signal
          await gatePromise
          if (exec.signal.aborted) throw new Error('ABORTED: cancelled by host')
          return { ok: true }
        },
      })
    },
  }
  const client = new Client()
  const adapter = await start(client, cancelPlugin)
  try {
    await client.initialize()
    client.send({ jsonrpc: '2.0', id: 4242, method: 'tools/call', params: { name: 'hang', arguments: {} } })
    const call = client.awaitResponse(4242)
    await new Promise(resolve => setImmediate(resolve))
    assert.ok(observedSignal, 'the tool observed its exec signal')
    client.send({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 4242 } })
    await new Promise(resolve => setTimeout(resolve, 50))
    assert.equal(observedSignal.aborted, true, 'the host cancel must abort the signal')
    gate.resolve(undefined)
    const frame = await call
    const result = frame.result as { isError?: boolean } | undefined
    assert.equal(result?.isError, true, 'the cancelled call settles as a tool error')
    assert.match(textOf(frame.result), /ABORTED|abort/i)
  } finally {
    await adapter.dispose()
  }
})

// W1-18（A3）：sampling 挂起期间 run 被取消——宿主不回包也要 settle：
// promise 随 signal 以错误收束，工具调用不悬挂。
test('a cancelled run settles a pending sampling promise with an error', async () => {
  const client = new Client()
  let releaseTool: ((v: unknown) => void) | undefined
  const toolPromise = new Promise(resolve => { releaseTool = resolve })
  const cancelPlugin: DshPluginLike = {
    name: 'cancel-sample',
    apply(ctx: DshContext) {
      ctx.tools.register({
        name: 'sample_then_hang',
        description: 'Samples, then hangs until released.',
        parameters: { type: 'object', properties: {} },
        output: { render: () => [{ type: 'text', text: 'x' }] },
        execute: async (_args: unknown, exec: { signal: AbortSignal; }) => {
          try {
            const stream = await ctx.llm.stream({ messages: [{ role: 'user', content: [{ type: 'text', text: 'hi' }] }] })
            for await (const _chunk of stream) {
              // drain
            }
            return { sampled: true }
          } catch (error) {
            if (exec.signal.aborted) throw new Error(`ABORTED: ${(error as Error).message}`)
            throw error as Error
          } finally {
            releaseTool?.(undefined)
          }
        },
      })
    },
  }
  const adapter = await start(client, cancelPlugin)
  try {
    await client.initialize()
    client.send({ jsonrpc: '2.0', id: 4243, method: 'tools/call', params: { name: 'sample_then_hang', arguments: {} } })
    const call = client.awaitResponse(4243)
    await client.serverRequest() // sampling 请求已发出——不回包
    client.send({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 4243 } })
    const frame = await Promise.race([
      call,
      new Promise<never>((_, reject) => setTimeout(() => reject(new Error('timed out: the call hung')), 1500)),
    ])
    const result = frame.result as { isError?: boolean } | undefined
    assert.equal(result?.isError, true, 'the sampling hang must settle as an error after cancel')
  } finally {
    await adapter.dispose()
  }
})

// W1-26（A4-6）：宿主以字符串回显请求 id（"101" 而非 101）时 promise
// 仍须 settle——不得静默悬挂。
test('string-echoed response ids still settle pending requests', async () => {
  const client = new Client()
  const adapter = await start(client, {
    name: 'string-id',
    apply(ctx: DshContext) {
      ctx.tools.register({
        name: 'sampler',
        description: 'Triggers one sampling request.',
        parameters: { type: 'object', properties: {} },
        output: { render: (_args: unknown, value: unknown) => [{ type: 'text', text: JSON.stringify(value) }] },
        execute: async () => {
          const stream = await ctx.llm.stream({ messages: [{ role: 'user', content: [{ type: 'text', text: 'hi' }] }] })
          const chunks: string[] = []
          for await (const chunk of stream) {
            if (chunk.type === 'text-delta') chunks.push(chunk.text)
          }
          return { text: chunks.join('') }
        },
      })
    },
  })
  try {
    await client.initialize()
    const call = client.call('tools/call', { name: 'sampler', arguments: {} })
    const request = await client.serverRequest()
    // 宿主以字符串回显 id（JSON-RPC 允许）。
    client.send({ jsonrpc: '2.0', id: String(request.id), result: { role: 'assistant', content: [{ type: 'text', text: 'pong' }], model: 'm', stopReason: 'endTurn' } })
    const outcome = await Promise.race([
      call,
      new Promise<never>((_, reject) => setTimeout(() => reject(new Error('timed out: string id did not settle')), 1500)),
    ])
    assert.equal(outcome.error, undefined, `unexpected error: ${JSON.stringify(outcome.error)}`)
    assert.match(textOf(outcome.result), /pong/)
  } finally {
    await adapter.dispose()
  }
})

// F-3（审计）：取消先于调用登记到达（目标调用仍在串行链排队）——
// 登记时对账消化，取消不丢失：该调用立即被 abort。
test('a cancel arriving before registration is reconciled when the call starts', async () => {
  let observedSignal: AbortSignal | undefined
  const started = { resolve: (_: unknown) => {} } as { resolve: (v: unknown) => void }
  const startedPromise = new Promise(resolve => { started.resolve = resolve })
  const cancelPlugin: DshPluginLike = {
    name: 'late-cancel',
    apply(ctx: DshContext) {
      ctx.tools.register({
        name: 'hang2',
        description: 'Observes the signal.',
        parameters: { type: 'object', properties: {} },
        output: { render: () => [{ type: 'text', text: 'unreachable' }] },
        execute: async (_args: unknown, exec: { signal: AbortSignal }) => {
          observedSignal = exec.signal
          started.resolve(undefined)
          await new Promise(resolve => setTimeout(resolve, 50))
          if (exec.signal.aborted) throw new Error('ABORTED: reconciled late cancel')
          return { ok: true }
        },
      })
    },
  }
  const client = new Client()
  const adapter = await start(client, cancelPlugin)
  try {
    await client.initialize()
    // 取消先到：此刻 5001 的 tools/call 还没发出/登记。
    client.send({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 5001 } })
    // 然后目标调用才到达串行链头并登记——对账即 abort。
    client.send({ jsonrpc: '2.0', id: 5001, method: 'tools/call', params: { name: 'hang2', arguments: {} } })
    const frame = await client.awaitResponse(5001)
    assert.ok(observedSignal, 'the tool observed its signal')
    assert.equal(observedSignal.aborted, true, 'the deferred cancel aborts at registration')
    const result = frame.result as { isError?: boolean } | undefined
    assert.equal(result?.isError, true, 'the call settles as an error')
    assert.match(textOf(frame.result), /ABORTED|abort/i)
    await startedPromise
  } finally {
    await adapter.dispose()
  }
})
