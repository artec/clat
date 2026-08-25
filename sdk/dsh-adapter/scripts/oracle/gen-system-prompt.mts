import { errorShape, importHarness, writeOracle } from './common.mts'

const { Context } = await importHarness('vendor/cordis/src/index.ts')
const promptMod = await importHarness('packages/core/system-prompt/src/index.ts')
const SystemPrompt = promptMod.default
const { renderPrompt, renderContextSnapshot } = promptMod

const ctx = new Context()
await ctx.plugin(SystemPrompt, { persona: 'Oracle persona.' })
const changes: string[] = []
ctx.on('system-prompt/change', () => changes.push('change'))
ctx.systemPrompt.section({ name: 'later', order: 20, text: 'Later {{subject}}.' })
ctx.systemPrompt.section({ name: 'earlier', order: 10, text: 'Earlier.' })
ctx.systemPrompt.context({ name: 'ctx-later', order: 20, text: 'Context later.' })
ctx.systemPrompt.context({ name: 'ctx-earlier', order: 10, text: 'Context earlier.' })
ctx.systemPrompt.variable('subject', () => 'fixture')
ctx.systemPrompt.tools(() => ({
  schemas: [
    { name: 'zeta', description: 'z', parameters: { type: 'object' } },
    { name: 'alpha', description: 'a', parameters: { type: 'object' } },
  ],
}))
ctx.on('system-prompt/assemble', async (assembly: any, _context: any, next: () => Promise<any>) => {
  assembly.sections.push({ name: 'waterfall', text: 'Waterfall.' })
  return next()
})
const assembly = await ctx.systemPrompt.assemble({ cwd: '/oracle' })

const completeCtx = new Context()
await completeCtx.plugin(SystemPrompt, { includeHarnessIdentity: false })
completeCtx.systemPrompt.section({ name: 'ordinary', order: 0, text: 'ordinary' })
completeCtx.systemPrompt.section({ name: 'complete', order: 1, text: 'complete', complete: true })
const complete = await completeCtx.systemPrompt.assemble()
let completeConflict: unknown
completeCtx.systemPrompt.section({ name: 'complete-2', order: 2, text: 'second', complete: true })
try {
  await completeCtx.systemPrompt.assemble()
} catch (error) {
  completeConflict = errorShape(error)
}

const suppressedCtx = new Context()
await suppressedCtx.plugin(SystemPrompt)
let contextCalls = 0
suppressedCtx.systemPrompt.context({ name: 'secret', order: 0, text: () => `called-${++contextCalls}` })
const unsuppress = suppressedCtx.systemPrompt.suppressRuntimeContext()
const suppressed = await suppressedCtx.systemPrompt.assemble()
unsuppress()
const restored = await suppressedCtx.systemPrompt.assemble()

const badVariableCtx = new Context()
await badVariableCtx.plugin(SystemPrompt, { includeHarnessIdentity: false })
badVariableCtx.systemPrompt.section({ name: 'bad', order: 0, text: '{{missing}}' })
let badVariable: unknown
try {
  renderPrompt(await badVariableCtx.systemPrompt.assemble())
} catch (error) {
  badVariable = errorShape(error)
}

await writeOracle('system-prompt.json', {
  orderedAssembly: assembly,
  rendered: {
    prompt: renderPrompt(assembly),
    context: renderContextSnapshot(assembly),
  },
  changeCount: changes.length,
  complete: { assembly: complete, conflict: completeConflict },
  suppression: {
    suppressedContexts: suppressed.contexts,
    providerCallsWhileSuppressed: 0,
    actualCallsAfterRestore: contextCalls,
    restoredContexts: restored.contexts,
  },
  badVariable,
})
