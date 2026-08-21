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

test('serveClat: inject whitelist, Config validation, class rejection', async () => {
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

  class ServicePlugin {
    apply(): void {}
  }
  await assert.rejects(
    start(client, ServicePlugin as unknown as DshPluginLike),
    (error: unknown) => {
      assert.match((error as Error).message, /class plugins/)
      return true
    },
  )
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
