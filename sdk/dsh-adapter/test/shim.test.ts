/** Shim unit tests — invariants INV-D1/D3/D5/D6 (docs/todo/dsh-adapter.md §3). */

import assert from 'node:assert/strict'
import test from 'node:test'
import { AdapterError } from '../src/errors.js'
import { Shim } from '../src/shim.js'
import type { HostChannel, SamplingParams, ElicitationParams } from '../src/shim.js'
import type { DshContext, ToolDefinitionLike } from '../src/types.js'

interface HostScript {
  samplingResult?: unknown
  samplingError?: { code: number; message: string }
  elicitationResult?: unknown
  capabilities?: { sampling: boolean; elicitation: boolean; hostServices: boolean }
  samplingParams?: SamplingParams[]
  elicitationParams?: ElicitationParams[]
}

function fakeHost(script: HostScript = {}): HostChannel & { params: { sampling: SamplingParams[]; elicitation: ElicitationParams[] } } {
  const params: { sampling: SamplingParams[]; elicitation: ElicitationParams[] } = { sampling: [], elicitation: [] }
  const channel: HostChannel & { params: typeof params } = {
    capabilities: script.capabilities ?? { sampling: true, elicitation: true, hostServices: false },
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

test('CLAT host services project context, filesystem, shell, and read-only mirrors', async () => {
  const calls: Array<[string, Record<string, unknown>]> = []
  let contextActive = true
  const context = {
    protocolVersion: '0.1.0' as const,
    project: { root: '/workspace' },
    run: {
      sessionId: 'session-1',
      provider: 'OpenAI Compatible',
      model: 'fixture',
      messages: [{ User: { content: [{ Text: 'hello' }] } }],
    },
    hostTools: ['read_file', 'list_files', 'run_command'],
  }
  const host: HostChannel = {
    capabilities: { sampling: true, elicitation: true, hostServices: true },
    sampling: async () => ({}),
    elicitation: async () => ({}),
    context: async () => {
      if (!contextActive) throw new Error('no active run')
      return context
    },
    async hostTool(name, arguments_) {
      calls.push([name, arguments_])
      if (name === 'read_file') {
        return { path: arguments_.path, content: '1 | alpha\n2 | beta\n', truncated: false }
      }
      if (name === 'list_files') {
        return { entries: [{ path: 'src', kind: 'directory' }], truncated: false }
      }
      if (name === 'run_command') {
        return { exit_code: 0, signal: null, timed_out: false, stdout: 'ok', stderr: '', stdout_truncated: false, stderr_truncated: false }
      }
      return {}
    },
    log: () => {},
  }
  const shim = new Shim(host, 'host-fixture')
  const ctx = shim.buildContext()
  shim.updateHostContext(context)

  assert.equal((await ctx.clat.context()).run.sessionId, 'session-1')
  const target = await ctx.fs.resolve('README.md')
  assert.equal(target.displayPath, '/workspace/README.md')
  assert.equal(await ctx.fs.readText(target), 'alpha\nbeta\n')
  assert.deepEqual((await ctx.fs.listDir(await ctx.fs.resolve('.'))).map(item => item.name), ['src'])
  const spec = ctx.shell.resolve({ command: 'printf ok' })
  const result = await ctx.shell.run(spec) as { stdout: { text: string } }
  assert.equal(result.stdout.text, 'ok')
  assert.equal(ctx.sessions.get('session-1')?.messages.length, 1)
  assert.equal(ctx.agents.roots()[0]?.session.id, 'session-1')
  assert.throws(() => ctx.sessions.create(), { code: 'READ_ONLY_HOST_SERVICE' })
  assert.deepEqual(calls.map(([name]) => name), ['read_file', 'list_files', 'run_command'])

  contextActive = false
  await assert.rejects(ctx.clat.context(), /no active run/)
  assert.deepEqual(ctx.sessions.list(), [])
})

test('INV-D3: unsupported ctx services fail loudly; get/then pass through', () => {
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  assert.equal(ctx.get('launchEnvironment'), undefined)
  assert.equal((ctx as unknown as { then?: unknown }).then, undefined)
  assert.throws(() => (ctx as unknown as { subagents: unknown }).subagents, (error: unknown) => {
    assert.ok(error instanceof AdapterError)
    assert.equal(error.code, 'SPINE_SERVICE')
    assert.match(error.message, /ctx\.subagents/)
    assert.match(error.message, /tools, llm, userQuestions/)
    return true
  })
})

test('ctx.inject follows host "not mounted" semantics: callback skipped, noted, plugin survives', async () => {
  // 回归锁（2026-08-21，dsh-free-search 实测）：dsh-settings 的
  // installSettingsSection 契约是"settings 服务未挂载 → 注册不生效、
  // 插件照常工作"。适配器此前不提供 ctx.inject，属性访问即抛，
  // 优雅降级路径无从执行——按契约改为跳过回调 + stderr 记录。
  const notes: string[] = []
  const host = fakeHost()
  const originalLog = host.log
  host.log = (...args: unknown[]) => {
    notes.push(args.map(String).join(' '))
    originalLog(...args)
  }
  const shim = new Shim(host, 'p')
  const ctx = shim.buildContext()
  let ran = false
  const skipped = ctx.inject(['settings'], () => {
    ran = true
  })
  assert.equal(ran, false, '未挂载服务的回调不得执行')
  assert.equal(notes.length, 1)
  assert.match(notes[0]!, /settings/)
  assert.match(notes[0]!, /skipped/)
  const skippedFiber = await skipped
  await skippedFiber.dispose()
  // 单依赖（非数组）形式同样适用
  ctx.inject('settings', () => {
    ran = true
  })
  assert.equal(ran, false)
  assert.equal(notes.length, 2)
})

test('ctx.get and ctx.inject expose mounted adapter services and own async callback effects', async () => {
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  assert.equal(ctx.get('systemPrompt'), ctx.systemPrompt)
  assert.equal(ctx.get('tools'), ctx.tools)
  let injected: DshContext | undefined
  const order: string[] = []
  const mounted = ctx.inject(['tools', 'systemPrompt'], async candidate => {
    injected = candidate
    await Promise.resolve()
    candidate.tools.register(echoTool('injected'))
    candidate.effect(() => () => order.push('inner-cleanup'))
    return () => order.push('returned-cleanup')
  })
  const fiber = await mounted
  assert.equal(injected, ctx)
  assert(shim.listTools().some(tool => tool.name === 'injected'))
  await fiber.dispose()
  assert(!shim.listTools().some(tool => tool.name === 'injected'))
  assert.deepEqual(order, ['returned-cleanup', 'inner-cleanup'])
  await shim.disposeAll()
  assert.deepEqual(order, ['returned-cleanup', 'inner-cleanup'])
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
    yield () => order.push('cleanup-b3')
  })
  assert.deepEqual(order, ['setup-a', 'setup-b'], 'setup runs eagerly')
  await disposeA()
  assert.deepEqual(order, ['setup-a', 'setup-b', 'cleanup-a'], 'per-effect disposer runs only its own cleanup')
  await shim.disposeAll()
  assert.deepEqual(order, ['setup-a', 'setup-b', 'cleanup-a', 'cleanup-b3', 'cleanup-b2', 'cleanup-b1'], 'disposeAll pops effects LIFO and every yielded cleanup runs in reverse registration order')
})

test('effect accepts direct cleanup, promised cleanup, and async generators', async () => {
  const order: string[] = []
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  ctx.effect(() => {
    order.push('direct:setup')
    return () => order.push('direct:cleanup')
  })
  ctx.effect(async () => {
    order.push('promise:setup')
    return () => order.push('promise:cleanup')
  })
  ctx.effect(async function* () {
    order.push('generator:setup')
    yield () => order.push('generator:cleanup-1')
    yield () => order.push('generator:cleanup-2')
  })
  await shim.disposeAll()
  assert.deepEqual(order, [
    'direct:setup',
    'promise:setup',
    'generator:setup',
    'generator:cleanup-2',
    'generator:cleanup-1',
    'promise:cleanup',
    'direct:cleanup',
  ])
})

test('async effect disposer is awaitable and yields a callable disposer', async () => {
  const order: string[] = []
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  const registered = ctx.effect(async function* () {
    await Promise.resolve()
    order.push('setup')
    yield () => order.push('cleanup')
  })
  const dispose = await (registered as unknown as PromiseLike<() => Promise<void>>)
  assert.deepEqual(order, ['setup'])
  await dispose()
  assert.deepEqual(order, ['setup', 'cleanup'])
  await shim.disposeAll()
  assert.deepEqual(order, ['setup', 'cleanup'])
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

test('W1-06: one failing cleanup never truncates the teardown', async () => {
  // 三个 effect（LIFO 应为 C → B → A），B 的 cleanup 抛错，C 的数组里
  // 第二个函数抛错。pre-fix 红：disposeAll 在 C 的首错处 reject，
  // cleanup-b/cleanup-a 与 #tools.clear() 全部被跳过，且 #disposed 已
  // 置 true——剩余清理永久没有重试机会。
  const order: string[] = []
  const shim = new Shim(fakeHost(), 'p')
  const ctx = shim.buildContext()
  ctx.effect(function* () {
    order.push('setup-a')
    yield () => order.push('cleanup-a')
  })
  ctx.effect(function* () {
    order.push('setup-b')
    yield () => {
      order.push('cleanup-b')
      throw new Error('b explodes')
    }
  })
  ctx.effect(function* () {
    order.push('setup-c')
    yield [
      () => order.push('cleanup-c1'),
      () => {
        throw new Error('c2 explodes')
      },
      () => order.push('cleanup-c3'),
    ]
  })
  ctx.tools.register(echoTool())

  await assert.rejects(
    () => shim.disposeAll(),
    (error: unknown) => {
      // C（数组内 1 失败 → 单错原样抛）与 B（1 失败）→ disposeAll 聚
      // 合为 CLEANUP_FAILED。
      return error instanceof AdapterError && error.code === 'CLEANUP_FAILED'
    },
  )
  assert.deepEqual(order, [
    'setup-a',
    'setup-b',
    'setup-c',
    'cleanup-c3',
    'cleanup-c1',
    'cleanup-b',
    'cleanup-a',
  ], 'every cleanup step runs LIFO despite failures')
  assert.equal(shim.listTools().length, 0, 'tools must be cleared even when cleanups fail')
  // 幂等：第二次 dispose 无新副作用、不再抛错。
  await shim.disposeAll()
  assert.deepEqual(order, [
    'setup-a',
    'setup-b',
    'setup-c',
    'cleanup-c3',
    'cleanup-c1',
    'cleanup-b',
    'cleanup-a',
  ])
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
  const noSampling = new Shim(fakeHost({ capabilities: { sampling: false, elicitation: true, hostServices: false } }), 'p')
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

  const noElicitation = new Shim(fakeHost({ capabilities: { sampling: true, elicitation: false, hostServices: false } }), 'p')
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
