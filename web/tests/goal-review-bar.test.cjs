const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const vm = require('vm');

function setup() {
  const nodes = new Map();
  function node(id) {
    return { id, hidden: false, disabled: false, dataset: {}, children: [],
      append(...items) { this.children.push(...items); },
      querySelector() { return { replaceWith() {} }; },
    };
  }
  const goal = { id:'goal-a', revision:4, phase:'blocked', objective:'Finish',
    blockedReason:{code:'awaiting-user-acceptance'} };
  const value = { session:'session', goal:{goal,armed:false}, busy:false, handoff:false, plan_mode:false };
  const actions = [], feedback = [];
  let bar;
  const context = vm.createContext({
    state:{runActive:false, switching:false, compactionActive:false, desktopUpdateLocked:false, stream:'stream', connected:true, sessionId:'session', selectionGeneration:1},
    workbenchShell:{workflow:value,workflowStream:'stream',workflowSelection:1},
    dom:{'composer-shell':{before(item){bar=item;}},prompt:{}},
    el:() => node(''), svgIcon:() => node(''),
    shellButton(id, icon, label, action) { const item=node(id); item.click=action; nodes.set(id,item); return item; },
    workflowText:key=>key, CLATI18n:{msg:key=>key,bind(){},attribute(){}},
    builtinReady:()=>true, builtinUnavailableReason:()=>'', composerOwns:()=>true,
    composerScope:'session', modelSetup:{ensure:async()=>true},
    workflowController:{goal(){},feedback(id){feedback.push(id);}},
    sendWorkflowIntent:async intent=>{actions.push(intent);},
    refreshShellContext:async()=>{}, confirm:()=>true,
  });
  vm.runInContext(fs.readFileSync(process.env.GOAL_BAR_SOURCE || 'web/work-mode.js','utf8'),context);
  vm.runInContext('installComposerGoalBar(); composerGoalBar.render()',context);
  return {context,nodes,actions,feedback,goal,value,bar};
}

test('awaiting acceptance exposes review actions for the current goal', async () => {
  const {nodes,actions,feedback,goal} = setup();
  assert.equal(nodes.get('composer-goal-start').hidden,true);
  assert.equal(nodes.get('composer-goal-accept').hidden,false);
  assert.equal(nodes.get('composer-goal-feedback').hidden,false);
  nodes.get('composer-goal-feedback').click();
  assert.deepEqual(feedback,['goal-a']);
  await nodes.get('composer-goal-accept').click();
  assert.equal(actions[0].verb,'complete');
  assert.equal(actions[0].id,goal.id);
  assert.equal(actions[0].revision,goal.revision);
});

test('a completed goal disappears from the composer and remains available in workflow state', () => {
  const {context,goal,value,bar} = setup();
  assert.equal(bar.hidden,false);
  goal.phase = 'complete';
  goal.blockedReason = null;
  vm.runInContext('composerGoalBar.render()',context);
  assert.equal(bar.hidden,true);
  assert.equal(value.goal.goal.id,'goal-a');
});

test('ordinary paused goals keep the continue action', () => {
  const {context,nodes,goal} = setup();
  goal.phase = 'paused';
  goal.blockedReason = null;
  vm.runInContext('composerGoalBar.render()',context);
  assert.equal(nodes.get('composer-goal-start').hidden,false);
  assert.equal(nodes.get('composer-goal-accept').hidden,true);
  assert.equal(nodes.get('composer-goal-feedback').hidden,true);
});
