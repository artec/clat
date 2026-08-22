// B8（F-2 闭环，2026-08-22）：用钉靶 DSH checkout（0.1.1-rc.2 =
// b150a551b8）的**真实写路径**产出两类 golden fixture：
//
//   1) interrupted-session.jsonl.zstd —— 流中取消的会话：
//      assistant/message 携带 `interrupted: true` 的部分产出前缀定稿
//      （事件形状镜像 agent-loop src/agent.ts:352-368 的取消分支）；
//   2) team-events-session.jsonl.zstd —— 含 4 个 `team/*` 已知类型事件
//      （必需信封、无 ignorable）的正常会话。
//
// 运行（在 clat 仓库根）：
//   cd ../deepseek-harness && \
//   ./node_modules/.bin/tsx ../clat/tests/fixtures/dsh-session/gen-dsh-fixtures.ts
//   （或 DSH_ROOT=… 指定 checkout；产物落本目录，随后提交进库。）
//
// 第二阶段（DSH 读腿）：CLAT 自产的 interrupted 会话日志
//（`CLAT_CLAT_LOG` 指向，由 clat 的门控测试
// `interrupted_session_log_is_written_for_dsh_cross_reading` 写出）经
// DSH 真实读取器（JsonlSessionPersistence.load）接受并语义一致——
// 互证方向与 2026-08-18 原语级互证相同，这次走完整 load 路径。

import { copyFile, mkdir, mkdtemp, readFile, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const harnessRoot = resolve(
  process.env.DSH_ROOT ?? join(import.meta.dirname, '..', '..', '..', '..', 'deepseek-harness'),
)

async function importHarness(relative: string): Promise<any> {
  const url = pathToFileURL(join(harnessRoot, relative)).href
  return import(url)
}

// 真实源码（钉靶 rc.2）。cordis 是 harness 的 workspace 包——经
// session 包的 node_modules 符号链接取其源入口（lib 无 js 产物）。
const sessionMod = await importHarness('packages/core/session/src/index.ts')
const jsonlMod = await importHarness('packages/session/session-persistence-jsonl/src/index.ts')
const llmMod = await importHarness('packages/llm/llm/src/message.ts')
const cordis = await importHarness(
  'packages/core/session/node_modules/@deepseek-ai/cordis/src/index.ts',
)

const { Context } = cordis
const { SessionStore, SessionId } = sessionMod
const { JsonlSessionPersistence } = jsonlMod
const { logPath } = await importHarness(
  'packages/session/session-persistence-jsonl/src/format.ts',
)
const { createUserMessage, createAssistantMessage } = llmMod

const outDir = import.meta.dirname
const cwd = '/Users/deng/Documents/GitHub/clat'

async function withPersistence<T>(
  run: (ctx: any, root: string) => Promise<T>,
): Promise<{ root: string; value: T }> {
  const root = await mkdtemp(join(tmpdir(), 'dsh-b8-gen-'))
  const ctx = new Context()
  await ctx.plugin(SessionStore)
  const fiber = await ctx.plugin(JsonlSessionPersistence, {
    root,
    compression: 'zstd',
    writeBatchMaxDelayMs: 1,
  })
  const value = await run(ctx, root)
  await fiber.dispose()
  return { root, value }
}

// ---- fixture 1：流中取消（interrupted 前缀定稿）----

const INTERRUPTED_ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0b01'

async function generateInterrupted(ctx: any, root: string): Promise<string> {
  const session = ctx.sessions.create(SessionId(INTERRUPTED_ID), {
    meta: { cwd, createdAt: Date.UTC(2026, 7, 22, 8, 0, 0) },
  })
  session.append('turn/start', { turn: 1 })
  session.append(
    'user/message',
    createUserMessage({
      content: [{ type: 'text', text: 'stream an answer, then get cancelled mid-stream' }],
      source: { kind: 'user' },
    }),
    { surfaceOp: 'append' },
  )
  session.append('step/start', { turn: 1, step: 1 })
  const chunkSeqs: number[] = []
  for (const text of ['partial ', 'answer before ']) {
    const appended = session.append(
      'assistant/chunk',
      { turn: 1, step: 1, chunk: { type: 'text', text } },
    )
    chunkSeqs.push(appended.seq)
  }
  // 取消分支（agent.ts:352-368）：部分产出定稿为带 interrupted 的
  // assistant/message，未派发的 tool calls 不出现。
  session.append(
    'assistant/message',
    {
      turn: 1,
      step: 1,
      message: createAssistantMessage({
        content: [{ type: 'text', text: 'partial answer before ' }],
        source: { provider: 'mock', model: 'mock' },
      }),
      interrupted: true,
    },
    { surfaceOp: 'append', sourceEventSeqs: chunkSeqs },
  )
  session.append('turn/end', { turn: 1, reason: { kind: 'aborted', reason: { kind: 'user' } } })
  await ctx.sessions.flush(session)
  return logPath(root, cwd, SessionId(INTERRUPTED_ID), 'zstd')
}

// ---- fixture 2：team/* 已知类型（必需信封、无 ignorable）----

const TEAM_ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0b02'

async function generateTeamEvents(ctx: any, root: string): Promise<string> {
  const session = ctx.sessions.create(SessionId(TEAM_ID), {
    meta: { cwd, createdAt: Date.UTC(2026, 7, 22, 8, 1, 0) },
  })
  session.append('turn/start', { turn: 1 })
  session.append(
    'user/message',
    createUserMessage({
      content: [{ type: 'text', text: 'delegate some work to teammates' }],
      source: { kind: 'user' },
    }),
    { surfaceOp: 'append' },
  )
  session.append('step/start', { turn: 1, step: 1 })
  // 4 个 team/* 已知类型：信封必需、无 ignorable；payload 取最小可信
  // 形状（钉靶 DSH 尚无真实生产者——known-event-types 只是读取器前向
  // 兼容名单，CLAT 侧 B3 已钉住「拒读→放行」语义，本 fixture 补的是
  // 端到端字节与完整 load 路径）。
  session.append('team/member', { turn: 1, memberId: 'member-1', role: 'worker' })
  session.append('team/message/queued', { turn: 1, memberId: 'member-1', text: 'please sum the column' })
  session.append('team/message/delivered', { turn: 1, memberId: 'member-1', receipt: 'ok' })
  session.append('team/task', { turn: 1, taskId: 'task-1', status: 'completed' })
  session.append(
    'assistant/message',
    {
      turn: 1,
      step: 1,
      message: createAssistantMessage({
        content: [{ type: 'text', text: 'delegated and collected' }],
        source: { provider: 'mock', model: 'mock' },
      }),
    },
    { surfaceOp: 'append' },
  )
  session.append('step/end', { turn: 1, step: 1 })
  session.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
  await ctx.sessions.flush(session)
  return logPath(root, cwd, SessionId(TEAM_ID), 'zstd')
}

// ---- 第二阶段：DSH 读腿（CLAT 自产 interrupted 日志）----

async function crossReadClatLog(): Promise<void> {
  const clatLog = process.env.CLAT_CLAT_LOG
  if (!clatLog) {
    console.log('[dsh-read-leg] CLAT_CLAT_LOG 未设置——跳过（先跑 clat 的门控测试写出产物）')
    return
  }
  const info = await stat(clatLog).then(() => true, () => false)
  if (!info) {
    console.log(`[dsh-read-leg] ${clatLog} 不存在——跳过`)
    return
  }
  // CLAT 门载测试写日志时用的 id/cwd（见该测试注释）。
  const CLAT_ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0c01'
  const { root, value } = await withPersistence(async (ctx: any, root: string) => {
    const target = logPath(root, cwd, SessionId(CLAT_ID), 'zstd')
    await mkdir(dirname(target), { recursive: true })
    await copyFile(clatLog, target)
    const loaded = await ctx.sessionPersistence.load(SessionId(CLAT_ID))
    return loaded as { events: Array<{ type: string; data: any }> }
  })
  await rm(root, { recursive: true, force: true })
  const interrupted = value.events.find(
    (event) => event.type === 'assistant/message' && event.data?.interrupted === true,
  )
  if (!interrupted) {
    throw new Error('[dsh-read-leg] FAIL：DSH 读取器未在 CLAT 日志中找到 interrupted 前缀定稿')
  }
  const text = interrupted.data.message?.content?.[0]?.text
  console.log(`[dsh-read-leg] PASS：CLAT 自产 interrupted 日志经 DSH JsonlSessionPersistence.load 接受；`)
  console.log(`[dsh-read-leg]        assistant/message.interrupted=true 存在，前缀文本 ${JSON.stringify(text)}`)
  console.log(`[dsh-read-leg]        事件总数 ${value.events.length}（含 header 种子与 seed 标记）`)
}

// ---- main ----

const interrupted = await withPersistence(generateInterrupted)
const team = await withPersistence(generateTeamEvents)
await copyFile(interrupted.value, join(outDir, 'interrupted-session.jsonl.zstd'))
await copyFile(team.value, join(outDir, 'team-events-session.jsonl.zstd'))
await rm(interrupted.root, { recursive: true, force: true })
await rm(team.root, { recursive: true, force: true })
console.log('fixtures written:', join(outDir, 'interrupted-session.jsonl.zstd'), join(outDir, 'team-events-session.jsonl.zstd'))

await crossReadClatLog()
