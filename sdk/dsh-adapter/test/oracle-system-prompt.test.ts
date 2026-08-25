import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'
import { EventBus } from '../src/events.js'
import { SystemPromptSeam } from '../src/system-prompt.js'

const fixture = JSON.parse(readFileSync(
  resolve(process.cwd(), '../../tests/fixtures/dsh-oracle/system-prompt.json'),
  'utf8',
)).sections

function seam(): { prompt: SystemPromptSeam; events: EventBus } {
  const cleanups: (() => unknown)[] = []
  const events = new EventBus(cleanup => cleanups.push(cleanup))
  return { prompt: new SystemPromptSeam(events, cleanup => cleanups.push(cleanup)), events }
}

test('SystemPrompt oracle: ordering, tools, variables, waterfall, and rendering', async () => {
  const { prompt, events } = seam()
  let changes = 0
  events.on('system-prompt/change', () => { changes += 1 })
  prompt.section({ name: 'later', order: 20, text: 'Later {{subject}}.' })
  prompt.section({ name: 'earlier', order: 10, text: 'Earlier.' })
  prompt.context({ name: 'ctx-later', order: 20, text: 'Context later.' })
  prompt.context({ name: 'ctx-earlier', order: 10, text: 'Context earlier.' })
  prompt.variable('subject', () => 'fixture')
  prompt.tools(() => ({ schemas: [
    { name: 'zeta', description: 'z', parameters: { type: 'object' } },
    { name: 'alpha', description: 'a', parameters: { type: 'object' } },
  ] }))
  events.on('system-prompt/assemble', async (assembly, _context, next) => {
    const value = assembly as { sections: { name: string; text: string }[] }
    value.sections.push({ name: 'waterfall', text: 'Waterfall.' })
    return (next as () => Promise<unknown>)()
  })
  const rendered = await prompt.render({ cwd: '/oracle' })
  const contributed = fixture.orderedAssembly.sections.filter(
    (section: { name: string }) => !['harness:identity', 'deployment:persona'].includes(section.name),
  )
  assert.deepEqual(rendered.assembly.sections, contributed)
  assert.deepEqual(rendered.assembly.contexts, fixture.orderedAssembly.contexts)
  assert.deepEqual(rendered.assembly.tools, fixture.orderedAssembly.tools)
  assert.deepEqual(rendered.assembly.variables, fixture.orderedAssembly.variables)
  assert.equal(rendered.prompt, 'Earlier.\n\nLater fixture.\n\nWaterfall.')
  assert.equal(rendered.context, fixture.rendered.context)
  assert.equal(changes, fixture.changeCount)
})

test('SystemPrompt oracle: complete, suppression, and unknown-variable failures', async () => {
  const complete = seam().prompt
  complete.section({ name: 'ordinary', order: 0, text: 'ordinary' })
  complete.section({ name: 'complete', order: 1, text: 'complete', complete: true })
  assert.deepEqual((await complete.assemble()).sections, fixture.complete.assembly.sections)
  complete.section({ name: 'complete-2', order: 2, text: 'second', complete: true })
  await assert.rejects(complete.assemble(), new RegExp(fixture.complete.conflict.message.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))

  const suppressed = seam().prompt
  let calls = 0
  suppressed.context({ name: 'secret', order: 0, text: () => `called-${++calls}` })
  const release = suppressed.suppressRuntimeContext()
  assert.deepEqual((await suppressed.assemble()).contexts, fixture.suppression.suppressedContexts)
  assert.equal(calls, fixture.suppression.providerCallsWhileSuppressed)
  release()
  assert.deepEqual((await suppressed.assemble()).contexts, fixture.suppression.restoredContexts)

  const bad = seam().prompt
  bad.section({ name: 'bad', order: 0, text: '{{missing}}' })
  await assert.rejects(bad.render(), new RegExp(fixture.badVariable.message.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
})
