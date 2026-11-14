/* The composer owns unsent Goal mode. Host owns all goal facts. */
let composerGoalDraft = null;
let composerGoalBar = null;
let composerGoalPending = null;
const goalPlugin = 'builtin.goal';

function composerWorkflow() {
  const shell = workbenchShell;
  const value = shell?.workflow;
  return state.connected && value?.session === state.sessionId &&
    shell.workflowStream === state.stream && shell.workflowSelection === state.selectionGeneration ? value : null;
}

function composerWorkMode(value) {
  if (composerGoalDraft) return 'goal';
  if (value?.plan_mode) return 'plan';
  return value?.goal?.armed ? 'goal' : 'normal';
}

function goalComposerLocked() {
  return state.switching || state.compactionActive || state.desktopUpdateLocked ||
    (composerGoalPending !== null && composerOwns(composerGoalPending));
}

function canComposeGoal(value) {
  const goal = value?.goal?.goal;
  return value && builtinReady(goalPlugin) && !retainedPlan(value) && !value.busy &&
    !value.handoff && !state.runActive && !goalComposerLocked() && (!goal || goal.phase === 'complete');
}

function setComposerGoalMode(active) {
  composerGoalDraft = active ? (composerGoalDraft || {}) : null;
  state.composerGeneration++;
  saveComposerText();
  syncInteractionControls();
  if (active) dom.prompt.focus();
}

function chooseComposerGoal() {
  const value = composerWorkflow();
  if (canComposeGoal(value)) setComposerGoalMode(true);
}

async function submitComposerGoal() {
  const value = composerWorkflow();
  const draft = composerGoalDraft;
  const owner = composerScope, generation = state.composerGeneration;
  const objective = dom.prompt.value.trim();
  if (!objective || !canComposeGoal(value)) return;
  if (state.draft.images.length) { updateRunState(workflowText('goal_text_only')); return; }
  composerGoalPending = owner;
  syncInteractionControls();
  try {
    if (!await modelSetup.ensure() || !composerOwns(owner) || draft !== composerGoalDraft ||
        generation !== state.composerGeneration || value.session !== state.sessionId) return;
    const intent = {action:'create_goal', objective};
    const result = await sendWorkflowIntent(intent, value);
    if (result !== undefined) settleComposerGoalDraft(owner, generation, draft);
  } catch (error) {
    if (composerOwns(owner)) updateRunState(error.message);
  } finally {
    if (composerGoalPending === owner) composerGoalPending = null;
    syncInteractionControls();
    void refreshShellContext();
  }
}

function installComposerGoalBar() {
  const bar = el('div','composer-goal-bar'); bar.id = 'composer-goal-bar'; bar.hidden = true;
  const details = shellButton('goal-badge','goal',()=>CLATI18n.msg('web.goal_details'),()=>workflowController.goal());
  const status = el('strong','composer-goal-status');
  const objective = el('span','composer-goal-objective');
  details.querySelector('.shell-label').replaceWith(status, objective);
  const pause = shellButton('composer-goal-pause','pause',()=>workflowText('pause_goal'),()=>act('pause'));
  const start = shellButton('composer-goal-start','play',()=>workflowText('continue_goal'),()=>act('start'));
  const accept = shellButton('composer-goal-accept','check',()=>workflowText('accept_goal'),()=>act('complete'));
  const feedback = shellButton('composer-goal-feedback','chat',()=>workflowText('feedback_continue'),()=>workflowController.feedback(composerWorkflow()?.goal?.goal?.id));
  const remove = shellButton('composer-goal-remove','trash',()=>workflowText('delete_goal'),()=>act('clear'));
  const expand = shellButton('composer-goal-details','open',()=>CLATI18n.msg('web.goal_details'),()=>workflowController.goal());
  bar.append(details,remove,pause,start,accept,feedback,expand);
  dom['composer-shell'].before(bar);
  let acting = false;
  async function act(verb) {
    const value = composerWorkflow(), goal = value?.goal?.goal;
    if (!goal || acting || goalComposerLocked()) return;
    if (verb === 'clear' && !await CLATDialog.confirm(workflowText('end_goal_confirm'))) return;
    const owner = composerScope, stream = state.stream;
    acting = true; render();
    try {
      if (verb === 'start' && !await modelSetup.ensure()) return;
      if (!composerOwns(owner) || stream !== state.stream) return;
      await sendWorkflowIntent({action:'goal',verb,id:goal.id,revision:goal.revision,summary:null},value);
    } catch (error) {
      if (composerOwns(owner) && stream === state.stream) updateRunState(error.message);
    } finally {
      acting = false; render(); void refreshShellContext();
    }
  }
  function render() {
    const value = composerWorkflow(), goal = value?.goal?.goal;
    bar.hidden = !goal || goal.phase === 'complete';
    if (bar.hidden) return;
    const blocked = acting || goalComposerLocked() || !builtinReady(goalPlugin);
    const armed = value.goal.armed;
    const busy = value.busy || state.runActive;
    const pausing = goal.phase === 'active' && armed;
    const awaiting = goal.blockedReason?.code === 'awaiting-user-acceptance';
    bar.dataset.goalId = goal.id;
    CLATI18n.bind(status,'textContent',()=>workflowText(workflowGoalStatus(value)));
    objective.textContent = goal.objective;
    details.title = goal.objective;
    pause.hidden = !pausing; pause.disabled = blocked;
    start.hidden = pausing || awaiting || goal.phase === 'complete';
    start.disabled = blocked || busy || value.handoff || value.plan_mode;
    accept.hidden = feedback.hidden = !awaiting;
    accept.disabled = blocked || busy || Boolean(value.handoff);
    feedback.disabled = blocked || busy || Boolean(value.handoff) || value.plan_mode;
    remove.disabled = blocked || (busy && !armed);
    for (const button of [pause,start,remove]) {
      CLATI18n.attribute(button,'title',()=>!builtinReady(goalPlugin) ? builtinUnavailableReason(goalPlugin) :
        workflowText(button === pause ? 'pause_goal' : button === start ? 'continue_goal' : 'delete_goal'));
    }
  }
  composerGoalBar = {render};
}

function syncComposerGoal() {
  const value = composerWorkflow();
  const trigger = document.getElementById('composer-plan-toggle');
  if (trigger) {
    const mode = composerWorkMode(value);
    if (trigger.dataset.mode !== mode) {
      trigger.dataset.mode = mode;
      trigger.querySelector('svg').replaceWith(svgIcon(mode === 'goal' ? 'goal' : 'plan'));
    }
    const label = () => workflowText(mode === 'goal' ? 'goal_mode' : mode === 'plan' ? 'planning' : 'work_mode');
    CLATI18n.attribute(trigger,'aria-label',label);
    CLATI18n.bind(trigger.querySelector('.shell-label'),'textContent',label);
    trigger.setAttribute('aria-pressed', String(mode !== 'normal'));
  }
  const addPlan = document.getElementById('add-plan');
  if (addPlan) {
    disableBuiltin(addPlan,value,'builtin.plan_mode');
    addPlan.disabled ||= !value || value.busy || state.runActive ||
      goalComposerLocked() || workbenchShell.planPending;
  }
  const addGoal = document.getElementById('add-goal');
  if (addGoal) {
    addGoal.disabled = !canComposeGoal(value);
    CLATI18n.attribute(addGoal,'title',()=>!builtinReady(goalPlugin) ? builtinUnavailableReason(goalPlugin) :
      retainedPlan(value) ? CLATI18n.msg('extension.retained_plan') : workflowText('goal_mode'));
  }
  CLATI18n.attribute(dom.prompt,'placeholder',()=>composerGoalDraft ?
    workflowText('goal_objective') : CLATI18n.msg('web.describe_the_outcome_you_want'));
  const popup = document.getElementById('work-mode-menu');
  if (popup && !popup.hidden) syncWorkModeRows(popup, value);
  composerGoalBar?.render();
}

function syncWorkModeRows(popup, value) {
  const current = composerWorkMode(value);
  for (const row of popup.querySelectorAll('[data-work-mode]')) {
    const mode = row.dataset.workMode;
    if (mode === 'plan' || value?.plan_mode) disableBuiltin(row,value,'builtin.plan_mode');
    else row.disabled = false;
    if (mode === 'goal') {
      row.disabled = !canComposeGoal(value);
      // 老行为（eda98b39 的 disableBuiltin）保留：goal 行禁用时悬停仍给原因。
      if (row.disabled && !builtinReady(goalPlugin))
        CLATI18n.attribute(row, 'title', () => builtinUnavailableReason(goalPlugin));
    }
    row.setAttribute('aria-pressed', String(mode === current));
  }
}

function installWorkModeMenu(trigger) {
  const popup=el('section','usage-popover work-mode-menu');popup.hidden=true;popup.id='work-mode-menu';
  popup.setAttribute('role','dialog');trigger.setAttribute('aria-haspopup','dialog');trigger.setAttribute('aria-controls',popup.id);
  CLATI18n.attribute(trigger,'aria-label',()=>workflowText(composerGoalDraft?'goal_mode':'work_mode'));
  let controller;
  async function setMode(mode){
    controller.close();
    if(mode==='goal'){chooseComposerGoal();return;}
    if(composerGoalDraft && mode==='normal' && !composerWorkflow()?.plan_mode){setComposerGoalMode(false);return;}
    const owner = composerScope, draft = composerGoalDraft, stream = state.stream;
    try{
      if(mode==='plan'&&state.runActive&&!await CLATDialog.confirm(workflowText('stop_and_plan_confirm')))return;
      const result = await sendWorkflowIntent({action:'mode',mode});
      if(result!==undefined && draft && composerOwns(owner) && draft===composerGoalDraft)setComposerGoalMode(false);
    }catch(error){if(composerOwns(owner) && stream===state.stream)updateRunState(error.message);}
  }
  function render(){
    popup.replaceChildren(el('h3','',()=>workflowText('work_mode')));
    const value=composerWorkflow();
    for(const [mode,key,icon]of [['normal','normal','chat'],['plan',state.runActive?'stop_and_plan':'planning','plan'],['goal','goal_mode','goal']]){
      const row=shellButton('',icon,()=>workflowText(key),()=>void setMode(mode),false);
      const check = svgIcon('check');
      check.classList.add('work-mode-check');
      row.dataset.workMode=mode;
      row.append(check);
      popup.append(row);
    }
    syncWorkModeRows(popup, value);
    if(retainedPlan(value))popup.append(el('p','picker-hint',()=>CLATI18n.msg('extension.retained_plan')));
    popup.append(shellButton('work-mode-details','tools',()=>workflowText('open_workflow'),()=>{controller.close();workflowController.open();},false));
  }
  controller=attachComposerPopup(trigger,trigger,popup,render,'left');trigger.addEventListener('click',()=>controller.toggle());
  return controller;
}
