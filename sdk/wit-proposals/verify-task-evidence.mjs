// Captured evidence inspection only; task_probe must execute the components first.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { records, matrix } from './consumer-evidence.mjs'

if (process.argv.length !== 4) throw new Error('usage: node verify-task-evidence.mjs MUTANT_LOG FINAL_LOG')
const mutantLog = await readFile(process.argv[2], 'utf8')
const finalLog = await readFile(process.argv[3], 'utf8')
const mutant = records(mutantLog, 'TASK_CONSUMER ')
const final = records(finalLog, 'TASK_CONSUMER ')
matrix(mutant)
matrix(final)
function sequence(log) {
  const rows = records(log, 'TASK_SEQUENCE ')
  assert.deepEqual(rows.map(({ guest }) => `${guest.stage}/${guest.order}/${guest.trigger}`), [
    'body-parallel/during/timer', 'dns/during/timer', 'headers/during/promise',
    'body/none/promise', 'dns/none/promise', 'body/after/promise',
  ])
  return rows
}
const mutantSequence = sequence(mutantLog)
const finalSequence = sequence(finalLog)
for (const row of [...mutant, ...mutantSequence].filter(row => row.guest.order === 'during')) {
  assert.equal(row.guest.outcome.name, 'AbortError')
  assert.equal(row.resource_table_empty, false, 'deleting native task cancellation must leave real resources')
  assert.ok(!row.host_before_store_drop.some(event => event.endsWith(':cancel')))
}
for (const row of [...final, ...finalSequence]) {
  const { guest, host_before_store_drop: events } = row
  assert.equal(row.resource_table_empty, true)
  if (['none', 'after'].includes(guest.order)) {
    assert.deepEqual(guest.outcome, { kind: 'success', body: 'ok' })
    assert.equal(guest.aborted, guest.order === 'after')
  } else {
    assert.equal(guest.outcome.name, 'AbortError')
    assert.equal(guest.aborted, true)
    if (guest.order === 'before') assert.deepEqual(events, [])
    else {
      const phase = guest.stage.startsWith('body') ? 'entity' : guest.stage
      const count = guest.stage === 'body-parallel' ? 2 : 1
      assert.equal(events.filter(event => event === `${phase}:cancel`).length, count)
      assert.ok(!events.includes(`${phase}:complete`))
      assert.ok(row.elapsed_ms <= 250)
    }
  }
}
console.log(JSON.stringify({ consumerCancellationPreRed: 6, reuseCancellationPreRed: 3, patchedCases: 21,
  scope: 'native semantic task fixtures only; not actual DNS/HTTP or host security acceptance' }))
