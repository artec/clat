import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { scanDshCompatibility } from '../src/scanner.js'

async function fixture(root: string, directory: string, name: string, source: string): Promise<void> {
  const packageRoot = path.join(root, directory)
  await mkdir(path.join(packageRoot, 'src'), { recursive: true })
  await writeFile(path.join(packageRoot, 'package.json'), JSON.stringify({ name }), 'utf8')
  await writeFile(path.join(packageRoot, 'src', 'index.ts'), source, 'utf8')
}

test('scanner emits deterministic seam classifications and ignores test-only usage', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'clat-dsh-scan-'))
  try {
    await fixture(root, 'portable', '@fixture/portable', `export function apply(ctx) { ctx.tools.register({}) }`)
    await fixture(root, 'bridged', '@fixture/bridged', `export const inject = ['fs', 'shell']; export function apply(ctx) { return ctx.fs.resolve('.') }`)
    await fixture(root, 'partial', '@fixture/partial', `export const inject = ['tools', 'subagents']; export function apply(ctx) { ctx.tools; ctx.subagents }`)
    await fixture(root, 'mutation', '@fixture/mutation', `export const inject = ['sessions']; export function apply(ctx) { return ctx.sessions.create() }`)
    const matrix = await scanDshCompatibility(root)
    assert.equal(matrix.schemaVersion, 1)
    assert.equal(matrix.counts.packages, 4)
    assert.equal(matrix.counts.portable, 1)
    assert.equal(matrix.counts['host-bridged'], 1)
    assert.equal(matrix.counts.partial, 2)
    assert.deepEqual(
      matrix.packages.map(item => [item.name, item.status]),
      [
        ['@fixture/bridged', 'host-bridged'],
        ['@fixture/mutation', 'partial'],
        ['@fixture/partial', 'partial'],
        ['@fixture/portable', 'portable'],
      ],
    )
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
