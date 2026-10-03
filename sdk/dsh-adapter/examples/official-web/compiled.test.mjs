import assert from 'node:assert/strict'
import test from 'node:test'
import { spawn } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { McpClient, bounded } from '../test-support/mcp-client.mjs'

test('compiled recipe runs without Node in PATH, ignores ambient files and exits on EOF', async () => {
  const binary = process.env.CLAT_OFFICIAL_WEB_BIN
  assert.ok(binary && path.isAbsolute(binary), 'set CLAT_OFFICIAL_WEB_BIN to the compiled executable')
  const cwd = await mkdtemp(path.join(os.tmpdir(), 'clat-web-executable-'))
  const env = { ...process.env, PATH: '', CLAT_PLUGIN_CONFIG: '{}' }
  for (const key of ['DEEPSEEK_API_KEY', 'DEEPSEEK_SEARCH_BASE_URL', 'DSH_WEB_SEARCH_PROVIDER', 'DSH_WEB_FETCH_PROVIDER', 'NODE_OPTIONS']) delete env[key]
  let child, client
  try {
    await writeFile(path.join(cwd, '.env'), 'DEEPSEEK_API_KEY=unapproved-fixture-key\nDEEPSEEK_SEARCH_BASE_URL=http://127.0.0.1:9\n')
    await writeFile(path.join(cwd, 'bunfig.toml'), 'invalid = [\n')
    child = spawn(binary, [], { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] })
    const exited = new Promise((resolve, reject) => {
      child.once('error', reject)
      child.once('exit', (code, signal) => resolve({ code, signal }))
    })
    // Observe startup errors even if the handshake times out first.
    void exited.catch(() => {})
    child.stderr.resume()
    client = new McpClient(child.stdin, child.stdout)
    assert.equal((await client.initialize()).error, undefined)
    assert.equal((await client.call('tools/list')).result.tools.length, 2)
    const result = await client.call('tools/call', { name: 'web_search', arguments: { queries: ['fixture'] } })
    assert.equal(result.result.isError, true)
    assert.match(result.result.content[0].text, /WEB_PROVIDER_CREDENTIAL_MISSING/)
    child.stdin.end()
    assert.deepEqual(await bounded(exited, 'compiled EOF shutdown'), { code: 0, signal: null })
  } finally {
    client?.close()
    if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
    await rm(cwd, { recursive: true, force: true })
  }
})
