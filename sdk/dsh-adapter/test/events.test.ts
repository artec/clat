import assert from 'node:assert/strict'
import test from 'node:test'
import { EventBus } from '../src/events.js'

function bus(): EventBus {
  return new EventBus(() => {})
}

test('Cordis event order, prepend, once, disposer, and explicit this', () => {
  const events = bus()
  const seen: string[] = []
  const subject = { id: 'subject' }
  events.on('x', function (this: unknown, value) {
    assert.equal(this, subject)
    seen.push(`normal:${String(value)}`)
  })
  const dispose = events.on('x', value => seen.push(`first:${String(value)}`), true)
  events.once('x', value => seen.push(`once:${String(value)}`))
  events.emit(subject, 'x', 1)
  events.emit(subject, 'x', 2)
  assert.deepEqual(seen, ['first:1', 'normal:1', 'once:1', 'first:2', 'normal:2'])
  assert.equal(dispose(), true)
  assert.equal(dispose(), false)
})

test('Cordis bail and serial stop on the first non-null/non-false value', async () => {
  const events = bus()
  const seen: string[] = []
  events.on('x', () => { seen.push('a'); return false })
  events.on('x', () => { seen.push('b'); return 0 })
  events.on('x', () => { seen.push('c'); return 'late' })
  assert.equal(events.bail('x'), 0)
  assert.deepEqual(seen, ['a', 'b'])
  seen.length = 0
  assert.equal(await events.serial('x'), 0)
  assert.deepEqual(seen, ['a', 'b'])
})

test('Cordis parallel settles all listeners and aggregates failures', async () => {
  const events = bus()
  const seen: string[] = []
  events.on('x', async () => { seen.push('a'); throw new Error('a failed') })
  events.on('x', async () => { seen.push('b'); throw new Error('b failed') })
  await assert.rejects(events.parallel('x'), error => {
    assert.ok(error instanceof AggregateError)
    assert.equal(error.errors.length, 2)
    return true
  })
  assert.deepEqual(seen, ['a', 'b'])
})

test('Cordis waterfall composes around the final continuation', async () => {
  const events = bus()
  const seen: string[] = []
  events.on('x', async (value, next) => {
    seen.push(`outer:${String(value)}`)
    const result = await (next as () => Promise<string>)()
    seen.push('outer:after')
    return `[${result}]`
  })
  events.on('x', async (_value, next) => {
    seen.push('inner')
    return (next as () => Promise<string>)()
  })
  const result = await events.waterfall('x', 'v', async () => {
    seen.push('base')
    return 'ok'
  })
  assert.equal(result, '[ok]')
  assert.deepEqual(seen, ['outer:v', 'inner', 'base', 'outer:after'])
})
