const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

function element(matches) {
  const node = {nodeType: 1, parentElement: null};
  node.closest = selector => matches?.(selector) ? node : null;
  return node;
}
function range(id, field) {
  const boundary = field
    ? {id, nodeType: 3, parentElement: field}
    : {id, nodeType: 3, parentElement: element()};
  const value = {startContainer: boundary, startOffset: 0, endContainer: boundary, endOffset: 4};
  value.cloneRange = () => ({...value, cloneRange: value.cloneRange});
  return value;
}
function harness() {
  const listeners = [];
  const selection = {
    ranges: [], anchorNode: null,
    get rangeCount() { return this.ranges.length; },
    getRangeAt(index) { return this.ranges[index]; },
    removeAllRanges() { this.ranges = []; this.anchorNode = null; },
    addRange(next) { this.ranges = [next]; this.anchorNode = next.startContainer; },
  };
  const document = {
    addEventListener(type, fn, capture) { listeners.push({type, fn, capture: Boolean(capture)}); },
  };
  const context = {document, window: {getSelection: () => selection}};
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(path.join(__dirname, '..', 'composer-ux.js'), 'utf8'), context);
  vm.runInContext('installContextSelectionGuard()', context);
  function emit(type, event) {
    for (const row of listeners) if (row.type === type && row.capture) row.fn(event);
  }
  return {
    selection, emit,
    place(next) {
      selection.ranges = next ? [next] : [];
      selection.anchorNode = next ? next.startContainer : null;
    },
    id() { return selection.ranges[0]?.startContainer.id ?? null; },
  };
}
function click(button, target, extra) {
  return {button, target, ctrlKey: false, metaKey: false, shiftKey: false, ...extra};
}

test('a context click keeps the earlier selection and drops the block under the pointer', () => {
  const guard = harness();
  const prior = range('kept');
  guard.place(prior);
  guard.emit('pointerdown', click(2, element()));
  guard.place(range('tasks'));
  guard.emit('contextmenu', click(2, element()));
  assert.equal(guard.id(), 'kept');
  guard.place(range('history'));
  guard.emit('selectionchange');
  assert.equal(guard.id(), 'kept');
});

test('a context click on an empty selection does not keep the block it selected', () => {
  const guard = harness();
  guard.emit('pointerdown', click(2, element()));
  guard.place(range('settings'));
  guard.emit('selectionchange');
  assert.equal(guard.selection.rangeCount, 0);
});

test('the menu event does not adopt a block selected after the pointer went down', () => {
  const guard = harness();
  guard.place(range('kept'));
  guard.emit('pointerdown', click(2, element()));
  guard.place(range('resources'));
  guard.emit('contextmenu', click(2, element()));
  assert.equal(guard.id(), 'kept');
});

test('a later context click snapshots the selection the user made after the previous gesture', () => {
  const guard = harness();
  guard.emit('pointerdown', click(2, element()));
  guard.place(range('first-block'));
  guard.emit('selectionchange');
  guard.emit('pointerdown', click(0, element()));
  const chosen = range('chosen');
  guard.place(chosen);
  guard.emit('pointerdown', click(2, element()));
  guard.place(range('second-block'));
  guard.emit('selectionchange');
  assert.equal(guard.id(), 'chosen');
});

test('a left drag still selects, and a context gesture can be released from the keyboard', () => {
  const guard = harness();
  guard.emit('pointerdown', click(2, element()));
  guard.emit('pointerdown', click(0, element()));
  guard.place(range('dragged'));
  guard.emit('selectionchange');
  assert.equal(guard.id(), 'dragged');
  guard.emit('pointerdown', click(2, element()));
  guard.emit('keydown', {key: 'a', metaKey: true, ctrlKey: false, shiftKey: false, target: element()});
  guard.place(range('all'));
  guard.emit('selectionchange');
  assert.equal(guard.id(), 'all');
});

test('keys inside the context menu do not accept the block selected underneath', () => {
  const guard = harness();
  guard.place(range('kept'));
  guard.emit('pointerdown', click(2, element()));
  const menu = element(selector => selector.includes('[role="menu"]'));
  guard.emit('keydown', {key: 'ArrowDown', shiftKey: true, ctrlKey: false, metaKey: false, target: menu});
  guard.place(range('menu-block'));
  guard.emit('selectionchange');
  assert.equal(guard.id(), 'kept');
});

test('ctrl-click is a context gesture, and a text control keeps the selection it received', () => {
  const guard = harness();
  guard.place(range('kept'));
  guard.emit('pointerdown', click(0, element(), {ctrlKey: true}));
  guard.place(range('ctrl-block'));
  guard.emit('selectionchange');
  assert.equal(guard.id(), 'kept');
  for (const kind of ['textarea', 'contenteditable']) {
    const field = element(selector => selector.includes(kind));
    guard.emit('pointerdown', click(2, field));
    guard.place(range(kind, field));
    guard.emit('selectionchange');
    assert.equal(guard.id(), kind);
  }
});

test('selectstart from a context gesture is cancelled outside text controls only', () => {
  const guard = harness();
  const page = click(2, element());
  page.preventDefault = () => { page.defaultPrevented = true; };
  guard.emit('pointerdown', click(2, element()));
  guard.emit('selectstart', page);
  assert.equal(page.defaultPrevented, true);
  for (const kind of ['textarea', 'contenteditable']) {
    const field = element(selector => selector.includes(kind));
    const typing = click(2, field);
    typing.preventDefault = () => { typing.defaultPrevented = true; };
    guard.emit('pointerdown', click(2, field));
    guard.emit('selectstart', typing);
    assert.equal(typing.defaultPrevented, undefined, kind);
  }
  const drag = click(0, element());
  drag.preventDefault = () => { drag.defaultPrevented = true; };
  guard.emit('pointerdown', click(0, element()));
  guard.emit('selectstart', drag);
  assert.equal(drag.defaultPrevented, undefined);
});

test('a mouse context gesture without pointerdown still rejects the block under the pointer', () => {
  const guard = harness();
  guard.place(range('kept'));
  guard.emit('mousedown', click(2, element()));
  guard.place(range('help'));
  guard.emit('selectionchange');
  assert.equal(guard.id(), 'kept');
});
