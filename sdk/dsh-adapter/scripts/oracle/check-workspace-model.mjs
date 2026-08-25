import { checkOracle } from './check-common.mjs'
await checkOracle('gen-workspace-model.mts', 'packages/workspace/workspace/src/spec.ts', ['export const workspaceRecord', 'export const workspaceDomainState'])
