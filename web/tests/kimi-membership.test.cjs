const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

class FakeNode {
  constructor(tag, css, content) {
    this.tag = tag;
    this.css = css;
    this.text = typeof content === 'function' ? content() : content;
    this.children = [];
    this.handlers = {};
    this.classList = { add() {} };
  }
  append(...children) { this.children.push(...children); }
  setAttribute() {}
  addEventListener(event, handler) { this.handlers[event] = handler; }
  querySelectorAll(tag) {
    return this.children.flatMap(child => [
      ...(child.tag === tag ? [child] : []),
      ...child.querySelectorAll(tag),
    ]);
  }
  remove() {}
}

function harness(view) {
  const host = new FakeNode('div');
  const calls = [];
  const scope = {
    el: (tag, css, content) => new FakeNode(tag, css, content),
    modelField: name => name === 'presets' ? host : new FakeNode('div'),
    modelCapabilityName: name => name,
    settingsUi: key => key,
    CLATI18n: {attribute() {}, bind() {}, msg: key => key},
    rpc: async (method, params) => {
      calls.push({method, params});
      if (method === 'model.preset.tier') view.kimi_tier = params.tier;
    },
    refreshWorkbench: async () => {},
    modelAction: (_button, callback) => callback(),
  };
  scope.refreshModelSettings = async () => scope.renderSettingsPresets(view);
  vm.createContext(scope);
  vm.runInContext(fs.readFileSync('web/settings-models.js', 'utf8'), scope);
  const kimi = () => host.children.find(element =>
    element.children[0].children[0].text === 'Kimi Coding Plan');
  const tier = () => kimi().children[0].children.find(node => node.css === 'model-vendor-entry');
  return {scope, host, calls, kimi, tier};
}
const kimiPreset = {id: 'k3.1-pro', name: 'Kimi K3.1 Pro',
  vendor: 'Kimi Coding Plan', image_input: true};
const qwenPreset = {id: 'qwen4-max', name: 'Qwen4 Max',
  vendor: 'Qwen Token Plan', image_input: true};

test('Kimi tier radios use the Qwen heading layout and persist selection immediately', async () => {
  const view = {presets: [qwenPreset, kimiPreset], active_profile: null,
    current: {preset: 'qwen4-max'}, kimi_tier: 'plus', qwen_entry: 'aliyun'};
  const h = harness(view);
  h.scope.renderSettingsPresets(view);
  const tier = h.tier();
  assert.equal(tier.tag, 'div');
  assert.equal(tier.css, 'model-vendor-entry');
  const radios = tier.querySelectorAll('input');
  assert.deepEqual(radios.map(r => r.value), ['plus', 'pro']);
  assert.equal(radios[0].checked, true);
  assert.equal(radios[1].checked, false);

  // Inactive Kimi: changing the account tier commits now, before Apply.
  tier.handlers.change({target: radios[1]});
  assert.equal(h.kimi().children[1].children[2].disabled, true);
  await new Promise(setImmediate);
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].method, 'model.preset.tier');
  assert.equal(h.calls[0].params.tier, 'pro');
  assert.equal(radios[1].checked, true);
  assert.equal(h.kimi().children[1].children[2].disabled, false);

  // Re-render and switch active model: persisted server view stays authoritative.
  view.current.preset = 'k3.1-pro';
  h.scope.renderSettingsPresets(view);
  assert.equal(h.tier().querySelectorAll('input')[1].checked, true);
  h.kimi().children[1].children[0].value = 'k3.1-pro';
  await h.kimi().children[1].children[2].onclick();
  assert.equal(h.calls.at(-1).method, 'model.preset.select');
  assert.equal(h.calls.at(-1).params.id, 'k3.1-pro');
});
