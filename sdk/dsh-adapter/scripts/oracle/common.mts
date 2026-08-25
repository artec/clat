import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdir, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

export const PINNED_REVISION = 'b150a551b8'
export const harnessRoot = resolve(
  process.env.DSH_ROOT ?? join(import.meta.dirname, '..', '..', '..', '..', '..', 'deepseek-harness'),
)

export function assertPinnedRevision(): string {
  const revision = execFileSync('git', ['rev-parse', 'HEAD'], {
    cwd: harnessRoot,
    encoding: 'utf8',
  }).trim()
  if (!revision.startsWith(PINNED_REVISION)) {
    throw new Error(`DSH revision ${revision} does not match pinned ${PINNED_REVISION}`)
  }
  return revision
}

export function generatedAt(revision: string): string {
  return execFileSync('git', ['show', '-s', '--format=%cI', revision], {
    cwd: harnessRoot,
    encoding: 'utf8',
  }).trim()
}

export async function importHarness(relative: string): Promise<any> {
  return import(pathToFileURL(join(harnessRoot, relative)).href)
}

export function oracleEnvelope(sections: Record<string, unknown>): Record<string, unknown> {
  const revision = assertPinnedRevision()
  return {
    pinnedRevision: PINNED_REVISION,
    pinnedCommit: revision,
    generatedAt: generatedAt(revision),
    toolVersions: { node: process.versions.node },
    sections,
  }
}

export function stableJson(value: unknown): string {
  return `${JSON.stringify(value, null, 2)}\n`
}

export async function writeOracle(defaultName: string, sections: Record<string, unknown>): Promise<void> {
  const outputArg = process.argv.indexOf('--output')
  const output = outputArg >= 0
    ? process.argv[outputArg + 1]
    : join(import.meta.dirname, '..', '..', '..', '..', 'tests', 'fixtures', 'dsh-oracle', defaultName)
  if (!output) throw new Error('--output requires a path')
  await mkdir(dirname(resolve(output)), { recursive: true })
  await writeFile(resolve(output), stableJson(oracleEnvelope(sections)), 'utf8')
}

export function errorShape(error: unknown): { name: string; message: string } {
  const value = error instanceof Error ? error : new Error(String(error))
  return { name: value.name, message: value.message }
}

export function sha256(value: string | Buffer): string {
  return createHash('sha256').update(value).digest('hex')
}
