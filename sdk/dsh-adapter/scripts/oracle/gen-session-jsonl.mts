import { importHarness, writeOracle } from './common.mts'

const session = await importHarness('packages/core/session/src/index.ts')
const format = await importHarness('packages/session/session-persistence-jsonl/src/format.ts')
const { packChunkRuns, decodeStorageRecord, interruptedTurnClosers } = session
const { eventLines, scanLog, toHeaderLine } = format

function chunk(seq: number, text: string): any {
  return {
    type: 'assistant/chunk',
    seq,
    time: 1000 + seq * 10,
    data: { turn: 1, step: 1, chunk: { type: 'text-delta', index: 0, text } },
  }
}

const two = [chunk(0, 'a'), chunk(1, 'b')]
const three = [...two, chunk(2, 'c')]
const packedTwo = packChunkRuns(two)
const packedThree = packChunkRuns(three)
const decodedThree = packedThree.flatMap((record: unknown) => decodeStorageRecord(JSON.parse(JSON.stringify(record))))

const lone = chunk(0, '\ud800')
const loneLine = eventLines([lone], true)

const header = toHeaderLine({
  version: 0,
  id: '018f2a64-9d3f-7cde-8123-9a4f2b6c0f01',
  createdAt: 1724572800000,
  cwd: '/oracle',
  delegationDepth: 0,
})
const committed = {
  type: 'turn/start', seq: 0, time: 1, data: { turn: 1 },
}
const headerLine = `${JSON.stringify(header)}\n`
const committedLine = `${JSON.stringify(committed)}\n`
const torn = '{"type":"assistant/chunk","seq":1'
const scanned = scanLog(Buffer.from(headerLine + committedLine + torn, 'utf8'))

const openTurn = [
  { type: 'turn/start', seq: 0, time: 0, data: { turn: 1 } },
  { type: 'step/start', seq: 1, time: 1, data: { turn: 1, step: 1 } },
]
const closers = interruptedTurnClosers(openTurn)

await writeOracle('session-jsonl.json', {
  packing: {
    two: packedTwo,
    three: packedThree,
    decodedThree,
    packedAtThree: packedThree.length === 1 && packedThree[0]?.type === 'text-chunks',
  },
  loneSurrogate: {
    jsonLine: loneLine,
    utf8Hex: Buffer.from(loneLine, 'utf8').toString('hex'),
    parsedTextCodeUnits: Array.from(JSON.parse(loneLine).data.chunk.text as string, char => char.charCodeAt(0)),
  },
  tornTail: {
    eventTypes: scanned.events.map((event: any) => event.type),
    committedBytes: scanned.committedBytes,
    totalBytes: Buffer.byteLength(headerLine + committedLine + torn),
    safePrefix: headerLine + committedLine,
  },
  repair: { closers },
})
