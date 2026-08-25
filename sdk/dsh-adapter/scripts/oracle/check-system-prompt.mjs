import { checkOracle } from './check-common.mjs'
await checkOracle('gen-system-prompt.mts', 'packages/core/system-prompt/src/index.ts', ['export class SystemPrompt', 'async assemble(context'])
