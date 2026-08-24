/** Web seam + built-in web_search tests (Phase 3b; mirrors dsh-web/dsh-tool-web semantics). */

import assert from 'node:assert/strict'
import test from 'node:test'
import { Shim } from '../src/shim.js'
import type { HostChannel } from '../src/shim.js'
import type { ToolDefinitionLike, WebSearchProviderLike, WebSearchResultLike } from '../src/types.js'

const quietHost: HostChannel = {
  capabilities: { sampling: true, elicitation: true },
  sampling: async () => ({}),
  elicitation: async () => ({}),
  log: () => {},
}

function fakeProvider(id: string, pages: string[][], usable = true): WebSearchProviderLike & { calls: string[] } {
  const state = { calls: [] as string[] }
  return {
    id,
    calls: state.calls,
    available: () => usable,
    async search(request) {
      state.calls.push(request.query)
      const sources = (pages[state.calls.length - 1] ?? []).map(url => ({ url }))
      return { sources, truncated: false }
    },
  }
}

function sourceUrls(value: unknown): string[] {
  return ((value as { sources?: Array<{ url: string }> }).sources ?? []).map(source => source.url)
}

test('seam selection semantics mirror dsh-web WebRuntime', async () => {
  const shim = new Shim(quietHost, 'p')
  const ctx = shim.buildContext()

  await assert.rejects(shim.callTool('web_search', { queries: ['x'] }, 'c0'), { code: 'UNKNOWN_TOOL' },
    'no provider registered → no built-in tool at all')

  const exa = fakeProvider('exa', [['https://a']])
  ctx.web.registerSearchProvider(exa)
  assert(shim.listTools().some(tool => tool.name === 'web_search'), 'one provider → web_search listed')

  const bravo = fakeProvider('bravo', [['https://b']])
  ctx.web.registerSearchProvider(bravo)
  await assert.rejects(shim.callTool('web_search', { queries: ['x'] }, 'c1'), { code: 'WEB_PROVIDER_AMBIGUOUS' })

  process.env.DSH_WEB_SEARCH_PROVIDER = 'exa'
  try {
    const single = await shim.callTool('web_search', { queries: ['x'] }, 'c2')
    assert.deepEqual(sourceUrls(single.structuredContent), ['https://a'])
    process.env.DSH_WEB_SEARCH_PROVIDER = 'missing'
    await assert.rejects(shim.callTool('web_search', { queries: ['x'] }, 'c3'), { code: 'WEB_PROVIDER_CONFIGURED_MISSING' })
    process.env.DSH_WEB_SEARCH_PROVIDER = 'bravo'
    const bravoUnusable = fakeProvider('bravo2', [], false)
    await assert.rejects(
      (async () => {
        const shim2 = new Shim(quietHost, 'p')
        const ctx2 = shim2.buildContext()
        ctx2.web.registerSearchProvider(bravoUnusable)
        process.env.DSH_WEB_SEARCH_PROVIDER = 'bravo2'
        await shim2.callTool('web_search', { queries: ['x'] }, 'c')
      })(),
      { code: 'WEB_PROVIDER_CONFIGURED_UNAVAILABLE' },
    )
  } finally {
    delete process.env.DSH_WEB_SEARCH_PROVIDER
  }

  assert.throws(() => ctx.web.registerSearchProvider(fakeProvider('exa', [])), { code: 'WEB_DUPLICATE_PROVIDER' })
  assert.throws(
    () => ctx.web.registerSearchProvider({ ...fakeProvider('', []), id: '' }),
    { code: 'WEB_BAD_PROVIDER' },
  )
})

test('unusable-only providers report WEB_PROVIDER_UNAVAILABLE', async () => {
  const shim = new Shim(quietHost, 'p')
  shim.buildContext().web.registerSearchProvider(fakeProvider('keyless', [], false))
  await assert.rejects(shim.callTool('web_search', { queries: ['x'] }, 'c'), { code: 'WEB_PROVIDER_UNAVAILABLE' })
})

test('web_search single query keeps the provider result; maxResults truncates', async () => {
  const shim = new Shim(quietHost, 'p')
  const provider: WebSearchProviderLike = {
    id: 'p',
    available: () => true,
    async search(request) {
      const urls = ['https://1', 'https://2', 'https://3'].map(url => ({ url, title: 't' }))
      const result: WebSearchResultLike = { content: 'answer text', sources: request.maxResults === undefined ? urls : urls.slice(0, request.maxResults), truncated: false }
      return result
    },
  }
  shim.buildContext().web.registerSearchProvider(provider)
  const outcome = await shim.callTool('web_search', { queries: ['q'] }, 'c')
  assert.deepEqual(sourceUrls(outcome.structuredContent), ['https://1', 'https://2', 'https://3'])
  assert.equal((outcome.structuredContent as { content?: string }).content, 'answer text')
  const text = (outcome.content[0] as unknown as { text: string }).text
  assert.match(text, /answer text/)
  assert.match(text, /- \[t\]\(https:\/\/1\)/)
  assert.match(text, /Cite the relevant URLs/)
})

test('web_search multi-query merge: round-robin, url dedup, cap, truncated', async () => {
  const shim = new Shim(quietHost, 'p')
  const pages = new Map<string, string[]>([
    ['cats', ['https://c1', 'https://shared', 'https://c2']],
    ['dogs', ['https://d1', 'https://shared', 'https://d2']],
  ])
  const provider: WebSearchProviderLike = {
    id: 'p',
    available: () => true,
    async search(request) {
      const urls = (pages.get(request.query) ?? []).map(url => ({ url }))
      const capped = request.maxResults !== undefined ? urls.slice(0, request.maxResults) : urls
      return { sources: capped, truncated: capped.length < urls.length }
    },
  }
  shim.buildContext().web.registerSearchProvider(provider)
  // 工具固定传 maxResults=8：两问各 3 条，合并 round-robin 去重 shared。
  const outcome = await shim.callTool('web_search', { queries: ['cats', 'dogs'] }, 'c')
  assert.deepEqual(sourceUrls(outcome.structuredContent), [
    'https://c1', 'https://d1', 'https://shared', 'https://c2', 'https://d2',
  ])
  assert.equal((outcome.structuredContent as { truncated?: boolean }).truncated, false)

  // 合并层截断：单问 12 条 → 上限 8，truncated 置位。
  const many: WebSearchProviderLike = {
    id: 'many',
    available: () => true,
    async search() {
      return { sources: Array.from({ length: 12 }, (_, i) => ({ url: `https://x/${i}` })), truncated: false }
    },
  }
  const shim2 = new Shim(quietHost, 'p')
  shim2.buildContext().web.registerSearchProvider(many)
  const capped = await shim2.callTool('web_search', { queries: ['x'] }, 'c')
  assert.equal(sourceUrls(capped.structuredContent).length, 8)
  assert.equal((capped.structuredContent as { truncated?: boolean }).truncated, true)
})

test('web_search argument validation mirrors parseSearchArgs', async () => {
  const shim = new Shim(quietHost, 'p')
  shim.buildContext().web.registerSearchProvider(fakeProvider('p', [['https://a']]))
  for (const bad of [{}, { queries: [] }, { queries: ['a', ''] }, { queries: ['a', 'b', 'c', 'd', 'e'] }]) {
    await assert.rejects(shim.callTool('web_search', bad, 'c'), Error, `bad args: ${JSON.stringify(bad)}`)
  }
  // 精确重复折叠后仍合法。
  const outcome = await shim.callTool('web_search', { queries: ['a', 'a'] }, 'c')
  assert.deepEqual(sourceUrls(outcome.structuredContent), ['https://a'])
})

test('provider failure propagates as an isError tool result', async () => {
  const shim = new Shim(quietHost, 'p')
  const provider: WebSearchProviderLike = {
    id: 'p',
    available: () => true,
    async search() {
      throw new Error('upstream 503')
    },
  }
  shim.buildContext().web.registerSearchProvider(provider)
  await assert.rejects(shim.callTool('web_search', { queries: ['x'] }, 'c'), /upstream 503/)
})

test('plugin tool named web_search shadows the built-in; fetch providers expose only web_fetch', async () => {
  const shim = new Shim(quietHost, 'p')
  const ctx = shim.buildContext()
  const shadow: ToolDefinitionLike = {
    name: 'web_search',
    description: 'shadow',
    parameters: { type: 'object', properties: {} },
    output: { render: () => [{ type: 'text', text: 'shadowed' }] },
    execute: async () => 'shadow-value',
  }
  ctx.web.registerSearchProvider(fakeProvider('p', [['https://a']]))
  ctx.web.registerFetchProvider({
    id: 'f',
    available: () => true,
    fetch: async request => ({
      url: request.url,
      statusCode: 200,
      body: { kind: 'text', content: 'fetched' },
      truncated: false,
    }),
  })
  ctx.tools.register(shadow)
  const outcome = await shim.callTool('web_search', {}, 'c')
  assert.equal(outcome.structuredContent, 'shadow-value', 'plugin registry wins')
  const names = shim.listTools().map(tool => tool.name)
  assert.equal(names.filter(name => name === 'web_search').length, 1)

  const fetchOnly = new Shim(quietHost, 'p')
  fetchOnly.buildContext().web.registerFetchProvider({
    id: 'f',
    available: () => true,
    fetch: async request => ({
      url: request.url,
      statusCode: 200,
      body: { kind: 'text', content: 'body' },
      truncated: false,
    }),
  })
  assert(!fetchOnly.listTools().some(tool => tool.name === 'web_search'), 'fetch-only registration never lists web_search')
  assert(fetchOnly.listTools().some(tool => tool.name === 'web_fetch'), 'fetch-only registration lists web_fetch')
  const fetched = await fetchOnly.callTool('web_fetch', { url: 'https://example.com' }, 'fetch-call')
  assert.match(String((fetched.content[0] as { text?: unknown }).text), /body/)
})

test('disposeAll drops providers', async () => {
  const shim = new Shim(quietHost, 'p')
  const ctx = shim.buildContext()
  const provider = fakeProvider('p', [['https://a']])
  ctx.web.registerSearchProvider(provider)
  await shim.disposeAll()
  assert(!shim.listTools().some(tool => tool.name === 'web_search'))
})
