// DV-1 B8 fixture: generate through the pinned DSH 0.1.3 SessionStore +
// JsonlSessionPersistence live write path, then read the bytes back through a
// fresh DSH read handle before copying the v2 artifact into CLAT.
//
// Run from the CLAT repository root:
//   cd ../deepseek-harness && \
//   ./node_modules/.bin/tsx ../clat/tests/fixtures/dsh-session/gen-dv1-fixture.mts

import { copyFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const harnessRoot = resolve(
  process.env.DSH_ROOT ?? join(import.meta.dirname, '..', '..', '..', '..', 'deepseek-harness'),
)

async function importHarness(relative: string): Promise<any> {
  return import(pathToFileURL(join(harnessRoot, relative)).href)
}

const sessionMod = await importHarness('packages/core/session/src/index.ts')
const jsonlMod = await importHarness('packages/session/session-persistence-jsonl/src/index.ts')
const llmMod = await importHarness('packages/llm/llm/src/index.ts')
const cordis = await importHarness(
  'packages/core/session/node_modules/@deepseek-ai/cordis/src/index.ts',
)
const formatMod = await importHarness(
  'packages/session/session-persistence-jsonl/src/format.ts',
)

const { Context } = cordis
const { default: SessionStore, SessionId } = sessionMod
const { default: JsonlSessionPersistence } = jsonlMod
const {
  AssistantStreamAccumulator,
  createAssistantMessage,
  createUserMessage,
} = llmMod
const { logPath } = formatMod

const ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0d01'
const CWD = '/Users/deng/Documents/GitHub/clat'
const out = join(import.meta.dirname, 'v2-session-0.1.3.jsonl.zstd')
const root = await mkdtemp(join(tmpdir(), 'dsh-dv1-b8-'))
const ctx = new Context()
await ctx.plugin(SessionStore)
const persistenceFiber = await ctx.plugin(JsonlSessionPersistence, {
  root,
  compression: 'zstd',
})

try {
  const session = ctx.sessions.create(SessionId(ID), {
    meta: { cwd: CWD, createdAt: Date.UTC(2026, 8, 7, 0, 0, 0) },
  })
  const handle = await ctx.sessionPersistence.create(session.header)

  session.append('turn/start', { turn: 1 })
  const sources = []
  for (const text of ['one', 'two', 'three']) {
    const message = createUserMessage({
      content: [{ type: 'text', text }],
      source: { kind: 'user' },
    })
    sources.push(session.append('user/message', message, { surfaceOp: 'append' }).seq)
  }
  session.append(
    'user/message',
    createUserMessage({
      content: [{ type: 'text', text: 'one two three' }],
      source: { kind: 'user' },
    }),
    {
      sourceEventSeqs: sources,
      surfaceOp: { op: 'replace', start: sources[0], end: sources[2] },
    },
  )
  session.append('step/start', { turn: 1, step: 1 })
  session.append('request/header', {
    header: {
      config: { provider: 'mock', model: 'mock' },
      system: 'DV-1 v2 fixture',
      tools: [],
    },
    reason: 'series',
    startsSeries: true,
  })

  const failed = new AssistantStreamAccumulator()
  failed.push({ time: 100, chunk: { type: 'text-delta', index: 0, text: 'partial' } })
  failed.push({ time: 101, chunk: { type: 'reasoning-delta', index: 1, text: 'thinking' } })
  failed.push({
    time: 102,
    chunk: {
      type: 'tool-call-delta', index: 2, id: 'call-fixture', name: 'read_file', argumentsDelta: '{',
    },
  })
  failed.push({
    time: 103,
    chunk: { type: 'finish', reason: { kind: 'error', failure: { message: 'retry', code: 'SERVER' } } },
  })
  session.append('assistant/attempt', {
    turn: 1,
    step: 1,
    stream: [...failed.snapshot()],
  })

  const settled = new AssistantStreamAccumulator()
  settled.push({ time: 110, chunk: { type: 'text-delta', index: 0, text: 'done' } })
  settled.push({ time: 111, chunk: { type: 'finish', reason: { kind: 'stop' } } })
  session.append(
    'assistant/message',
    {
      turn: 1,
      step: 1,
      stream: [...settled.snapshot()],
      message: createAssistantMessage({
        content: [{ type: 'text', text: 'done' }],
        source: { provider: 'mock', model: 'mock' },
      }),
    },
    { surfaceOp: 'append' },
  )
  session.append('step/end', { turn: 1, step: 1 })
  session.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
  await ctx.sessions.flush(session)
  await handle.close()

  // DSH read leg: decoded provenance, request/header series, attempt, and
  // all four compact AssistantStreamRecord variants must survive its reader.
  const reader = await ctx.sessionPersistence.open(SessionId(ID), 'read')
  const events = await reader.read()
  await reader.close()
  const replacement = events.find((event: any) => event.sourceEventSeqs?.length === 3)
  if (JSON.stringify(replacement?.sourceEventSeqs) !== JSON.stringify(sources)) {
    throw new Error('DSH v2 read leg did not expand the sourceEventSeqs range')
  }
  const header = events.find((event: any) => event.type === 'request/header')
  if (header?.data?.reason !== 'series' || header.data.startsSeries !== true) {
    throw new Error('DSH v2 read leg lost request/header series')
  }
  const attempt = events.find((event: any) => event.type === 'assistant/attempt')
  const kinds = new Set(attempt?.data?.stream?.map((record: any) => record.type))
  for (const kind of ['text-chunks', 'reasoning-chunks', 'tool-call-chunks', 'chunk']) {
    if (!kinds.has(kind)) throw new Error(`DSH v2 attempt lacks ${kind}`)
  }

  await copyFile(logPath(root, CWD, SessionId(ID), 'zstd'), out)
  console.log(`DSH v2 write+read fixture: ${out}`)
} finally {
  await persistenceFiber.dispose()
  await ctx.fiber.dispose()
  await rm(root, { recursive: true, force: true })
}
