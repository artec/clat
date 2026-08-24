/**
 * `@artec/clat-dsh-adapter` entry: mount an existing DSH plugin on a static
 * Cordis-shaped shim and serve its portable capabilities as an MCP stdio server
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
import type {
  DshContext,
  DshPluginLike,
  DshServiceConstructorLike,
  ToolDefinitionLike,
  ToolHint,
} from './types.js'

export { AdapterError } from './errors.js'
export { JsonRpcError } from './server.js'
export { Shim } from './shim.js'
export { McpStdioServer } from './server.js'
export { WebSeam } from './web.js'
export { EventBus, isBailed } from './events.js'
export { SystemPromptSeam } from './system-prompt.js'
export { scanDshCompatibility, writeCompatibilityMatrix } from './scanner.js'
export type { CompatibilityMatrix, CompatibilityStatus, PackageCompatibility } from './scanner.js'
export type {
  AssembleContextLike,
  AskAnswerLike,
  AskItemLike,
  AskOptionLike,
  AskRequestLike,
  ContentBlockLike,
  ClatHostContextLike,
  ClatHostLike,
  DshContext,
  DshPluginLike,
  DshServiceConstructorLike,
  EventOptionsLike,
  FinishReasonLike,
  FileSystemLike,
  FsInfoLike,
  FsTargetLike,
  GenerateOptionsLike,
  InjectFiberLike,
  InjectResultLike,
  LoggerLike,
  MessageLike,
  AgentMirrorLike,
  AgentRegistryLike,
  PromptAssemblyLike,
  PromptContextLike,
  PromptSectionLike,
  StreamChunk,
  SessionMirrorLike,
  SessionStoreLike,
  ShellLike,
  SystemPromptLike,
  TextContentBlock,
  ToolDefinitionLike,
  ToolHint,
  ToolRunContextLike,
  WebFetchProviderLike,
  WebFetchRequestLike,
  WebFetchResultLike,
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

/** Function/object plugins and static `Service` subclasses are accepted. */
type PluginInput =
  | DshPluginLike
  | DshServiceConstructorLike
  | ((ctx: DshContext, config: unknown) => unknown)

function isClassPlugin(value: unknown): boolean {
  if (typeof value !== 'function') return false
  const proto = (value as { prototype?: unknown }).prototype
  if (proto === null || proto === undefined) return false
  return Function.prototype.toString.call(value).startsWith('class ')
    || Reflect.ownKeys(proto).some(name => name !== 'constructor')
}

function normalizePlugin(plugin: PluginInput): DshPluginLike {
  if (isClassPlugin(plugin)) {
    const ServicePlugin = plugin as DshServiceConstructorLike
    return {
      name: ServicePlugin.name,
      inject: ServicePlugin.inject,
      Config: ServicePlugin.Config,
      async apply(ctx, config) {
        const instance = new ServicePlugin(ctx, config) as Record<PropertyKey, unknown>
        const hooks = instance[Symbol.for('cordis.initHooks')]
        if (Array.isArray(hooks)) {
          for (const hook of hooks) {
            if (typeof hook === 'function') hook.call(instance)
          }
        }
        const init = instance[Symbol.for('cordis.init')]
        if (typeof init !== 'function') return
        await consumeLifecycle(ctx, init.call(instance))
      },
    }
  }
  if (typeof plugin === 'function') {
    return {
      apply: plugin as (ctx: DshContext, config: unknown) => unknown,
    }
  }
  if (plugin !== null && typeof plugin === 'object' && typeof plugin.apply === 'function') {
    return plugin
  }
  throw new AdapterError('BAD_PLUGIN', 'serveClat: expected a plugin object with apply(ctx, config) or an apply function')
}

/** Consume Cordis Service.init's Promise/generator forms and own yielded cleanup. */
async function consumeLifecycle(ctx: DshContext, lifecycle: unknown): Promise<void> {
  const register = (candidate: unknown) => {
    if (typeof candidate !== 'function' && !Array.isArray(candidate)) return
    ctx.effect(function* () {
      yield candidate
    }, 'Service.init()')
  }
  if (lifecycle !== null && typeof lifecycle === 'object'
      && Symbol.asyncIterator in lifecycle) {
    for await (const candidate of lifecycle as AsyncIterable<unknown>) register(candidate)
    return
  }
  if (lifecycle !== null && typeof lifecycle === 'object'
      && Symbol.iterator in lifecycle) {
    for (const candidate of lifecycle as Iterable<unknown>) register(candidate)
    return
  }
  register(await lifecycle)
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
          `(provides: ${Shim.injectableKeys().join(', ')}). This static adapter cannot synthesize ` +
          `a missing DSH host service; compose or port that service first.`,
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
      listPrompts: () => shim?.listPrompts() ?? [],
      getPrompt: (promptName, args) => {
        if (shim === undefined) throw new AdapterError('DISPOSED', 'the adapter is shutting down')
        return shim.getPrompt(promptName, args)
      },
      callTool: (toolName, args, callId) => {
        if (shim === undefined) throw new AdapterError('DISPOSED', 'the adapter is shutting down')
        return shim.callTool(toolName, args, callId)
      },
      hostContextChanged: context => shim?.updateHostContext(context),
      dispose: () => shim?.disposeAll() ?? Promise.resolve(),
    },
  })
  shim = new Shim(
    {
      sampling: params => server.sampling(params),
      beginCall: callId => server.beginCall(callId),
      elicitation: params => server.elicitation(params),
      context: () => server.hostContext(),
      hostTool: (toolName, args) => server.hostTool(toolName, args),
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
    const context = shim.buildContext()
    await consumeLifecycle(context, normalized.apply(context, config))
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
