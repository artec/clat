/**
 * The `ctx.web` capability seam (Phase 3b): provider registries plus
 * execution-time provider selection, mirroring @deepseek-ai/dsh-web's
 * WebRuntime semantics 1:1 (selection never depends on registration order;
 * `maxResults` is enforced on the way back). The model-facing `web_search`
 * tool mirrors @deepseek-ai/dsh-tool-web's search.ts (queries array,
 * round-robin merge, sources output).
 */

import { AdapterError } from './errors.js'
import type {
  ToolDefinitionLike,
  WebFetchProviderLike,
  WebSearchProviderLike,
  WebSearchRequestLike,
  WebSearchResultLike,
  WebSearchSourceLike,
} from './types.js'

/** Default upper bounds, mirroring dsh-tool-web's deployment defaults. */
export const WEB_SEARCH_MAX_RESULTS = 8
export const WEB_SEARCH_MAX_QUERIES = 4

/** Shared web error codes (mirrors dsh-web's WebError codes). */
export type WebErrorCode =
  | 'WEB_DUPLICATE_PROVIDER'
  | 'WEB_PROVIDER_CONFIGURED_MISSING'
  | 'WEB_PROVIDER_CONFIGURED_UNAVAILABLE'
  | 'WEB_PROVIDER_UNAVAILABLE'
  | 'WEB_PROVIDER_AMBIGUOUS'

function webError(code: WebErrorCode, message: string): AdapterError {
  return new AdapterError(code, message)
}

/**
 * The web seam instance: registries + selection + the built-in `web_search`
 * tool. Selection inputs: `$DSH_WEB_SEARCH_PROVIDER` env (exactly as dsh-web's
 * WebRuntime reads it) or the single usable provider.
 */
export class WebSeam {
  readonly #searchProviders = new Map<string, WebSearchProviderLike>()
  readonly #fetchProviders = new Map<string, WebFetchProviderLike>()
  readonly #log: (...args: unknown[]) => void

  constructor(log: (...args: unknown[]) => void) {
    this.#log = log
  }

  /** Register a search provider; duplicate ids rejected (WEB_DUPLICATE_PROVIDER). */
  registerSearchProvider(provider: WebSearchProviderLike): () => void {
    return this.#register(this.#searchProviders, provider, 'search')
  }

  /** Register a fetch provider (accepted; v0 exposes no web_fetch tool over it). */
  registerFetchProvider(provider: WebFetchProviderLike): () => void {
    return this.#register(this.#fetchProviders, provider, 'fetch')
  }

  #register<P extends { id: string }>(store: Map<string, P>, provider: P, kind: 'search' | 'fetch'): () => void {
    if (provider === null || typeof provider !== 'object' || typeof provider.id !== 'string' || provider.id === '') {
      throw new AdapterError('WEB_BAD_PROVIDER', `ctx.web.register${kind === 'search' ? 'Search' : 'Fetch'}Provider: provider.id must be a non-empty string`)
    }
    if (store.has(provider.id)) {
      throw webError(
        'WEB_DUPLICATE_PROVIDER',
        `a web provider with id "${provider.id}" is already registered`,
      )
    }
    store.set(provider.id, provider)
    if (kind === 'fetch') {
      this.#log(`note: fetch provider "${provider.id}" registered; v0 of the adapter exposes no web_fetch tool`)
    }
    return () => {
      store.delete(provider.id)
    }
  }

  /** Whether any search provider is registered (gates the built-in tool). */
  hasSearchProvider(): boolean {
    return this.#searchProviders.size > 0
  }

  /** Drop all providers (disposeAll path). */
  dispose(): void {
    this.#searchProviders.clear()
    this.#fetchProviders.clear()
  }

  /** Run one search through the selected provider; enforce maxResults. */
  async search(request: WebSearchRequestLike, signal?: AbortSignal): Promise<WebSearchResultLike> {
    const configuredId = process.env.DSH_WEB_SEARCH_PROVIDER
    const provider = resolveProvider(this.#searchProviders, configuredId)
    const result = await provider.search(request, signal)
    return capSources(result, request.maxResults)
  }

  /** The model-facing `web_search` tool (mirrors dsh-tool-web search.ts). */
  webSearchTool(): ToolDefinitionLike {
    return {
      name: 'web_search',
      description: `Search the web for current information. Provide 1–${WEB_SEARCH_MAX_QUERIES} queries in the required queries array. Returns an optional summary answer and a list of source URLs.`,
      parameters: {
        type: 'object',
        properties: {
          queries: {
            type: 'array',
            items: { type: 'string' },
            description: `Required search queries; accepts 1–${WEB_SEARCH_MAX_QUERIES} items and merges their results.`,
          },
        },
        required: ['queries'],
      },
      output: {
        render: (_args: unknown, value: unknown) => [{ type: 'text', text: formatSearchOutput(value as WebSearchResultLike) }],
      },
      execute: async (args: unknown, exec) => {
        const queries = parseSearchArgs((args ?? {}) as { queries?: unknown })
        const result = await runSearchQueries(this, queries, WEB_SEARCH_MAX_RESULTS, exec.signal)
        return {
          ...(result.content !== undefined ? { content: result.content } : {}),
          sources: result.sources.map(projectSource),
          truncated: result.truncated,
        }
      },
    }
  }
}

/** Resolve the selected provider or throw the matching WebError (dsh-web semantics). */
function resolveProvider<P extends { id: string; available(): boolean }>(
  providers: Map<string, P>,
  configuredId: string | undefined,
): P {
  if (configuredId !== undefined) {
    const provider = providers.get(configuredId)
    if (provider === undefined) {
      throw webError('WEB_PROVIDER_CONFIGURED_MISSING', `configured web provider "${configuredId}" is not registered`)
    }
    if (!provider.available()) {
      throw webError('WEB_PROVIDER_CONFIGURED_UNAVAILABLE', `configured web provider "${configuredId}" is registered but unavailable`)
    }
    return provider
  }
  const usable = [...providers.values()].filter(provider => provider.available())
  const [single] = usable
  if (single === undefined) {
    throw webError('WEB_PROVIDER_UNAVAILABLE', 'no usable web provider is registered')
  }
  if (usable.length > 1) {
    const ids = usable.map(provider => provider.id).join(', ')
    throw webError('WEB_PROVIDER_AMBIGUOUS', `multiple usable web providers are registered (${ids}); configure one explicitly`)
  }
  return single
}

/** Enforce maxResults on a search result: truncate sources[] and flag it. */
function capSources(result: WebSearchResultLike, maxResults: number | undefined): WebSearchResultLike {
  if (maxResults === undefined || result.sources.length <= maxResults) return result
  return { ...result, sources: result.sources.slice(0, maxResults), truncated: true }
}

/** Validate `queries`: non-empty, ≤ max, non-blank; exact duplicates collapsed. */
export function parseSearchArgs(args: { queries?: unknown }): string[] {
  const queries = args.queries
  if (!Array.isArray(queries) || queries.length === 0) {
    throw new Error('queries must contain at least one query')
  }
  if (queries.length > WEB_SEARCH_MAX_QUERIES) {
    throw new Error(`queries must contain at most ${WEB_SEARCH_MAX_QUERIES} queries`)
  }
  if (queries.some(query => typeof query !== 'string' || query.trim().length === 0)) {
    throw new Error('each query must be a non-empty string')
  }
  return [...new Set(queries as string[])]
}

/** Format a search result as one model-facing text block (dsh-tool-web format). */
export function formatSearchOutput(result: WebSearchResultLike): string {
  const parts: string[] = []
  if (result.content !== undefined && result.content.length > 0) parts.push(result.content)
  if (result.sources.length > 0) {
    const lines = result.sources.map(source => {
      const label = sourceLabel(source.url, source.title)
      const meta: string[] = []
      if (source.snippet !== undefined && source.snippet.length > 0) meta.push(source.snippet)
      if (source.publishedAt !== undefined && source.publishedAt.length > 0) meta.push(`(${source.publishedAt})`)
      const suffix = meta.length > 0 ? ` — ${meta.join(' ')}` : ''
      return `- [${label}](${source.url})${suffix}`
    })
    parts.push(`Sources:\n${lines.join('\n')}`)
  } else if (result.content === undefined || result.content.length === 0) {
    parts.push('No results found.')
  }
  if (result.truncated) parts.push(`(Showing the first ${result.sources.length} sources. Refine the query for more.)`)
  parts.push('Cite the relevant URLs above as markdown links in your answer.')
  return parts.join('\n\n')
}

/** Display label for a source: its title, else its hostname. */
function sourceLabel(url: string, title: string | undefined): string {
  if (title !== undefined && title.length > 0) return title
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}

/** Project a source omitting absent optional fields (byte-identical to dsh-tool-web). */
function projectSource(source: WebSearchSourceLike): WebSearchSourceLike {
  return {
    url: source.url,
    ...(source.title !== undefined ? { title: source.title } : {}),
    ...(source.snippet !== undefined ? { snippet: source.snippet } : {}),
    ...(source.publishedAt !== undefined ? { publishedAt: source.publishedAt } : {}),
  }
}

/**
 * Run one or more searches: a single query keeps the provider's exact result;
 * multiple queries run concurrently and merge (round-robin, url-dedup, capped).
 * A failed search aborts its siblings; first failure rethrown after settle.
 */
async function runSearchQueries(
  seam: WebSeam,
  queries: string[],
  maxResults: number,
  signal: AbortSignal,
): Promise<WebSearchResultLike> {
  if (queries.length === 1) {
    const [first] = queries
    if (first === undefined) throw new Error('queries must contain at least one query')
    return seam.search({ query: first, maxResults }, signal)
  }
  const controller = new AbortController()
  const batchSignal = AbortSignal.any([signal, controller.signal])
  let firstFailure: { error: unknown } | undefined
  const results: WebSearchResultLike[] = []
  const searches = queries.map(async (query, index) => {
    try {
      results[index] = await seam.search({ query, maxResults }, batchSignal)
    } catch (error) {
      if (firstFailure === undefined) firstFailure = { error }
      controller.abort(error)
      throw error
    }
  })
  await Promise.allSettled(searches)
  if (firstFailure !== undefined) throw firstFailure.error
  return mergeSearchResults(results, maxResults)
}

/** Merge per-query results into one deduplicated, round-robin, capped result. */
function mergeSearchResults(results: WebSearchResultLike[], maxResults: number): WebSearchResultLike {
  const seen = new Set<string>()
  const sources: WebSearchSourceLike[] = []
  let sourceRanks = 0
  for (const result of results) {
    sourceRanks = Math.max(sourceRanks, result.sources.length)
  }
  let droppedSource = false
  merge: for (let rank = 0; rank < sourceRanks; rank++) {
    for (const result of results) {
      const source = result.sources[rank]
      if (source !== undefined && !seen.has(source.url)) {
        seen.add(source.url)
        if (sources.length === maxResults) {
          droppedSource = true
          break merge
        }
        sources.push(source)
      }
    }
  }
  const contents = results.flatMap(result => {
    if (result.content === undefined || result.content.length === 0) return []
    return [result.content]
  })
  return {
    ...(contents.length > 0 ? { content: contents.join('\n\n') } : {}),
    sources,
    truncated: results.some(result => result.truncated) || droppedSource,
  }
}
