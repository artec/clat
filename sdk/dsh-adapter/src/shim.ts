/**
 * Static Cordis-shaped host shim: the ctx a DSH plugin sees. Every portable
 * capability maps 1:1 onto MCP (INV-D1); anything outside the supported
 * service list fails loudly (INV-D3). See docs/todo/dsh-adapter.md §4.3.
 */

import { AsyncLocalStorage } from 'node:async_hooks'
import { AdapterError } from './errors.js'
import { EventBus } from './events.js'
import { HostServicesSeam } from './host-services.js'
import { SystemPromptSeam } from './system-prompt.js'
import { WebSeam } from './web.js'
import type {
  AskAnswerLike,
  AskItemLike,
  AskRequestLike,
  ContentBlockLike,
  DshContext,
  FinishReasonLike,
  GenerateOptionsLike,
  InjectFiberLike,
  InjectResultLike,
  MessageLike,
  StreamChunk,
  TextContentBlock,
  ToolDefinitionLike,
  ToolRunContextLike,
} from './types.js'

/** MCP params the server sends for sampling/createMessage. */
export interface SamplingParams {
  messages: { role: 'user' | 'assistant'; content: { type: 'text'; text: string } }[]
  systemPrompt?: string
  maxTokens: number
  temperature?: number
  stopSequences?: string[]
}

/** MCP params the server sends for elicitation/create. */
export interface ElicitationParams {
  message: string
  requestedSchema: {
    type: 'object'
    properties: Record<string, Record<string, unknown>>
    required: string[]
  }
}

/** Server→client channel the shim depends on (implemented by server.ts). */
export interface HostChannel {
  /** Send sampling/createMessage; resolves with the JSON-RPC result object. */
  sampling(params: SamplingParams): Promise<unknown>
  /** W1-18（A3）：取当前 tools/call 的取消信号（宿主 cancelled/shutdown
   * 触发 abort）。可选——旧宿主不提供时退化为 v0 的永不取消。 */
  beginCall?(callId: string): AbortSignal
  /** Send elicitation/create; resolves with the JSON-RPC result object. */
  elicitation(params: ElicitationParams): Promise<unknown>
  /** CLAT experimental host-services extension. */
  context?(): Promise<import('./types.js').ClatHostContextLike>
  hostTool?(name: string, arguments_: Record<string, unknown>): Promise<unknown>
  /** Host capabilities seen at initialize. */
  readonly capabilities: { sampling: boolean; elicitation: boolean; hostServices: boolean }
  /** stderr diagnostics. */
  log(...args: unknown[]): void
}

/** CLAT elicitation limits (plugin_host.rs MAX_ELICIT_FIELDS/OPTIONS). */
const MAX_FIELDS = 16
const MAX_OPTIONS = 16
/** DSH maxTokens is optional; MCP sampling requires it. */
const DEFAULT_MAX_TOKENS = 4096

/** Services the shim provides (inject-checkable subset first four). */
const SERVICE_KEYS = [
  'tools', 'llm', 'userQuestions', 'web', 'systemPrompt', 'clat', 'fs', 'shell', 'sessions', 'agents',
  'reflect', 'get', 'set', 'provide', 'effect', 'logger', 'inject',
  'on', 'once', 'emit', 'parallel', 'serial', 'bail', 'waterfall',
] as const
const INJECTABLE_KEYS = [
  'tools', 'llm', 'userQuestions', 'web', 'systemPrompt', 'clat', 'fs', 'shell', 'sessions', 'agents',
] as const

/** One registered question's field mapping for answer reconstruction. */
interface FieldPlan {
  field: string
  item: AskItemLike
  multiSelect: boolean
  labels: string[]
}

/**
 * The shim: tool registry + effect cleanup stack + the ctx Proxy handed to
 * `apply()`. One instance per serveClat call.
 */
export class Shim {
  readonly #host: HostChannel
  /** W1-18（A3）：当前在途 tools/call 的取消信号（#processChain 串行，
   * 单槽安全）；execute/sampling/elicitation 的等待都随它 race。 */
  #activeSignal: AbortSignal | undefined
  readonly #pluginName: string
  readonly #tools = new Map<string, ToolDefinitionLike>()
  readonly #provided = new Map<string, unknown>()
  readonly #web: WebSeam
  readonly #cleanups: (() => unknown)[] = []
  readonly #cleanupScope = new AsyncLocalStorage<Array<() => unknown>>()
  readonly #events: EventBus
  readonly #systemPrompt: SystemPromptSeam
  readonly #hostServices: HostServicesSeam
  #context: DshContext | undefined
  #disposed = false

  constructor(host: HostChannel, pluginName: string) {
    this.#host = host
    this.#pluginName = pluginName
    this.#web = new WebSeam(host.log)
    const trackCleanup = (cleanup: () => unknown) => this.#trackCleanup(cleanup)
    this.#events = new EventBus(trackCleanup)
    this.#systemPrompt = new SystemPromptSeam(this.#events, trackCleanup)
    this.#hostServices = new HostServicesSeam(host)
  }

  get pluginName(): string {
    return this.#pluginName
  }

  /** Inject keys the shim can satisfy today. */
  static injectableKeys(): readonly string[] {
    return INJECTABLE_KEYS
  }

  /**
   * Cordis `ctx.inject(deps, callback)` under host semantics: run immediately
   * when every requested adapter service is mounted; otherwise keep the
   * documented optional-dependency behavior and skip the callback.
   */
  #inject(deps: string | string[], callback: (ctx: DshContext) => unknown): InjectResultLike {
    const names = Array.isArray(deps) ? deps : [deps]
    const parentCleanups = this.#cleanupTarget()
    const ownedCleanups: Array<() => unknown> = []
    let setup: Promise<void>
    if (names.every(name =>
      (INJECTABLE_KEYS as readonly string[]).includes(name) || this.#provided.has(name))) {
      if (this.#context === undefined) throw new AdapterError('BAD_CONTEXT', 'adapter context is not initialized')
      let produced: unknown
      try {
        produced = this.#cleanupScope.run(ownedCleanups, () => callback(this.#context as DshContext))
        setup = collectEffectCleanups(produced).then(cleanups => {
          ownedCleanups.push(...cleanups)
        })
      } catch (error) {
        setup = Promise.reject(error)
      }
    } else {
      this.#host.log(
        `ctx.inject([${names.join(', ')}]) requested services this adapter does not mount; ` +
          `the wiring is skipped (host "not mounted" semantics) — declare what you need via the ` +
          `plugin's static inject export if it is required`,
      )
      setup = Promise.resolve()
    }

    // Avoid an unhandled rejection when a plugin intentionally treats inject
    // as fire-and-forget; awaiting the returned handle still observes it.
    void setup.catch(error => this.#host.log('ctx.inject callback failed:', error))
    let active = true
    const entry = async () => {
      if (!active) return
      active = false
      const errors: unknown[] = []
      try {
        await setup
      } catch (error) {
        errors.push(error)
      }
      while (ownedCleanups.length > 0) {
        const cleanup = ownedCleanups.pop()
        if (cleanup === undefined) continue
        try {
          await cleanup()
        } catch (error) {
          errors.push(error)
        }
      }
      if (errors.length === 1) throw errors[0]
      if (errors.length > 1) {
        throw new AdapterError('CLEANUP_FAILED', `${errors.length} ctx.inject setup/cleanup steps failed`)
      }
    }
    parentCleanups.push(entry)
    const fiber: InjectFiberLike = {
      dispose: async () => {
        if (!active) return
        const index = parentCleanups.indexOf(entry)
        if (index >= 0) parentCleanups.splice(index, 1)
        await entry()
      },
    }
    const wrapped = Object.create(fiber) as InjectResultLike
    Object.defineProperty(wrapped, 'then', {
      value: (onFulfilled: ((value: InjectFiberLike) => unknown) | undefined,
        onRejected: ((reason: unknown) => unknown) | undefined) => setup
        .then(() => fiber)
        .then(onFulfilled, onRejected),
    })
    return wrapped
  }

  /** The ctx object `apply()` receives (Proxy: whitelist + INV-D3 rejects). */
  buildContext(): DshContext {
    if (this.#context !== undefined) return this.#context
    const services: DshContext = {
      tools: {
        register: (tool: ToolDefinitionLike) => this.#registerTool(tool),
      },
      llm: {
        stream: (options: GenerateOptionsLike) => this.#llmStream(options),
      },
      userQuestions: {
        ask: (request: AskRequestLike) => this.#ask(request),
      },
      web: {
        registerSearchProvider: provider => this.#trackedRegistration(
          this.#web.registerSearchProvider(provider),
        ),
        registerFetchProvider: provider => this.#trackedRegistration(
          this.#web.registerFetchProvider(provider),
        ),
        search: (request, signal) => this.#web.search(request, signal),
        fetch: (request, signal) => this.#web.fetch(request, signal),
      },
      systemPrompt: this.#systemPrompt,
      clat: this.#hostServices.clat,
      fs: this.#hostServices.fs,
      shell: this.#hostServices.shell,
      sessions: this.#hostServices.sessions,
      agents: this.#hostServices.agents,
      reflect: {
        get: (key: string) => this.#get(key),
        set: (key: string, value: unknown) => this.#set(key, value),
        provide: (key: string, value?: unknown, check?: () => boolean) =>
          this.#provide(key, value, check),
      },
      get: (key: string) => this.#get(key),
      set: (key: string, value: unknown) => this.#set(key, value),
      provide: (key: string, value?: unknown) => this.#provide(key, value),
      effect: (setup, label) => this.#effect(setup, label),
      logger: this.#logger(),
      inject: (deps: string | string[], callback: (ctx: DshContext) => unknown) =>
        this.#inject(deps, callback),
      on: (name, listener, options) => this.#events.on(name, listener, options),
      once: (name, listener, options) => this.#events.once(name, listener, options),
      emit: (...args) => this.#events.emit(...args),
      parallel: (...args) => this.#events.parallel(...args),
      serial: (...args) => this.#events.serial(...args),
      bail: (...args) => this.#events.bail(...args),
      waterfall: (...args) => this.#events.waterfall(...args),
    }
    Object.defineProperties(services, {
      [Symbol.for('cordis.isolate')]: { value: Object.create(null), enumerable: false },
      [Symbol.for('cordis.intercept')]: { value: Object.create(null), enumerable: false },
    })
    const provided = this.#provided
    this.#context = new Proxy(services, {
      get(target, property, receiver) {
        if (property === 'then') return undefined
        if (typeof property === 'symbol') return Reflect.get(target, property, receiver)
        if ((SERVICE_KEYS as readonly string[]).includes(property)) {
          return Reflect.get(target, property, receiver)
        }
        if (provided.has(property)) return provided.get(property)
        throw new AdapterError(
          'SPINE_SERVICE',
          `ctx.${String(property)} is not provided by @artec/clat-dsh-adapter (supported: ` +
            `${SERVICE_KEYS.join(', ')}). Spine/UI services are out of scope for the MCP ` +
            `plugin bridge — restructure the capability as a tool (ctx.tools.register) ` +
            `or use CLAT's native features.`,
        )
      },
    })
    this.#hostServices.attachContext(this.#context)
    return this.#context
  }

  /** Receives one detached current-run snapshot from the CLAT host. */
  updateHostContext(context: import('./types.js').ClatHostContextLike | null): void {
    this.#hostServices.updateContext(context)
  }

  #get(key: string): unknown {
    if ((INJECTABLE_KEYS as readonly string[]).includes(key)) {
      return (this.#context as unknown as Record<string, unknown> | undefined)?.[key]
    }
    return this.#provided.get(key)
  }

  #set(key: string, value: unknown): void {
    if (!this.#provided.has(key)) throw new Error(`cannot set unprovided service "${key}"`)
    this.#provided.set(key, value)
  }

  #provide(key: string, value?: unknown, check?: () => boolean): () => void {
    this.#assertLive()
    if (typeof key !== 'string' || key === '') throw new TypeError('ctx.provide: key must be a non-empty string')
    if ((INJECTABLE_KEYS as readonly string[]).includes(key) || this.#provided.has(key)) {
      throw new Error(`service "${key}" is already provided`)
    }
    if (check !== undefined && check.call(value) === false) {
      return () => {}
    }
    this.#provided.set(key, value)
    let active = true
    const dispose = () => {
      if (!active) return
      active = false
      if (this.#provided.get(key) === value) this.#provided.delete(key)
    }
    this.#trackCleanup(dispose)
    return dispose
  }

  // ---- ctx.tools ----------------------------------------------------------

  #registerTool(tool: ToolDefinitionLike): () => void {
    this.#assertLive()
    if (tool === null || typeof tool !== 'object' || typeof tool.name !== 'string' || tool.name === '') {
      throw new AdapterError('BAD_TOOL_DEFINITION', 'ctx.tools.register: tool.name must be a non-empty string')
    }
    const parameters = tool.parameters
    if (
      parameters === null || typeof parameters !== 'object' || Array.isArray(parameters) ||
      (parameters as Record<string, unknown>).type !== 'object'
    ) {
      throw new AdapterError(
        'UNCOMPILED_PARAMETERS',
        `ctx.tools.register: tool "${tool.name}" has no compiled JSON Schema parameters. ` +
          `Build the definition with defineTool() from @deepseek-ai/dsh-tools (its output ` +
          `carries the compiled schema); the adapter does not compile the parameter DSL itself.`,
      )
    }
    if (typeof tool.execute !== 'function') {
      throw new AdapterError('BAD_TOOL_DEFINITION', `ctx.tools.register: tool "${tool.name}" has no execute()`)
    }
    if (tool.output === null || typeof tool.output !== 'object' || typeof tool.output.render !== 'function') {
      throw new AdapterError('BAD_TOOL_DEFINITION', `ctx.tools.register: tool "${tool.name}" has no output.render()`)
    }
    if (this.#tools.has(tool.name)) {
      throw new AdapterError('DUPLICATE_TOOL', `ctx.tools.register: tool "${tool.name}" is already registered`)
    }
    this.#tools.set(tool.name, tool)
    this.#host.log(`registered tool ${tool.name}`)
    return this.#trackedRegistration(() => {
      this.#tools.delete(tool.name)
    })
  }

  /** tools/list projection (name/description/compiled parameters). */
  listTools(): ToolDefinitionLike[] {
    // 内置 web_search 仅在 ≥1 个 search provider 注册、且插件未以同名
    // 工具覆盖时出现（Phase 3b；同名时插件注册表优先，不重复列出）。
    const builtIns: ToolDefinitionLike[] = []
    if (this.#web.hasSearchProvider() && !this.#tools.has('web_search')) {
      builtIns.push(this.#web.webSearchTool())
    }
    if (this.#web.hasFetchProvider() && !this.#tools.has('web_fetch')) {
      builtIns.push(this.#web.webFetchTool())
    }
    return [...this.#tools.values(), ...builtIns]
  }

  /** tools/call: execute + render. Throws propagate to isError results. */
  async callTool(name: string, args: unknown, callId: string): Promise<{ content: ContentBlockLike[]; structuredContent: unknown }> {
    this.#assertLive()
    const tool = this.#tools.get(name) ?? this.#builtinTool(name)
    if (tool === undefined) {
      throw new AdapterError('UNKNOWN_TOOL', `unknown tool: ${name}`)
    }
    // W1-18（A3）：信号接宿主取消源（cancelled 通知 / shutdown）。
    const signal = this.#host.beginCall?.(callId) ?? new AbortController().signal
    this.#activeSignal = signal
    const exec: ToolRunContextLike = {
      callId,
      name: tool.name,
      arguments: args,
      signal,
      deferContext: () => {
        this.#host.log(`warn: ${name} called exec.deferContext(); the MCP bridge has no agent-loop context ferry — ignored`)
      },
      concludeTurn: () => {
        this.#host.log(`warn: ${name} called exec.concludeTurn(); the MCP bridge has no agent loop — ignored`)
      },
    }
    let value: unknown
    try {
      value = await raceAbort(Promise.resolve(tool.execute(args, exec)), signal, 'tool execute')
    } finally {
      this.#activeSignal = undefined
    }
    const content = tool.output.render(args, value)
    if (!Array.isArray(content)) {
      throw new AdapterError('BAD_RENDER', `tool "${name}" output.render() returned a non-array`)
    }
    return { content, structuredContent: value }
  }

  /** Built-in tools (after the plugin registry: a plugin tool of the same name wins). */
  #builtinTool(name: string): ToolDefinitionLike | undefined {
    if (name === 'web_search' && this.#web.hasSearchProvider()) {
      return this.#web.webSearchTool()
    }
    if (name === 'web_fetch' && this.#web.hasFetchProvider()) {
      return this.#web.webFetchTool()
    }
    return undefined
  }

  /** MCP prompts/list projection for DSH system-prompt contributions. */
  listPrompts(): { name: string; description: string }[] {
    return this.#systemPrompt.hasContributions()
      ? [{ name: 'dsh-system-prompt', description: `System-prompt contribution from DSH plugin ${this.#pluginName}` }]
      : []
  }

  /** Resolve the DSH prompt at request time with strict variable semantics. */
  async getPrompt(name: string, arguments_: Record<string, string> = {}): Promise<{
    description: string
    prompt: string
    context: string
  }> {
    if (name !== 'dsh-system-prompt' || !this.#systemPrompt.hasContributions()) {
      throw new AdapterError('UNKNOWN_PROMPT', `unknown prompt: ${name}`)
    }
    const resolved = await this.#systemPrompt.render({
      cwd: arguments_['cwd'],
      provider: arguments_['provider'],
      model: arguments_['model'],
    })
    return {
      description: `System-prompt contribution from DSH plugin ${this.#pluginName}`,
      prompt: resolved.prompt,
      context: resolved.context,
    }
  }

  // ---- ctx.llm ------------------------------------------------------------

  #llmStream(options: GenerateOptionsLike): AsyncIterable<StreamChunk> {
    const run = async (): Promise<{ text: string; stopReason: string }> => {
      if (!this.#host.capabilities.sampling) {
        throw new AdapterError(
          'NO_SAMPLING',
          'the MCP host did not declare sampling; ctx.llm is unavailable against this host',
        )
      }
      if (options.tools !== undefined && options.tools.length > 0) {
        throw new AdapterError(
          'TOOLS_IN_SAMPLING',
          'ctx.llm.stream with tools is not supported: MCP sampling has no tool-calling face',
        )
      }
      if (options.signal?.aborted) {
        throw new AdapterError('ABORTED', 'llm stream was aborted before it started')
      }
      if (options.provider !== undefined || options.model !== undefined) {
        this.#host.log(
          `note: llm provider/model (${String(options.provider)}/${String(options.model)}) are ignored — ` +
            `MCP sampling always uses the host session model`,
        )
      }
      const params = this.#samplingParams(options)
      const result = await raceAbort(
        this.#host.sampling(params),
        this.#activeSignal,
        'sampling',
      )
      return this.#parseSamplingResult(result)
    }
    return this.#chunkStream(run)
  }

  #samplingParams(options: GenerateOptionsLike): SamplingParams {
    if (!Array.isArray(options.messages) || options.messages.length === 0) {
      throw new AdapterError('BAD_REQUEST', 'ctx.llm.stream: messages must be a non-empty array')
    }
    const systemParts: string[] = []
    if (typeof options.system === 'string' && options.system !== '') systemParts.push(options.system)
    const messages: SamplingParams['messages'] = []
    for (const message of options.messages) {
      const role = (message as MessageLike).role
      const text = this.#contentText((message as MessageLike).content, `messages[${messages.length}]`)
      if (role === 'system') {
        systemParts.push(text)
        continue
      }
      if (role !== 'user' && role !== 'assistant') {
        throw new AdapterError('BAD_REQUEST', `ctx.llm.stream: unsupported message role ${String(role)}`)
      }
      messages.push({ role, content: { type: 'text', text } })
    }
    if (messages.length === 0) {
      throw new AdapterError('BAD_REQUEST', 'ctx.llm.stream: no user/assistant messages to sample')
    }
    const params: SamplingParams = {
      messages,
      maxTokens: options.maxTokens !== undefined && Number.isFinite(options.maxTokens) && options.maxTokens > 0
        ? Math.floor(options.maxTokens)
        : DEFAULT_MAX_TOKENS,
    }
    if (systemParts.length > 0) params.systemPrompt = systemParts.join('\n\n')
    if (options.temperature !== undefined) params.temperature = options.temperature
    if (Array.isArray(options.stop) && options.stop.length > 0) params.stopSequences = [...options.stop]
    return params
  }

  /** Fold a sampling result (single block or block array) into plain text. */
  #parseSamplingResult(result: unknown): { text: string; stopReason: string } {
    if (result === null || typeof result !== 'object') {
      throw new AdapterError('BAD_RESPONSE', 'sampling result is not an object')
    }
    const record = result as { content?: unknown; stopReason?: unknown }
    const text = this.#contentText(record.content, 'sampling result')
    const stopReason = typeof record.stopReason === 'string' ? record.stopReason : 'endTurn'
    return { text, stopReason }
  }

  #contentText(content: unknown, where: string): string {
    const blockText = (block: unknown): string => {
      if (block === null || typeof block !== 'object' || (block as ContentBlockLike).type !== 'text') {
        throw new AdapterError(
          'NON_TEXT_CONTENT',
          `${where}: only text content blocks survive the MCP sampling bridge ` +
            `(got type ${block === null || typeof block !== 'object' ? typeof block : String((block as ContentBlockLike).type)})`,
        )
      }
      const text = (block as TextContentBlock).text
      if (typeof text !== 'string') {
        throw new AdapterError('NON_TEXT_CONTENT', `${where}: text block without text`)
      }
      return text
    }
    if (Array.isArray(content)) return content.map(blockText).join('\n')
    if (content !== null && typeof content === 'object' && (content as ContentBlockLike).type !== undefined) {
      return blockText(content)
    }
    throw new AdapterError('NON_TEXT_CONTENT', `${where}: content must be a block or block array`)
  }

  /** Adapt one completed sampling call into the dsh-llm chunk protocol. */
  async *#chunkStream(run: () => Promise<{ text: string; stopReason: string }>): AsyncGenerator<StreamChunk> {
    const { text, stopReason } = await run()
    yield { type: 'block-start', index: 0, blockType: 'text' }
    if (text !== '') yield { type: 'text-delta', index: 0, text }
    yield { type: 'block-end', index: 0, block: { type: 'text', text } }
    yield { type: 'finish', reason: finishReason(stopReason) }
  }

  // ---- ctx.userQuestions --------------------------------------------------

  async #ask(request: AskRequestLike): Promise<AskAnswerLike> {
    if (request.agent !== undefined) {
      throw new AdapterError(
        'AGENT_ASK_UNSUPPORTED',
        'ask({agent}) needs the host agents registry; agent-scoped asks are not supported over the MCP bridge',
      )
    }
    const questions = request.questions
    if (!Array.isArray(questions) || questions.length === 0) {
      throw new AdapterError('EMPTY_QUESTIONS', 'ask_user_question requires at least one question')
    }
    if (questions.length > MAX_FIELDS) {
      throw new AdapterError(
        'TOO_MANY_QUESTIONS',
        `ask_user_question carries ${questions.length} questions; the elicitation bridge supports at most ${MAX_FIELDS}`,
      )
    }
    if (!this.#host.capabilities.elicitation) {
      throw new AdapterError(
        'NO_ELICITATION',
        'the MCP host did not declare elicitation; ctx.userQuestions is unavailable against this host',
      )
    }
    // Intent validation mirrors UserQuestionService.ask (BAD_INTENT).
    for (const question of questions) {
      const intent = question.intent
      if (intent === undefined) continue
      if (!(question.options ?? []).some(option => option.label === intent.approve)) {
        throw new AdapterError(
          'BAD_INTENT',
          `question ${question.id} declares intent ${intent.kind} whose approve label ` +
            `${JSON.stringify(intent.approve)} names none of its options`,
        )
      }
      if (question.detail === undefined) {
        throw new AdapterError(
          'BAD_INTENT',
          `question ${question.id} declares intent ${intent.kind} without the detail it reviews`,
        )
      }
    }
    const { properties, required, plans } = this.#questionFields(questions)
    const result = await this.#host.elicitation({
      message: questions[0]?.question ?? '',
      requestedSchema: { type: 'object', properties, required },
    })
    if (result === null || typeof result !== 'object') {
      throw new AdapterError('BAD_RESPONSE', 'elicitation result is not an object')
    }
    const action = (result as { action?: unknown }).action
    if (action === 'decline') {
      throw new AdapterError('USER_DECLINED', 'the user declined the question form')
    }
    if (action === 'cancel') {
      throw new AdapterError('USER_CANCELLED', 'the question form was cancelled')
    }
    if (action !== 'accept') {
      throw new AdapterError('BAD_RESPONSE', `elicitation result action ${String(action)}`)
    }
    const content = (result as { content?: unknown }).content
    if (content === null || typeof content !== 'object' || Array.isArray(content)) {
      throw new AdapterError('BAD_RESPONSE', 'elicitation result content is not an object')
    }
    const values = content as Record<string, unknown>
    const answers = questions.map(item => {
      const plan = plans.find(candidate => candidate.item === item)
      const raw = plan !== undefined ? values[plan.field] : undefined
      const value = raw === undefined ? '' : String(raw)
      if (plan !== undefined && plan.multiSelect) {
        return { id: item.id, ...this.#parseMultiSelect(value, plan.labels) }
      }
      if (plan !== undefined && plan.labels.length > 0) {
        return { id: item.id, selected: [value] }
      }
      return { id: item.id, selected: [], custom: value === '' ? undefined : value }
    })
    return { answers }
  }

  /** Map questions onto elicitation fields (INV: enumValues for single-select). */
  #questionFields(questions: AskItemLike[]): {
    properties: Record<string, Record<string, unknown>>
    required: string[]
    plans: FieldPlan[]
  } {
    const used = new Set<string>()
    const properties: Record<string, Record<string, unknown>> = {}
    const plans: FieldPlan[] = []
    for (const item of questions) {
      const labels = (item.options ?? []).map(option => option.label)
      if (labels.length > MAX_OPTIONS) {
        throw new AdapterError(
          'TOO_MANY_OPTIONS',
          `question ${item.id} offers ${labels.length} options; the elicitation bridge supports at most ${MAX_OPTIONS}`,
        )
      }
      const field = uniqueField(item.id, used)
      const property: Record<string, unknown> = {
        title: item.header ?? item.id,
        description: questionDescription(item),
      }
      if (labels.length > 0 && !item.multiSelect) {
        // CLAT reads `enumValues`; the standard `enum` keeps other hosts honest.
        property.enumValues = labels
        property.enum = labels
      } else {
        property.type = 'string'
        if (item.multiSelect && labels.length > 0) {
          property.description =
            `${property.description as string}\n(multi-select over: ${labels.join(', ')} — enter comma-separated labels; ` +
            `text matching no label becomes the custom answer)`
        }
      }
      properties[field] = property
      plans.push({ field, item, multiSelect: item.multiSelect === true, labels })
    }
    return { properties, required: plans.map(plan => plan.field), plans }
  }

  #parseMultiSelect(value: string, labels: string[]): { selected: string[]; custom?: string } {
    const tokens = value.split(/[,，;；]/).map(token => token.trim()).filter(token => token !== '')
    const selected: string[] = []
    const leftovers: string[] = []
    for (const token of tokens) {
      const hit = labels.find(label => label.toLowerCase() === token.toLowerCase())
      if (hit !== undefined) {
        if (!selected.includes(hit)) selected.push(hit)
      } else {
        leftovers.push(token)
      }
    }
    const custom = leftovers.join(', ')
    return { selected, ...(custom === '' ? {} : { custom }) }
  }

  // ---- ctx.effect / logger / dispose --------------------------------------

  #effect(setup: () => unknown, label?: string): () => Promise<void> {
    this.#assertLive()
    void label
    const produced = setup()
    const syncIterator = isSyncIterator(produced) ? produced : undefined
    const asyncIterator = isAsyncIterator(produced) ? produced : undefined
    // Cordis drains generator effects during setup and owns every yielded
    // disposer. Teardown runs those disposers in reverse yield order.
    const cleanup = syncIterator !== undefined
      ? Promise.resolve(collectSyncCleanups(syncIterator))
      : asyncIterator !== undefined
        ? collectAsyncCleanups(asyncIterator)
        : Promise.resolve(produced).then(normalizeCleanup)
    const entry = async () => {
      // 每步隔离（W1-06）：单个 cleanup 抛错不得截断同一 effect 的
      // 其余拆解步骤；全部尝试完再聚合上报。
      const errors: unknown[] = []
      let steps: Array<() => unknown> = []
      try {
        // Cordis waits for asynchronous effect setup before teardown. Waiting
        // also lets an async generator yield every disposer instead of
        // racing `return()` against its second `next()`.
        steps = await cleanup
      } catch (error) {
        errors.push(error)
      }
      for (const fn of steps.reverse()) {
        try {
          await fn()
        } catch (error) {
          errors.push(error)
        }
      }
      if (errors.length > 0) {
        this.#host.log(`effect cleanup failed (${errors.length} step(s)):`)
        for (const error of errors) this.#host.log(' ', error)
        throw errors.length === 1 ? errors[0] : new AdapterError('CLEANUP_FAILED', `${errors.length} cleanup steps failed`)
      }
    }
    const cleanupTarget = this.#cleanupTarget()
    cleanupTarget.push(entry)
    const dispose = async () => {
      const index = cleanupTarget.indexOf(entry)
      if (index >= 0) {
        cleanupTarget.splice(index, 1)
        await entry()
      }
    }
    // Cordis' async effect disposer is PromiseLike: awaiting registration
    // waits for setup and yields a callable disposer. Use a distinct closure
    // as the fulfillment value to avoid thenable self-assimilation.
    Object.defineProperty(dispose, 'then', {
      value: (onFulfilled: ((value: () => Promise<void>) => unknown) | undefined,
        onRejected: ((reason: unknown) => unknown) | undefined) => cleanup
        .then(() => () => dispose())
        .then(onFulfilled, onRejected),
    })
    return dispose
  }

  #logger() {
    const create = (name?: string) => {
      const prefix = `[dsh:${name ?? this.#pluginName}]`
      const emit = (level: string) => (...args: unknown[]) => {
        this.#host.log(prefix, level, ...args)
      }
      const logger = ((child?: string) => create(child ?? name)) as import('./types.js').LoggerLike
      logger.debug = emit('debug')
      logger.info = emit('info')
      logger.warn = emit('warn')
      logger.error = emit('error')
      logger.success = emit('success')
      return logger
    }
    return create()
  }

  #cleanupTarget(): Array<() => unknown> {
    return this.#cleanupScope.getStore() ?? this.#cleanups
  }

  #trackCleanup(cleanup: () => unknown): void {
    this.#cleanupTarget().push(cleanup)
  }

  #trackedRegistration<T extends () => unknown>(dispose: T): T {
    const target = this.#cleanupTarget()
    let active = true
    const owned = (() => {
      if (!active) return
      active = false
      const index = target.indexOf(owned)
      if (index >= 0) target.splice(index, 1)
      return dispose()
    }) as T
    target.push(owned)
    return owned
  }

  /** LIFO dispose (INV-D4): stdin EOF / shutdown path. 每步隔离（W1-06）：
   * 单个 cleanup 抛错不得截断其余逆序拆解，也不得跳过 tools/web 清空；
   * 全部尝试完后聚合报错一次，幂等性不变。 */
  async disposeAll(): Promise<void> {
    if (this.#disposed) return
    this.#disposed = true
    const errors: unknown[] = []
    while (this.#cleanups.length > 0) {
      const cleanup = this.#cleanups.pop()
      if (cleanup === undefined) continue
      try {
        await cleanup()
      } catch (error) {
        errors.push(error)
      }
    }
    this.#tools.clear()
    this.#provided.clear()
    this.#web.dispose()
    this.#systemPrompt.clear()
    this.#events.clear()
    if (errors.length === 1) throw errors[0]
    if (errors.length > 1) {
      throw new AdapterError('CLEANUP_FAILED', `${errors.length} effect cleanups failed during dispose`)
    }
  }

  #assertLive(): void {
    if (this.#disposed) {
      throw new AdapterError('DISPOSED', 'the adapter is shutting down')
    }
  }
}

/** finishReason mapping: MCP stopReason → dsh-llm FinishReason. */
function finishReason(stopReason: string): FinishReasonLike {
  switch (stopReason) {
    case 'maxTokens': return { kind: 'max-tokens' }
    case 'toolUse': return { kind: 'tool-calls' }
    default: return { kind: 'stop' }
  }
}

/** Schema-safe, collision-free field name for a question id. */
function uniqueField(id: string, used: Set<string>): string {
  let base = id.replace(/[^A-Za-z0-9_]/g, '_').replace(/^_+|_+$/g, '')
  if (base === '') base = 'q'
  let candidate = base
  let suffix = 2
  while (used.has(candidate)) {
    candidate = `${base}_${suffix}`
    suffix += 1
  }
  used.add(candidate)
  return candidate
}

/** Question text shown to the user: question + detail + option blurbs. */
function questionDescription(item: AskItemLike): string {
  const parts = [item.question]
  if (item.detail !== undefined) parts.push(item.detail)
  for (const option of item.options ?? []) {
    if (option.description !== undefined) parts.push(`• ${option.label} — ${option.description}`)
  }
  return parts.join('\n')
}

/** Cordis effect cleanup shape: function | array of functions | nothing. */
function normalizeCleanup(value: unknown): Array<() => unknown> {
  if (typeof value === 'function') return [value as () => unknown]
  if (Array.isArray(value)) {
    return value.filter(entry => typeof entry === 'function') as Array<() => unknown>
  }
  return []
}

function collectSyncCleanups(iterator: Iterator<unknown>): Array<() => unknown> {
  const cleanups: Array<() => unknown> = []
  while (true) {
    const step = iterator.next()
    if (step.done) return cleanups
    cleanups.push(...normalizeCleanup(step.value))
  }
}

function collectEffectCleanups(produced: unknown): Promise<Array<() => unknown>> {
  if (isSyncIterator(produced)) return Promise.resolve(collectSyncCleanups(produced))
  if (isAsyncIterator(produced)) return collectAsyncCleanups(produced)
  return Promise.resolve(produced).then(normalizeCleanup)
}

async function collectAsyncCleanups(iterator: AsyncIterator<unknown>): Promise<Array<() => unknown>> {
  const cleanups: Array<() => unknown> = []
  while (true) {
    const step = await iterator.next()
    if (step.done) return cleanups
    cleanups.push(...normalizeCleanup(step.value))
  }
}

function isSyncIterator(value: unknown): value is Iterator<unknown> {
  return value !== null && typeof value === 'object'
    && typeof (value as { next?: unknown }).next === 'function'
    && !(Symbol.asyncIterator in value)
}

function isAsyncIterator(value: unknown): value is AsyncIterator<unknown> {
  return value !== null && typeof value === 'object'
    && Symbol.asyncIterator in value
    && typeof (value as { next?: unknown }).next === 'function'
}

/** W1-18（A3）：把一个宿主等待挂到取消信号上——signal abort 即以
 * ABORTED 错误 settle（宿主不回包/超时后 promise 不再悬挂）。 */
function raceAbort<T>(promise: Promise<T>, signal: AbortSignal | undefined, what: string): Promise<T> {
  if (signal === undefined) return promise
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(new AdapterError('ABORTED', `${what} was aborted (host cancellation)`))
    // 先挂内部 promise 的消化器再判早退：early-abort 立即拒绝外层的
    // 同时，内部 promise 迟到的 settle/rejection 必须被消费——否则
    // unhandledRejection 杀进程（F-3 测试实测暴露）。
    let settled = false
    promise.then(
      value => {
        if (!settled) {
          settled = true
          signal.removeEventListener('abort', onAbort)
          resolve(value)
        }
      },
      error => {
        if (!settled) {
          settled = true
          signal.removeEventListener('abort', onAbort)
          reject(error instanceof Error ? error : new Error(String(error)))
        }
      },
    )
    if (signal.aborted) {
      settled = true
      onAbort()
      return
    }
    signal.addEventListener('abort', () => {
      if (!settled) {
        settled = true
        onAbort()
      }
    }, { once: true })
  })
}
