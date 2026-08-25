import { checkOracle } from './check-common.mjs'
await checkOracle('gen-session-jsonl.mts', 'packages/core/session/src/chunk-rows.ts', ['export function packChunkRuns', 'export function decodeStorageRecord'])
