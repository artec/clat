// Author-only native engine timers and typed WASI resources; not the net Promise bridge.
import { dnsStart } from 'clat:net-task/egress@0.1.0'
const origin = 'https://typed.example.com'
function resolve() {
  const task = dnsStart(origin, 1000)
  let pollable, resolution
  try {
    if (task.get() !== undefined) throw new Error('nonblocking get')
    pollable = task.subscribe()
    pollable.block()
    const result = task.get()
    if (result?.tag !== 'ok' || result.val.tag !== 'resolved') throw new Error('typed DNS completion')
    resolution = result.val.val
    if (resolution.addresses()[0]?.ip !== '8.8.8.8') throw new Error('typed metadata')
    if (task.get()?.val !== 'consumed') throw new Error('single consume')
    return 'resolved'
  } finally {
    resolution?.[Symbol.dispose]()
    pollable?.[Symbol.dispose]()
    task[Symbol.dispose]()
  }
}
export async function run(scenario) {
  try {
    if (scenario === 'native-timer') {
      await new Promise(resolve => setTimeout(resolve, 1))
      return 'native-timer-dropped'
    }
    if (scenario === 'resolve') return resolve()
    if (scenario === 'cancel') {
      const task = dnsStart(origin, 1000)
      const pollable = task.subscribe()
      try {
        task.cancel()
        pollable.block()
        if (task.get()?.val !== 'cancelled') throw new Error('typed cancel')
        return 'cancelled'
      } finally { pollable[Symbol.dispose](); task[Symbol.dispose]() }
    }
    throw new Error('unknown scenario')
  } catch (error) { return `consumer-error:${error.message}:${error.payload}` }
}
