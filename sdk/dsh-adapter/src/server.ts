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
import type { ClatHostContextLike, ContentBlockLike, ToolDefinitionLike, ToolHint } from './types.js'

/** What the server needs from the shim to answer host requests. */
export interface ServerHandler {
  listTools(): ToolDefinitionLike[]
  listPrompts(): { name: string; description: string }[]
  getPrompt(name: string, arguments_: Record<string, string>): Promise<{
    description: string
    prompt: string
    context: string
  }>
  callTool(name: string, arguments_: unknown, callId: string): Promise<{
    content: ContentBlockLike[]
    structuredContent: unknown
  }>
  /** W1-18（A3）：在途 tools/call 开始——返回该调用的取消信号（宿主
   * notifications/cancelled / shutdown 会触发 abort）。可选：旧宿主
   * 实现不提供时取消传播退化为 v0 行为。
   */
  beginCall?(callId: string): AbortSignal
  hostContextChanged?(context: ClatHostContextLike | null): void
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
  web_fetch: { readOnlyHint: true, openWorldHint: true },
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

  /** W1-18（A3）：在途 tools/call 的取消控制器（key = String(request id)）。 */
  readonly #calls = new Map<string, AbortController>()
  /** F-3（审计）：先于登记到达的取消（目标调用还在串行链里排队）——
   * 登记时对账消化，取消不丢失。 */
  readonly #lateCancels = new Set<string>()
  #nextId = 1
  #initialized = false
  #closed = false
  #shutdownPromise: Promise<void> | undefined
  #processChain: Promise<void> = Promise.resolve()
  #writeChain: Promise<void> = Promise.resolve()
  #capabilities = { sampling: false, elicitation: false, hostServices: false }
  #rl: ReturnType<typeof createInterface> | undefined

  constructor(options: ServeServerOptions) {
    this.#options = options
    this.#input = options.input ?? process.stdin
    this.#output = options.output ?? process.stdout
    this.#log = options.log ?? ((...args: unknown[]) => console.error('[clat-dsh-adapter]', ...args))
  }

  /** Host capabilities observed at initialize (INV-D1's error basis). */
  get clientCapabilities(): { sampling: boolean; elicitation: boolean; hostServices: boolean } {
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
      // W1-18（A3）：notifications/cancelled 是控制信号——绕过串行链
      // 立即生效。串行链此刻正被在途的 tools/call 占着（那正是要被取
      // 消的调用），排队等于永不转发。
      if (parsed.method === 'notifications/cancelled') {
        this.#onNotification(parsed.method, parsed.params)
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

  async hostContext(): Promise<ClatHostContextLike> {
    return await this.request('io.artec.clat/context/get', {}) as ClatHostContextLike
  }

  async hostTool(name: string, arguments_: Record<string, unknown>): Promise<unknown> {
    const result = await this.request('io.artec.clat/tools/call', { name, arguments: arguments_ })
    if (result === null || typeof result !== 'object' || !('output' in result)) {
      throw new JsonRpcError(JSONRPC_INTERNAL, 'CLAT host tool returned an invalid envelope')
    }
    return (result as { output: unknown }).output
  }

  /** Graceful shutdown: settle pending, run disposers LIFO (INV-D4). Idempotent. */
  shutdown(): Promise<void> {
    if (this.#shutdownPromise !== undefined) return this.#shutdownPromise
    this.#closed = true
    // Install the single-flight promise before #shutdown closes readline:
    // `rl.close()` emits `close` synchronously and re-enters shutdown().
    this.#shutdownPromise = Promise.resolve().then(() => this.#shutdown())
    return this.#shutdownPromise
  }

  async #shutdown(): Promise<void> {
    // W1-18（A3）：关闭即取消——在途工具的外部副作用尽快停。
    for (const controller of this.#calls.values()) controller.abort()
    this.#calls.clear()
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
      // W1-26（A4-6）：半行/垃圾行有 stderr 诊断（长度 + 前缀）——静默
      // 吞帧的 EOF 场景可排查。
      this.#log(`unparseable line (${line.length} chars): ${line.slice(0, 80)}${line.length > 80 ? '…' : ''}`)
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
    if (method === 'io.artec.clat/context/changed') {
      const context = (params as { context?: unknown } | undefined)?.context
      if (context === null) {
        this.#handler().hostContextChanged?.(null)
      } else if (context !== undefined && typeof context === 'object' && !Array.isArray(context)) {
        this.#handler().hostContextChanged?.(context as ClatHostContextLike)
      } else {
        this.#log('note: ignoring malformed CLAT host-context notification')
      }
      return
    }
    if (method === 'notifications/cancelled') {
      const id = (params as { requestId?: unknown } | undefined)?.requestId
      const controller = this.#calls.get(String(id))
      if (controller !== undefined) {
        // W1-18（A3）：取消传播——在途 tools/call 的 exec.signal 触发
        // abort，插件的 execute / sampling 等待随 race 以错误收束。
        controller.abort()
        this.#log(`note: cancelled in-flight tools/call ${String(id)}`)
      } else {
        // F-3：目标调用可能仍在串行链里排队（登记未发生）——记晚到
        // 取消，登记时对账。
        this.#lateCancels.add(String(id))
        this.#log(`note: notifications/cancelled for ${String(id)} arrived before its call registered — deferred`)
      }
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
      const experimental = capabilities?.experimental as Record<string, unknown> | undefined
      const hostServices = experimental?.['io.artec.clat/hostServices'] as Record<string, unknown> | undefined
      this.#capabilities = {
        sampling: capabilities?.sampling !== undefined,
        elicitation: capabilities?.elicitation !== undefined,
        hostServices: hostServices?.version === '0.1.0',
      }
      if (this.#options.ready !== undefined) await this.#options.ready
      this.#initialized = true
      const requested = (params as { protocolVersion?: unknown } | undefined)?.protocolVersion
      return {
        protocolVersion: typeof requested === 'string' ? requested : '2025-06-18',
        capabilities: {
          tools: { listChanged: false },
          prompts: { listChanged: false },
          experimental: {
            'io.artec.clat/hostServices': { version: '0.1.0' },
          },
        },
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
      case 'prompts/list':
        return {
          prompts: this.#handler().listPrompts().map(prompt => ({
            name: prompt.name,
            description: prompt.description,
            arguments: [
              { name: 'cwd', description: 'Current CLAT project directory.', required: false },
              { name: 'provider', description: 'Current CLAT model provider.', required: false },
              { name: 'model', description: 'Current CLAT model name.', required: false },
            ],
            _meta: { 'io.artec.clat/dshSystemPrompt': true },
          })),
        }
      case 'prompts/get': {
        const record = params as { name?: unknown; arguments?: unknown } | undefined
        if (typeof record?.name !== 'string' || record.name === '') {
          throw new JsonRpcError(JSONRPC_INVALID_PARAMS, 'prompts/get: name must be a non-empty string')
        }
        const raw = record.arguments ?? {}
        if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
          throw new JsonRpcError(JSONRPC_INVALID_PARAMS, 'prompts/get: arguments must be an object')
        }
        const arguments_: Record<string, string> = {}
        for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
          if (typeof value !== 'string') {
            throw new JsonRpcError(JSONRPC_INVALID_PARAMS, `prompts/get: argument ${key} must be a string`)
          }
          arguments_[key] = value
        }
        const resolved = await this.#handler().getPrompt(record.name, arguments_)
        return {
          description: resolved.description,
          messages: resolved.prompt === '' ? [] : [{
            role: 'user',
            content: { type: 'text', text: resolved.prompt },
          }],
          _meta: {
            'io.artec.clat/systemPrompt': resolved.prompt,
            'io.artec.clat/runtimeContext': resolved.context,
          },
        }
      }
      case 'tools/call': {
        const record = params as { name?: unknown; arguments?: unknown } | undefined
        const name = record?.name
        if (typeof name !== 'string' || name === '') {
          throw new JsonRpcError(JSONRPC_INVALID_PARAMS, 'tools/call: name must be a non-empty string')
        }
        const args = record?.arguments ?? {}
        // W1-18（A3）：调用登记进取消映射，finally 撤销（正常完成不
        // abort）。F-3：先到的取消在此对账——登记即消化。
        const controller = new AbortController()
        this.#calls.set(String(id), controller)
        if (this.#lateCancels.delete(String(id))) {
          controller.abort()
        }
        try {
          const outcome = await this.#handler().callTool(name, args, String(id))
          const result: Record<string, unknown> = { content: outcome.content }
          if (outcome.structuredContent !== undefined) result.structuredContent = outcome.structuredContent
          // W1-12：序列化闸——非可序列化载荷（BigInt/循环引用等）在此
          // 降为结构化 tool error（模型可读、可改走 text 渲染），而不是
          // 深入写链后炸掉整个 stdio 服务。
          try {
            JSON.stringify(result)
          } catch (error) {
            return {
              isError: true,
              content: [{
                type: 'text',
                text: `RESULT_NOT_SERIALIZABLE: tool "${name}" returned a value JSON cannot serialize (${errorMessage(error)}); convert it in the tool body or output.render`,
              }],
            }
          }
          return result
        } catch (error) {
          return { isError: true, content: [{ type: 'text', text: errorMessage(error) }] }
        } finally {
          this.#calls.delete(String(id))
        }
      }
      default:
        throw new JsonRpcError(JSONRPC_METHOD_NOT_FOUND, `unknown method: ${method}`)
    }
  }

  #onResponse(id: number | string, result: unknown, error: unknown): void {
    // W1-26（A4-6）：宿主以字符串回显数字 id（"101"）是合法 JSON-RPC —
    // 归一化匹配，不让 promise 悬挂。
    const key = typeof id === 'number' ? id : Number(id)
    const entry = Number.isFinite(key) ? this.#pending.get(key) : undefined
    if (entry === undefined) {
      this.#log(`note: response for unknown request id ${String(id)}`)
      return
    }
    this.#pending.delete(key)
    const shape = error as JsonRpcErrorShape | undefined
    if (shape !== null && typeof shape === 'object' && typeof shape.code === 'number') {
      entry.reject(new JsonRpcError(shape.code, shape.message ?? 'request failed'))
      return
    }
    entry.resolve(result)
  }

  /** W1-18（A3）：取（或建）一次 tools/call 的取消信号。 */
  beginCall(callId: string): AbortSignal {
    let controller = this.#calls.get(callId)
    if (controller === undefined) {
      controller = new AbortController()
      this.#calls.set(callId, controller)
    }
    return controller.signal
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
      const text = this.#frameText(payload)
      if (text === undefined) return
      return new Promise<void>(resolve => {
        this.#output.write(`${text}\n`, () => resolve())
      })
    })
  }

  /** W1-12：帧文本永不抛——载荷不可序列化时降级为该请求 id 的内部
   * 错误帧（无法定位 id 的通知类载荷则丢弃并记日志）。写链不再被
   * 一次坏载荷永久毒化（毒化 = 服务失声 + unhandledRejection 杀进程）。 */
  #frameText(payload: unknown): string | undefined {
    try {
      return JSON.stringify(payload)
    } catch (error) {
      this.#log('response payload is not JSON serializable:', errorMessage(error))
      const id = (payload as { id?: number | string } | null)?.id
      if (typeof id !== 'number' && typeof id !== 'string') return undefined
      return `{"jsonrpc":"2.0","id":${JSON.stringify(id)},"error":{"code":-32603,"message":"internal error: response payload is not JSON serializable"}}`
    }
  }
}
