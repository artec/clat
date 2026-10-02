// Pinned upstream: deepseek-harness 639ed015397290b3745d163aafe02ffee4aa3f84
// Run from its checkout:
//   ./node_modules/.bin/tsx ../clat/tests/fixtures/dsh-session/gen-v4-fixtures.mts
// The source V3 golden remains immutable; both V4 files use DSH's released
// migration, validator, physical codec, and checksum-enabled zstd framing.
import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { constants, zstdCompressSync } from 'node:zlib'

const harnessRoot = resolve(process.env.DSH_ROOT ?? join(import.meta.dirname, '..', '..', '..', '..', 'deepseek-harness'))
const importHarness = (relative: string): Promise<any> => import(pathToFileURL(join(harnessRoot, relative)).href)
const v3 = await importHarness('packages/session/session-format-v2-to-v3/src/index.ts')
const v4 = await importHarness('packages/session/session-format-v3-to-v4/src/index.ts')
const { SessionFormatEventCollector } = await importHarness('packages/session/session-format/src/index.ts')
const { SessionStore, SessionId } = await importHarness('packages/core/session/src/index.ts')
const { Context } = await importHarness('packages/core/session/node_modules/@deepseek-ai/cordis/src/index.ts')
const { createDeveloperMessage, createUserMessage, createAssistantMessage, createToolResultMessage } =
  await importHarness('packages/llm/llm/src/message.ts')
const input = join(import.meta.dirname, 'v3-migrated-0.1.5.jsonl.zstd')
readFileSync(input) // fail explicitly if the pinned source is absent
const lines = execFileSync('zstd', ['-dc', input], { maxBuffer: 16 * 1024 * 1024 })
  .toString('utf8').trimEnd().split('\n').map(line => JSON.parse(line))
const sourceHeader = v3.releasedV3SessionFormatCodec.decodeHeader(lines[0])
const sourceCollector = new SessionFormatEventCollector()
const decoder = v3.releasedV3SessionFormatCodec.createDecoder(lines[0], 'strict')
for (const line of lines.slice(1)) decoder.decodeRow(line, sourceCollector)
decoder.finish(sourceCollector)
const sourceEvents = sourceCollector.values
const targetHeader = v4.sessionFormatV3ToV4.migrateHeader(sourceHeader)
const migration = v4.createSessionFormatV3ToV4([])
const stage = migration.createStage({
  sourceHeader, targetHeader, sourceInheritedEventCount: 0, sourceKind: 'decoded',
})
const targetCollector = new SessionFormatEventCollector()
for (const event of sourceEvents) stage.transformEvent(event, targetCollector)
const inheritedEventCount = stage.finish(targetCollector)
const known = new Set([...sourceEvents.map((event: any) => event.type), 'developer/message', 'subagent/catalog'])
const migrated = { header: targetHeader, inheritedEventCount, events: targetCollector.values }
v4.restoreReleasedV4Artifact(structuredClone(migrated), known)

const checksum = { params: { [constants.ZSTD_c_checksumFlag]: 1 } }
function save(file: string, artifact: any): void {
  v4.restoreReleasedV4Artifact(structuredClone(artifact), known)
  const header = v4.releasedV4SessionFormatCodec.encodeHeader(artifact.header, artifact.inheritedEventCount)
  const rows = artifact.events.map((event: any) => v4.releasedV4SessionFormatCodec.encodeEvent(event))
  const bytes = Buffer.concat([
    zstdCompressSync(Buffer.from(JSON.stringify(header) + '\n'), checksum),
    zstdCompressSync(Buffer.from(rows.map((row: any) => JSON.stringify(row)).join('\n') + '\n'), checksum),
  ])
  writeFileSync(join(import.meta.dirname, file), bytes)
}
save('v4-migrated-0.2.0.jsonl.zstd', migrated)

// Native V4 SessionStore append path, then the same physical codec the DSH
// persistence writer calls. No migrated event is reused for this golden.
// Fix generated IDs and timestamps so regenerating the golden is byte-exact.
const fixedMessage = (message: any, id: string): any => ({ ...message, id })
const ctx = new Context()
await ctx.plugin(SessionStore)
const native = ctx.sessions.create(SessionId('018f2a64-9d3f-7cde-8123-9a4f2b6c0e04'), {
  meta: { cwd: '/Users/deng/Documents/GitHub/clat', createdAt: Date.UTC(2026, 9, 2) },
})
native.append('turn/start', { turn: 1 })
native.append('step/start', { turn: 1, step: 1 })
native.append('developer/message', {
  turn: 1, step: 1,
  message: fixedMessage(createDeveloperMessage({
    source: { kind: 'runtime-context' },
    content: [{ type: 'text', text: 'Use the repository instructions.' }],
  }), '018f2a64-9d3f-7cde-8123-9a4f2b6c0e10'),
}, { surfaceOp: 'append' })
native.append('user/message', fixedMessage(createUserMessage({
  source: { kind: 'user' }, content: [{ type: 'text', text: 'check the file' }],
}), '018f2a64-9d3f-7cde-8123-9a4f2b6c0e11'), { surfaceOp: 'append' })
native.append('assistant/message', {
  turn: 1, step: 1, stream: [],
  message: fixedMessage(createAssistantMessage({
    source: { kind: 'model', provider: 'mock', model: 'mock' },
    content: [{ type: 'tool-call', id: 'call-1', name: 'read_file', arguments: '{}' }],
  }), '018f2a64-9d3f-7cde-8123-9a4f2b6c0e12'),
}, { surfaceOp: 'append' })
native.append('tool/call', {
  turn: 1, step: 1, callId: 'call-1', name: 'read_file', arguments: '{}',
})
native.append('tool/result', {
  turn: 1, step: 1,
  message: fixedMessage(createToolResultMessage({
    callId: 'call-1', content: [{ type: 'text', text: 'ok' }], isError: false,
  }), '018f2a64-9d3f-7cde-8123-9a4f2b6c0e13'),
}, { surfaceOp: 'append' })
native.append('step/end', { turn: 1, step: 1 })
native.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
await ctx.sessions.flush(native)
const nativeHeader = { ...native.header, delegationDepth: 0 }
const nativeEvents = native.snapshotEvents().map((event: any, index: number) => ({
  ...event, time: Date.UTC(2026, 9, 2) + index,
}))
save('v4-native-0.2.0.jsonl.zstd', { header: nativeHeader, inheritedEventCount: 0, events: nativeEvents })
console.log(`V4 goldens: migrated ${migrated.events.length} events, native ${nativeEvents.length} events`)
