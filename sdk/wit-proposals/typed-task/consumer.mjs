// Actual typed task resources only. Native WASI subscription/async completion
// are verified by the host tests and remain a separate engine-integration leg.
import { dnsStart } from 'clat:net-task/egress@0.1.0'
const origin = 'https://typed.example.com'
function drive(scenario) {
  let task = dnsStart(origin, 1000)
  try {
    if (scenario === 'cancel') {
      task.cancel()
      const result = task.get()
      if (result?.tag !== 'err' || result.val !== 'cancelled') throw new Error('cancel result must be typed err')
      return 'cancelled'
    }
    if (task.get() !== undefined) throw new Error('get must be nonblocking')
    if (scenario === 'stale') {
      task[Symbol.dispose]()
      task = undefined
      const next = dnsStart(origin, 1000)
      try {
        if (next.get() !== undefined) throw new Error('new task corrupted')
      } finally { next[Symbol.dispose]() }
      return 'stale-rejected'
    }
    if (scenario === 'quota') {
      const children = []
      try {
        for (let i = 1; i < 16; i++) children.push(dnsStart(origin, 1000))
        try { dnsStart(origin, 1000); throw new Error('task budget missing') }
        catch (error) { if (error.payload !== 'limit-exceeded') throw error }
      } finally { for (const child of children) child[Symbol.dispose]() }
      return 'quota-rejected'
    }
    return 'pending'
  } finally {
    task?.[Symbol.dispose]()
  }
}

export function run(scenario) {
  try { return drive(scenario) }
  catch (error) { return `consumer-error:${error.message}:${error.payload}` }
}
