import assert from 'node:assert/strict'
import test from 'node:test'
import { EventBus } from '../src/events.js'
import { SystemPromptSeam } from '../src/system-prompt.js'

function seam(): { prompt: SystemPromptSeam; events: EventBus } {
  const cleanups: (() => unknown)[] = []
  const events = new EventBus(cleanup => cleanups.push(cleanup))
  return { prompt: new SystemPromptSeam(events, cleanup => cleanups.push(cleanup)), events }
}

test('systemPrompt orders sections and contexts and interpolates strict variables', async () => {
  const { prompt } = seam()
  prompt.section({ name: 'later', order: 20, text: 'cwd={{cwd}}' })
  prompt.section({ name: 'first', order: 10, text: context => `model={{model}}/${String(context.provider)}` })
  prompt.context({ name: 'policy', order: 1, text: 'provider={{provider}}' })
  const rendered = await prompt.render({ cwd: '/work', provider: 'p', model: 'm' })
  assert.equal(rendered.prompt, 'model=m/p\n\ncwd=/work')
  assert.equal(
    rendered.context,
    'Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\nprovider=p',
  )
})

test('systemPrompt duplicate, invalid order, undefined variable, and complete-section rules', async () => {
  const { prompt } = seam()
  prompt.section({ name: 'x', order: 1, text: '{{model}}' })
  assert.throws(() => prompt.section({ name: 'x', order: 2, text: 'dup' }), /already registered/)
  assert.throws(() => prompt.context({ name: 'bad', order: Number.NaN, text: '' }), /finite/)
  await assert.rejects(prompt.render(), /has no value/)

  const complete = seam().prompt
  complete.section({ name: 'exact', order: 1, text: 'only', complete: true })
  complete.section({ name: 'ignored', order: 2, text: 'ignored' })
  assert.equal((await complete.render()).prompt, 'only')
  complete.section({ name: 'second', order: 3, text: 'second', complete: true })
  await assert.rejects(complete.assemble(), /multiple complete/)
})

test('systemPrompt assemble waterfall and runtime-context suppression match DSH', async () => {
  const { prompt, events } = seam()
  prompt.section({ name: 'base', order: 0, text: 'base' })
  prompt.context({ name: 'dynamic', order: 0, text: 'runtime' })
  events.on('system-prompt/assemble', async (assembly, _context, next) => {
    const value = await (next as () => Promise<{ sections: { name: string; text: string }[] }>)()
    value.sections.push({ name: 'listener', text: 'listener' })
    return value
  })
  const suppress = prompt.suppressRuntimeContext()
  const rendered = await prompt.render()
  assert.equal(rendered.prompt, 'base\n\nlistener')
  assert.equal(rendered.context, '')
  suppress()
  assert.match((await prompt.render()).context, /runtime/)
})
