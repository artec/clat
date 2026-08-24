import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import test from 'node:test'
import { runDshCli } from '../src/dsh-cli.js'

async function fixture(): Promise<{ base: string; source: string; port: string; artifact: string }> {
  // Keep the fixture under this package so the generated wrapper can resolve
  // the package's self-reference without installing another adapter copy.
  const base = await mkdtemp(path.join(process.cwd(), '.tmp-clat-dsh-cli-'))
  const source = path.join(base, 'source')
  const port = path.join(base, 'port')
  const artifact = path.join(base, 'artifact')
  await mkdir(path.join(source, 'src'), { recursive: true })
  await writeFile(path.join(source, 'package.json'), JSON.stringify({
    name: '@fixture/dsh-echo',
    version: '1.2.3',
    type: 'module',
    exports: './src/index.ts',
  }), 'utf8')
  await writeFile(path.join(source, 'src', 'index.ts'), `
export default {
  apply(ctx) {
    ctx.tools.register({
      name: 'fixture_echo',
      description: 'fixture',
      parameters: { type: 'object', properties: {} },
      output: { render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }] },
      execute: async () => ({ ok: true }),
    })
  },
}
`, 'utf8')
  return { base, source, port, artifact }
}

test('clat-dsh inspect and port produce source-grounded deterministic metadata', async () => {
  const item = await fixture()
  try {
    const inspected = await runDshCli(['inspect', item.source])
    assert.equal(inspected.code, 0, inspected.stderr)
    const report = JSON.parse(inspected.stdout ?? '{}') as {
      metadata?: { name?: string }
      compatibility?: { status?: string; seams?: string[] }
    }
    assert.equal(report.metadata?.name, '@fixture/dsh-echo')
    assert.equal(report.compatibility?.status, 'portable')
    assert.deepEqual(report.compatibility?.seams, ['tools'])

    const ported = await runDshCli(['port', item.source, '--out', item.port])
    assert.equal(ported.code, 0, ported.stderr)
    const plan = JSON.parse(await readFile(path.join(item.port, 'clat-port.json'), 'utf8')) as {
      id?: string
      compatibility?: { unsupportedSeams?: string[] }
    }
    assert.equal(plan.id, 'dsh.fixture.dsh-echo')
    assert.deepEqual(plan.compatibility?.unsupportedSeams, [])
    assert.match(await readFile(path.join(item.port, 'clat.mjs'), 'utf8'), /CLAT_PLUGIN_CONFIG/)
  } finally {
    await rm(item.base, { recursive: true, force: true })
  }
})

test('clat-dsh package refuses unsupported seams before creating output', async () => {
  const item = await fixture()
  try {
    await writeFile(path.join(item.source, 'src', 'index.ts'), `
export function apply(ctx) { return ctx.subagents.create({}) }
`, 'utf8')
    assert.equal((await runDshCli(['port', item.source, '--out', item.port])).code, 0)
    const packaged = await runDshCli(['package', item.port, '--out', item.artifact])
    assert.equal(packaged.code, 1)
    assert.match(packaged.stderr ?? '', /unsupported seams/)
    assert.equal(existsSync(item.artifact), false)
  } finally {
    await rm(item.base, { recursive: true, force: true })
  }
})

test('failed forced packaging preserves the previous output directory', async () => {
  const item = await fixture()
  try {
    assert.equal((await runDshCli(['port', item.source, '--out', item.port])).code, 0)
    await mkdir(item.artifact, { recursive: true })
    await writeFile(path.join(item.artifact, 'sentinel'), 'old-output', 'utf8')
    const packaged = await runDshCli([
      'package', item.port, '--out', item.artifact, '--force',
      '--bun', 'definitely-missing-bun-for-test',
    ])
    assert.equal(packaged.code, 1)
    assert.equal(await readFile(path.join(item.artifact, 'sentinel'), 'utf8'), 'old-output')
  } finally {
    await rm(item.base, { recursive: true, force: true })
  }
})

const bunAvailable = spawnSync('bun', ['--version'], { stdio: 'ignore' }).status === 0

test('clat-dsh test and package build a standalone MCP artifact', { skip: !bunAvailable }, async () => {
  const item = await fixture()
  try {
    assert.equal((await runDshCli(['port', item.source, '--out', item.port])).code, 0)
    const tested = await runDshCli(['test', item.port])
    assert.equal(tested.code, 0, tested.stderr)
    const packaged = await runDshCli(['package', item.port, '--out', item.artifact])
    assert.equal(packaged.code, 0, packaged.stderr)
    const manifest = JSON.parse(await readFile(path.join(item.artifact, 'clat-plugin.json'), 'utf8')) as {
      id?: string
      version?: string
      runtime?: { kind?: string; sha256?: string }
      capabilities?: { tools?: boolean }
    }
    assert.equal(manifest.id, 'dsh.fixture.dsh-echo')
    assert.equal(manifest.version, '1.2.3')
    assert.equal(manifest.runtime?.kind, 'mcp-stdio')
    assert.match(manifest.runtime?.sha256 ?? '', /^[0-9a-f]{64}$/)
    assert.equal(manifest.capabilities?.tools, true)
  } finally {
    await rm(item.base, { recursive: true, force: true })
  }
})

const exaPackage = path.join(process.cwd(), 'examples/exa/node_modules/@deepseek-ai/dsh-web-search-exa')

test('published official Exa plugin ports and packages without source modification', {
  skip: !bunAvailable || !existsSync(path.join(exaPackage, 'package.json')),
}, async () => {
  const base = await mkdtemp(path.join(process.cwd(), 'examples/exa/.tmp-clat-dsh-exa-'))
  const port = path.join(base, 'port')
  const artifact = path.join(base, 'artifact')
  try {
    const ported = await runDshCli(['port', exaPackage, '--out', port])
    assert.equal(ported.code, 0, ported.stderr)
    const plan = JSON.parse(await readFile(path.join(port, 'clat-port.json'), 'utf8')) as {
      compatibility?: { status?: string; unsupportedSeams?: string[] }
    }
    assert.equal(plan.compatibility?.status, 'portable')
    assert.deepEqual(plan.compatibility?.unsupportedSeams, [])
    const packaged = await runDshCli(['package', port, '--out', artifact])
    assert.equal(packaged.code, 0, packaged.stderr)
    const manifest = JSON.parse(await readFile(path.join(artifact, 'clat-plugin.json'), 'utf8')) as {
      id?: string
      version?: string
    }
    assert.equal(manifest.id, 'dsh.deepseek-ai.dsh-web-search-exa')
    assert.equal(manifest.version, '0.0.1-rc.1')
  } finally {
    await rm(base, { recursive: true, force: true })
  }
})
