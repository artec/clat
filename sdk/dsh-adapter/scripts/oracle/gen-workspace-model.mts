import { mkdtemp, mkdir, rm, symlink } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { errorShape, importHarness, writeOracle } from './common.mts'

const workspace = await importHarness('packages/workspace/workspace/src/index.ts')
const { workspaceRecord, workspaceDomainState, workspaceDomainSpec, realpathNormalize } = workspace

const recordInput = {
  path: '/oracle/project',
  title: 'Oracle',
  sessionIds: ['018f2a64-9d3f-7cde-8123-9a4f2b6c0f01'],
  createdAt: '2026-08-25T00:00:00.000Z',
  updatedAt: '2026-08-25T00:00:01.000Z',
}
const parsedRecord = workspaceRecord.parse(recordInput)
const defaultedState = workspaceDomainState.parse({ initialized: true, workspaceIds: ['workspace-1'] })
const pendingState = workspaceDomainState.parse({
  initialized: true,
  workspaceIds: ['workspace-1'],
  archivedSessionIds: ['018f2a64-9d3f-7cde-8123-9a4f2b6c0f01'],
  pendingMutation: { operation: 'delete', workspaceId: 'workspace-1' },
})
let invalidRecord: unknown
try {
  workspaceRecord.parse({ ...recordInput, sessionIds: 'not-an-array' })
} catch (error) {
  invalidRecord = { name: errorShape(error).name, rejected: true }
}

const root = await mkdtemp(join(tmpdir(), 'dsh-workspace-oracle-'))
let pathFacts: Record<string, unknown>
try {
  const target = join(root, 'target')
  const nested = join(target, 'nested')
  const alias = join(root, 'alias')
  await mkdir(nested, { recursive: true })
  await symlink(target, alias, 'dir')
  const canonical = await realpathNormalize(target)
  const dotdot = await realpathNormalize(join(nested, '..'))
  const aliasCanonical = await realpathNormalize(alias)
  let missing: unknown
  try {
    await realpathNormalize(join(root, 'missing'))
  } catch (error) {
    missing = { code: (error as NodeJS.ErrnoException).code, rejected: true }
  }
  pathFacts = {
    dotdotEqualsCanonical: dotdot === canonical,
    symlinkEqualsCanonical: aliasCanonical === canonical,
    missing,
  }
} finally {
  await rm(root, { recursive: true, force: true })
}

await writeOracle('workspace-model.json', {
  domain: { name: workspaceDomainSpec.name, version: workspaceDomainSpec.version },
  record: parsedRecord,
  stateDefaulting: defaultedState,
  pendingMutation: pendingState,
  invalidRecord,
  pathIdentity: pathFacts,
})
