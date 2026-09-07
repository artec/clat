import { importHarness, sha256, writeOracle } from './common.mts'

const session = await importHarness('packages/core/session/src/index.ts')
const format = await importHarness('packages/session/session-persistence-jsonl/src/format.ts')
const assistant = await importHarness('packages/llm/llm/src/assistant-stream.ts')
const { interruptedTurnClosers } = session
const { eventLines, scanLog, toHeaderLine } = format
const { AssistantStreamAccumulator, expandAssistantStream } = assistant

// Exercise every released AssistantStreamRecord variant through DSH's real
// accumulator rather than hand-authoring its storage shape.
const stream = new AssistantStreamAccumulator()
stream.push({ time: 1010, chunk: { type: 'block-start', index: 0, blockType: 'text' } })
stream.push({ time: 1020, chunk: { type: 'text-delta', index: 0, text: 'hel' } })
stream.push({ time: 1025, chunk: { type: 'text-delta', index: 0, text: 'lo' } })
stream.push({ time: 1030, chunk: { type: 'reasoning-delta', index: 1, text: 'plan' } })
stream.push({ time: 1040, chunk: { type: 'reasoning-delta', index: 1, text: ' it' } })
stream.push({ time: 1050, chunk: {
  type: 'tool-call-delta', index: 2, id: 'call-oracle', name: 'lookup', argumentsDelta: '{"q":',
} })
stream.push({ time: 1057, chunk: {
  type: 'tool-call-delta', index: 2, id: 'call-oracle', name: 'lookup', argumentsDelta: '"v2"}',
} })
stream.push({ time: 1060, chunk: { type: 'finish', reason: { kind: 'stop' } } })
const compactStream = stream.snapshot()
const expandedStream = expandAssistantStream(compactStream)

const events = [
  { type: 'turn/start', seq: 0, time: 1000, data: { turn: 1 } },
  {
    type: 'request/header', seq: 1, time: 1001,
    data: {
      header: { config: { provider: 'oracle', model: 'v2' }, system: 'oracle', tools: [] },
      reason: 'series',
    },
  },
  { type: 'step/start', seq: 2, time: 1002, data: { turn: 1, step: 1 } },
  { type: 'assistant/attempt', seq: 3, time: 1061, data: { turn: 1, step: 1, stream: compactStream } },
  {
    type: 'assistant/message', seq: 4, time: 1062,
    sourceEventSeqs: [0, 1, 2, 3],
    surfaceOp: 'append',
    data: {
      turn: 1,
      step: 1,
      message: {
        id: 'assistant-oracle', role: 'assistant',
        content: [{ type: 'text', text: 'hello' }],
        source: { kind: 'model', provider: 'oracle', model: 'v2' },
      },
      stream: compactStream,
    },
  },
  { type: 'step/end', seq: 5, time: 1063, data: { turn: 1, step: 1 } },
  { type: 'turn/end', seq: 6, time: 1064, data: { turn: 1, reason: { kind: 'completed' } } },
]

const header = toHeaderLine({
  version: 2,
  id: '018f2a64-9d3f-7cde-8123-9a4f2b6c0f01',
  createdAt: 1724572800000,
  cwd: '/oracle',
  isSeeded: false,
  delegationDepth: 0,
})
const headerLine = `${JSON.stringify(header)}\n`
const eventText = eventLines(events)
const artifact = `${headerLine}${eventText}\n`
const scannedArtifact = scanLog(Buffer.from(artifact, 'utf8'))
const storedRows = eventText.split('\n').map((line: string) => JSON.parse(line))

const lone = {
  type: 'assistant/attempt', seq: 0, time: 1000,
  data: {
    turn: 1, step: 1,
    stream: [{ type: 'text-chunks', time0: 1000, index: 0, dt: [], texts: ['\ud800'] }],
  },
}
const loneLine = eventLines([lone])

const committed = { type: 'turn/start', seq: 0, time: 1, data: { turn: 1 } }
const committedLine = `${eventLines([committed])}\n`
const torn = '{"type":"assistant/attempt","seq":1'
const scannedTorn = scanLog(Buffer.from(headerLine + committedLine + torn, 'utf8'))

const openTurn = [
  { type: 'turn/start', seq: 0, time: 0, data: { turn: 1 } },
  { type: 'step/start', seq: 1, time: 1, data: { turn: 1, step: 1 } },
]
const closers = interruptedTurnClosers(openTurn)

await writeOracle('session-jsonl.json', {
  header: {
    stored: header,
    requiredIsSeeded: Object.hasOwn(header, 'isSeeded'),
    filename: 'session.v2.jsonl.zstd',
  },
  assistantStreams: {
    compact: compactStream,
    recordTypes: compactStream.map((record: any) => record.type),
    expanded: expandedStream,
    attempt: scannedArtifact.events.find((event: any) => event.type === 'assistant/attempt'),
    message: scannedArtifact.events.find((event: any) => event.type === 'assistant/message'),
  },
  provenance: {
    stored: storedRows.find((event: any) => event.type === 'assistant/message').sourceEventSeqs,
    decoded: scannedArtifact.events.find((event: any) => event.type === 'assistant/message').sourceEventSeqs,
  },
  requestHeader: scannedArtifact.events.find((event: any) => event.type === 'request/header'),
  artifact: {
    jsonl: artifact,
    sha256: sha256(artifact),
    eventTypes: scannedArtifact.events.map((event: any) => event.type),
    committedBytes: scannedArtifact.committedBytes,
  },
  loneSurrogate: {
    jsonLine: loneLine,
    utf8Hex: Buffer.from(loneLine, 'utf8').toString('hex'),
    parsedTextCodeUnits: Array.from(
      JSON.parse(loneLine).data.stream[0].texts[0] as string,
      char => char.charCodeAt(0),
    ),
  },
  tornTail: {
    eventTypes: scannedTorn.events.map((event: any) => event.type),
    committedBytes: scannedTorn.committedBytes,
    totalBytes: Buffer.byteLength(headerLine + committedLine + torn),
    safePrefix: headerLine + committedLine,
  },
  repair: { closers },
})
