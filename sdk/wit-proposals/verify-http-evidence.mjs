// Inspect real component verdicts; never substitute for running the components.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { records, matrix } from './consumer-evidence.mjs'

if (process.argv.length !== 4) throw new Error('usage: node verify-http-evidence.mjs STOCK_LOG PATCHED_LOG')
function cancelled(record) {
  const { guest, host_before_store_drop: events, elapsed_ms: elapsed } = record
  const phase = guest.stage.startsWith('body') ? 'entity' : guest.stage
  return guest.aborted && guest.outcome.kind === 'error' && guest.outcome.name === 'AbortError'
    && events.includes(`${phase}:cancel`) && !events.includes(`${phase}:complete`) && elapsed <= 250
}
function verify(record) {
  const { guest, host_before_store_drop: events } = record
  if (guest.order === 'before') {
    assert.equal(guest.outcome.name, 'AbortError')
    assert.equal(guest.aborted, true)
    assert.deepEqual(events, [])
  } else if (guest.order === 'during') {
    assert.ok(cancelled(record), JSON.stringify(record))
  } else {
    assert.deepEqual(guest.outcome, { kind: 'success', body: 'ok' })
    assert.equal(guest.aborted, guest.order === 'after')
  }
}
const stock = records(await readFile(process.argv[2], 'utf8'), 'HTTP_CONSUMER ')
const patchedLog = await readFile(process.argv[3], 'utf8')
const patched = records(patchedLog, 'HTTP_CONSUMER ')
const sequence = records(patchedLog, 'HTTP_SEQUENCE ')
matrix(stock)
matrix(patched)
const negatives = stock.filter(record => record.guest.order === 'during')
assert.equal(negatives.length, 6)
assert.ok(negatives.every(record => !cancelled(record)), 'stock must reproduce each physical-cancellation defect')
assert.equal(sequence.length, 6)
assert.deepEqual(sequence.map(({ guest }) => `${guest.stage}/${guest.order}/${guest.trigger}`), [
  'body-parallel/during/timer', 'body/during/timer', 'headers/during/promise',
  'body/none/promise', 'headers/none/promise', 'body/after/promise',
])
for (const record of [...patched, ...sequence]) verify(record)
console.log(JSON.stringify({ stockPhysicalPreRed: 6, patchedCases: 21,
  scope: 'HTTP fixture cancellation only; dns label is request-start hold, not DNS' }))
