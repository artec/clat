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
    await fixture(root, 'noise', '@fixture/noise', `export function calculate(ctx) { return ctx.invariants.length + ctx.slots.length }`)
    await fixture(root, 'service', '@fixture/service', `import { Service } from 'cordis'; export class Plugin extends Service { static inject = ['fs']; run() { return this.ctx.fs.readText({}) } }`)
    const matrix = await scanDshCompatibility(root)
    assert.equal(matrix.schemaVersion, 2)
    assert.equal(matrix.counts.packages, 6)
    assert.equal(matrix.counts.portable, 1)
    assert.equal(matrix.counts['host-bridged'], 2)
    assert.equal(matrix.counts.partial, 2)
    assert.equal(matrix.counts['not-plugin'], 1)
    assert.deepEqual(
      matrix.packages.map(item => [item.name, item.status]),
      [
        ['@fixture/bridged', 'host-bridged'],
        ['@fixture/mutation', 'partial'],
        ['@fixture/noise', 'not-plugin'],
        ['@fixture/partial', 'partial'],
        ['@fixture/portable', 'portable'],
        ['@fixture/service', 'host-bridged'],
      ],
    )
    const mutation = matrix.packages.find(item => item.name === '@fixture/mutation')
    assert.deepEqual(mutation?.unsupportedSeams, ['sessions.create'])
    const noise = matrix.packages.find(item => item.name === '@fixture/noise')
    assert.deepEqual(noise?.seams, [], 'an arbitrary ctx variable is not DSH evidence')
    const service = matrix.packages.find(item => item.name === '@fixture/service')
    assert.equal(service?.analysis.serviceClasses, 1)
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
