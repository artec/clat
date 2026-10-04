// Uses the engine's native fetch/timers, never Node shims or JS polling.
export async function run(stage, order, trigger) {
  const trace = []
  const controller = new AbortController()
  controller.signal.addEventListener('abort', () => trace.push('abort'))
  let timer
  const cancel = () => controller.abort()
  const schedule = () => {
    if (trigger === 'timer') timer = setTimeout(cancel, 5)
    else Promise.resolve().then(cancel)
  }
  if (order === 'before') cancel()
  trace.push('enter')
  let outcome
  const bodyPhase = stage.startsWith('body')
  try {
    const pending = fetch(`https://api.deepseek.com/${bodyPhase ? 'body' : stage}`, { signal: controller.signal })
    if (order === 'during' && !bodyPhase) schedule()
    const response = await pending
    trace.push('headers')
    if (order === 'during' && bodyPhase) schedule()
    let body
    if (stage === 'body-parallel') {
      const reader = response.body.getReader()
      const reads = await Promise.all([reader.read(), reader.read()])
      body = reads.filter(chunk => !chunk.done).map(chunk => new TextDecoder().decode(chunk.value)).join('')
    } else body = await response.text()
    trace.push('return')
    outcome = { kind: 'success', body }
  } catch (error) {
    trace.push('error')
    outcome = { kind: 'error', name: error.name, message: String(error.message) }
  } finally {
    if (timer !== undefined) clearTimeout(timer)
  }
  if (order === 'after') cancel()
  return JSON.stringify({ stage, order, trigger, trace, outcome, aborted: controller.signal.aborted })
}
