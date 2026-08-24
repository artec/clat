/**
 * Static-Cordis implementation of DSH's `ctx.systemPrompt` registry.
 *
 * It preserves DSH registration, ordering, complete-section, variable,
 * context, tool-provider, change-event, and assemble-waterfall semantics.
 * The bridge has one global scope; per-Agent scoped shadowing remains a native
 * CLAT concern and is deliberately not emulated in JavaScript.
 */

import type { EventBus } from './events.js'
import type {
  AssembleContextLike,
  PromptAssemblyLike,
  PromptContextLike,
  PromptSectionLike,
  SystemPromptLike,
  ToolProviderResultLike,
} from './types.js'

const VARIABLE_NAME = /^[a-z][a-z0-9_]*$/
const GROUP_AT = /^\{\{([^{}]*)\}\}/

type ToolProvider = (context: AssembleContextLike) => ToolProviderResultLike
type VariableProvider = (context: AssembleContextLike) => string | undefined

export class SystemPromptSeam implements SystemPromptLike {
  readonly #events: EventBus
  readonly #trackCleanup: (cleanup: () => unknown) => void
  readonly #sections = new Map<string, PromptSectionLike>()
  readonly #contexts = new Map<string, PromptContextLike>()
  readonly #toolProviders: ToolProvider[] = []
  readonly #variables = new Map<string, VariableProvider>()
  #runtimeContextSuppressors = 0

  constructor(events: EventBus, trackCleanup: (cleanup: () => unknown) => void) {
    this.#events = events
    this.#trackCleanup = trackCleanup
  }

  section(section: PromptSectionLike): () => void {
    this.#validateNamedEntry('section', section.name, section.order)
    if (this.#sections.has(section.name)) {
      throw new Error(`prompt section "${section.name}" is already registered`)
    }
    this.#sections.set(section.name, section)
    this.#changed()
    return this.#trackedDisposer(() => this.#sections.delete(section.name))
  }

  context(context: PromptContextLike): () => void {
    this.#validateNamedEntry('context', context.name, context.order)
    if (this.#contexts.has(context.name)) {
      throw new Error(`prompt context "${context.name}" is already registered`)
    }
    this.#contexts.set(context.name, context)
    this.#changed()
    return this.#trackedDisposer(() => this.#contexts.delete(context.name))
  }

  suppressRuntimeContext(): () => void {
    this.#runtimeContextSuppressors += 1
    this.#changed()
    return this.#trackedDisposer(() => {
      this.#runtimeContextSuppressors = Math.max(0, this.#runtimeContextSuppressors - 1)
      return true
    })
  }

  tools(provider: ToolProvider): () => void {
    if (typeof provider !== 'function') throw new TypeError('systemPrompt.tools: provider must be a function')
    this.#toolProviders.push(provider)
    this.#changed()
    return this.#trackedDisposer(() => {
      const index = this.#toolProviders.indexOf(provider)
      if (index < 0) return false
      this.#toolProviders.splice(index, 1)
      return true
    })
  }

  variable(name: string, provider: VariableProvider): () => void {
    if (!VARIABLE_NAME.test(name)) {
      throw new Error(`invalid prompt variable name "${name}" (must match ${String(VARIABLE_NAME)})`)
    }
    if (typeof provider !== 'function') throw new TypeError('systemPrompt.variable: provider must be a function')
    if (this.#variables.has(name)) {
      throw new Error(`prompt variable "${name}" is already registered`)
    }
    this.#variables.set(name, provider)
    this.#changed()
    return this.#trackedDisposer(() => this.#variables.delete(name))
  }

  async assemble(context: AssembleContextLike = {}): Promise<PromptAssemblyLike> {
    const variables: Record<string, string | undefined> = {
      cwd: typeof context.cwd === 'string' ? context.cwd : process.cwd(),
      provider: typeof context.provider === 'string' ? context.provider : undefined,
      model: typeof context.model === 'string' ? context.model : undefined,
      ...(context.variables ?? {}),
    }
    for (const [name, provider] of this.#variables) variables[name] = provider(context)

    const definitions = [...this.#sections.values()].sort((a, b) => a.order - b.order)
    const complete = definitions.filter(section => section.complete === true)
    if (complete.length > 1) {
      throw new Error(`multiple complete prompt sections are active: ${complete.map(section => JSON.stringify(section.name)).join(', ')}`)
    }
    const sections = definitions.map(section => ({
      name: section.name,
      text: typeof section.text === 'function' ? section.text(context) : section.text,
    }))
    const contexts = this.#runtimeContextSuppressors > 0 ? [] : [...this.#contexts.values()]
      .sort((a, b) => a.order - b.order)
      .map(entry => ({
        name: entry.name,
        text: typeof entry.text === 'function' ? entry.text(context) : entry.text,
      }))
    const tools = this.#toolProviders.flatMap(provider => provider(context).schemas.map(schema => ({
      name: schema.name,
      description: schema.description,
      parameters: structuredClone(schema.parameters),
    }))).sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)
    const assembly: PromptAssemblyLike = { sections, contexts, tools, variables }
    const transformed = await this.#events.waterfall(
      this, 'system-prompt/assemble', assembly, context,
      () => Promise.resolve(assembly),
    ) as PromptAssemblyLike
    return {
      ...transformed,
      sections: complete[0] === undefined
        ? transformed.sections
        : sections.filter(section => section.name === complete[0]?.name),
      contexts: this.#runtimeContextSuppressors > 0 ? [] : transformed.contexts,
    }
  }

  async render(context: AssembleContextLike = {}): Promise<{
    prompt: string
    context: string
    assembly: PromptAssemblyLike
  }> {
    const assembly = await this.assemble(context)
    const prompt = assembly.sections
      .map(section => interpolate(section, assembly.variables, 'section'))
      .filter(text => text.length > 0)
      .join('\n\n')
    const body = assembly.contexts
      .map(entry => interpolate(entry, assembly.variables, 'context'))
      .filter(text => text.length > 0)
      .join('\n\n')
    return {
      prompt,
      context: body.length === 0
        ? ''
        : `Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\n${body}`,
      assembly,
    }
  }

  hasContributions(): boolean {
    return this.#sections.size > 0 || this.#contexts.size > 0 || this.#toolProviders.length > 0
  }

  clear(): void {
    this.#sections.clear()
    this.#contexts.clear()
    this.#toolProviders.splice(0)
    this.#variables.clear()
    this.#runtimeContextSuppressors = 0
  }

  #validateNamedEntry(kind: 'section' | 'context', name: string, order: number): void {
    if (typeof name !== 'string' || name === '') throw new TypeError(`prompt ${kind} name must be a non-empty string`)
    if (!Number.isFinite(order)) throw new TypeError(`prompt ${kind} "${name}" order must be a finite number`)
  }

  #trackedDisposer(remove: () => boolean): () => void {
    let active = true
    const dispose = () => {
      if (!active) return
      active = false
      if (remove()) this.#changed()
    }
    this.#trackCleanup(dispose)
    return dispose
  }

  #changed(): void {
    this.#events.emit('system-prompt/change')
  }
}

function interpolate(
  input: { name: string; text: string },
  variables: Record<string, string | undefined>,
  kind: 'section' | 'context',
): string {
  const text = input.text
  let result = ''
  let last = 0
  for (let open = text.indexOf('{{'); open >= 0; open = text.indexOf('{{', last)) {
    const group = GROUP_AT.exec(text.slice(open))
    if (group === null) {
      if (text.indexOf('}}', open + 2) >= 0) {
        throw new Error(`malformed prompt variable reference at "${text.slice(open, open + 16)}…" in ${kind} "${input.name}" (references are complete simple {{name}} groups)`)
      }
      result += text.slice(last, open + 2)
      last = open + 2
      continue
    }
    const name = group[0].slice(2, -2)
    if (!VARIABLE_NAME.test(name)) {
      throw new Error(`malformed prompt variable reference "{{${name}}}" in ${kind} "${input.name}" (variable names match ${String(VARIABLE_NAME)})`)
    }
    if (!Object.hasOwn(variables, name)) {
      const known = Object.keys(variables)
      throw new Error(`unknown prompt variable "{{${name}}}" in ${kind} "${input.name}"; registered variables: ${known.length > 0 ? known.join(', ') : '(none)'}`)
    }
    const value = variables[name]
    if (value === undefined) {
      throw new Error(`prompt variable "{{${name}}}" has no value for this assembly (${kind} "${input.name}")`)
    }
    result += text.slice(last, open) + value
    last = open + group[0].length
  }
  return result + text.slice(last)
}
