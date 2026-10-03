import { PassThrough } from 'node:stream'
import { createInterface } from 'node:readline'

export class McpClient {
  input = new PassThrough()
  output = new PassThrough()
  nextId = 1
  pending = new Map()

  constructor(input = this.input, output = this.output) {
    this.input = input
    this.output = output
    this.lines = createInterface({ input: output })
    this.lines.on('line', line => {
      const frame = JSON.parse(line)
      const waiter = this.pending.get(frame.id)
      if (waiter === undefined) return
      this.pending.delete(frame.id)
      clearTimeout(waiter.timer)
      waiter.resolve(frame)
    })
  }

  call(method, params) {
    const id = this.nextId++
    const response = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`MCP ${method} timed out`))
      }, 5000)
      this.pending.set(id, { resolve, reject, timer })
    })
    this.input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return response
  }

  notify(method, params) {
    this.input.write(`${JSON.stringify({ jsonrpc: '2.0', method, params })}\n`)
  }

  initialize() {
    return this.call('initialize', { protocolVersion: '2025-06-18', capabilities: {} })
  }

  close() {
    this.lines.close()
    for (const waiter of this.pending.values()) {
      clearTimeout(waiter.timer)
      waiter.reject(new Error('MCP client closed'))
    }
    this.pending.clear()
  }
}

export async function bounded(promise, label) {
  let timer
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out`)), 5000)
    })])
  } finally {
    clearTimeout(timer)
  }
}
