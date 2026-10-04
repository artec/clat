// Native Promise scheduling over the typed net task, not a synchronous JS poll loop.
const origin = 'https://typed.example.com'
export async function run(scenario) {
  if (scenario.startsWith('{')) {
    const config = JSON.parse(scenario)
    if (config.phase === 'lifetime') return lifetime(config)
    return config.phase === 'mapping' ? requestMapping(config) : transport(config)
  }
  const controller = new AbortController()
  const trace = []
  const order = scenario.split(':')[0]
  let thenReads = 0
  if (order === 'prototype') Object.defineProperty(Object.prototype, 'then', {configurable:true, get() { thenReads++; return undefined }})
  controller.signal.addEventListener('abort', () => trace.push('abort'))
  const abort = () => controller.abort()
  if (order === 'before') abort()
  let timer, resolution
  trace.push('enter')
  try {
    const pending = clatNetDns(origin, order === 'quota' ? 3000 : 1000, controller.signal)
    if (order === 'microtask') Promise.resolve().then(abort)
    if (order === 'during') timer = setTimeout(abort, 5)
    if (order === 'parallel') {
      const all = await Promise.all([pending, clatNetDns(origin, 1000, controller.signal)])
      resolution = all[0]
      clatNetDispose(all[1])
    } else resolution = await pending
    if (order === 'quota') {
      const held = []
      try {
        for (let i = 1; i < 14; i++) held.push(await clatNetDns(origin, 3000, controller.signal))
        try { await clatNetDns(origin, 3000, controller.signal); throw new Error('resource quota missing') }
        catch (error) { if (error.message !== 'limit-exceeded') throw error }
      } finally { for (const value of held) clatNetDispose(value) }
    }
    if (order === 'validation') {
      for (const timeout of [NaN, Infinity, 0, 0.5, 30001]) {
        try { clatNetDns(origin, timeout, controller.signal); throw new Error('invalid timeout accepted') }
        catch (error) { if (error.message !== 'invalid-request') throw error }
      }
      for (const value of [{}, 1, Object.create(Object.getPrototypeOf(resolution))]) {
        try { clatNetInfo(value); throw new Error('forged handle accepted') }
        catch (error) { if (error.message !== 'opaque network resource required') throw error }
      }
      const cancelled = new AbortController()
      cancelled.abort()
      try { await clatNetHttp(resolution, origin, 1000, 100, cancelled.signal); throw new Error('pre-abort accepted') }
      catch (error) { if (error.name !== 'AbortError') throw error }
      if (clatNetInfo(resolution)[0] !== '8.8.8.8') throw new Error('pre-abort consumed input')
    }
    if (order === 'denied') {
      try { await clatNetHttp(resolution, 'https://denied.invalid', 1000, 100, controller.signal); throw new Error('fence accepted') }
      catch (error) { if (error.message !== 'capability-denied') throw error }
      try { clatNetInfo(resolution); throw new Error('owned credential reused') }
      catch (error) { if (error.message !== 'consumed') throw error }
      return JSON.stringify({order, kind:'success', consumed:true})
    }
    if (order === 'prototype' && thenReads !== 0) throw new Error('ambient then getter reached credential')
    const addresses = clatNetInfo(resolution)
    const discovery = clatNetDns64(resolution)
    if (order === 'dns64') {
      if (discovery.length !== 2 || discovery[0][0] !== '2001:4860:1234::c000:aa' || discovery[0][1] !== 'ipv6' || discovery[1][0] !== '2001:4860:1234::c000:ab') throw new Error('DNS64 metadata lost')
    } else if (discovery.length !== 0) throw new Error('unexpected DNS64 discovery')
    const detailed = clatNetAddresses(resolution)
    if (detailed[0][0] !== (order === 'dns64' ? '2001:4860:1234::808:808' : '8.8.8.8') || detailed[0][1] !== (order === 'dns64' ? 'ipv6' : 'ipv4')) throw new Error('address family lost')
    if (addresses.length !== 1 || addresses[0] !== (order === 'dns64' ? '2001:4860:1234::808:808' : '8.8.8.8')) throw new Error('typed metadata')
    if (Object.keys(resolution).length !== 0) throw new Error('handle exposed')
    try { clatNetInfo({}); throw new Error('forgery accepted') }
    catch (error) { if (error.message !== 'opaque network resource required') throw error }
    clatNetDispose(resolution)
    clatNetDispose(resolution)
    try { clatNetInfo(resolution); throw new Error('stale accepted') }
    catch (error) { if (error.message !== 'consumed') throw error }
    trace.push('return')
    if (order === 'after') abort()
    return JSON.stringify({order, trace, kind:'success', addresses})
  } catch (error) {
    trace.push('error')
    return JSON.stringify({order, trace, kind:'error', name:error.name, message:error.message})
  } finally {
    if (order === 'prototype') delete Object.prototype.then
    if (timer !== undefined) clearTimeout(timer)
    if (resolution !== undefined) clatNetDispose(resolution)
  }
}


async function transport({origin, phase, order}) {
  let thenReads = 0
  if (order === 'prototype') Object.defineProperty(Object.prototype, 'then', {configurable:true, get() { thenReads++; return undefined }})
  const setup = new AbortController()
  const controller = new AbortController()
  let resolution, response, timer
  const trace = []
  controller.signal.addEventListener('abort', () => trace.push('abort'))
  try {
    resolution = await clatNetDns(origin, 1000, setup.signal)
    if (phase === 'body') response = await clatNetHttp(resolution, origin, 1500, 100, setup.signal)
    if (order === 'before') controller.abort()
    trace.push('enter')
    const pending = phase === 'headers'
      ? clatNetHttp(resolution, origin, 1500, 100, controller.signal)
      : clatNetRead(response, 2, controller.signal)
    if (order !== 'before') {
      try { clatNetInfo(phase === 'headers' ? resolution : response); throw new Error('owned input reused') }
      catch (error) { if (error.message !== 'consumed') throw error }
    }
    if (order === 'microtask') Promise.resolve().then(() => controller.abort())
    if (order === 'during') timer = setTimeout(() => controller.abort(), 5)
    const value = await pending
    if (phase === 'headers') {
      response = value
      if (clatNetInfo(response) !== 201) throw new Error('typed status')
    } else {
      response = value.response
      if (!(value.bytes instanceof Uint8Array) || String.fromCharCode(...value.bytes) !== 'xy') throw new Error('typed chunk')
      const last = await clatNetRead(response, 2, setup.signal)
      response = last.response
      if (last.bytes.length !== 0) throw new Error('typed EOF')
    }
    if (order === 'prototype' && thenReads !== 0) throw new Error('ambient then getter reached chunk')
    trace.push('return')
    if (order === 'after') controller.abort()
    return JSON.stringify({phase, order, trace, kind:'success'})
  } catch (error) {
    trace.push('error')
    return JSON.stringify({phase, order, trace, kind:'error', name:error.name, message:error.message})
  } finally {
    if (order === 'prototype') delete Object.prototype.then
    if (timer !== undefined) clearTimeout(timer)
    if (resolution !== undefined) clatNetDispose(resolution)
    if (response !== undefined) clatNetDispose(response)
  }
}

async function requestMapping({origin, order}) {
  const controller = new AbortController()
  let resolution, response
  try {
    resolution = await clatNetDns(origin, 1000, controller.signal)
    const invoke = (verb, headers, body, signal = controller.signal) =>
      clatNetRequest(resolution, origin, verb, headers, body, 1500, 100, signal)
    const body = new Uint8Array([0, 255, 65])
    const headers = [['X-Input', 'original'], ['X-Duplicate', 'one'], ['X-Duplicate', 'two']]
    if (order === 'invalid') {
      const cases = [
        ['TRACE', headers, body], ['get', headers, body], ['GET', {}, body],
        ['GET', [[]], body], ['GET', [[1, 'value']], body],
        ['GET', [['X-Input', 'nul\0']], body], ['GET', Array(65).fill(['x', 'y']), body],
        ['GET', [['x', 'a'.repeat(32768)]], body], ['GET', headers, []],
        ['GET', headers, new Uint8Array(1048577)],
      ]
      for (const args of cases) {
        try { await invoke(...args); throw new Error('invalid request accepted') }
        catch (error) { if (!['invalid-request', 'limit-exceeded'].includes(error.message)) throw error }
        if (clatNetInfo(resolution).length !== 1) throw new Error('invalid conversion consumed input')
      }
      return JSON.stringify({phase:'mapping', order, kind:'success'})
    }
    if (order === 'reentry' || order === 'abort') {
      Object.defineProperty(headers[0], 1, {get() {
        if (order === 'reentry') clatNetDispose(resolution)
        else controller.abort()
        return 'original'
      }})
      try { await invoke('POST', headers, body); throw new Error('reentry accepted') }
      catch (error) {
        if (order === 'reentry' ? error.message !== 'consumed' : error.name !== 'AbortError') throw error
      }
      if (order === 'abort' && clatNetInfo(resolution).length !== 1) throw new Error('pre-abort consumed')
      return JSON.stringify({phase:'mapping', order, kind:'success'})
    }
    if (order === 'denied' || order === 'forbidden') {
      try {
        await invoke('POST', order === 'forbidden' ? [['Host', 'escape.invalid']] : headers, body)
        throw new Error('authority accepted')
      } catch (error) {
        if (error.message !== (order === 'denied' ? 'capability-denied' : 'invalid-request')) throw error
      }
      try { clatNetInfo(resolution); throw new Error('host rejection did not consume') }
      catch (error) { if (error.message !== 'consumed') throw error }
      return JSON.stringify({phase:'mapping', order, kind:'success'})
    }
    const pending = invoke(order, headers, body)
    body.fill(42)
    headers[0][1] = 'tampered'
    response = await pending
    if (clatNetInfo(response) !== 201) throw new Error('request status')
    const entries = clatNetHeaders(response)
    if (!entries.some(([name, value]) => name === 'x-test' && value === 'typed')) throw new Error('response headers')
    entries[0][1] = 'tampered'
    if (!clatNetHeaders(response).some(([name, value]) => name === 'x-test' && value === 'typed')) throw new Error('metadata aliased')
    if (order !== 'HEAD') {
      const chunk = await clatNetRead(response, 2, controller.signal)
      response = chunk.response
      if (String.fromCharCode(...chunk.bytes) !== 'xy') throw new Error('request body result')
    }
    return JSON.stringify({phase:'mapping', order, kind:'success'})
  } catch (error) {
    return JSON.stringify({phase:'mapping', order, kind:'error', name:error.name, message:error.message})
  } finally {
    if (resolution !== undefined) clatNetDispose(resolution)
    if (response !== undefined) clatNetDispose(response)
  }
}


// Deliberately abandon opaque JS wrappers: the host invocation boundary owns cleanup.
async function lifetime({origin, order}) {
  globalThis.invocationCount = (globalThis.invocationCount || 0) + 1
  if (globalThis.invocationCount !== 1) throw new Error('guest instance survived invocation')
  const resolution = await clatNetDns(origin, 1000, new AbortController().signal)
  if (order === 'resolution') return 'lost-resolution'
  const response = await clatNetHttp(resolution, origin, 1500, 100, new AbortController().signal)
  if (clatNetInfo(response) !== 201) throw new Error('idle response missing')
  if (order === 'throw') throw new Error('controlled lost-response exception')
  return 'lost-response'
}
