// Derive the second flavor without editing the published Bun shim.
import assert from 'node:assert/strict'
const replace = (source,before,after) => {
  assert.equal(source.split(before).length-1,1,`scope transformation drift: ${before}`)
  return source.replace(before,after)
}
export function explicitScope(source) {
  source=replace(source,"import { AsyncLocalStorage } from 'node:async_hooks'\n",'')
  source=replace(source,'readonly #cleanupScope = new AsyncLocalStorage<Array<() => unknown>>()','readonly #closedScopes = new WeakSet<Array<() => unknown>>()')
  source=replace(source,'const trackCleanup = (cleanup: () => unknown) => this.#trackCleanup(cleanup)','const trackCleanup = (_cleanup: () => unknown) => {}')
  source=injectScope(source)
  source=contextScope(source)
  source=registrationScope(source)
  source=replace(source,'  #cleanupTarget(): Array<() => unknown> {\n    return this.#cleanupScope.getStore() ?? this.#cleanups\n  }',`  #assertScope(scope: Array<() => unknown>): void {
    this.#assertLive()
    if (this.#closedScopes.has(scope)) throw new AdapterError('DISPOSED', 'the injection scope is disposed')
  }

  #providedWeb(value: object, scope: Array<() => unknown>): object {
    return new Proxy(value, {
      get: (target, key) => {
        const entry = Reflect.get(target, key, target)
        if (typeof entry !== 'function') return entry
        return (...args: unknown[]) => {
          this.#assertScope(scope)
          const result = entry.apply(target, args)
          return key === 'registerSearchProvider' || key === 'registerFetchProvider'
            ? this.#trackedRegistration(result, scope) : result
        }
      },
    })
  }

  #promptScope(scope: Array<() => unknown>): DshContext['systemPrompt'] {
    const writes = new Set(['section', 'context', 'tools', 'variable', 'suppressRuntimeContext'])
    return new Proxy(this.#systemPrompt, {
      get: (target, key) => {
        const value = Reflect.get(target, key, target)
        if (typeof value !== 'function') return value
        return (...args: unknown[]) => {
          this.#assertScope(scope)
          const result = value.apply(target, args)
          return writes.has(String(key)) ? this.#trackedRegistration(result, scope) : result
        }
      },
    })
  }`)
  source=replace(source,'#trackCleanup(cleanup: () => unknown): void {\n    this.#cleanupTarget().push(cleanup)','#trackCleanup(cleanup: () => unknown, scope: Array<() => unknown>): void {\n    scope.push(cleanup)')
  source=replace(source,'#trackedRegistration<T extends () => unknown>(dispose: T): T {\n    const target = this.#cleanupTarget()','#trackedRegistration<T extends () => unknown>(dispose: T, scope: Array<() => unknown>): T {\n    const target = scope')
  assert(!source.includes('cleanupScope')&&!source.includes('#cleanupTarget'))
  return source
}
function injectScope(source) {
  source=replace(source,'#inject(deps: string | string[], callback: (ctx: DshContext) => unknown): InjectResultLike {','#inject(deps: string | string[], callback: (ctx: DshContext) => unknown, scope: Array<() => unknown>): InjectResultLike {\n    this.#assertScope(scope)')
  source=replace(source,'const parentCleanups = this.#cleanupTarget()','const parentCleanups = scope')
  source=replace(source,'this.#cleanupScope.run(ownedCleanups, () => callback(this.#context as DshContext))','callback(this.buildContext(ownedCleanups))')
  source=replace(source,'      while (ownedCleanups.length > 0) {','      this.#closedScopes.add(ownedCleanups)\n      while (ownedCleanups.length > 0) {')
  return source
}
function contextScope(source) {
  source=replace(source,'buildContext(): DshContext {\n    if (this.#context !== undefined) return this.#context','buildContext(scope: Array<() => unknown> = this.#cleanups): DshContext {\n    if (scope === this.#cleanups && this.#context !== undefined) return this.#context\n    let context: DshContext')
  const substitutions=[
    ['tools: this.#toolService()','tools: this.#toolService(scope)'],
    ['this.#web.registerSearchProvider(provider),','this.#web.registerSearchProvider(provider), scope,'],
    ['this.#web.registerFetchProvider(provider),','this.#web.registerFetchProvider(provider), scope,'],
    ['systemPrompt: this.#systemPrompt,','systemPrompt: this.#promptScope(scope),'],
    ['this.#effect(setup, label)','this.#effect(setup, label, scope)'],
    ['this.#inject(deps, callback)','this.#inject(deps, callback, scope)'],
    ['this.#events.on(name, listener, options)','this.#trackedRegistration(this.#events.on(name, listener, options), scope)'],
    ['this.#events.once(name, listener, options)','this.#trackedRegistration(this.#events.once(name, listener, options), scope)'],
    ['this.#context = new Proxy(services,','context = new Proxy(services,'],
    ['get(target, property, receiver) {','get: (target, property, receiver) => {\n        this.#assertScope(scope)'],
  ]
  for(const [before,after] of substitutions)source=replace(source,before,after)
  source=source.replaceAll('this.#get(key)','this.#get(key, context)').replaceAll('this.#set(key, value)','this.#scopedSet(key, value, scope)')
  source=replace(source,'this.#provide(key, value, check)','this.#provide(key, value, check, scope)')
  source=replace(source,'this.#provide(key, value),','this.#provide(key, value, undefined, scope),')
  source=replace(source,'    this.#hostServices.attachContext(this.#context)\n    return this.#context',`    if (scope === this.#cleanups) {
      this.#context = context
      this.#hostServices.attachContext(context)
    }
    return context`)
  source=replace(source, '    const provided = this.#provided', '    const provided = this.#provided\n    const providedViews = new Map<unknown, unknown>()')
  source=replace(source, 'if (provided.has(property)) return provided.get(property)', `if (provided.has(property)) {
          const value = provided.get(property)
          if (property !== 'web' || value === null || typeof value !== 'object') return value
          if (!providedViews.has(value)) providedViews.set(value, this.#providedWeb(value, scope))
          return providedViews.get(value)
        }`)
  source=replace(source, 'if (this.#provided.has(key)) return this.#provided.get(key)', 'if (this.#provided.has(key)) return (context as unknown as Record<string, unknown>)[key]')
  source=replace(source,'#get(key: string): unknown {','#get(key: string, context: DshContext): unknown {')
  source=replace(source,'(this.#context as unknown as Record<string, unknown> | undefined)?.[key]','(context as unknown as Record<string, unknown>)[key]')
  return source
}
function registrationScope(source) {
  const bindings=[
    ['#provide(key: string, value?: unknown, check?: () => boolean): () => void {\n    this.#assertLive()','#provide(key: string, value: unknown, check: (() => boolean) | undefined, scope: Array<() => unknown>): () => void {\n    this.#assertScope(scope)'],
    ['this.#trackCleanup(dispose)','this.#trackCleanup(dispose, scope)'],
    ["#toolService(): DshContext['tools']", "#toolService(scope: Array<() => unknown>): DshContext['tools']"],
    ['this.#registerTool(tool)','this.#registerTool(tool, scope)'],
    ['#registerTool(tool: ToolDefinitionLike): () => void {\n    this.#assertLive()','#registerTool(tool: ToolDefinitionLike, scope: Array<() => unknown>): () => void {\n    this.#assertScope(scope)'],
    ['      this.#tools.delete(tool.name)\n    })','      this.#tools.delete(tool.name)\n    }, scope)'],
    ['#effect(setup: () => unknown, label?: string): () => Promise<void> {\n    this.#assertLive()','#effect(setup: () => unknown, label: string | undefined, scope: Array<() => unknown>): () => Promise<void> {\n    this.#assertScope(scope)'],
    ['const cleanupTarget = this.#cleanupTarget()','const cleanupTarget = scope'],
    ['  #set(key: string, value: unknown): void {',`  #scopedSet(key: string, value: unknown, scope: Array<() => unknown>): void {
    this.#assertScope(scope)
    this.#set(key, value)
  }

  #set(key: string, value: unknown): void {`],
  ]
  for(const [before,after] of bindings)source=replace(source,before,after)
  // Check scope before any registration takes effect, including captured services.
  for(const kind of ['Search','Fetch']) {
    source=replace(source,`register${kind}Provider: provider => this.#trackedRegistration(\n          this.#web.register${kind}Provider(provider), scope,\n        ),`,`register${kind}Provider: provider => {\n          this.#assertScope(scope)\n          return this.#trackedRegistration(this.#web.register${kind}Provider(provider), scope)\n        },`)
  }
  for(const kind of ['on','once'])source=replace(source,`${kind}: (name, listener, options) => this.#trackedRegistration(this.#events.${kind}(name, listener, options), scope),`,`${kind}: (name, listener, options) => {\n        this.#assertScope(scope)\n        return this.#trackedRegistration(this.#events.${kind}(name, listener, options), scope)\n      },`)
  return source
}
