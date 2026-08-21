// 真实 web-search-exa 验收（免网络）：npm 发布物 0.0.1-rc.1 原样挂载，
// 断言 inject=['web'] 被接受、web_search 面板出现、无 API key 时
// WEB_PROVIDER_UNAVAILABLE 正确上抛。带 key 的联网冒烟见 README（手动）。
import assert from 'node:assert/strict'
import test from 'node:test'
import { PassThrough } from 'node:stream'
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '@deepseek-ai/dsh-web-search-exa'

test('real web-search-exa mounts unmodified and serves web_search', async () => {
  delete process.env.EXA_API_KEY
  const input = new PassThrough()
  const output = new PassThrough()
  let nextId = 1
  const frames = []
  const wake = []
  output.on('data', chunk => {
    for (const line of chunk.toString().split('\n')) {
      if (line.trim() === '') continue
      frames.push(JSON.parse(line))
      for (const w of wake.splice(0)) w()
    }
  })
  const waitFor = predicate => new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timed out waiting for a frame')), 5000)
    const poll = () => {
      const index = frames.findIndex(predicate)
      if (index >= 0) {
        clearTimeout(timer)
        resolve(frames.splice(index, 1)[0])
        return
      }
      wake.push(poll)
    }
    poll()
  })
  const call = async (method, params) => {
    const id = nextId++
    input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return waitFor(frame => frame.id === id)
  }

  const adapter = await serveClat(
    { apply, Config, inject, name },
    { name: 'web-search-exa', version: '0.0.0', input, output },
  )
  try {
    await call('initialize', { protocolVersion: '2025-06-18', capabilities: {} })
    const listed = await call('tools/list')
    const tools = listed.result.tools.map(tool => tool.name)
    assert.deepEqual(tools, ['web_search'], 'provider 注册后内置工具出现')
    const searched = await call('tools/call', { name: 'web_search', arguments: { queries: ['clat'] } })
    assert.equal(searched.result.isError, true)
    assert.match(searched.result.content[0].text, /WEB_PROVIDER_UNAVAILABLE/, '无 key → 不可用错误原样上抛')
  } finally {
    await adapter.dispose()
  }
})
