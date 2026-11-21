const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

class FakeNode {
  constructor(tag, css, content) {
    this.tag = tag;
    this.css = css || '';
    this.text = typeof content === 'function' ? content() : content;
    this.children = [];
  }
  append(...children) { this.children.push(...children); }
  replaceChildren() { this.children = []; }
}

function fixture() {
  const state = {
    sessionId: 'old-session', selectionGeneration: 1, connected: true,
    runActive: false, compactionActive: false, switching: false,
    desktopUpdateLocked: false,
    workbench: { session: { id: 'new-session' },
      model: { model: 'k3.1-pro', max_context_tokens: 262144 } },
  };
  let capacity = 262144;
  const ring = {
    dataset: {}, style: { setProperty() {} },
    classList: { toggle() {} },
    hasAttribute() { return false; },
    removeAttribute() {},
  };
  const popup = new FakeNode('section');
  const trigger = {
    classList: { toggle() {} }, setAttribute() {},
    disabled: false,
  };
  const updates = [];
  const scope = {
    state, workbenchShell: { allowance: { update(v) { updates.push(v); } } },
    document: {
      querySelector(selector) {
        if (selector === '.context-ring') return ring;
        if (selector === '.context-usage-popover') return popup;
        return null;
      },
      getElementById(id) { return id === 'context-usage' ? trigger : null; },
    },
    el: (tag, css, value) => new FakeNode(tag, css, value),
    CLATI18n: { locale: 'en-US', attribute() {} },
    experienceText: label => label,
    matchMedia: () => ({matches:true}),
    rpc: async method => {
      assert.equal(method, 'conversation.stats');
      return { context: { used: 12000, capacity }, compaction_recommended: false };
    },
  };
  const source = fs.readFileSync('web/conversation-experience.js', 'utf8');
  vm.createContext(scope);
  vm.runInContext(source.slice(source.indexOf('let usageEpoch = 0')), scope);

  function firstRow() {
    const row = popup.children.find(child => child.css === 'usage-row');
    assert.ok(row, 'context is the first row of the popover');
    assert.equal(row.children[0].text, 'context');
    return row.children[1].text;
  }
  return { state, updates, firstRow,
    refreshUsage: () => vm.runInContext('refreshUsage()', scope),
    setCapacity(value) { capacity = value; },
  };
}

test('composer context row follows the current Host model when session and model change', async () => {
  const app = fixture();
  // The workbench reply has already switched to a new session; the browser
  // copies state.sessionId immediately afterwards. The stats result belongs
  // to the new session, not the old one.
  const pending = app.refreshUsage();
  app.state.sessionId = 'new-session';
  await pending;
  assert.match(app.firstRow(), /12,000 \/ 262,144/);
  assert.equal(app.updates.at(-1).context.capacity, 262144);

  // Same Kimi model ID, membership changed from Plus to Pro.
  app.state.workbench.model.max_context_tokens = 1048576;
  app.setCapacity(1048576);
  await app.refreshUsage();
  assert.match(app.firstRow(), /12,000 \/ 1,048,576/);

  // Non-binary model windows (Grok or user-specified) are handled exactly.
  app.state.workbench.model = { model: 'grok-4.7', max_context_tokens: 500000 };
  app.setCapacity(500000);
  await app.refreshUsage();
  assert.match(app.firstRow(), /12,000 \/ 500,000/);

  app.state.workbench.model.max_context_tokens = 131072;
  app.setCapacity(131072);
  await app.refreshUsage();
  assert.match(app.firstRow(), /12,000 \/ 131,072/);
});
