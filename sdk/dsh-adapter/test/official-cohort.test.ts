import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import test from 'node:test'
import { runDshCli } from '../src/dsh-cli.js'
import { scanDshCompatibility } from '../src/scanner.js'

const checkout = process.env['DSH_CHECKOUT'] ?? path.resolve(process.cwd(), '../../../deepseek-harness')
const available = existsSync(path.join(checkout, 'package.json'))

test('pinned 12-package official DSH cohort keeps its semantic compatibility evidence', { skip: !available }, async () => {
  const cohort = JSON.parse(await readFile(path.join(process.cwd(), 'compat/official-cohort.json'), 'utf8')) as {
    dshRevision: string
    packages: Array<{ directory: string; name: string; status: string; unsupported: string[] }>
  }
  assert.equal(cohort.packages.length, 12)
  const matrix = await scanDshCompatibility(checkout)
  assert.equal(matrix.source.revision, cohort.dshRevision)
  for (const expected of cohort.packages) {
    const result = await runDshCli(['inspect', path.join(checkout, expected.directory)])
    assert.equal(result.code, 0, `${expected.name}: ${result.stderr ?? ''}`)
    const inspected = JSON.parse(result.stdout ?? '{}') as {
      metadata?: { name?: string }
      compatibility?: { status?: string; unsupportedSeams?: string[] }
    }
    assert.equal(inspected.metadata?.name, expected.name)
    assert.equal(inspected.compatibility?.status, expected.status, expected.name)
    assert.deepEqual(inspected.compatibility?.unsupportedSeams, expected.unsupported, expected.name)
  }
})
