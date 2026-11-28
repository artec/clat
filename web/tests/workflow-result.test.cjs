const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function page() {
  let language = 'en';
  const markdown = [];
  const el = (tag, cls, text) => ({
    tag, cls, text, children: [], dataset: {},
    append(...nodes) { this.children.push(...nodes); },
    querySelectorAll() { return []; },
    get textContent() { return typeof this.text === 'function' ? this.text() : this.text || ''; },
  });
  const context = {
    el,
    workflowText: key => language + ':workflow.' + key,
    workflowGoalStatus: () => 'complete',
    builtinEnabled: () => true,
    CLATI18n: {msg: (key, args) => language + ':' + key + (args ? JSON.stringify(args) : '')},
    renderMarkdown: (_node, source) => { markdown.push(source); },
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync('web/workflow-view.js', 'utf8'), context);
  return {
    markdown,
    actions: input => Array.from(context.goalHandoffActions(input)),
    label: action => context.goalHandoffAcceptLabel(action),
    language: next => { language = next; },
    render(lastResult) {
      markdown.length = 0;
      const detail = el('section');
      const goal = {id: 'g', revision: 1, objective: '交付', phase: 'complete', lastResult, roundsStarted: 2, tokensUsed: 10};
      context.renderWorkflowGoal({
        value: {documents: [], busy: false, plan_mode: false, goal: {goal, armed: false}},
        detail,
      }, goal);
      return {detail, goal};
    },
  };
}
const texts = node => [node.textContent, ...node.children.flatMap(texts)].join('\n');

test('the default acceptance result follows the active language and authored results stay verbatim', () => {
  const view = page();
  const accepted = view.render('Accepted by user');
  assert.deepEqual(view.markdown, []);
  assert.equal(accepted.goal.lastResult, 'Accepted by user');
  for (const language of ['zh-Hans', 'zh-Hant', 'en']) {
    view.language(language);
    const shown = texts(accepted.detail);
    assert.ok(shown.includes(language + ':workflow.accepted_by_user'));
    assert.equal(shown.includes('Accepted by user'), false);
  }
  const authored = view.render('## Summary\n\nVerified the empty goal.');
  assert.deepEqual(view.markdown, ['## Summary\n\nVerified the empty goal.']);
  assert.equal(texts(authored.detail).includes(':workflow.accepted_by_user'), false);
});

test('model handoff offers start before it is on, and stop once it is on', () => {
  const view = page();
  assert.deepEqual(view.actions({
    locked: false, canPause: false, settings: true, handoff: false, busy: false, switching: false,
  }), ['apply', 'settings']);
  view.language('zh-Hans');
  assert.equal(view.label('apply'), 'zh-Hans:workflow.handoff_start');
  assert.deepEqual(view.actions({
    locked: true, canPause: true, settings: true, handoff: true, busy: false, switching: false,
  }), ['pause', 'remove', 'settings']);
  assert.equal(view.label('remove'), 'zh-Hans:workflow.handoff_stop');
  assert.deepEqual(view.actions({
    locked: true, canPause: true, settings: true, handoff: true, busy: true, switching: false,
  }), ['pause', 'settings']);
});
