// Pinned DSH dsh-v0.2.2-rc.1 (66491b05e3). Run from that checkout:
//   node_modules/.bin/tsx /path/to/clat/tests/fixtures/dsh-session/gen-r21-fork-fixtures.mts
// R21-C2: ① parentless seeded fork (isSeeded, no parentSession, inherited
// delivery ack naming the PARENT id — the "wrong Session" marker old V4
// readers reject); ② normal top-level control. Old goldens untouched.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { constants, zstdCompressSync } from 'node:zlib'

const pin = '66491b05e3f05ad35bcdd4f6cd7ed26041d9e3db'
const root = process.env.DSH_ROOT ?? process.cwd()
assert.equal(execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), pin)
assert.equal(execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim(), '', 'upstream must be clean')
const load = (path: string): Promise<any> => import(pathToFileURL(join(root, path)).href)
const { SessionStore, SessionId, buildForkSeed } = await load('packages/core/session/src/index.ts')
const { Context } = await load('packages/core/session/node_modules/@deepseek-ai/cordis/src/index.ts')
const { sessionFormatCatalog: codec } = await load('packages/session/session-format-catalog/src/index.ts')
const { createUserMessage } = await load('packages/llm/llm/src/message.ts')
const epoch = Date.UTC(2026, 10, 11)
const checksum = { params: { [constants.ZSTD_c_checksumFlag]: 1 } }
assert.equal(codec.currentVersion, 4)

function userMessage(id: string, text: string) {
  return {
    ...createUserMessage({
      source: { kind: 'user-rpc', principal: { kind: 'share-viewer', id: 'fixture-viewer' } },
      content: [{ type: 'text', text }],
    }),
    id,
  }
}

// 铸造 + 编码 + 发布读校验往返;固定非语义随机量(id/时间),两次重生字节一致。
function publish(name: string, sessionObj: any, inheritedEventCount: number) {
  const header = codec.encodeCurrentHeader(
    { ...sessionObj.header, delegationDepth: 0 }, inheritedEventCount,
  )
  const events = sessionObj.snapshotEvents().map((event: any, index: number) => ({ ...event, time: epoch + index }))
  const rows = events.map((event: any) => codec.encodeCurrentEvent(event))
  const reader = codec.createRestore(header, { recovery: 'strict', validation: 'current' })
  for (const row of rows) reader.decodeRow(row)
  const restored = reader.finish()
  assert.equal(restored.inheritedEventCount, inheritedEventCount)
  assert.deepEqual(restored.events, events, 'released current restore roundtrip')
  const bytes = Buffer.concat([
    zstdCompressSync(Buffer.from(JSON.stringify(header) + '\n'), checksum),
    zstdCompressSync(Buffer.from(rows.map((row: any) => JSON.stringify(row)).join('\n') + '\n'), checksum),
  ])
  const file = `${name}.jsonl.zstd`
  writeFileSync(join(import.meta.dirname, file), bytes)
  console.log(`${createHash('sha256').update(bytes).digest('hex')}  ${file} (${events.length} events, inherited ${inheritedEventCount})`)
}

// —— ② 正常顶层对照会话(也是 fork 的父) ——
const ctx = new Context()
await ctx.plugin(SessionStore)
const parentId = '018f7b21-r21d-7000-8000-000000000001'
const parent = ctx.sessions.create(SessionId(parentId), {
  meta: { cwd: '/Users/deng/Documents/GitHub/clat', createdAt: epoch },
})
parent.append('turn/start', { turn: 1 })
parent.append('step/start', { turn: 1, step: 1 })
parent.append('user/message', userMessage('018f7b21-r21d-7000-8000-00000000a001', 'parent turn before fork'), { surfaceOp: 'append' })
// 真实投递确认形状(session-log-deepseek/src/index.ts:253):名字本会话。
parent.append('session-log-deepseek/delivery-accepted', {
  sessionId: SessionId(parentId), sessionFormatVersion: 4, throughSeq: 2,
})
parent.append('step/end', { turn: 1, step: 1 })
parent.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
await ctx.sessions.flush(parent)
publish('v4-top-level-0.2.2', parent, 0)

// —— ① 无父 fork:继承含投递确认的前缀,自开 turn 2 ——
const parentEvents = parent.snapshotEvents()
const boundary = parentEvents.length - 1
const seed = buildForkSeed(parentEvents, boundary)
const inherited = boundary + 1
assert.equal(seed[inherited].type, 'session/end-seed')
assert.equal(seed[inherited].data.inherited, true)
// 继承的投递确认仍指父会话——上游类型注释:"inherited fork markers
// retain the parent's id"。这正是旧 V4 读者"names the wrong Session"
// 拒载源;CLAT 不做该校验,金样证明照常装载。
assert.equal(seed[3].type, 'session-log-deepseek/delivery-accepted')
assert.equal(seed[3].data.sessionId, parentId)
const forkId = '018f7b21-r21d-7000-8000-000000000002'
const fork = ctx.sessions.create(SessionId(forkId), {
  seed,
  meta: { cwd: '/Users/deng/Documents/GitHub/clat', createdAt: epoch + 100, isSeeded: true },
  inheritedEventCount: inherited,
})
assert.equal(fork.header.isSeeded, true)
assert.equal(fork.header.parentSession, undefined, 'parentless fork: no parentSession field')
fork.append('turn/start', { turn: 2 })
fork.append('step/start', { turn: 2, step: 1 })
fork.append('user/message', userMessage('018f7b21-r21d-7000-8000-00000000b001', 'fork continues on its own'), { surfaceOp: 'append' })
fork.append('step/end', { turn: 2, step: 1 })
fork.append('turn/end', { turn: 2, reason: { kind: 'completed' } })
await ctx.sessions.flush(fork)
publish('v4-fork-seeded-0.2.2', fork, inherited)
