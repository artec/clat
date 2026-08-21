/** Shim unit tests — invariants INV-D1/D3/D5/D6 (docs/todo/dsh-adapter.md §3). */

import assert from 'node:assert/strict'
import test from 'node:test'
import { AdapterError } from '../src/errors.js'
import { Shim } from '../src/shim.js'
import type { HostChannel, SamplingParams, ElicitationParams } from '../src/shim.js'
import type { ToolDefinitionLike } from '../src/types.js'

interface HostScript {
  samplingResult?: unknown
  samplingError?: { code: number; message: string }
  elicitationResult?: unknown
  capabilities?: { sampling: boolean; elicitation: boolean }
  samplingParams?: SamplingParams[]
  elicitationParams?: ElicitationParams[]
}

function fakeHost(script: HostScript = {}): HostChannel & { params: { sampling: SamplingParams[]; elicitation: ElicitationParams[] } } {
  const params: { sampling: SamplingParams[]; elicitation: ElicitationParams[] } = { sampling: [], elicitation: [] }
  const channel: HostChannel & { params: typeof params } = {
    capabilities: script.capabilities ?? { sampling: true, elicitation: true },
    async sampling(p) {
      params.sampling.push(p)
      if (script.samplingError !== undefined) {
        const e = new Error(script.samplingError.message) as Error & { code: number }
        e.code = script.samplingError.code
        throw e
      }
      return script.samplingResult ?? { content: { type: 'text', text: 'ok' }, stopReason: 'endTurn' }
    },
    async elicitation(p) {
      params.elicitation.push(p)
      return script.elicitationResult ?? { action: 'accept', content: {} }
    },
    log: () => {},
    params,
  }
  return channel
}

function echoTool(name = 'echo'): ToolDefinitionLike {
  return {
    name,
    description: 'echo',
    parameters: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    output: { render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }] },
    execute: async args => args,
  }
}

function collect<T>(stream: AsyncIterable<T>): Promise<T[]> {
  return (async () => {
    const chunks: T[] = []
    for await (const chunk of stream) chunks.push(chunk)
    return chunks
  })()
}

test('tools: register, list, call, and rejections', async () => {
  const host = fakeHost()
  const shim = new Shim(host, 'p')
  const ctx = shim.buildContext()
  const dispose = ctx.tools.register(echoTool())
  assert.equal(shim.listTools().length, 1)
  const outcome = await shim.callTool('echo', { text: 'hi' }, 'call-1')
  assert.equal((outcome.content[0] as unknown as { text: string }).text, '{"text":"hi"}')

  assert.throws(() => ctx.tools.register(echoTool()), /already registered/, 'duplicate tool')
  assert.throws(
    () => ctx.tools.register({ ...echoTool('raw'), parameters: { queries: { type: 'string' } } as unknown as Record<string, unknown> }),
    /defineTool/,
    'uncompiled parameters guide to defineTool',
  )
  assert.throws(() => ctx.tools.register({ ...echoTool('noexec'), execute: undefined as unknown as ToolDefinitionLike['execute'] }), /execute/)
  dispose()
  assert.equal(shim.listTools().length, 0)
})

test('INV-D3: unsupported ctx services fail loudly; get/then pass through', () => {
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  assert.equal(ctx.get('launchEnvironment'), undefined)
  assert.equal((ctx as unknown as { then?: unknown }).then, undefined)
  assert.throws(() => (ctx as unknown as { sessions: unknown }).sessions, (error: unknown) => {
    assert.ok(error instanceof AdapterError)
    assert.equal(error.code, 'SPINE_SERVICE')
    assert.match(error.message, /ctx\.sessions/)
    assert.match(error.message, /tools, llm, userQuestions/)
    return true
  })
})

test('effect: setup immediate, cleanups LIFO, per-effect disposer removes', async () => {
  const order: string[] = []
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  const disposeA = ctx.effect(function* () {
    order.push('setup-a')
    yield () => order.push('cleanup-a')
  })
  ctx.effect(function* () {
    order.push('setup-b')
    yield [() => order.push('cleanup-b1'), () => order.push('cleanup-b2')]
  })
  assert.deepEqual(order, ['setup-a', 'setup-b'], 'setup runs eagerly')
  await disposeA()
  assert.deepEqual(order, ['setup-a', 'setup-b', 'cleanup-a'], 'per-effect disposer runs only its own cleanup')
  await shim.disposeAll()
  assert.deepEqual(order, ['setup-a', 'setup-b', 'cleanup-a', 'cleanup-b1', 'cleanup-b2'], 'disposeAll pops effects LIFO; a yielded array runs in array order')
})

test('disposeAll: later registrations fail closed', async () => {
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  await shim.disposeAll()
  assert.throws(() => ctx.tools.register(echoTool()), (error: unknown) => {
    assert.ok(error instanceof AdapterError)
    assert.equal(error.code, 'DISPOSED')
    return true
  })
})

test('INV-D1: llm.stream maps to one sampling call and the dsh chunk protocol', async () => {
  const host = fakeHost({ samplingResult: { content: { type: 'text', text: 'bonjour' }, stopReason: 'endTurn' } })
  const shim = new Shim(host, 'p')
  const ctx = shim.buildContext()
  const chunks = await collect(ctx.llm.stream({
    system: 'be brief',
    messages: [
      { role: 'system', content: [{ type: 'text', text: 'extra system' }] },
      { role: 'user', content: [{ type: 'text', text: 'translate hi' }] },
    ],
    temperature: 0.2,
    maxTokens: 32,
  }))
  assert.deepEqual(chunks.map(chunk => chunk.type), ['block-start', 'text-delta', 'block-end', 'finish'])
  const finish = chunks[3]
  assert.deepEqual(finish !== undefined && finish.type === 'finish' ? finish.reason : undefined, { kind: 'stop' })
  const [params] = host.params.sampling
  assert.ok(params)
  assert.equal(params.messages.length, 1)
  assert.equal(params.messages[0]?.content.text, 'translate hi')
  assert.equal(params.systemPrompt, 'be brief\n\nextra system')
  assert.equal(params.maxTokens, 32)
  assert.equal(params.temperature, 0.2)
})

test('llm.stream: default maxTokens, block-array content, max-tokens mapping', async () => {
  const host = fakeHost({ samplingResult: { content: [{ type: 'text', text: 'a' }, { type: 'text', text: 'b' }], stopReason: 'maxTokens' } })
  const shim = new Shim(host, 'p')
  const chunks = await collect(shim.buildContext().llm.stream({
    messages: [{ role: 'user', content: [{ type: 'text', text: 'q' }] }],
  }))
  const finish = chunks[3]
  assert.deepEqual(finish !== undefined && finish.type === 'finish' ? finish.reason : undefined, { kind: 'max-tokens' })
  const block = chunks[2]
  assert.equal(block !== undefined && block.type === 'block-end' ? (block.block as unknown as { text: string }).text : undefined, 'a\nb')
  assert.equal(host.params.sampling[0]?.maxTokens, 4096, 'maxTokens defaults to 4096')
})

test('llm.stream fail-closed paths', async () => {
  const noSampling = new Shim(fakeHost({ capabilities: { sampling: false, elicitation: true } }), 'p')
  await assert.rejects(collect(noSampling.buildContext().llm.stream({ messages: [{ role: 'user', content: [{ type: 'text', text: 'q' }] }] })), {
    code: 'NO_SAMPLING',
  })

  const host = fakeHost()
  const shim = new Shim(host, 'p')
  await assert.rejects(
    collect(shim.buildContext().llm.stream({
      tools: [{ name: 'x' }],
      messages: [{ role: 'user', content: [{ type: 'text', text: 'q' }] }],
    })),
    { code: 'TOOLS_IN_SAMPLING' },
  )
  await assert.rejects(
    collect(shim.buildContext().llm.stream({
      messages: [{ role: 'user', content: [{ type: 'image', data: '…' }] }],
    })),
    { code: 'NON_TEXT_CONTENT' },
  )
})

test('ask: single-select maps to enumValues; answers reconstruct selected', async () => {
  const host = fakeHost({ elicitationResult: { action: 'accept', content: { flavor: 'pistachio' } } })
  const shim = new Shim(host, 'p')
  const answer = await shim.buildContext().userQuestions.ask({
    questions: [{
      id: 'flavor',
      question: 'Which flavor?',
      detail: 'pick one',
      options: [{ label: 'vanilla', description: 'classic' }, { label: 'pistachio' }],
    }],
  })
  assert.deepEqual(answer.answers[0]?.selected, ['pistachio'])
  const schema = host.params.elicitation[0]?.requestedSchema
  assert.ok(schema)
  const flavor = schema.properties['flavor']
  assert.ok(flavor)
  assert.deepEqual(flavor['enumValues'], ['vanilla', 'pistachio'])
  assert.deepEqual(schema.required, ['flavor'])
  assert.equal(host.params.elicitation[0]?.message, 'Which flavor?')
  assert.match(String(flavor['description']), /pick one/)
  assert.match(String(flavor['description']), /vanilla — classic/)
})

test('ask: multi-select degrades to text and parses labels back', async () => {
  const host = fakeHost({ elicitationResult: { action: 'accept', content: { toppings: 'sprinkles, FUDGE, extra note' } } })
  const shim = new Shim(host, 'p')
  const answer = await shim.buildContext().userQuestions.ask({
    questions: [{
      id: 'toppings',
      question: 'Which toppings?',
      multiSelect: true,
      options: [{ label: 'sprinkles' }, { label: 'fudge' }, { label: 'cherries' }],
    }],
  })
  const item = answer.answers[0]
  assert.deepEqual(item?.selected, ['sprinkles', 'fudge'])
  assert.equal(item?.custom, 'extra note')
  const property = host.params.elicitation[0]?.requestedSchema.properties['toppings']
  assert.equal(property?.['type'], 'string')
  assert.match(String(property?.['description']), /multi-select/)
})

test('ask: free-text question becomes custom', async () => {
  const host = fakeHost({ elicitationResult: { action: 'accept', content: { note: 'no sugar' } } })
  const shim = new Shim(host, 'p')
  const answer = await shim.buildContext().userQuestions.ask({ questions: [{ id: 'note', question: 'Note?' }] })
  const item = answer.answers[0]
  assert.deepEqual(item?.selected, [])
  assert.equal(item?.custom, 'no sugar')
})

test('ask: field ids are sanitized and de-collided', async () => {
  const host = fakeHost({ elicitationResult: { action: 'accept', content: { what_sauce: 'a', a_b: 'b', a_b_2: 'c' } } })
  const shim = new Shim(host, 'p')
  await shim.buildContext().userQuestions.ask({
    questions: [
      { id: 'what sauce?', question: 'q1' },
      { id: 'a-b', question: 'q2' },
      { id: 'a b', question: 'q3' },
    ],
  })
  assert.deepEqual(Object.keys(host.params.elicitation[0]?.requestedSchema.properties ?? {}), ['what_sauce', 'a_b', 'a_b_2'])
})

test('ask: decline/cancel and validation failures', async () => {
  const declined = new Shim(fakeHost({ elicitationResult: { action: 'decline' } }), 'p')
  await assert.rejects(declined.buildContext().userQuestions.ask({ questions: [{ id: 'x', question: 'q' }] }), { code: 'USER_DECLINED' })

  const cancelled = new Shim(fakeHost({ elicitationResult: { action: 'cancel' } }), 'p')
  await assert.rejects(cancelled.buildContext().userQuestions.ask({ questions: [{ id: 'x', question: 'q' }] }), { code: 'USER_CANCELLED' })

  const noElicitation = new Shim(fakeHost({ capabilities: { sampling: true, elicitation: false } }), 'p')
  await assert.rejects(noElicitation.buildContext().userQuestions.ask({ questions: [{ id: 'x', question: 'q' }] }), { code: 'NO_ELICITATION' })

  const shim = new Shim(fakeHost(), 'p')
  const ask = shim.buildContext().userQuestions.ask
  await assert.rejects(ask({ questions: [] }), { code: 'EMPTY_QUESTIONS' })
  await assert.rejects(ask({ questions: [{ id: 'x', question: 'q' }], agent: {} }), { code: 'AGENT_ASK_UNSUPPORTED' })
  await assert.rejects(
    ask({ questions: [{ id: 'x', question: 'q', intent: { kind: 'plan-review', approve: 'ship' }, options: [{ label: 'go' }] }] }),
    { code: 'BAD_INTENT' },
  )
  await assert.rejects(
    ask({ questions: [{ id: 'x', question: 'q', intent: { kind: 'plan-review', approve: 'go' }, options: [{ label: 'go' }] }] }),
    { code: 'BAD_INTENT' },
  )
  await assert.rejects(
    ask({ questions: Array.from({ length: 17 }, (_, i) => ({ id: `q${i}`, question: 'q' })) }),
    { code: 'TOO_MANY_QUESTIONS' },
  )
})
