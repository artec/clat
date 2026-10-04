import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp, rm, readFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { builtinAttempt, engineProbe, strictOriginalAttempt, originalInventory, loweredOriginalAttempt, bundle, compile, repo } from './build.mjs'

async function temporary(fn) {
  const out = await mkdtemp(path.join(os.tmpdir(), 'clat-plg3-'))
  try { return await fn(out) } finally { await rm(out, { recursive: true, force: true }) }
}

function cargo(env) {
  return new Promise((resolve, reject) => {
    const child = spawn('cargo', ['test', '-p', 'clat-core', '--features', 'test-support',
      'plg3_component_engine_probe', '--', '--ignored', '--nocapture'], {
      cwd: repo, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'],
    })
    let output = ''
    const collect = data => { output += data; process.stdout.write(data) }
    child.stdout.on('data', collect); child.stderr.on('data', collect)
    child.on('error', reject)
    child.on('exit', code => {
      if (code !== 0) reject(new Error(`probe cargo failed (${code})`))
      else if (!output.includes('1 passed; 0 failed')) reject(new Error('probe did not execute exactly one test'))
      else resolve()
    })
  })
}

test('unaltered official quartet cannot be treated as a WIT-only component', async () => temporary(async out => {
  const before = await originalInventory()
  assert.ok(before.slice(0, 4).every(item => item.version === '0.2.0-rc.2'))
  const attempt = await strictOriginalAttempt(out)
  for (const name of ['node:async_hooks', 'node:dns/promises', 'node:net']) {
    assert.ok(attempt.diagnostics.some(item => item.text.includes(name)), name)
    assert.ok(attempt.external.includes(name), name)
  }
  assert.ok(attempt.external.includes('undici'))
  assert.match(attempt.componentFailure, /Failed to initialize component/)
  assert.deepEqual(await originalInventory(), before)
}))

test('isolated original AsyncLocalStorage import has no WIT implementation', async () => temporary(async out => {
  assert.match(await builtinAttempt(out), /node:async_hooks|AsyncLocalStorage|import/)
}))

test('lowering original property literals exposes the remaining Node dependency', async () => temporary(async out => {
  const before = await originalInventory()
  const attempt = await loweredOriginalAttempt(out)
  assert.equal(attempt.unicodeVersion, '17.0.0')
  assert.equal(attempt.literals, 3)
  assert.doesNotMatch(attempt.componentFailure, /invalid class property name/)
  assert.match(attempt.componentFailure, /Error loading module.*node:/s)
  assert.deepEqual(await originalInventory(), before)
}))

test('guest regex patterns are original; removing lowering fails at component initialization', async () => temporary(async out => {
  const original = await readFile(path.join(repo, 'sdk/dsh-adapter/examples/official-web/node_modules/@deepseek-ai/dsh-tools/lib/index.js'), 'utf8')
  for (const pattern of ['/^[\\p{XID_Start}_]\\p{XID_Continue}*$/u', '/[^\\p{XID_Continue}]+|_+/u', '/^\\p{XID_Start}/u']) {
    assert.ok(original.includes(pattern), pattern)
  }
  const graph = await bundle('engine-probe.mjs')
  await assert.rejects(compile(graph.outputFiles[0].text, out), /invalid class property name/)
}))

test('JS async component executes under existing CLAT limits and linker', async () => temporary(async out => {
  const probe = await engineProbe(out)
  // Inspect the actual emitted world, not ComponentizeJS's guest-only imports summary.
  assert.ok(probe.bytes < 32 * 1024 * 1024)
  assert.doesNotMatch(probe.wit, /wasi:|node:|undici/)
  assert.match(probe.wit, /export clat:plugin\/tools@0\.1\.0/)
  assert.equal(probe.regexLowering.literals, 3)
  await cargo({ CLAT_PLG3_COMPONENT: path.join(out, 'engine-probe.wasm') })
}))
