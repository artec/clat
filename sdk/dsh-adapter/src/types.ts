/**
 * Runtime shapes of the DeepSeek Harness (DSH) plugin API exposed by the
 * adapter. Pinned to DSH revision `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`
 * (`dsh-v0.1.1-rc.2`).
 *
 * These are deliberately structural: a real plugin brings its own types via
 * `@deepseek-ai/cordis` / `@deepseek-ai/dsh-tools` type-only imports; at
 * runtime only the duck-typed surface below is exercised.
 */

/** `ContentBlock` subset the adapter produces or accepts (text blocks). */
export interface TextContentBlock {
  type: 'text'
  text: string
}

/** Any DSH content block; only `type: 'text'` is meaningful over MCP sampling. */
export interface ContentBlockLike {
  type: string
  [key: string]: unknown
}

/** One conversation message as `ctx.llm` receives it. */
export interface MessageLike {
  role: 'system' | 'user' | 'assistant'
  content: ContentBlockLike[]
  [key: string]: unknown
}

/** `ctx.llm.stream(options)` input (GenerateOptions). */
export interface GenerateOptionsLike {
  provider?: string
  model?: string
  messages: MessageLike[]
  system?: string
  /** Unsupported over MCP sampling: non-empty → error (INV-D1). */
  tools?: unknown[]
  temperature?: number
  maxTokens?: number
  stop?: string[]
  signal?: AbortSignal
  sessionId?: string
  purpose?: string
  [key: string]: unknown
}

/** `ctx.llm.stream` output chunk (raw streaming protocol of dsh-llm). */
export type StreamChunk =
  | { type: 'block-start'; index: number; blockType: string }
  | { type: 'text-delta'; index: number; text: string }
  | { type: 'block-end'; index: number; block: ContentBlockLike }
  | { type: 'usage'; usage: unknown }
  | { type: 'finish'; reason: FinishReasonLike }

/** FinishReason of dsh-llm (subset the adapter emits). */
export type FinishReasonLike =
  | { kind: 'stop' }
  | { kind: 'tool-calls' }
  | { kind: 'max-tokens' }
  | { kind: 'aborted'; failure: unknown }
  | { kind: 'error'; failure: unknown }

/** What `execute(args, exec)` receives (ToolRunContext subset). */
export interface ToolRunContextLike {
  readonly callId: string
  readonly rootCallId?: string
  readonly name: string
  readonly arguments: unknown
  readonly signal: AbortSignal
  deferContext(context: unknown): void
  concludeTurn(): void
}

/**
 * A compiled tool definition — exactly what `defineTool()` from
 * `@deepseek-ai/dsh-tools` returns: `parameters` is already JSON Schema.
 * Hand-built definitions with the same shape are accepted; uncompiled DSL
 * specs are rejected with guidance.
 */
export interface ToolDefinitionLike {
  name: string
  description: string
  parameters: Record<string, unknown>
  output: {
    render(args: unknown, value: unknown): ContentBlockLike[]
    [key: string]: unknown
  }
  execute(args: unknown, exec: ToolRunContextLike): Promise<unknown>
  timeoutMs?: number
  [key: string]: unknown
}

/** Event registration options (matching Cordis). */
export interface EventOptionsLike {
  prepend?: boolean
  global?: boolean
}

/** Per-assembly context accepted by DSH SystemPrompt plus bridge variables. */
export interface AssembleContextLike {
  signal?: AbortSignal
  cwd?: string
  provider?: string
  model?: string
  variables?: Record<string, string | undefined>
  [key: string]: unknown
}

export interface PromptSectionLike {
  readonly name: string
  readonly order: number
  readonly text: string | ((context: AssembleContextLike) => string)
  readonly complete?: boolean
}

export interface PromptContextLike {
  readonly name: string
  readonly order: number
  readonly text: string | ((context: AssembleContextLike) => string)
}

export interface ToolSchemaLike {
  name: string
  description: string
  parameters: Record<string, unknown>
}

export interface ToolProviderResultLike {
  readonly schemas: readonly ToolSchemaLike[]
  readonly knownNames?: readonly string[]
}

export interface PromptAssemblyLike {
  sections: { name: string; text: string }[]
  contexts: { name: string; text: string }[]
  tools: ToolSchemaLike[]
  variables: Record<string, string | undefined>
}

/** DSH `ctx.systemPrompt` service surface implemented by the adapter. */
export interface SystemPromptLike {
  section(section: PromptSectionLike): () => void
  context(context: PromptContextLike): () => void
  suppressRuntimeContext(): () => void
  tools(provider: (context: AssembleContextLike) => ToolProviderResultLike): () => void
  variable(name: string, provider: (context: AssembleContextLike) => string | undefined): () => void
  assemble(context?: AssembleContextLike): Promise<PromptAssemblyLike>
}

export interface ReflectServiceLike {
  get(key: string): unknown
  set(key: string, value: unknown): void
  provide(key: string, value?: unknown, check?: () => boolean): () => void
}

/** Static adapter counterpart of Cordis' `Fiber & PromiseLike<Fiber>`. */
export interface InjectFiberLike {
  dispose(): Promise<void>
}

export type InjectResultLike = InjectFiberLike & PromiseLike<InjectFiberLike>

/** `ctx.userQuestions.ask` input (AskUserQuestionRequest). */
export interface AskRequestLike {
  questions: AskItemLike[]
  agent?: unknown
  signal?: AbortSignal
}

/** One question (AskUserQuestionItem). */
export interface AskItemLike {
  id: string
  question: string
  detail?: string
  header?: string
  options?: AskOptionLike[]
  multiSelect?: boolean
  intent?: { kind: 'plan-review'; approve: string }
}

/** One selectable answer (AskUserQuestionOption). */
export interface AskOptionLike {
  label: string
  description?: string
}

/** `ctx.userQuestions.ask` output (AskUserQuestionAnswer). */
export interface AskAnswerLike {
  answers: { id: string; selected: string[]; custom?: string }[]
}

/** Cordis logger surface (all levels go to stderr — INV-D4). */
export interface LoggerLike {
  (name?: string): LoggerLike
  debug(...args: unknown[]): void
  info(...args: unknown[]): void
  warn(...args: unknown[]): void
  error(...args: unknown[]): void
  success(...args: unknown[]): void
}

/** The ctx the adapter hands to `apply()` (Proxy behind this interface). */
export interface DshContext {
  tools: { register(tool: ToolDefinitionLike): () => void }
  llm: { stream(options: GenerateOptionsLike): AsyncIterable<StreamChunk> }
  userQuestions: { ask(request: AskRequestLike): Promise<AskAnswerLike> }
  web: {
    registerSearchProvider(provider: WebSearchProviderLike): () => void
    registerFetchProvider(provider: WebFetchProviderLike): () => void
    search(request: WebSearchRequestLike, signal?: AbortSignal): Promise<WebSearchResultLike>
    fetch(request: WebFetchRequestLike, signal?: AbortSignal): Promise<WebFetchResultLike>
  }
  systemPrompt: SystemPromptLike
  reflect: ReflectServiceLike
  get(key: string): unknown
  set(key: string, value: unknown): void
  provide(key: string, value?: unknown): () => void
  effect(setup: () => unknown, label?: string): () => Promise<void>
  logger: LoggerLike
  inject(deps: string | string[], callback: (ctx: DshContext) => unknown): InjectResultLike
  on(name: string | symbol, listener: (...args: unknown[]) => unknown, options?: boolean | EventOptionsLike): () => boolean
  once(name: string | symbol, listener: (...args: unknown[]) => unknown, options?: boolean | EventOptionsLike): () => boolean
  emit(...args: unknown[]): void
  parallel(...args: unknown[]): Promise<void>
  serial(...args: unknown[]): Promise<unknown>
  bail(...args: unknown[]): unknown
  waterfall(...args: unknown[]): unknown
}

/** A DSH plugin object as the author exports it. */
export interface DshPluginLike {
  name?: string
  inject?: readonly string[]
  /** schemastery (or compatible) validator; called with the serveClat config. */
  Config?: (config: unknown) => unknown
  apply(ctx: DshContext, config: unknown): unknown
}

/** Static class-plugin shape (`class Foo extends Service`). */
export interface DshServiceConstructorLike {
  new (ctx: DshContext, config: unknown): unknown
  name?: string
  inject?: readonly string[]
  Config?: (config: unknown) => unknown
}

/** Author-side effect hints → MCP annotations (CLAT ToolEffect in braces). */
export type ToolHint = 'read-only' | 'network' | 'write' | 'destructive'

/** What one search-capable backend is asked to search (dsh-web types). */
export interface WebSearchRequestLike {
  query: string
  maxResults?: number
}

/** One citeable source; only `url` is mandatory. */
export interface WebSearchSourceLike {
  url: string
  title?: string
  snippet?: string
  publishedAt?: string
}

/** Normalized search outcome. */
export interface WebSearchResultLike {
  content?: string
  sources: WebSearchSourceLike[]
  truncated: boolean
}

/** A search-capable backend (registered with `ctx.web`). */
export interface WebSearchProviderLike {
  id: string
  /** Cheap local usability check; must not make network calls. */
  available(): boolean
  search(request: WebSearchRequestLike, signal?: AbortSignal): Promise<WebSearchResultLike>
}

/** Normalized fetch request used by DSH web providers. */
export interface WebFetchRequestLike {
  url: string
}

export interface WebFetchResultLike {
  url: string
  statusCode: number
  body: { kind: 'html' | 'text'; content: string }
  truncated: boolean
}

/** A fetch-capable backend registered with `ctx.web`. */
export interface WebFetchProviderLike {
  id: string
  available(): boolean
  fetch(request: WebFetchRequestLike, signal?: AbortSignal): Promise<WebFetchResultLike>
}
