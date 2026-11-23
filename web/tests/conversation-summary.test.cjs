const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

const LINE_HEIGHT = 22.4;
const LINES = 5;

// A summary body whose layout follows its text and the inspector width, the
// way a block of CJK text wraps: `charsPerLine` characters per line.
function body(tag, className) {
  const classes = new Set(), attributes = new Map();
  let layout = { charsPerLine: 18, visible: true };
  const node = {
    tag, className, children: [], hidden: false, onclick: null, onkeydown: null,
    classList: {
      toggle(name, force) {
        const on = force === undefined ? !classes.has(name) : force;
        if (on) classes.add(name); else classes.delete(name);
        return on;
      },
      contains: name => classes.has(name),
    },
    setAttribute: (name, value) => attributes.set(name, String(value)),
    getAttribute: name => attributes.get(name) ?? null,
    removeAttribute: name => attributes.delete(name),
    append(...children) { node.children.push(...children); },
    getClientRects: () => layout.visible ? [{}] : [],
    get scrollHeight() {
      assert.ok(layout.visible, 'layout was read while the card was hidden');
      return Math.max(1, Math.ceil(node.textContent.length / layout.charsPerLine)) * LINE_HEIGHT;
    },
    get clientHeight() {
      return classes.has('is-clamped') ? Math.min(node.scrollHeight, LINES * LINE_HEIGHT) : node.scrollHeight;
    },
    click() { node.onclick?.({}); },
    layout(next) { layout = { ...layout, ...next }; },
  };
  Object.defineProperty(node, 'textContent', {
    get: () => node.children.join(''),
    set: value => { node.children = [String(value)]; },
  });
  return node;
}

const observers = [];
let selection = null;
const summary = { text: '', locale: 'zh-Hans', updated_at_ms: Date.now() };
const scope = vm.createContext({
  el: body,
  svgIcon: () => body('svg', ''),
  getComputedStyle: () => ({ lineHeight: LINE_HEIGHT + 'px' }),
  ResizeObserver: class {
    constructor(callback) { this.callback = callback; observers.push(this); }
    observe(target) { this.target = target; }
  },
  window: { getSelection: () => selection },
  CLATI18n: { msg: key => key, locale: 'zh-Hans', attribute() {}, bind: (node, name, read) => { node[name] = read(); } },
  shellText: key => key,
  state: { switching: false, sessionId: 's1', workbench: { session: { id: 's1', summary } } },
});
const source = fs.readFileSync('web/workbench-shell.js', 'utf8').replace(/\r\n/g, '\n');
vm.runInContext(source.slice(source.indexOf('const SUMMARY_LINES'), source.indexOf('function mountShellDialog')), scope);

function render(text, charsPerLine = 18) {
  scope.workbenchShell = { conversationSummary: scope.createConversationSummary() };
  const view = scope.workbenchShell.conversationSummary;
  view.text.layout({ charsPerLine, visible: true });
  summary.text = text;
  scope.renderConversationSummary();
  assert.equal(view.card.hidden, false);
  return view;
}

const key = (view, name) => {
  let prevented = false;
  view.text.onkeydown({ key: name, preventDefault: () => { prevented = true; } });
  return prevented;
};

test('a summary longer than five lines is clamped, faded and announced as a collapsed disclosure', () => {
  const view = render('摘'.repeat(18 * 7));
  assert.ok(view.text.classList.contains('is-overflowing'));
  assert.ok(view.text.classList.contains('is-clamped'));
  assert.equal(view.text.clientHeight, LINES * LINE_HEIGHT);
  assert.ok(view.text.scrollHeight > view.text.clientHeight);
  assert.equal(view.text.getAttribute('role'), 'button');
  assert.equal(view.text.getAttribute('tabindex'), '0');
  assert.equal(view.text.getAttribute('aria-expanded'), 'false');
});

test('clicking the body expands it in place and the next click returns it to five lines', () => {
  const view = render('摘'.repeat(18 * 7));
  view.text.click();
  assert.ok(view.text.classList.contains('is-overflowing'));
  assert.ok(!view.text.classList.contains('is-clamped'));
  assert.equal(view.text.clientHeight, view.text.scrollHeight);
  assert.equal(view.text.getAttribute('aria-expanded'), 'true');
  view.text.click();
  assert.ok(view.text.classList.contains('is-clamped'));
  assert.equal(view.text.clientHeight, LINES * LINE_HEIGHT);
  assert.equal(view.text.getAttribute('aria-expanded'), 'false');
});

test('a body that fits five lines gets no fade, focus or toggle', () => {
  const view = render('摘'.repeat(18 * LINES));
  assert.ok(!view.text.classList.contains('is-overflowing'));
  assert.ok(!view.text.classList.contains('is-clamped'));
  assert.equal(view.text.getAttribute('role'), null);
  assert.equal(view.text.getAttribute('tabindex'), null);
  assert.equal(view.text.getAttribute('aria-expanded'), null);
  view.text.click();
  assert.equal(key(view, 'Enter'), false);
  assert.equal(view.text.getAttribute('role'), null);
});

test('selecting text inside the body is reading, not a toggle', () => {
  const view = render('摘'.repeat(18 * 7));
  selection = '摘';
  try { view.text.click(); } finally { selection = null; }
  assert.ok(view.text.classList.contains('is-clamped'));
  view.text.click();
  assert.ok(!view.text.classList.contains('is-clamped'));
});

test('Enter and Space toggle the body like a click', () => {
  const view = render('摘'.repeat(18 * 7));
  assert.equal(key(view, 'Enter'), true);
  assert.equal(view.text.getAttribute('aria-expanded'), 'true');
  assert.equal(key(view, ' '), true);
  assert.equal(view.text.getAttribute('aria-expanded'), 'false');
  assert.equal(key(view, 'a'), false);
  assert.equal(view.text.getAttribute('aria-expanded'), 'false');
});

test('new text and a resized inspector are measured again', () => {
  const before = observers.length;
  const view = render('摘'.repeat(18 * 7));
  view.text.click();
  // The next summary fits: the expanded state has nothing left to control.
  summary.text = '摘'.repeat(18 * 3);
  scope.renderConversationSummary();
  assert.equal(view.text.getAttribute('role'), null);
  assert.ok(!view.text.classList.contains('is-clamped'));
  // Narrowing the inspector wraps the same text beyond five lines.
  view.text.layout({ charsPerLine: 8 });
  const observer = observers[before];
  assert.ok(observer, 'the body is observed for size changes');
  assert.equal(observer.target, view.text);
  observer.callback();
  assert.ok(view.text.classList.contains('is-overflowing'));
  assert.ok(!view.text.classList.contains('is-clamped'));
  assert.equal(view.text.getAttribute('aria-expanded'), 'true');
  view.text.click();
  assert.equal(view.text.getAttribute('aria-expanded'), 'false');
  assert.ok(view.text.classList.contains('is-clamped'));
});

test('a hidden card is not measured until it is laid out', () => {
  scope.workbenchShell = { conversationSummary: scope.createConversationSummary() };
  const view = scope.workbenchShell.conversationSummary;
  summary.text = '摘'.repeat(18 * 7);
  view.text.layout({ visible: false });
  assert.equal(view.card.hidden, true);
  scope.renderConversationSummary();
  assert.equal(view.card.hidden, false);
  assert.equal(view.text.getAttribute('role'), null);
  view.text.layout({ visible: true });
  observers.at(-1).callback();
  assert.equal(view.text.getAttribute('role'), 'button');
});
