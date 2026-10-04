// Shared shape checks for captured author consumer evidence; no component execution.
import assert from 'node:assert/strict'

export function records(log, prefix) {
  return log.split(/\r?\n/).filter(line => line.startsWith(prefix))
    .map(line => JSON.parse(line.slice(prefix.length)))
}
export function matrix(records) {
  assert.equal(records.length, 15)
  const keys = records.map(({ guest }) => `${guest.stage}/${guest.order}/${guest.trigger}`).sort()
  const expected = ['dns', 'headers', 'body'].flatMap(stage => [
    'none/promise', 'before/promise', 'during/promise', 'during/timer', 'after/promise',
  ].map(order => `${stage}/${order}`)).sort()
  assert.deepEqual(keys, expected)
}
