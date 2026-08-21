/**
 * Minimal runtime shapes of the DeepSeek Harness (DSH) leaf-plugin API the
 * adapter hosts. Pinned to DSH revision `99f6f02f` (0.1.0-rc.7); validated
 * against the local rc.8 checkout (`141eb6f`, plugin-facing src spot-checked
 * equivalent — see docs/todo/dsh-adapter.md §2).
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
  }
  get(key: string): unknown
  effect(setup: () => Generator<unknown, unknown, unknown>, label?: string): () => Promise<void>
  logger: LoggerLike
}

/** A DSH plugin object as the author exports it. */
export interface DshPluginLike {
  name?: string
  inject?: readonly string[]
  /** schemastery (or compatible) validator; called with the serveClat config. */
  Config?: (config: unknown) => unknown
  apply(ctx: DshContext, config: unknown): void | Promise<void>
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

/** A fetch-capable backend (registered with `ctx.web`; v0 has no web_fetch tool). */
export interface WebFetchProviderLike {
  id: string
  available(): boolean
  fetch(request: { url: string }, signal?: AbortSignal): Promise<unknown>
}
