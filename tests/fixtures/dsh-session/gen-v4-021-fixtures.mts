// Pinned DSH dsh-v0.2.1-rc.2. Run from that checkout:
// node --import=tsx ../clat/tests/fixtures/dsh-session/gen-v4-021-fixtures.mts
// Native SessionStore append, released codec and current restore; old goldens untouched.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { constants, zstdCompressSync } from 'node:zlib'

const pin = '687f282e8625c6b8d11ff58211a90524be2e4317'
const root = process.env.DSH_ROOT ?? process.cwd()
assert.equal(execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), pin)
assert.equal(execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim(), '', 'upstream must be clean')
const load = (path: string): Promise<any> => import(pathToFileURL(join(root, path)).href)
const { SessionStore, SessionId, appendPluginRecord } = await load('packages/core/session/src/index.ts')
const { Context } = await load('packages/core/session/node_modules/@deepseek-ai/cordis/src/index.ts')
const { sessionFormatCatalog: codec } = await load('packages/session/session-format-catalog/src/index.ts')
const { createUserMessage } = await load('packages/llm/llm/src/message.ts')
const epoch = Date.UTC(2026, 9, 21)
const checksum = { params: { [constants.ZSTD_c_checksumFlag]: 1 } }
assert.equal(codec.currentVersion, 4)

for (const settled of [false, true]) {
  const ctx = new Context()
  await ctx.plugin(SessionStore)
  const id = `018f2a64-9d3f-7cde-8123-9a4f2b6c0f0${settled ? 2 : 1}`
  const session = ctx.sessions.create(SessionId(id), {
    meta: { cwd: '/Users/deng/Documents/GitHub/clat', createdAt: epoch },
  })
  session.append('working-directory/change', { cwd: '/Users/deng/Documents/GitHub/clat/work' })
  session.append('turn/start', { turn: 1 })
  session.append('step/start', { turn: 1, step: 1 })
  session.append('user/message', { ...createUserMessage({
    source: { kind: 'user-rpc', principal: { kind: 'share-viewer', id: 'fixture-viewer' } },
    content: [{ type: 'text', text: 'observe the job' }],
  }), id: '018f2a64-9d3f-7cde-8123-9a4f2b6c0f10' }, { surfaceOp: 'append' })
  const registration = {
    jobId: 'bash-fixture', owner: id, kind: 'bash', label: 'fixture command', startedAt: epoch,
    toolCall: { callId: 'nested-call', rootCallId: 'root-call' },
  }
  session.append('job/registered', registration)
  if (settled) session.append('job/settled', {
    ...registration, finishedAt: epoch + 10, status: 'completed', cause: 'producer',
    producerReported: true, detail: 'exit code: 0',
  })
  // Experimental records are opaque, not new required vocabulary seats.
  appendPluginRecord(session, 'plugin:r20/probe', { arbitrary: [null, { future: true }] })
  session.append('step/end', { turn: 1, step: 1 })
  session.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
  await ctx.sessions.flush(session)
  const header = codec.encodeCurrentHeader({ ...session.header, delegationDepth: 0 }, 0)
  const events = session.snapshotEvents().map((event: any, index: number) => ({ ...event, time: epoch + index }))
  const rows = events.map((event: any) => codec.encodeCurrentEvent(event))
  const reader = codec.createRestore(header, { recovery: 'strict', validation: 'current' })
  for (const row of rows) reader.decodeRow(row)
  assert.deepEqual(reader.finish().events, events, 'released current restore roundtrip')
  const bytes = Buffer.concat([
    zstdCompressSync(Buffer.from(JSON.stringify(header) + '\n'), checksum),
    zstdCompressSync(Buffer.from(rows.map((row: any) => JSON.stringify(row)).join('\n') + '\n'), checksum),
  ])
  const file = `v4-${settled ? 'settled' : 'registered'}-0.2.1.jsonl.zstd`
  writeFileSync(join(import.meta.dirname, file), bytes)
  console.log(`${createHash('sha256').update(bytes).digest('hex')}  ${file} (${events.length} events)`)
}
