import { checkOracle } from './check-common.mjs'
await checkOracle('gen-session-jsonl.mts', 'packages/session/session-persistence-jsonl/src/format.ts', ['export function eventLines', 'export class SessionLogScanner', 'encodeSeqRanges(record.sourceEventSeqs)'])
