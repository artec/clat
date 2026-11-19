const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

test('host-wide job activity invalidates every project list without treating completion alerts as activity', async () => {
  const source = fs.readFileSync(path.join(__dirname, '..', 'project-directory-watch.js'), 'utf8');
  const frames = [
    { kind: 'workspace.activity', session_id: 'from-other-project' },
    { kind: 'job_finished', session_id: 'from-other-project' },
    { kind: 'workspace.updated' },
    { kind: 'completed' },
  ].map(value => ['event: notification', 'data: ' + JSON.stringify(value), '', ''].join(String.fromCharCode(10))).join('');
  const encoded = Buffer.from(frames);
  let reads = 0;
  const scope = {
    reloads: 0,
    AbortController,
    TextDecoder,
    clearTimeout() {},
    setTimeout() { return 1; },
    window: { addEventListener() {} },
    fetch: async () => ({
      ok: true,
      body: { getReader: () => ({ read: async () => reads++ ? { done: true } : { done: false, value: encoded } }) },
    }),
  };
  vm.createContext(scope);
  vm.runInContext(source, scope);
  vm.runInContext('createProjectDirectoryWatch(async () => { reloads++; }).start()', scope);
  await new Promise(resolve => setImmediate(resolve));
  // One refresh on stream connect; one each for the two directory invalidations.
  assert.equal(reads, 2);
  assert.equal(scope.reloads, 3);
});
