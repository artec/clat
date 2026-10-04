// Native DNS/HTTP semantic task scheduling, no Node timers or JS polling.
export async function run(phase, order, trigger) {
  const controller = new AbortController()
  const trace = []
  let timer
  controller.signal.addEventListener('abort', () => trace.push('abort'))
  const abort = () => controller.abort()
  if (order === 'before') abort()
  trace.push('enter')
  const pending = phase === 'body-parallel'
    ? Promise.all([clatNetTaskProbe('body', controller.signal), clatNetTaskProbe('body', controller.signal)])
      .then(values => values[0])
    : clatNetTaskProbe(phase, controller.signal)
  if (order === 'during') {
    if (trigger === 'timer') timer = setTimeout(abort, 5)
    else Promise.resolve().then(abort)
  }
  let outcome
  try {
    const value = await pending
    trace.push('return')
    outcome = { kind: 'success', body: value }
  } catch (error) {
    trace.push('error')
    outcome = { kind: 'error', name: error.name }
  } finally {
    if (timer !== undefined) clearTimeout(timer)
  }
  if (order === 'after') abort()
  return JSON.stringify({ stage: phase, order, trigger, aborted: controller.signal.aborted, trace, outcome })
}
