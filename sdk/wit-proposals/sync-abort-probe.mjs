import { dnsResolve, httpRequest } from 'clat:net/egress@0.1.0'

// This is a consumer test, not a fetch/Node mapping layer.
export async function run(phase) {
  const trace = []
  const controller = new AbortController()
  controller.signal.addEventListener('abort', () => trace.push('abort'))
  const scheduled = Promise.resolve().then(() => controller.abort())
  trace.push('enter')
  const target = dnsResolve('https://api.deepseek.com', 1000)
  if (phase !== 'dns') {
    const response = httpRequest(target, { url: 'https://api.deepseek.com/',
      verb: 'get', headers: [], body: new Uint8Array(), timeoutMs: 1000,
      maxResponseBytes: 1024 })
    if (phase === 'body') response.readBody(1)
  }
  trace.push('return')
  await scheduled
  return JSON.stringify({ phase, trace, aborted: controller.signal.aborted })
}
