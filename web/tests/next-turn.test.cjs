const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
function client() {
  const ctx = vm.createContext({
    state: { composerGeneration: 1, draft: { epoch: 0, clientMessageId: 'image', images: [] } },
    dom: { prompt: { value: 'pending words' } },
    workspacePrefix: '', resizePrompt() {}, clearSubmittedPrompt(generation) {
      if (ctx.state.composerGeneration === generation) ctx.dom.prompt.value = '';
    }, invalidateSuggestion() {}, clearDraft() {}, updateRunState() {}, addNoticeLine() {},
    newOpaqueClientId: () => 'unique-steering', rpc: async () => ({ outcome: 'queued' }),
  });
  vm.runInContext(fs.readFileSync(process.env.QUE_COMPOSER_SOURCE || 'web/composer-ux.js', 'utf8'), ctx);
  vm.runInContext("composerScope = 'session'; composerSession = 'session';", ctx);
  return ctx;
}
test('QUE-1 unclaimed text restores alongside new input', async () => {
  const ctx = client();
  await vm.runInContext("submitComposerSteering({text:'pending words',images:[],owner:'session',draftEpoch:0,composerGeneration:1})", ctx);
  ctx.dom.prompt.value = 'new thought'; ctx.state.composerGeneration++;
  vm.runInContext('typeof finishTextSteering === \"function\" && finishTextSteering()', ctx);
  assert.equal(ctx.dom.prompt.value, 'pending words\nnew thought');
});
test('QUE-1 durable claim prevents restoration', async () => {
  const ctx = client();
  await vm.runInContext("submitComposerSteering({text:'pending words',images:[],owner:'session',draftEpoch:0,composerGeneration:1})", ctx);
  vm.runInContext("pendingTextSteering.delete('unique-steering'); finishTextSteering();", ctx);
  assert.equal(ctx.dom.prompt.value, '');
});
test('QUE-1 terminal before HTTP acknowledgement still restores exactly once', async () => {
  const ctx = client(); let acknowledge;
  ctx.rpc = () => new Promise(resolve => { acknowledge = resolve; });
  const pending = vm.runInContext("submitComposerSteering({text:'pending words',images:[],owner:'session',draftEpoch:0,composerGeneration:1})", ctx);
  vm.runInContext('typeof finishTextSteering === \"function\" && finishTextSteering()', ctx);
  acknowledge({ outcome: 'queued' }); await pending;
  vm.runInContext('typeof finishTextSteering === \"function\" && finishTextSteering()', ctx);
  assert.equal(ctx.dom.prompt.value, 'pending words');
});
test('QUE-1 restoration respects session draft ownership', async () => {
  const ctx = client();
  await vm.runInContext("submitComposerSteering({text:'pending words',images:[],owner:'session',draftEpoch:0,composerGeneration:1})", ctx);
  vm.runInContext("composerScope = 'other';", ctx);
  ctx.dom.prompt.value = 'other input';
  vm.runInContext('typeof finishTextSteering === \"function\" && finishTextSteering()', ctx);
  assert.equal(ctx.dom.prompt.value, 'other input');
  assert.equal(vm.runInContext("composerDrafts.get('session').text", ctx), 'pending words');
});

test('lost recall reply retries the original session receipt and restores its draft', async () => {
  const ctx = client();
  ctx.state.selectionGeneration = 7;
  ctx.dom['queue-recall'] = { hidden: true };
  ctx.refreshWorkbench = async () => {};
  vm.runInContext(fs.readFileSync('web/next-turn.js', 'utf8'), ctx);
  let calls = [];
  ctx.rpc = async (method, params) => {
    calls.push({ method, ...params });
    if (calls.length === 1) throw new Error('lost response');
    return { item: { text: 'recalled original words' } };
  };
  await vm.runInContext('recallNextTurn()', ctx);
  assert.equal(ctx.dom['queue-recall'].hidden, false, 'receipt retry stays visible even after host queue empties');
  vm.runInContext("composerScope = 'other';", ctx);
  ctx.state.selectionGeneration = 8;
  ctx.dom.prompt.value = 'new session draft';
  await vm.runInContext('recallNextTurn()', ctx);
  assert.equal(calls[1].expected_selection_generation, 7);
  assert.equal(calls[1].clientMessageId, calls[0].clientMessageId);
  assert.equal(ctx.dom.prompt.value, 'new session draft');
  assert.equal(vm.runInContext("composerDrafts.get('session').text", ctx), 'recalled original words');
});
