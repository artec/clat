// SV（会话格式 V3 对齐，2026-09-13）：用钉靶 DSH checkout
//（0.1.5-rc.2 = c291e7961a，SESSION_FORMAT_VERSION = 3）产出 V3 金样：
//
//   1) v3-session-0.1.5.jsonl.zstd —— 原生 V3 会话（真实写路径）：
//      受保护 system 头（step/start 后立即 append）、request/header
//      **不含 system**、第二步提示词变化经 replace 恰好罩住头并引用
//      sourceEventSeqs=[head]、canonical 信封（startSeq/endSeq 由真实
//      codec 写出）。
//   2) v3-migrated-0.1.5.jsonl.zstd —— 上游 v2-to-v3 迁移器对 DV-3
//      oracle（0.1.3 真 V2 artifact）的迁移产物：restoreReleasedV3Artifact
//      全量校验后经 releasedV3SessionFormatCodec 编码落盘。
//
// 运行（在 clat 仓库根）：
//   cd ../deepseek-harness && \
//   ./node_modules/.bin/tsx ../clat-v3/tests/fixtures/dsh-session/gen-v3-fixtures.mts
//   （或 DSH_ROOT=… 指定 checkout；产物落本目录，随后提交进库。）

import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { zstdCompressSync, zstdDecompressSync, constants } from 'node:zlib'
import { writeFileSync } from 'node:fs'

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
const llmMod = await importHarness('packages/llm/llm/src/message.ts')
const v2v3Mod = await importHarness('packages/session/session-format-v2-to-v3/src/index.ts')
const cordis = await importHarness(
  'packages/core/session/node_modules/@deepseek-ai/cordis/src/index.ts',
)

const { Context } = cordis
const { SessionStore, SessionId } = sessionMod
const jsonlFormat = await importHarness(
  'packages/session/session-persistence-jsonl/src/format.ts',
)
const { createUserMessage, createAssistantMessage, createSystemMessage } = llmMod
const {
  releasedV3SessionFormatCodec,
  restoreReleasedV3Artifact,
  sessionFormatV2ToV3,
} = v2v3Mod

const SYSTEM_SOURCE = '@deepseek-ai/dsh-system-prompt'
const CHECKSUM = { params: { [constants.ZSTD_c_checksumFlag]: 1 } }

const outDir = import.meta.dirname
const cwd = '/Users/deng/Documents/GitHub/clat'

// rc.2 把 SessionStore 与 JsonlSessionPersistence 解耦（后者的
// create/open 句柄模型由 harness 层桥接），原生金样只需要 store 的
// append 期校验 + 快照；物理字节由真实 writer 编码函数产出。
async function withStore<T>(run: (ctx: any) => Promise<T>): Promise<T> {
  const ctx = new Context()
  await ctx.plugin(SessionStore)
  return await run(ctx)
}

// ---- fixture 1：原生 V3（受保护头 + 头替换 + canonical 信封）----

const NATIVE_ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0e01'

async function generateNativeV3(ctx: any): Promise<string> {
  const session = ctx.sessions.create(SessionId(NATIVE_ID), {
    meta: { cwd, createdAt: Date.UTC(2026, 8, 13, 8, 0, 0) },
  })
  const append = session.append.bind(session)

  // Turn 1：首 step/start 后立即保留 surface 节点 0（受保护头，镜像
  // runtime-context.ts SystemPromptProjection.project 的 head 分支）。
  append('turn/start', { turn: 1 })
  append('step/start', { turn: 1, step: 1 })
  const head = append(
    'system/message',
    {
      turn: 1,
      step: 1,
      message: createSystemMessage('Follow the repository instructions with care.', SYSTEM_SOURCE),
    },
    { surfaceOp: 'append' },
  ).seq
  append(
    'request/header',
    { header: { config: { provider: 'mock', model: 'mock' } }, reason: 'initial', startsSeries: true },
  )
  append(
    'user/message',
    createUserMessage({
      content: [{ type: 'text', text: 'summarize this workspace in one line' }],
      source: { kind: 'user' },
    }),
    { surfaceOp: 'append' },
  )
  append(
    'assistant/message',
    {
      turn: 1,
      step: 1,
      message: createAssistantMessage({
        content: [{ type: 'text', text: 'a local-first coding agent workspace' }],
        source: { provider: 'mock', model: 'mock' },
      }),
      stream: [],
    },
    { surfaceOp: 'append' },
  )
  append('step/end', { turn: 1, step: 1 })
  append('turn/end', { turn: 1, reason: { kind: 'completed' } })

  // Turn 2：提示词变化 → 替换消息恰好罩住受保护头并引用它（镜像
  // SystemPromptProjection.replace 的 intent 形状）。
  append('turn/start', { turn: 2 })
  append('step/start', { turn: 2, step: 1 })
  append(
    'system/message',
    {
      turn: 2,
      step: 1,
      message: createSystemMessage(
        'Follow the repository instructions with care. Be brief.',
        SYSTEM_SOURCE,
      ),
    },
    {
      surfaceOp: { op: 'replace', startSeq: head, endSeq: head },
      sourceEventSeqs: [head],
    },
  )
  append(
    'request/header',
    { header: { config: { provider: 'mock', model: 'mock' } }, reason: 'change' },
  )
  append(
    'user/message',
    createUserMessage({
      content: [{ type: 'text', text: 'now twice as brief' }],
      source: { kind: 'user' },
    }),
    { surfaceOp: 'append' },
  )
  append(
    'assistant/message',
    {
      turn: 2,
      step: 1,
      message: createAssistantMessage({
        content: [{ type: 'text', text: 'an agent workspace' }],
        source: { provider: 'mock', model: 'mock' },
      }),
      stream: [],
    },
    { surfaceOp: 'append' },
  )
  append('step/end', { turn: 2, step: 1 })
  append('turn/end', { turn: 2, reason: { kind: 'completed' } })
  await ctx.sessions.flush(session)

  // 读腿：SessionStore 的快照即真实写路径的最终事实（rc.2 的持久化
  // 服务 API 已改面，不再有 load；落盘字节由 CLAT 金样测试深验）。
  const events = session.snapshotEvents() as Array<{ type: string; data: any; surfaceOp: any }>
  const systemEvents = events.filter((event) => event.type === 'system/message')
  if (systemEvents.length !== 2) {
    throw new Error(
      `[native-v3-fixture] expected the protected head plus one replacement, got ${systemEvents.length}`,
    )
  }
  const headerless = events.filter(
    (event) => event.type === 'request/header' && event.data?.header?.system !== undefined,
  )
  if (headerless.length !== 0) {
    throw new Error('[native-v3-fixture] request/header must not carry system in v3')
  }
  if (events[0].type !== 'turn/start' || events[1].type !== 'step/start' || events[2].type !== 'system/message') {
    throw new Error('[native-v3-fixture] the protected head must follow the first step/start')
  }
  // 落盘：rc.2 把 store 与持久化解耦，这里直接用真实 writer 的物理
  // 编码函数（eventLines/toHeaderLine，与 JsonlSessionPersistence 写
  // 路径同源）+ DSH 原语 zstd（checksum 帧），字节级与写路径产物一致。
  // store 的逻辑头省略 delegationDepth（=0）；released 头校验要求
    // 字段在位，真实 wire writer（toHeaderLine）同样补齐后写出。
    const headerLine = JSON.stringify(
      releasedV3SessionFormatCodec.encodeHeader(
        { ...session.header, delegationDepth: 0 },
        session.inheritedEventCount ?? 0,
      ),
    )
  const bodyLines = jsonlFormat.eventLines(events)
  const headerFrame = zstdCompressSync(Buffer.from(headerLine + '\n', 'utf8'), CHECKSUM)
  const bodyFrame = zstdCompressSync(Buffer.from(bodyLines + '\n', 'utf8'), CHECKSUM)
  writeFileSync(join(outDir, 'v3-session-0.1.5.jsonl.zstd'), Buffer.concat([headerFrame, bodyFrame]))
  return '<embedded>'
}

// ---- fixture 2：上游 v2→v3 迁移器产物 ----
//
// 源 V2 artifact 在脚本内按钉靶形状铸造（方法论同上游
// migration.spec.ts 的合成源）：request/header 在开放 step 内
//（0.1.3 真实日志的头在 step/start 之前，rc.2 迁移器按规格拒绝
// ——"changed request prompt outside an open step"；该拒绝面由 CLAT
// 侧迁移判别测试另行钉住）。源覆盖：初始提示 + 变化（插入替换消息）、
// user/assistant/tool 面、compaction replace（shadowedRange 重映射 +
// 信封改名）。合成 system id 与时间锚点由迁移器确定性铸出。

const header2 = { version: 2, id: '018f2a64-9d3f-7cde-8123-9a4f2b6c0e02', createdAt: 42, isSeeded: false, delegationDepth: 0 }
const request2 = (system?: string) => ({
  header: { config: { provider: 'mock', model: 'mock' }, ...(system === undefined ? {} : { system }) },
  reason: 'initial',
})
const user2 = (id: string) => ({ id, role: 'user', source: { kind: 'user' }, content: [{ type: 'text', text: id }] })
const assistant2 = (turn: number, stepNo: number, id: string, content: any[]) => ({
  turn,
  step: stepNo,
  stream: [],
  message: { id, role: 'assistant', content, source: { kind: 'model', provider: 'mock', model: 'mock' } },
})
const result2 = (turn: number, stepNo: number) => ({
  turn,
  step: stepNo,
  message: {
    id: 'm-1-result',
    role: 'user',
    content: [{ type: 'tool-result', toolCallId: 'call-1', isError: false, content: [{ type: 'text', text: 'ok' }] }],
    source: { kind: 'tool', callId: 'call-1' },
  },
})
const at = (data: any, extra: any = {}) => ({ type: '', seq: 0, time: 42, data, ...extra })

const sourceEvents = [
  at({ turn: 1 }, { type: 'turn/start' }),
  at({ turn: 1, step: 1 }, { type: 'step/start' }),
  at(request2('first prompt'), { type: 'request/header' }),
  at(user2('first question'), { type: 'user/message', surfaceOp: 'append' }),
  at(assistant2(1, 1, 'm-0', [{ type: 'text', text: 'first answer' }]), { type: 'assistant/message', surfaceOp: 'append' }),
  at({ turn: 1, step: 1 }, { type: 'step/end' }),
  at({ turn: 1, step: 2 }, { type: 'step/start' }),
  at({ ...request2('second prompt'), reason: 'change' }, { type: 'request/header' }),
  at(
    assistant2(1, 2, 'm-1', [{ type: 'tool-call', id: 'call-1', name: 'search', arguments: '{"q":"x"}' }]),
    { type: 'assistant/message', surfaceOp: 'append' },
  ),
  at({ turn: 1, step: 2, callId: 'call-1', name: 'search', arguments: '{"q":"x"}' }, { type: 'tool/call' }),
  at(result2(1, 2), { type: 'tool/result', surfaceOp: 'append' }),
  at(assistant2(1, 2, 'm-2', [{ type: 'text', text: 'final answer' }]), { type: 'assistant/message', surfaceOp: 'append' }),
  at({ turn: 1, step: 2 }, { type: 'step/end' }),
  at({ compactionId: 'c-1', turn: 1 }, { type: 'compaction/start' }),
  // 压缩四连（DSH 真实顺序，region.ts commitCompactionBody）：
  // log-only summary 记录（shadowedRange/shadowedSeqs 引用源 seq）→
  // summary user/message 承载 surface replace 并引用
  // [start, summary, ...shadowed] → end。
  at(
    {
      compactionId: 'c-1',
      summary: [{ type: 'text', text: 'earlier context' }],
      shadowedRange: { start: 3, end: 4 },
      shadowedSeqs: [3, 4],
      shadowedTokenCount: 9,
      provider: 'mock',
      model: 'mock',
    },
    { type: 'compaction/summary' },
  ),
  {
    type: 'user/message',
    seq: 0,
    time: 42,
    data: { ...user2('[compacted earlier context]'), source: { kind: 'plugin', plugin: 'compaction' } },
    surfaceOp: { op: 'replace', start: 3, end: 4 },
    sourceEventSeqs: [12, 13, 3, 4],
  },
  at({ compactionId: 'c-1', turn: 1 }, { type: 'compaction/end' }),
  at({ turn: 1, reason: { kind: 'completed' } }, { type: 'turn/end' }),
].map((event, seq) => ({ ...event, seq }))

const formatMod = await importHarness('packages/session/session-format/src/index.ts')
const { SessionFormatEventCollector } = formatMod
const targetHeader = sessionFormatV2ToV3.migrateHeader(header2)
const stage = sessionFormatV2ToV3.createStage({
  sourceHeader: header2,
  targetHeader,
  sourceInheritedEventCount: 0,
  sourceKind: 'decoded',
})
const collector = new SessionFormatEventCollector()
for (const event of sourceEvents) stage.transformEvent(event, collector)
const inheritedEventCount = stage.finish(collector)
const collected = collector.values
const artifact = {
  header: targetHeader,
  inheritedEventCount,
  events: collected,
}
// 上游全量 current 校验（受保护头、开放 step 归属、PTC 关系、端点覆盖）。
restoreReleasedV3Artifact(structuredClone(artifact), new Set())

const headerValue = releasedV3SessionFormatCodec.encodeHeader(targetHeader, inheritedEventCount)
const eventValues = collected.map((event) => releasedV3SessionFormatCodec.encodeEvent(event))
// 自证：同一 codec 解码自己的编码（物理 framing + canonical 事件全通过）。
const decoded = releasedV3SessionFormatCodec.createDecoder(headerValue, 'strict')
for (const row of eventValues) {
  decoded.decodeRow(row, {
    emitRun() {},
    emitEvent() {},
  })
}
decoded.finish({ emitRun() {}, emitEvent() {} })

const MIGRATED_ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0e02'
const headerFrame = zstdCompressSync(
  Buffer.from(JSON.stringify(headerValue) + '\n', 'utf8'),
  CHECKSUM,
)
const bodyFrame = zstdCompressSync(
  Buffer.from(eventValues.map((row) => JSON.stringify(row)).join('\n') + '\n', 'utf8'),
  CHECKSUM,
)
writeFileSync(join(outDir, 'v3-migrated-0.1.5.jsonl.zstd'), Buffer.concat([headerFrame, bodyFrame]))
console.log('[migrated-v3-fixture] events', eventValues.length, 'inherited', inheritedEventCount)

// ---- main ----

await withStore(generateNativeV3)
console.log('[sv3] generated v3-session-0.1.5.jsonl.zstd and v3-migrated-0.1.5.jsonl.zstd')
