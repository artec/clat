// Real npm provider, unchanged; no public network or credentials.
import assert from 'node:assert/strict'
import test from 'node:test'
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '@deepseek-ai/dsh-web-search-exa'
import { McpClient } from '../test-support/mcp-client.mjs'

test('real web-search-exa mounts unmodified and serves web_search', async () => {
  const saved = process.env.EXA_API_KEY
  const selected = process.env.DSH_WEB_SEARCH_PROVIDER
  delete process.env.EXA_API_KEY
  delete process.env.DSH_WEB_SEARCH_PROVIDER
  const client = new McpClient()
  const adapter = await serveClat({ apply, Config, inject, name }, {
    name: 'web-search-exa', input: client.input, output: client.output,
  })
  try {
    await client.initialize()
    const listed = await client.call('tools/list')
    assert.deepEqual(listed.result.tools.map(tool => tool.name), ['web_search'])
    const searched = await client.call('tools/call', { name: 'web_search', arguments: { queries: ['clat'] } })
    assert.equal(searched.result.isError, true)
    assert.match(searched.result.content[0].text, /WEB_PROVIDER_UNAVAILABLE/)
  } finally {
    await adapter.dispose()
    client.close()
    if (saved === undefined) delete process.env.EXA_API_KEY
    else process.env.EXA_API_KEY = saved
    if (selected === undefined) delete process.env.DSH_WEB_SEARCH_PROVIDER
    else process.env.DSH_WEB_SEARCH_PROVIDER = selected
  }
})
