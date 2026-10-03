import assert from 'node:assert/strict'
import test from 'node:test'
import { Socket } from 'node:net'
import dns from 'node:dns/promises'
import { serveClat } from '@artec/clat-dsh-adapter'
import { HttpFetchProvider } from '@deepseek-ai/dsh-web-fetch-http'
import * as exa from '@deepseek-ai/dsh-web-search-exa'
import { McpClient, bounded } from '../test-support/mcp-client.mjs'
import { officialWeb } from './plugin.mjs'

const envKeys = ['DEEPSEEK_API_KEY', 'DEEPSEEK_SEARCH_BASE_URL', 'DSH_WEB_SEARCH_PROVIDER', 'DSH_WEB_FETCH_PROVIDER', 'EXA_API_KEY']
async function fixture(t, check, plugin = officialWeb, config = {}) {
  const saved = Object.fromEntries(envKeys.map(key => [key, process.env[key]]))
  for (const key of envKeys) delete process.env[key]
  t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected network') })
  const client = new McpClient()
  let adapter
  try {
    adapter = await serveClat(plugin, {
      config, input: client.input, output: client.output,
      toolHints: { web_search: 'network', web_fetch: 'network' },
    })
    assert.equal((await client.initialize()).error, undefined)
    await check(client, adapter)
  } finally {
    await adapter?.dispose()
    client.close()
    for (const key of envKeys) {
      if (saved[key] === undefined) delete process.env[key]
      else process.env[key] = saved[key]
    }
  }
}

const search = client => client.call('tools/call', { name: 'web_search', arguments: { queries: ['clat'] } })
const text = frame => frame.result.content.map(block => block.text).join('\n')
const answer = () => new Response(JSON.stringify({ content: [{
  type: 'web_search_tool_result', content: [{ type: 'web_search_result', url: 'https://example.com/clat', title: 'CLAT' }],
}] }), { status: 200, headers: { 'content-type': 'application/json' } })

test('unchanged quartet mounts without network; official tools and marked guidance are exposed', async t => {
  t.mock.method(Socket.prototype, 'connect', () => { throw new Error('unexpected socket') })
  t.mock.method(dns, 'lookup', () => { throw new Error('unexpected DNS lookup') })
  await fixture(t, async client => {
    const { result } = await client.call('tools/list')
    assert.deepEqual(result.tools.map(tool => tool.name), ['web_search', 'web_fetch'])
    for (const tool of result.tools) {
      assert.equal(tool.annotations.openWorldHint, true)
      assert.equal(tool.annotations.readOnlyHint, true)
    }
    assert.deepEqual(result.tools[0].inputSchema.required, ['queries'])
    const prompts = (await client.call('prompts/list')).result.prompts
    assert.equal(prompts[0]._meta['io.artec.clat/dshSystemPrompt'], true)
    const prompt = (await client.call('prompts/get', { name: prompts[0].name, arguments: { cwd: '/fixture' } })).result
    assert.match(prompt._meta['io.artec.clat/systemPrompt'], /external, untrusted data/)
    assert.match(prompt._meta['io.artec.clat/systemPrompt'], /web_fetch returns external, untrusted/)
  })
})

test('env key reaches unchanged DeepSeek HTTP request; malformed native search never returns empty success', async t => {
  await fixture(t, async client => {
    process.env.DEEPSEEK_API_KEY = 'fixture-only-key'
    t.mock.method(globalThis, 'fetch', async (url, options) => {
      assert.equal(url, 'https://api.deepseek.com/anthropic/v1/messages')
      assert.equal(options.headers['x-api-key'], 'fixture-only-key')
      assert.equal(options.redirect, 'error')
      assert.equal(JSON.parse(options.body).tools[0].type, 'web_search_20250305')
      return answer()
    })
    const result = await search(client)
    assert.equal(result.result.isError, undefined)
    assert.equal(result.result.structuredContent.sources[0].url, 'https://example.com/clat')
    assert.match(text(result), /Cite.*markdown links/)
    t.mock.method(globalThis, 'fetch', async () => new Response('{"content":[]}'))
    const broken = await search(client)
    assert.equal(broken.result.isError, true)
    assert.match(text(broken), /no web_search_tool_result/)
  })
})

test('missing and wrong keys are actionable tool errors, never blank results', async t => {
  await fixture(t, async client => {
    const missing = await search(client)
    assert.equal(missing.result.isError, true)
    assert.match(text(missing), /WEB_PROVIDER_CREDENTIAL_MISSING.*DEEPSEEK_API_KEY/)
    process.env.DEEPSEEK_API_KEY = 'wrong-fixture-key'
    t.mock.method(globalThis, 'fetch', async () => new Response('{"error":{"message":"invalid API key"}}', { status: 401 }))
    const wrong = await search(client)
    assert.equal(wrong.result.isError, true)
    assert.match(text(wrong), /HTTP 401.*invalid API key/)
    assert.ok(!text(wrong).includes('wrong-fixture-key'))
  })
})

test('private package config key is passed through official Config', async t => {
  await fixture(t, async client => {
    t.mock.method(globalThis, 'fetch', async (_url, options) => {
      assert.equal(options.headers['x-api-key'], 'private-fixture-key')
      return answer()
    })
    assert.equal((await search(client)).result.isError, undefined)
  }, officialWeb, { apiKey: 'private-fixture-key' })
})

test('official HTTP provider blocks loopback; official fetch tool converts HTML within its cap', async t => {
  await fixture(t, async client => {
    const blocked = await client.call('tools/call', { name: 'web_fetch', arguments: { url: 'http://127.0.0.1/private' } })
    assert.equal(blocked.result.isError, true)
    assert.match(text(blocked), /WEB_BLOCKED_URL/)
    t.mock.method(HttpFetchProvider.prototype, 'fetch', async ({ url }) => ({
      url, statusCode: 200, body: { kind: 'html', content: '<h1>Fixture</h1><p>Hello</p>' }, truncated: false,
    }))
    const result = await client.call('tools/call', { name: 'web_fetch', arguments: { url: 'https://example.com/' } })
    assert.match(text(result), /# Fixture/)
    t.mock.method(HttpFetchProvider.prototype, 'fetch', async ({ url }) => ({
      url, statusCode: 200, body: { kind: 'text', content: 'x'.repeat(150_000) }, truncated: false,
    }))
    const capped = await client.call('tools/call', { name: 'web_fetch', arguments: { url: 'https://example.com/' } })
    assert.ok(text(capped).length <= 100_000)
    assert.match(text(capped), /truncated/)
  })
})

test('multiple original providers require explicit selection', async t => {
  const plugin = { apply(ctx) { officialWeb.apply(ctx); exa.apply(ctx, exa.Config({ apiKey: 'fixture-exa-key' })) } }
  await fixture(t, async client => {
    process.env.DEEPSEEK_API_KEY = 'fixture-deepseek-key'
    const ambiguous = await search(client)
    assert.equal(ambiguous.result.isError, true)
    assert.match(text(ambiguous), /WEB_PROVIDER_AMBIGUOUS/)
  }, plugin)
  await fixture(t, async client => {
    process.env.DEEPSEEK_API_KEY = 'fixture-deepseek-key'
    t.mock.method(globalThis, 'fetch', async () => answer())
    assert.equal((await search(client)).result.isError, undefined)
  }, { apply(ctx) {
    process.env.DSH_WEB_SEARCH_PROVIDER = 'deepseek-official'
    plugin.apply(ctx)
  } })
})

test('MCP cancellation aborts original provider HTTP wait and leaves server usable', async t => {
  await fixture(t, async client => {
    process.env.DEEPSEEK_API_KEY = 'fixture-only-key'
    let started
    const ready = new Promise(resolve => { started = resolve })
    let aborted = false
    t.mock.method(globalThis, 'fetch', (_url, { signal }) => new Promise((_, reject) => {
      signal.addEventListener('abort', () => { aborted = true; reject(signal.reason) }, { once: true })
      started()
    }))
    const requestId = client.nextId
    const pending = search(client)
    await bounded(ready, 'provider HTTP start')
    client.notify('notifications/cancelled', { requestId })
    const cancelled = await pending
    assert.equal(cancelled.result.isError, true)
    assert.equal(aborted, true)
    assert.equal((await client.call('tools/list')).result.tools.length, 2)
  })
})

test('failed composition revokes original web service and tool registrations', async t => {
  t.mock.method(globalThis, 'fetch', () => { throw new Error('unexpected network') })
  const client = new McpClient()
  let ctx, fallback
  try {
    await assert.rejects(serveClat({ apply(context) {
      ctx = context
      fallback = ctx.web
      officialWeb.apply(ctx)
      assert.ok(ctx.tools.get('web_search'))
      throw new Error('fixture mount failure')
    } }, { input: client.input, output: client.output }), /fixture mount failure/)
    assert.equal(ctx.get('web'), fallback)
    assert.equal(ctx.tools.get('web_search'), undefined)
    assert.equal(ctx.tools.get('web_fetch'), undefined)
  } finally {
    client.close()
  }
})
