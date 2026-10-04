// Engine probe only: this is deliberately not an official DSH package.
import { EventBus } from '../dsh-adapter/src/events.ts'
import { get } from 'clat:plugin/config@0.1.0'
import { callTool } from 'clat:plugin/host@0.1.0'
import { regexProbe } from './regex-probe.mjs'

async function semantics() {
  const disposers = []
  const trace = []
  const events = new EventBus(dispose => disposers.push(dispose))
  events.on('around', async (value, next) => {
    trace.push('outer-before')
    await Promise.resolve()
    const result = await next()
    trace.push('outer-after')
    return value + result
  })
  events.on('around', async (_value, next) => {
    trace.push('inner-before')
    const result = await next()
    trace.push('inner-after')
    return result
  })
  const result = await events.waterfall('around', 3, async () => {
    await Promise.resolve()
    trace.push('leaf')
    return 7
  })
  async function* setup() {
    await Promise.resolve()
    yield async () => { await Promise.resolve(); trace.push('cleanup-first') }
    await Promise.resolve()
    yield () => trace.push('cleanup-second')
  }
  const cleanups = []
  for await (const cleanup of setup()) cleanups.push(cleanup)
  for (const cleanup of cleanups.reverse()) await cleanup()
  for (const dispose of disposers.reverse()) dispose()
  events.emit('around', 99, () => { throw new Error('disposed hook survived') })
  const controller = new AbortController()
  const cancelled = new Promise(resolve => controller.signal.addEventListener('abort', () => resolve('aborted')))
  controller.abort()
  return { result, trace, cancellation: await cancelled }
}

function configuration() {
  let config
  try { config = JSON.parse(get()) } catch { return { state: 'missing' } }
  const key = config.apiKey
  return { state: key === undefined ? 'omitted' : key === '' ? 'empty' : 'present' }
}

async function isolation() {
  let network = 'unexpected-success'
  try { await fetch('https://example.com') } catch { network = 'denied' }
  return { network, process: typeof globalThis.process, require: typeof globalThis.require }
}

export const tools = {
  listTools() {
    return ['async_semantics', 'config_states', 'isolation', 'write_fence', 'burn', 'spin', 'regex'].map(name => ({
      name, description: 'PLG-3 engine probe; not the official quartet',
      inputSchema: '{"type":"object"}', effect: 'pure',
    }))
  },
  async call(name, argumentsJson) {
    if (name === 'regex') return JSON.stringify(regexProbe())
    if (name === 'write_fence') {
      try { callTool('write_file', argumentsJson); return '{"denied":false}' }
      catch (error) { return JSON.stringify({ denied: true, error: String(error) }) }
    }
    if (name === 'burn') { let sum = 0; for (let i = 0; i < 100_000; i++) sum += i; return JSON.stringify(sum) }
    if (name === 'spin') { for (;;) {} }
    const result = name === 'async_semantics' ? await semantics()
      : name === 'config_states' ? configuration()
      : name === 'isolation' ? await isolation() : { unknown: true }
    return JSON.stringify(result)
  },
}
