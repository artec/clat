const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function registry(window) {
  const context = vm.createContext(window ? { window } : {});
  vm.runInContext(fs.readFileSync('web/workbench-state.js', 'utf8'), context);
  return context.CLATWorkbenchState;
}

test('native navigation sees inactive owners and pending non-text work', () => {
  const state = registry();
  state.register('visible-project', () => ({ text: false }));
  const hidden = { text: true };
  state.register('inactive-project', () => hidden);
  assert.equal(state.hasUnsavedWork(), true);
  hidden.text = false;
  assert.equal(state.hasUnsavedWork(), false);
  for (const key of ['images', 'submitting', 'composing']) {
    hidden[key] = true;
    assert.equal(state.hasUnsavedWork(), true, key);
    hidden[key] = false;
  }
});

test('installation preflight rejects hidden work before any project takes the input lock', () => {
  const window = new EventTarget();
  const state = registry(window);
  const hidden = { busy: true };
  state.register('hidden-project', () => hidden);
  let locked = false;
  window.addEventListener('clat-desktop-install-lock', event => {
    if (!event.defaultPrevented) locked = true;
  });
  assert.equal(window.dispatchEvent(new Event('clat-desktop-install-lock', { cancelable: true })), false);
  assert.equal(locked, false);
  hidden.busy = false;
  assert.equal(window.dispatchEvent(new Event('clat-desktop-install-lock', { cancelable: true })), true);
  assert.equal(locked, true);
});

test('a stale disposer cannot remove a replacement and failed reads fail closed', () => {
  const state = registry();
  const dispose = state.register('project', () => ({ text: false }));
  state.register('project', () => ({ text: true }));
  dispose();
  assert.equal(state.hasUnsavedWork(), true);
  state.register('project', () => { throw new Error('owner unavailable'); });
  assert.equal(state.snapshot().unavailable, true);
});
