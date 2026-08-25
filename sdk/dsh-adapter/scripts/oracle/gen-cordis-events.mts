import { errorShape, importHarness, writeOracle } from './common.mts'

const { Context, isBailed } = await importHarness('vendor/cordis/src/index.ts')

const orderCtx = new Context()
const order: string[] = []
const dispose = orderCtx.on('oracle/order', () => order.push('normal'))
orderCtx.on('oracle/order', () => order.push('prepended'), { prepend: true })
orderCtx.once('oracle/order', () => order.push('once'))
orderCtx.emit('oracle/order')
orderCtx.emit('oracle/order')
const firstDispose = dispose()
const secondDispose = dispose()
orderCtx.emit('oracle/order')

const receiverCtx = new Context()
const receiver = { marker: 'explicit-receiver' }
let observedReceiver: unknown
let observedArgument: unknown
receiverCtx.on('oracle/receiver', function (this: unknown, argument: unknown) {
  observedReceiver = this
  observedArgument = argument
})
receiverCtx.emit(receiver, 'oracle/receiver', 7)

const bailValues = [undefined, null, false, true, 0, '', [], {}]
const bailMatrix = bailValues.map(value => ({
  label: value === undefined ? 'undefined' : JSON.stringify(value),
  bailed: isBailed(value),
}))

const bailCtx = new Context()
const bailTrace: string[] = []
bailCtx.on('oracle/bail', () => { bailTrace.push('undefined'); return undefined })
bailCtx.on('oracle/bail', () => { bailTrace.push('false'); return false })
bailCtx.on('oracle/bail', () => { bailTrace.push('zero'); return 0 })
bailCtx.on('oracle/bail', () => { bailTrace.push('after'); return 'after' })
const bailResult = bailCtx.bail('oracle/bail')

const serialCtx = new Context()
const serialTrace: string[] = []
serialCtx.on('oracle/serial', async () => { serialTrace.push('a'); return null })
serialCtx.on('oracle/serial', async () => { serialTrace.push('b'); return 'stop' })
serialCtx.on('oracle/serial', async () => { serialTrace.push('c'); return undefined })
const serialResult = await serialCtx.serial('oracle/serial')

const parallelCtx = new Context()
const parallelTrace: string[] = []
parallelCtx.on('oracle/parallel', async () => { parallelTrace.push('a'); throw new Error('first') })
parallelCtx.on('oracle/parallel', async () => { parallelTrace.push('b'); return undefined })
parallelCtx.on('oracle/parallel', async () => { parallelTrace.push('c'); throw new TypeError('second') })
let parallelError: unknown
try {
  await parallelCtx.parallel('oracle/parallel')
} catch (error) {
  const aggregate = error as AggregateError
  parallelError = {
    ...errorShape(error),
    errors: Array.from(aggregate.errors ?? [], errorShape),
  }
}

const waterfallCtx = new Context()
const waterfallTrace: string[] = []
waterfallCtx.on('oracle/waterfall', (value: string, next: () => string) => {
  waterfallTrace.push(`a-before:${value}`)
  const nested = next()
  waterfallTrace.push(`a-after:${nested}`)
  return `A(${nested})`
})
waterfallCtx.on('oracle/waterfall', (value: string, next: () => string) => {
  waterfallTrace.push(`b-before:${value}`)
  return `B(${next()})`
})
const waterfallResult = waterfallCtx.waterfall(
  'oracle/waterfall',
  'input',
  () => { waterfallTrace.push('inner'); return 'base' },
)

await writeOracle('cordis-events.json', {
  registration: { order, firstDispose, secondDispose },
  receiver: {
    sameObject: observedReceiver === receiver,
    marker: (observedReceiver as { marker?: string })?.marker,
    argument: observedArgument,
  },
  bail: { matrix: bailMatrix, trace: bailTrace, result: bailResult },
  serial: { trace: serialTrace, result: serialResult },
  parallel: { trace: parallelTrace, error: parallelError },
  waterfall: { trace: waterfallTrace, result: waterfallResult },
})
