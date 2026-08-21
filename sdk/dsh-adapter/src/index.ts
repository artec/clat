/**
 * `@artec/clat-dsh-adapter` entry: mount an existing DSH leaf plugin object on a
 * minimal Cordis-shaped shim and serve it as an MCP stdio server
 * (docs/research/dsh-plugin-bridge.md §4, docs/todo/dsh-adapter.md).
 *
 * Author-facing usage (the whole port, ~3 lines):
 *
 *   import { serveClat } from '@artec/clat-dsh-adapter'
 *   import { myPlugin } from './index.js'
 *   serveClat(myPlugin, { name: 'my-plugin', config: { … } })
 */

import type { Readable, Writable } from 'node:stream'
import { AdapterError, errorMessage } from './errors.js'
import { McpStdioServer } from './server.js'
import { Shim } from './shim.js'
import type { DshContext, DshPluginLike, ToolDefinitionLike, ToolHint } from './types.js'

export { AdapterError } from './errors.js'
export { JsonRpcError } from './server.js'
export { Shim } from './shim.js'
export { McpStdioServer } from './server.js'
export { WebSeam } from './web.js'
export type {
  AskAnswerLike,
  AskItemLike,
  AskOptionLike,
  AskRequestLike,
  ContentBlockLike,
  DshContext,
  DshPluginLike,
  FinishReasonLike,
  GenerateOptionsLike,
  LoggerLike,
  MessageLike,
  StreamChunk,
  TextContentBlock,
  ToolDefinitionLike,
  ToolHint,
  ToolRunContextLike,
  WebFetchProviderLike,
  WebSearchProviderLike,
  WebSearchRequestLike,
  WebSearchResultLike,
  WebSearchSourceLike,
} from './types.js'

export interface ServeClatOptions {
  /** MCP serverInfo.name; defaults to plugin.name, then `dsh-plugin`. */
  name?: string
  version?: string
  /** Passed to the plugin's `apply(ctx, config)` (validated by its `Config` export). */
  config?: unknown
  /** Author's effect declarations for their own tools (INV-D5, MCP惯例). */
  toolHints?: Record<string, ToolHint>
  /** Overridable for tests; defaults to process stdio. */
  input?: Readable
  output?: Writable
}

export interface RunningAdapter {
  readonly name: string
  readonly server: McpStdioServer
  listTools(): ToolDefinitionLike[]
  dispose(): Promise<void>
}

/** A bare `apply` function works too; class plugins are rejected (INV-D3). */
type PluginInput = DshPluginLike | ((ctx: DshContext, config: unknown) => void | Promise<void>)

function isClassPlugin(value: unknown): boolean {
  if (typeof value !== 'function') return false
  const proto = (value as { prototype?: unknown }).prototype
  if (proto === null || proto === undefined) return false
  return Object.getOwnPropertyNames(proto).some(name => name !== 'constructor')
}

function normalizePlugin(plugin: PluginInput): DshPluginLike {
  if (isClassPlugin(plugin)) {
    throw new AdapterError(
      'SPINE_SERVICE',
      'class plugins (extending Service) own host services and are not supported by the MCP ' +
        'leaf-plugin bridge; restructure as a function plugin that registers into ctx.tools',
    )
  }
  if (typeof plugin === 'function') {
    return { apply: plugin }
  }
  if (plugin !== null && typeof plugin === 'object' && typeof plugin.apply === 'function') {
    return plugin
  }
  throw new AdapterError('BAD_PLUGIN', 'serveClat: expected a plugin object with apply(ctx, config) or an apply function')
}

/**
 * Mount the plugin and serve it over MCP stdio. Resolves once `apply` has
 * settled (the initialize response is gated on it); rejects if `apply` throws.
 */
export async function serveClat(plugin: PluginInput, options: ServeClatOptions = {}): Promise<RunningAdapter> {
  const normalized = normalizePlugin(plugin)
  const name = options.name ?? normalized.name ?? 'dsh-plugin'

  for (const key of normalized.inject ?? []) {
    if (!(Shim.injectableKeys() as readonly string[]).includes(key)) {
      throw new AdapterError(
        'SPINE_SERVICE',
        `plugin "${name}" declares inject ['${key}'] which @artec/clat-dsh-adapter does not provide ` +
          `(provides: ${Shim.injectableKeys().join(', ')}). Registry/spine seams are out of scope for ` +
          `the MCP leaf-plugin bridge — see docs/todo/dsh-adapter.md §3 (INV-D3).`,
      )
    }
  }

  let config: unknown = options.config ?? {}
  if (typeof normalized.Config === 'function') {
    config = normalized.Config(config)
  }

  let shim: Shim | undefined
  let resolveReady: () => void = () => {}
  const ready = new Promise<void>(resolve => {
    resolveReady = resolve
  })
  const server = new McpStdioServer({
    name,
    version: options.version,
    toolHints: options.toolHints,
    input: options.input,
    output: options.output,
    ready,
    handler: {
      listTools: () => shim?.listTools() ?? [],
      callTool: (toolName, args, callId) => {
        if (shim === undefined) throw new AdapterError('DISPOSED', 'the adapter is shutting down')
        return shim.callTool(toolName, args, callId)
      },
      dispose: () => shim?.disposeAll() ?? Promise.resolve(),
    },
  })
  shim = new Shim(
    {
      sampling: params => server.sampling(params),
      elicitation: params => server.elicitation(params),
      get capabilities() {
        return server.clientCapabilities
      },
      log: (...args: unknown[]) => {
        process.stderr.write(`[clat-dsh-adapter] ${args.map(arg => (typeof arg === 'string' ? arg : JSON.stringify(arg))).join(' ')}\n`)
      },
    },
    name,
  )

  server.start()
  try {
    await normalized.apply(shim.buildContext(), config)
  } catch (error) {
    process.stderr.write(`[clat-dsh-adapter] plugin "${name}" apply() failed: ${errorMessage(error)}\n`)
    await server.shutdown()
    throw error
  } finally {
    resolveReady()
  }
  return {
    name,
    server,
    listTools: () => shim?.listTools() ?? [],
    dispose: () => server.shutdown(),
  }
}
