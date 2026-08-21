/**
 * Hand-rolled MCP stdio server (zero runtime deps): newline-delimited
 * JSON-RPC 2.0 with initialize / tools / ping plus the two server-initiated
 * requests (sampling/createMessage, elicitation/create) the shim needs.
 * stdout carries protocol frames only; diagnostics go to stderr (INV-D4).
 */

import { createInterface } from 'node:readline'
import type { Readable, Writable } from 'node:stream'
import { errorMessage } from './errors.js'
import type { ElicitationParams, SamplingParams } from './shim.js'
import type { ContentBlockLike, ToolDefinitionLike, ToolHint } from './types.js'

/** What the server needs from the shim to answer host requests. */
export interface ServerHandler {
  listTools(): ToolDefinitionLike[]
  callTool(name: string, arguments_: unknown, callId: string): Promise<{ content: ContentBlockLike[]; structuredContent: unknown }>
  dispose(): Promise<void>
}

export interface ServeServerOptions {
  name: string
  version?: string
  handler: ServerHandler
  /** Author effect hints → MCP annotations (INV-D5). */
  toolHints?: Record<string, ToolHint>
  input?: Readable
  output?: Writable
  log?: (...args: unknown[]) => void
  /** initialize responses wait for this (apply() settlement) — see serveClat. */
  ready?: Promise<void>
}

/** toolHint → MCP annotations, tuned to CLAT's effect_from_annotations. */
const HINT_ANNOTATIONS: Record<ToolHint, Record<string, boolean> | undefined> = {
  'read-only': { readOnlyHint: true, openWorldHint: false },
  network: { readOnlyHint: true, openWorldHint: true },
  write: { readOnlyHint: false, destructiveHint: false, openWorldHint: false },
  // Absent annotations already mean destructive in CLAT — send nothing.
  destructive: undefined,
}

/** Adapter-owned built-in tools declare their own annotations (Phase 3b:
 * web_search is a read-only, open-world lookup → CLAT Network effect). */
const BUILTIN_ANNOTATIONS: Record<string, Record<string, boolean>> = {
  web_search: { readOnlyHint: true, openWorldHint: true },
}

const JSONRPC_PARSE_ERROR = -32700
const JSONRPC_METHOD_NOT_FOUND = -32601
const JSONRPC_INVALID_PARAMS = -32602
const JSONRPC_INTERNAL = -32603
const SERVER_NOT_INITIALIZED = -32002

interface JsonRpcErrorShape {
  code: number
  message: string
}

/** One parsed JSON-RPC frame (either direction). */
interface Frame {
  id?: number | string | null
  method?: string
  params?: unknown
  result?: unknown
  error?: JsonRpcErrorShape
}

/** A JSON-RPC error carrying its wire code. */
export class JsonRpcError extends Error {
  readonly code: number

  constructor(code: number, message: string) {
    super(message)
    this.name = 'JsonRpcError'
    this.code = code
  }
}

/** One MCP stdio server instance. */
export class McpStdioServer {
  readonly #options: ServeServerOptions
  readonly #input: Readable
  readonly #output: Writable
  readonly #log: (...args: unknown[]) => void
  readonly #pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>()
  #nextId = 1
  #initialized = false
  #closed = false
  #shutdownPromise: Promise<void> | undefined
  #processChain: Promise<void> = Promise.resolve()
  #writeChain: Promise<void> = Promise.resolve()
  #capabilities = { sampling: false, elicitation: false }
  #rl: ReturnType<typeof createInterface> | undefined

  constructor(options: ServeServerOptions) {
    this.#options = options
    this.#input = options.input ?? process.stdin
    this.#output = options.output ?? process.stdout
    this.#log = options.log ?? ((...args: unknown[]) => console.error('[clat-dsh-adapter]', ...args))
  }

  /** Host capabilities observed at initialize (INV-D1's error basis). */
  get clientCapabilities(): { sampling: boolean; elicitation: boolean } {
    return { ...this.#capabilities }
  }

  /** Begin consuming input. Registers signal handlers when on process stdio. */
  start(): void {
    const rl = createInterface({ input: this.#input })
    this.#rl = rl
    rl.on('line', line => {
      const parsed = this.#parseFrame(line)
      if (parsed === undefined) return
      // Responses to our server-initiated requests only settle a pending
      // promise — handle them immediately: a queued tools/call that awaits
      // such a response must not deadlock behind itself in the chain.
      if (this.#isResponse(parsed)) {
        this.#onResponse(parsed.id as number, parsed.result, parsed.error)
        return
      }
      // Requests/notifications run serialized in arrival order: initialize
      // awaits the apply() ready-gate, and a concurrent tools/list must not
      // overtake it into "not initialized".
      this.#processChain = this.#processChain
        .then(() => this.#dispatchFrame(parsed))
        .catch(error => this.#log('frame processing failed:', errorMessage(error)))
    })
    rl.on('close', () => {
      void this.shutdown()
    })
    if (this.#input === process.stdin) {
      const stop = (signal: NodeJS.Signals) => {
        void this.shutdown().finally(() => process.kill(process.pid, signal))
      }
      process.once('SIGINT', stop)
      process.once('SIGTERM', stop)
    }
  }

  /** Server→client request. Rejects with JsonRpcError on error responses. */
  async request(method: string, params: unknown): Promise<unknown> {
    if (this.#closed) {
      throw new JsonRpcError(JSONRPC_INTERNAL, `cannot send ${method}: the adapter is shutting down`)
    }
    const id = this.#nextId
    this.#nextId += 1
    const promise = new Promise<unknown>((resolve, reject) => {
      this.#pending.set(id, { resolve, reject })
    })
    this.#write({ jsonrpc: '2.0', id, method, params })
    return promise
  }

  /** sampling/createMessage (delegates JSON-RPC rejection verbatim). */
  sampling(params: SamplingParams): Promise<unknown> {
    return this.request('sampling/createMessage', params)
  }

  /** elicitation/create. */
  elicitation(params: ElicitationParams): Promise<unknown> {
    return this.request('elicitation/create', params)
  }

  /** Graceful shutdown: settle pending, run disposers LIFO (INV-D4). Idempotent. */
  shutdown(): Promise<void> {
    if (this.#shutdownPromise !== undefined) return this.#shutdownPromise
    this.#closed = true
    this.#shutdownPromise = this.#shutdown()
    return this.#shutdownPromise
  }

  async #shutdown(): Promise<void> {
    this.#rl?.close()
    const failure = new JsonRpcError(JSONRPC_INTERNAL, 'the adapter is shutting down')
    for (const [, entry] of this.#pending) entry.reject(failure)
    this.#pending.clear()
    try {
      await this.#handler().dispose()
    } catch (error) {
      this.#log('dispose failed:', errorMessage(error))
    }
  }

  #handler(): ServerHandler {
    return this.#options.handler
  }

  // ---- frame loop ---------------------------------------------------------

  /** Parse one line; writes a -32700 frame and returns undefined on garbage. */
  #parseFrame(line: string): Frame | undefined {
    const trimmed = line.trim()
    if (trimmed === '') return undefined
    let frame: unknown
    try {
      frame = JSON.parse(trimmed)
    } catch {
      this.#write({ jsonrpc: '2.0', id: null, error: { code: JSONRPC_PARSE_ERROR, message: 'parse error' } })
      return undefined
    }
    if (frame === null || typeof frame !== 'object' || Array.isArray(frame)) {
      this.#write({ jsonrpc: '2.0', id: null, error: { code: JSONRPC_PARSE_ERROR, message: 'frame must be an object' } })
      return undefined
    }
    return frame as Frame
  }

  /** A response to one of our server-initiated requests? */
  #isResponse(frame: Frame): boolean {
    return frame.method === undefined && frame.id !== undefined && ('result' in frame || 'error' in frame)
  }

  /** Serialized dispatch of client requests and notifications. */
  async #dispatchFrame(frame: Frame): Promise<void> {
    if (typeof frame.method !== 'string') return
    if (frame.id === undefined) {
      this.#onNotification(frame.method, frame.params)
      return
    }
    await this.#onRequest(frame.id as number | string, frame.method, frame.params)
  }

  #onNotification(method: string, params: unknown): void {
    if (method === 'notifications/cancelled') {
      const id = (params as { requestId?: unknown } | undefined)?.requestId
      this.#log(`note: received notifications/cancelled for ${String(id)} — cancellation is not forwarded in v0`)
      return
    }
    this.#log(`note: ignoring notification ${method}`)
  }

  async #onRequest(id: number | string, method: string, params: unknown): Promise<void> {
    try {
      const result = await this.#dispatch(method, params, id)
      this.#write({ jsonrpc: '2.0', id, result })
    } catch (error) {
      if (error instanceof JsonRpcError) {
        this.#write({ jsonrpc: '2.0', id, error: { code: error.code, message: error.message } })
        return
      }
      this.#log(`internal error in ${method}:`, errorMessage(error))
      this.#write({ jsonrpc: '2.0', id, error: { code: JSONRPC_INTERNAL, message: errorMessage(error) } })
    }
  }

  async #dispatch(method: string, params: unknown, id: number | string): Promise<unknown> {
    if (method === 'initialize') {
      const capabilities = (params as { capabilities?: Record<string, unknown> } | undefined)?.capabilities
      this.#capabilities = {
        sampling: capabilities?.sampling !== undefined,
        elicitation: capabilities?.elicitation !== undefined,
      }
      if (this.#options.ready !== undefined) await this.#options.ready
      this.#initialized = true
      const requested = (params as { protocolVersion?: unknown } | undefined)?.protocolVersion
      return {
        protocolVersion: typeof requested === 'string' ? requested : '2025-06-18',
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: this.#options.name, version: this.#options.version ?? '0.0.0' },
      }
    }
    if (!this.#initialized) {
      throw new JsonRpcError(SERVER_NOT_INITIALIZED, 'server not initialized')
    }
    switch (method) {
      case 'ping':
        return {}
      case 'tools/list':
        return { tools: this.#handler().listTools().map(tool => this.#toolDescriptor(tool)) }
      case 'tools/call': {
        const record = params as { name?: unknown; arguments?: unknown } | undefined
        const name = record?.name
        if (typeof name !== 'string' || name === '') {
          throw new JsonRpcError(JSONRPC_INVALID_PARAMS, 'tools/call: name must be a non-empty string')
        }
        const args = record?.arguments ?? {}
        try {
          const outcome = await this.#handler().callTool(name, args, String(id))
          const result: Record<string, unknown> = { content: outcome.content }
          if (outcome.structuredContent !== undefined) result.structuredContent = outcome.structuredContent
          return result
        } catch (error) {
          return { isError: true, content: [{ type: 'text', text: errorMessage(error) }] }
        }
      }
      default:
        throw new JsonRpcError(JSONRPC_METHOD_NOT_FOUND, `unknown method: ${method}`)
    }
  }

  #onResponse(id: number | string, result: unknown, error: unknown): void {
    const entry = typeof id === 'number' ? this.#pending.get(id) : undefined
    if (entry === undefined) {
      this.#log(`note: response for unknown request id ${String(id)}`)
      return
    }
    this.#pending.delete(id as number)
    const shape = error as JsonRpcErrorShape | undefined
    if (shape !== null && typeof shape === 'object' && typeof shape.code === 'number') {
      entry.reject(new JsonRpcError(shape.code, shape.message ?? 'request failed'))
      return
    }
    entry.resolve(result)
  }

  #toolDescriptor(tool: ToolDefinitionLike): Record<string, unknown> {
    const descriptor: Record<string, unknown> = {
      name: tool.name,
      description: tool.description,
      inputSchema: tool.parameters,
    }
    const hint = this.#options.toolHints?.[tool.name]
    const annotations = hint !== undefined ? HINT_ANNOTATIONS[hint] : BUILTIN_ANNOTATIONS[tool.name]
    if (annotations !== undefined) descriptor.annotations = annotations
    return descriptor
  }

  #write(payload: unknown): void {
    if (this.#closed) return
    this.#writeChain = this.#writeChain.then(() => {
      if (this.#closed) return
      return new Promise<void>(resolve => {
        this.#output.write(`${JSON.stringify(payload)}\n`, () => resolve())
      })
    })
  }
}
