/**
 * Cordis-compatible, process-local event bus used by the adapter context.
 *
 * The dispatcher intentionally follows vendored Cordis' observable semantics:
 * `emit` is synchronous, `parallel` aggregates rejected listeners, `serial`
 * and `bail` stop on the first bail value, and `waterfall` composes listeners
 * around the final continuation. Context filters/isolates are a host concern
 * and therefore do not exist in this single-plugin bridge.
 */

export interface EventOptionsLike {
  prepend?: boolean
  global?: boolean
}

type Listener = (...args: unknown[]) => unknown

interface Hook {
  callback: Listener
  options: EventOptionsLike
}

/** Cordis bail values are everything except null, false, and undefined. */
export function isBailed(value: unknown): boolean {
  return value !== null && value !== false && value !== undefined
}

export class EventBus {
  readonly #hooks = new Map<string | symbol, Hook[]>()
  readonly #trackCleanup: (cleanup: () => unknown) => void

  constructor(trackCleanup: (cleanup: () => unknown) => void) {
    this.#trackCleanup = trackCleanup
  }

  on(
    name: string | symbol,
    listener: Listener,
    options: boolean | EventOptionsLike = {},
  ): () => boolean {
    if (typeof listener !== 'function') throw new TypeError('ctx.on: listener must be a function')
    const normalized = typeof options === 'boolean' ? { prepend: options } : options
    const hooks = this.#hooks.get(name) ?? []
    const hook = { callback: listener, options: normalized }
    if (normalized.prepend === true) hooks.unshift(hook)
    else hooks.push(hook)
    this.#hooks.set(name, hooks)
    let active = true
    const dispose = () => {
      if (!active) return false
      active = false
      const current = this.#hooks.get(name)
      const index = current?.indexOf(hook) ?? -1
      if (index < 0 || current === undefined) return false
      current.splice(index, 1)
      if (current.length === 0) this.#hooks.delete(name)
      return true
    }
    this.#trackCleanup(dispose)
    return dispose
  }

  once(
    name: string | symbol,
    listener: Listener,
    options: boolean | EventOptionsLike = {},
  ): () => boolean {
    let dispose: () => boolean = () => false
    dispose = this.on(name, function (this: unknown, ...args: unknown[]) {
      dispose()
      return listener.apply(this, args)
    }, options)
    return dispose
  }

  emit(...raw: unknown[]): void {
    const { thisArg, name, args } = this.#dispatchArgs(raw)
    for (const listener of this.#listeners(name)) listener.apply(thisArg, args)
  }

  async parallel(...raw: unknown[]): Promise<void> {
    const { thisArg, name, args } = this.#dispatchArgs(raw)
    const results = await Promise.allSettled(
      this.#listeners(name).map(async listener => listener.apply(thisArg, args)),
    )
    const failures = results.flatMap(result => result.status === 'rejected' ? [result.reason] : [])
    if (failures.length > 0) throw new AggregateError(failures)
  }

  async serial(...raw: unknown[]): Promise<unknown> {
    const { thisArg, name, args } = this.#dispatchArgs(raw)
    for (const listener of this.#listeners(name)) {
      const result = await listener.apply(thisArg, args)
      if (isBailed(result)) return result
    }
    return undefined
  }

  bail(...raw: unknown[]): unknown {
    const { thisArg, name, args } = this.#dispatchArgs(raw)
    for (const listener of this.#listeners(name)) {
      const result = listener.apply(thisArg, args)
      if (isBailed(result)) return result
    }
    return undefined
  }

  waterfall(...raw: unknown[]): unknown {
    const { thisArg, name, args } = this.#dispatchArgs(raw)
    const listeners = this.#listeners(name)
    const inner = args.pop()
    if (typeof inner !== 'function') {
      throw new TypeError('ctx.waterfall: final argument must be a next function')
    }
    const next = (): unknown => {
      const listener = listeners.shift() ?? inner
      return listener.apply(thisArg, args)
    }
    args.push(next)
    return next()
  }

  clear(): void {
    this.#hooks.clear()
  }

  #listeners(name: string | symbol): Listener[] {
    return (this.#hooks.get(name) ?? []).map(hook => hook.callback)
  }

  /** Cordis treats a leading object/function as the explicit listener `this`. */
  #dispatchArgs(raw: unknown[]): { thisArg: unknown; name: string | symbol; args: unknown[] } {
    const args = [...raw]
    const first = args[0]
    // Match Cordis literally: `typeof null === 'object'`, so an explicit null
    // is consumed as the dispatch receiver instead of as the event name.
    const hasThis = typeof first === 'object' || typeof first === 'function'
    const thisArg = hasThis ? args.shift() : null
    const name = args.shift()
    if (typeof name !== 'string' && typeof name !== 'symbol') {
      throw new TypeError('event name must be a string or symbol')
    }
    return { thisArg, name, args }
  }
}
