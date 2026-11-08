const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
// terminal.js only declares functions at top level, so a bare sandbox can host
// the pure input-policy decision without any DOM stubs.
const ctx = vm.createContext({});
vm.runInContext(fs.readFileSync('web/development/terminal.js', 'utf8'), ctx);
const policy = ctx.terminalInputPolicy;

test('terminalInputPolicy retries transient failures with exponential backoff', () => {
  // Field-wise assertions: the result object lives in the vm realm, so
  // deepStrictEqual would compare prototypes across realms.
  assert.equal(policy({ code: 'busy' }, 1).action, 'retry');
  assert.equal(policy({ code: 'busy' }, 1).delay, 200);
  assert.equal(policy({ code: 'busy' }, 2).delay, 400);
  assert.equal(policy({ code: 'busy' }, 3).delay, 800);
});

test('terminalInputPolicy treats timeouts and network failures as transient', () => {
  assert.equal(policy({ name: 'AbortError' }, 1).action, 'retry');
  assert.equal(policy(new TypeError('fetch failed'), 1).action, 'retry');
  assert.equal(policy({}, 1).action, 'retry');
});

test('terminalInputPolicy demotes on definitive terminal errors', () => {
  const definitive = ['terminal_observer', 'terminal_sequence', 'terminal_ended',
    'terminal_missing', 'terminal_input_large', 'host_closed'];
  for (const code of definitive) {
    assert.equal(policy({ code }, 1).action, 'demote', code);
  }
  assert.equal(policy({ code: 'busy', authentication: 'invalid_credentials' }, 1).action, 'demote');
});

test('terminalInputPolicy demotes after the retry budget is exhausted', () => {
  assert.equal(policy({ code: 'busy' }, 4).action, 'demote');
  assert.equal(policy({ name: 'AbortError' }, 5).action, 'demote');
});
