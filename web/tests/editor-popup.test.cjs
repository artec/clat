const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function harness() {
  const placed = [];
  const listeners = {document: [], window: []};
  const listen = (bucket, type, fn, options) => {
    const row = {type, fn};
    bucket.push(row);
    options?.signal?.addEventListener('abort', () => {
      const index = bucket.indexOf(row);
      if (index >= 0) bucket.splice(index, 1);
    }, {once: true});
  };
  const document = {
    documentElement: {},
    activeElement: null,
    addEventListener: (type, fn, options) => listen(listeners.document, type, fn, options),
    dispatch(type, target) {
      for (const row of [...listeners.document]) {
        if (row.type === type) row.fn({target, preventDefault() {}, stopPropagation() {}});
      }
    },
  };
  const window = {
    addEventListener: (type, fn, options) => listen(listeners.window, type, fn, options),
  };
  const node = (tag, className = '') => {
    const element = {
      tag, className, children: [], dataset: {}, style: {}, disabled: false, parent: null,
      attrs: new Map(),
      classList: {contains: name => element.className.split(/\s+/).includes(name), add() {}},
      setAttribute: (name, value) => element.attrs.set(name, String(value)),
      getAttribute: name => element.attrs.get(name) ?? null,
      append(...kids) { for (const kid of kids) { kid.parent = element; element.children.push(kid); } },
      remove() {
        const index = element.parent?.children.indexOf(element) ?? -1;
        if (index >= 0) element.parent.children.splice(index, 1);
        element.parent = null;
      },
      contains(target) {
        return target === element || element.children.some(child => child.contains?.(target));
      },
      closest: () => null,
      focus() { document.activeElement = element; },
      getBoundingClientRect: () => ({left: 12, bottom: 48, top: 28, right: 140}),
      querySelector(selector) { return element.querySelectorAll(selector)[0] ?? null; },
      querySelectorAll(selector) {
        const found = [];
        const walk = current => {
          for (const child of current.children) {
            if (selector === 'button:not(:disabled)' && child.tag === 'button' && !child.disabled) found.push(child);
            walk(child);
          }
        };
        walk(element);
        return found;
      },
    };
    return element;
  };
  document.body = node('body');
  const scope = {
    AbortController,
    document,
    window,
    el: (tag, className) => node(tag, className || ''),
    svgIcon: () => node('svg'),
    positionSessionMenu: (menu, x, y, source) => placed.push({menu, x, y, source}),
    getComputedStyle: () => ({getPropertyValue: name => name === '--menu-gap' ? '9px' : ''}),
    context: {isActive: () => true},
    updateRunState() {},
    devText: key => key,
  };
  vm.createContext(scope);
  vm.runInContext(fs.readFileSync('web/development/editor-ui.js', 'utf8'), scope);
  const item = {key: 'files', icon: 'folder', label: '项目文件', run() { scope.runs.push('files'); }};
  scope.runs = [];
  scope.placed = placed;
  scope.node = node;
  scope.item = item;
  scope.heading = (className = 'resource-section-select') => {
    const source = node('button', className);
    source.setAttribute('aria-haspopup', 'menu');
    source.append(node('span'));
    return source;
  };
  scope.activate = source => {
    document.dispatch('pointerdown', source.children[0] || source);
    scope.editorPopup([item], source);
  };
  return scope;
}

function menus(scope) {
  return scope.document.body.children.filter(child => child.className === 'session-context-menu');
}

test('a section heading opens its menu and the next press closes it', () => {
  const scope = harness();
  const source = scope.heading();
  scope.activate(source);
  assert.equal(menus(scope).length, 1);
  assert.equal(source.getAttribute('aria-expanded'), 'true');
  assert.ok(scope.placed.at(-1).y > source.getBoundingClientRect().bottom);
  scope.activate(source);
  assert.equal(menus(scope).length, 0);
  assert.equal(source.getAttribute('aria-expanded'), 'false');
});

test('a press outside the section menu closes it, and a menu choice still runs', async () => {
  const scope = harness();
  const source = scope.heading();
  scope.activate(source);
  const choice = menus(scope)[0].querySelector('button:not(:disabled)');
  scope.document.dispatch('pointerdown', choice);
  assert.equal(menus(scope).length, 1);
  scope.document.dispatch('pointerdown', scope.node('div'));
  assert.equal(menus(scope).length, 0);
  scope.activate(source);
  await menus(scope)[0].querySelector('button:not(:disabled)').onclick();
  assert.deepEqual(scope.runs, ['files']);
  assert.equal(menus(scope).length, 0);
});

test('a cursor menu still closes when its source is pressed', () => {
  const scope = harness();
  const source = scope.heading('review-file');
  scope.editorMenu([scope.item], source, 4, 5);
  assert.equal(scope.placed.at(-1).y, 5);
  scope.document.dispatch('pointerdown', source);
  assert.equal(menus(scope).length, 0);
});
