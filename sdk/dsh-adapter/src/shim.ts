/**
 * Minimal Cordis-shaped host shim: the ctx a DSH leaf plugin sees. Every
 * capability maps 1:1 onto MCP (INV-D1); anything outside the supported
 * service list fails loudly (INV-D3). See docs/todo/dsh-adapter.md §4.3.
 */

import { AdapterError } from './errors.js'
import { WebSeam } from './web.js'
import type {
  AskAnswerLike,
  AskItemLike,
  AskRequestLike,
  ContentBlockLike,
  DshContext,
  FinishReasonLike,
  GenerateOptionsLike,
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
  /** Send elicitation/create; resolves with the JSON-RPC result object. */
  elicitation(params: ElicitationParams): Promise<unknown>
  /** Host capabilities seen at initialize. */
  readonly capabilities: { sampling: boolean; elicitation: boolean }
  /** stderr diagnostics. */
  log(...args: unknown[]): void
}

/** CLAT elicitation limits (plugin_host.rs MAX_ELICIT_FIELDS/OPTIONS). */
const MAX_FIELDS = 16
const MAX_OPTIONS = 16
/** DSH maxTokens is optional; MCP sampling requires it. */
const DEFAULT_MAX_TOKENS = 4096

/** Services the shim provides (inject-checkable subset first four). */
const SERVICE_KEYS = ['tools', 'llm', 'userQuestions', 'web', 'get', 'effect', 'logger', 'inject'] as const
const INJECTABLE_KEYS = ['tools', 'llm', 'userQuestions', 'web'] as const

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
  readonly #pluginName: string
  readonly #tools = new Map<string, ToolDefinitionLike>()
  readonly #web: WebSeam
  readonly #cleanups: (() => unknown)[] = []
  #disposed = false

  constructor(host: HostChannel, pluginName: string) {
    this.#host = host
    this.#pluginName = pluginName
    this.#web = new WebSeam(host.log)
  }

  get pluginName(): string {
    return this.#pluginName
  }

  /** Inject keys the shim can satisfy today. */
  static injectableKeys(): readonly string[] {
    return INJECTABLE_KEYS
  }

  /**
   * Cordis `ctx.inject(deps, callback)` under host semantics: the callback
   * runs only when every requested service is mounted. The adapter mounts
   * no injectable host services, so the callback is skipped and noted on
   * stderr — plugins written against the documented "not mounted → the
   * wiring never runs, the plugin keeps working" contract (e.g. optional
   * settings sections via dsh-settings) degrade gracefully instead of
   * dying at startup. A static `inject` export remains a hard requirement
   * (checked in serveClat), so plugins that *declare* a spine dependency
   * are still rejected up front.
   */
  #inject(deps: string | string[], callback: (ctx: unknown) => unknown): void {
    const names = Array.isArray(deps) ? deps : [deps]
    this.#host.log(
      `ctx.inject([${names.join(', ')}]) requested services this adapter does not mount; ` +
        `the wiring is skipped (host "not mounted" semantics) — declare what you need via the ` +
        `plugin's static inject export if it is required`,
    )
    void callback
  }

  /** The ctx object `apply()` receives (Proxy: whitelist + INV-D3 rejects). */
  buildContext(): DshContext {
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
        registerSearchProvider: provider => this.#web.registerSearchProvider(provider),
        registerFetchProvider: provider => this.#web.registerFetchProvider(provider),
      },
      get: () => undefined,
      effect: (setup, label) => this.#effect(setup, label),
      logger: this.#logger(),
      inject: (deps: string | string[], callback: (ctx: unknown) => unknown) =>
        this.#inject(deps, callback),
    }
    return new Proxy(services, {
      get(target, property, receiver) {
        if (typeof property === 'symbol' || property === 'then') return undefined
        if ((SERVICE_KEYS as readonly string[]).includes(property)) {
          return Reflect.get(target, property, receiver)
        }
        throw new AdapterError(
          'SPINE_SERVICE',
          `ctx.${String(property)} is not provided by @artec/clat-dsh-adapter (supported: ` +
            `${SERVICE_KEYS.join(', ')}). Spine/UI services are out of scope for the MCP ` +
            `leaf-plugin bridge — restructure the capability as a tool (ctx.tools.register) ` +
            `or use CLAT's native features.`,
        )
      },
    })
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
    return () => {
      this.#tools.delete(tool.name)
    }
  }

  /** tools/list projection (name/description/compiled parameters). */
  listTools(): ToolDefinitionLike[] {
    // 内置 web_search 仅在 ≥1 个 search provider 注册、且插件未以同名
    // 工具覆盖时出现（Phase 3b；同名时插件注册表优先，不重复列出）。
    const builtIns =
      this.#web.hasSearchProvider() && !this.#tools.has('web_search') ? [this.#web.webSearchTool()] : []
    return [...this.#tools.values(), ...builtIns]
  }

  /** tools/call: execute + render. Throws propagate to isError results. */
  async callTool(name: string, args: unknown, callId: string): Promise<{ content: ContentBlockLike[]; structuredContent: unknown }> {
    this.#assertLive()
    const tool = this.#tools.get(name) ?? this.#builtinTool(name)
    if (tool === undefined) {
      throw new AdapterError('UNKNOWN_TOOL', `unknown tool: ${name}`)
    }
    const exec: ToolRunContextLike = {
      callId,
      name: tool.name,
      arguments: args,
      signal: new AbortController().signal,
      deferContext: () => {
        this.#host.log(`warn: ${name} called exec.deferContext(); the MCP bridge has no agent-loop context ferry — ignored`)
      },
      concludeTurn: () => {
        this.#host.log(`warn: ${name} called exec.concludeTurn(); the MCP bridge has no agent loop — ignored`)
      },
    }
    const value = await tool.execute(args, exec)
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
    return undefined
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
      const result = await this.#host.sampling(params)
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

  #effect(setup: () => Generator<unknown, unknown, unknown>, label?: string): () => Promise<void> {
    this.#assertLive()
    void label
    const iterator = setup()
    const first = iterator.next()
    const cleanup = normalizeCleanup(first.value)
    const entry = () => {
      void iterator.return?.(undefined)
      for (const fn of cleanup) fn()
    }
    this.#cleanups.push(entry)
    return async () => {
      const index = this.#cleanups.indexOf(entry)
      if (index >= 0) {
        this.#cleanups.splice(index, 1)
        entry()
      }
    }
  }

  #logger() {
    const prefix = `[dsh:${this.#pluginName}]`
    const emit = (level: string) => (...args: unknown[]) => {
      this.#host.log(prefix, level, ...args)
    }
    return {
      debug: emit('debug'),
      info: emit('info'),
      warn: emit('warn'),
      error: emit('error'),
      success: emit('success'),
    }
  }

  /** LIFO dispose (INV-D4): stdin EOF / shutdown path. */
  async disposeAll(): Promise<void> {
    if (this.#disposed) return
    this.#disposed = true
    while (this.#cleanups.length > 0) {
      const cleanup = this.#cleanups.pop()
      if (cleanup !== undefined) await cleanup()
    }
    this.#tools.clear()
    this.#web.dispose()
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
