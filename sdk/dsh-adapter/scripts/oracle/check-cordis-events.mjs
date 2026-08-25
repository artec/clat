import { checkOracle } from './check-common.mjs'
await checkOracle('gen-cordis-events.mts', 'vendor/cordis/src/events.ts', ['export function isBailed', 'waterfall(...args: any[])'])
