import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'
import { EventBus, isBailed } from '../src/events.js'

const fixture = JSON.parse(readFileSync(
  resolve(process.cwd(), '../../tests/fixtures/dsh-oracle/cordis-events.json'),
  'utf8',
)).sections

function bus(): EventBus {
  return new EventBus(() => {})
}

test('Cordis oracle: registration order, once, disposal, and explicit receiver', () => {
  const events = bus()
  const order: string[] = []
  const dispose = events.on('x', () => order.push('normal'))
  events.on('x', () => order.push('prepended'), { prepend: true })
  events.once('x', () => order.push('once'))
  events.emit('x')
  events.emit('x')
  dispose()
  dispose()
  events.emit('x')
  assert.deepEqual(order, fixture.registration.order)

  const subject = { marker: 'explicit-receiver' }
  let receiver: unknown
  let argument: unknown
  events.on('receiver', function (this: unknown, value: unknown) {
    receiver = this
    argument = value
  })
  events.emit(subject, 'receiver', 7)
  assert.equal(receiver, subject)
  assert.deepEqual({ sameObject: receiver === subject, marker: subject.marker, argument }, fixture.receiver)
})

test('Cordis oracle: bail, serial, parallel, and waterfall semantics', async () => {
  assert.deepEqual([
    ['undefined', undefined], ['null', null], ['false', false], ['true', true],
    ['0', 0], ['""', ''], ['[]', []], ['{}', {}],
  ].map(([label, value]) => ({ label, bailed: isBailed(value) })), fixture.bail.matrix)

  const bail = bus()
  const bailTrace: string[] = []
  bail.on('x', () => { bailTrace.push('undefined'); return undefined })
  bail.on('x', () => { bailTrace.push('false'); return false })
  bail.on('x', () => { bailTrace.push('zero'); return 0 })
  bail.on('x', () => { bailTrace.push('after'); return 'after' })
  assert.deepEqual(
    { trace: bailTrace, result: bail.bail('x') },
    { trace: fixture.bail.trace, result: fixture.bail.result },
  )

  const serial = bus()
  const serialTrace: string[] = []
  serial.on('x', async () => { serialTrace.push('a'); return null })
  serial.on('x', async () => { serialTrace.push('b'); return 'stop' })
  serial.on('x', async () => { serialTrace.push('c'); return undefined })
  assert.deepEqual({ trace: serialTrace, result: await serial.serial('x') }, fixture.serial)

  const parallel = bus()
  const parallelTrace: string[] = []
  parallel.on('x', async () => { parallelTrace.push('a'); throw new Error('first') })
  parallel.on('x', async () => { parallelTrace.push('b') })
  parallel.on('x', async () => { parallelTrace.push('c'); throw new TypeError('second') })
  let parallelError: unknown
  try {
    await parallel.parallel('x')
  } catch (error) {
    const aggregate = error as AggregateError
    parallelError = {
      name: aggregate.name,
      message: aggregate.message,
      errors: aggregate.errors.map((member: Error) => ({ name: member.name, message: member.message })),
    }
  }
  assert.deepEqual({ trace: parallelTrace, error: parallelError }, fixture.parallel)

  const waterfall = bus()
  const waterfallTrace: string[] = []
  waterfall.on('x', (value, next) => {
    waterfallTrace.push(`a-before:${String(value)}`)
    const nested = (next as () => string)()
    waterfallTrace.push(`a-after:${nested}`)
    return `A(${nested})`
  })
  waterfall.on('x', (value, next) => {
    waterfallTrace.push(`b-before:${String(value)}`)
    return `B(${(next as () => string)()})`
  })
  const result = waterfall.waterfall('x', 'input', () => {
    waterfallTrace.push('inner')
    return 'base'
  })
  assert.deepEqual({ trace: waterfallTrace, result }, fixture.waterfall)
})
