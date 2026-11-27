/* List/detail rendering and forms; all mutations go through the workflow controller. */
function renderWorkflowView(view) {
  const {value,list,detail}=view;if(!value||view.editing)return;
  if(!view.selection)view.selection=value.documents.length?'plan:'+value.documents.at(-1).id+':'+value.documents.at(-1).revision:'goal';
  const top=list.scrollTop,read=detail.scrollTop;list.replaceChildren();detail.replaceChildren();
  list.append(el('h3','',()=>workflowText('plans')));
  for(const doc of [...value.documents].reverse()){
    const key='plan:'+doc.id+':'+doc.revision;list.append(navigationRow({title:doc.title,meta:()=>CLATI18n.msg('workflow.plan_revision',{revision:doc.revision,status:workflowText(doc.approved?'approved':'draft')}),icon:'plan',selected:view.selection===key,action:()=>view.select(key)}));
  }
  if(!value.documents.length)list.append(el('p','picker-hint',()=>workflowText('no_plans')));
  list.append(el('h3','',()=>workflowText('goals')),navigationRow({title:()=>value.goal?.goal?.objective||workflowText('no_goal'),meta:()=>workflowText(workflowGoalStatus(value)),icon:'check',selected:view.selection==='goal',action:()=>view.select('goal')}));
  list.append(el('h3','',()=>workflowText('todos')));
  for(const [index,todo]of value.todos.entries()){
    list.append(navigationRow({title:todo.content,meta:()=>workflowText('todo_'+todo.status),icon:'task-list',selected:view.selection==='todo:'+index,action:()=>view.select('todo:'+index)}));
  }
  if(value.task)list.append(navigationRow({title:value.task.objective,meta:()=>CLATI18n.msg('extension.builtin.task.name'),icon:'task-list',selected:view.selection==='task',action:()=>view.select('task')}));
  if(view.selection==='task'&&value.task){
    detail.append(el('h2','',()=>value.task.objective),el('p','',()=>value.task.progress.summary));
    if(!builtinEnabled(value,'builtin.task'))detail.append(el('p','picker-hint',()=>CLATI18n.msg('extension.retained_task')));
    const original=el('details');original.append(el('summary','',()=>CLATI18n.msg('dev.technical_details')),el('pre','review-diff',()=>JSON.stringify(value.task,null,2)));detail.append(original);
  }else if(view.selection.startsWith('plan:'))renderWorkflowPlan(view,value.documents.find(doc=>view.selection==='plan:'+doc.id+':'+doc.revision));
  else if(view.selection.startsWith('todo:')){
    const todo=value.todos[Number(view.selection.split(':')[1])];
    if(todo)detail.append(el('h2','',()=>todo.content),el('p','',()=>CLATI18n.msg('workflow.todo_status',{status:CLATI18n.msg('workflow.todo_'+todo.status)})));
  }else renderWorkflowGoal(view,value.goal?.goal);
  list.scrollTop=top;detail.scrollTop=read;
}
function workflowViewButton(view,key,run,disabled=false){
  const node=el('button','ghost',()=>workflowText(key));node.type='button';node.disabled=disabled;node.onclick=run;return node;
}
function planExecution(doc) {
  const target = doc.continue_goal;
  return target ? {
    key:'approve_continue_goal',
    intent:{action:'approve_for_goal',plan:doc.id,revision:doc.revision,goal:target.id,goal_revision:target.revision},
  } : {
    key:doc.approved?'execute_plan':'approve_execute',
    intent:{action:'approve',plan:doc.id,revision:doc.revision,execute:true},
  };
}

function renderWorkflowPlan(view,doc){
  if(!doc){view.selection=null;return;}
  const {value,detail}=view,text=el('div','rich-text');renderMarkdown(text,doc.text);
  const actions=el('div','file-range'),latest=!value.documents.some(other=>other.id===doc.id&&other.revision>doc.revision);
  const reviewing=value.review?.id===doc.id&&value.review?.revision===doc.revision,ready=doc.projection_ready!==false;
  const execution=planExecution(doc);
  const execute=workflowViewButton(view,execution.key,()=>view.act(execution.intent),!ready||!latest||Boolean(value.handoff)||value.busy&&!reviewing);
  execute.dataset.action=execution.intent.action;
  if(doc.continue_goal) execute.title=doc.continue_goal.objective;
  actions.append(execute,
    workflowViewButton(view,'approve_only',()=>view.act({action:'approve',plan:doc.id,revision:doc.revision,execute:false}),!ready||doc.approved||!latest||value.busy&&!reviewing),
    workflowViewButton(view,'revise',async()=>{if(await view.act({action:'revise',plan:doc.id,revision:doc.revision})){workbenchShell.chat();dom.prompt.focus();}},value.busy&&!reviewing));
  if(!builtinEnabled(value,'builtin.plan_mode')){
    for(const button of actions.querySelectorAll('button'))disableBuiltin(button,value,'builtin.plan_mode');
    detail.append(el('p','picker-hint',()=>retainedPlan(value)?CLATI18n.msg('extension.retained_plan'):builtinReason(value,'builtin.plan_mode')));
  }else if(!ready)detail.append(el('p','error',()=>workflowText('projection_unavailable')));
  if(builtinEnabled(value,'builtin.plan_mode')&&!builtinEnabled(value,'builtin.goal')){
    disableBuiltin(actions.firstElementChild,value,'builtin.goal');
    detail.append(el('p','picker-hint',()=>builtinReason(value,'builtin.goal')));
  }
  detail.append(el('h2','',()=>doc.title),el('p','picker-hint',()=>CLATI18n.msg('workflow.plan_revision',{revision:doc.revision,status:workflowText(doc.approved?'approved':'draft')})),actions,text);
}
function renderWorkflowGoal(view,goal){
  const {value,detail}=view;
  if(!goal){
    const overview=navigationOverview({title:()=>workflowText('no_goal'),description:()=>workflowText('no_plans'),icon:'plan'});
    const help = builtinEnabled(value,'builtin.goal') ? ()=>workflowText('goal_composer_help') : ()=>builtinReason(value,'builtin.goal');
    overview.append(el('p','picker-hint',help));
    detail.append(overview);return;
  }
  detail.append(el('h2','workflow-objective',()=>goal.objective),el('p','workflow-goal-state',()=>workflowText(workflowGoalStatus(value))));
  const action=verb=>view.act({action:'goal',verb,id:goal.id,revision:goal.revision,summary:null}),actions=el('div','file-range');
  if(goal.phase!=='complete')actions.append(
    workflowViewButton(view,'continue_goal',()=>action('start'),value.plan_mode||value.busy||Boolean(value.handoff)),
    workflowViewButton(view,'pause_goal',()=>action('pause'),!value.goal.armed&&!value.busy),
    workflowViewButton(view,'accept_goal',()=>action('complete'),value.busy),
    workflowViewButton(view,'feedback_continue',()=>renderWorkflowGoalFeedback(view,goal),value.busy||value.plan_mode),
    workflowViewButton(view,'end_goal',async()=>{if(await CLATDialog.confirm(workflowText('end_goal_confirm')))void action('clear');},value.busy));
  if(!builtinEnabled(value,'builtin.goal')){
    for(const button of actions.querySelectorAll('button'))disableBuiltin(button,value,'builtin.goal');
    detail.append(el('p','picker-hint',()=>builtinReason(value,'builtin.goal')));
  }
  detail.append(actions);
  renderGoalHandoff(view, goal);
  if(goal.lastResult){
    const result=el('div','rich-text workflow-result');
    renderMarkdown(result,goal.lastResult);
    detail.append(el('h3','workflow-result-heading',()=>workflowText('latest_result')),result);
  }
  if(goal.blockedReason)detail.append(el('p','picker-hint',()=>goal.blockedReason.code==='awaiting-user-acceptance'?workflowText('acceptance_help'):goal.blockedReason.message));
  const linked=value.documents.findLast(doc=>doc.goal_id===goal.id);
  if(linked)detail.append(workflowViewButton(view,'view_plan',()=>view.select('plan:'+linked.id+':'+linked.revision)));
  detail.append(el('p','picker-hint',()=>CLATI18n.msg('workflow.goal_progress',{rounds:goal.roundsStarted,tokens:goal.tokensUsed})));
}
function renderWorkflowGoalFeedback(view,goal){
  view.editing=true;const form=el('form','workflow-goal-form'),input=el('textarea','');input.rows=4;
  const label=()=>workflowText('feedback_prompt');CLATI18n.attribute(input,'aria-label',label);
  const submit=workflowViewButton(view,'feedback_continue',()=>{});submit.type='submit';
  form.append(el('label','',label),input,el('p','picker-hint',()=>workflowText('acceptance_help')),submit);
  if(!builtinEnabled(view.value,'builtin.goal')){disableBuiltin(submit,view.value,'builtin.goal');form.append(el('p','picker-hint',()=>builtinReason(view.value,'builtin.goal')));}
  if(retainedPlan(view.value)){disableBuiltin(submit,view.value,'builtin.plan_mode');form.append(el('p','picker-hint',()=>CLATI18n.msg('extension.retained_plan')));}
  view.detail.replaceChildren(form);view.page.dataset.detail='true';input.focus();
  form.onsubmit=async event=>{
    event.preventDefault();if(submit.disabled||!input.value.trim())return;submit.disabled=true;
    const token=view.intent,session=state.sessionId;
    const intent={action:'goal',verb:'feedback',id:goal.id,revision:goal.revision,summary:input.value.trim()};
    const done=await view.act(intent);
    if(token!==view.intent||session!==state.sessionId||!view.detail.contains(form))return;
    if(done){view.editing=false;renderWorkflowView(view);}else submit.disabled=retainedPlan(view.value)||!builtinEnabled(view.value,'builtin.goal');
  };
}

async function configureGoalHandoff(goal, acceptSettings, draft = null) {
  const modelSettings = await rpc('model.settings.get', {});
  const queue = goal?.handoff;
  const current = queue?.routes[queue.position];
  const available = modelSettings.profiles.filter(name => name !== (current?.profile || modelSettings.active_profile));
  const profiles = queue?.routes.slice(queue.position + 1).map(route => route.profile) || [...(draft?.profiles || [])];
  const form = el('div', 'workflow-goal-form');
  const makeLabel = (key, control) => {
    const label = el('label', '', () => workflowText(key));
    label.append(control);
    return label;
  };
  const threshold = el('input');
  threshold.type = 'number';
  threshold.min = 1;
  threshold.max = 99;
  threshold.required = true;
  threshold.value = queue?.threshold ?? draft?.threshold ?? 10;
  const note = el('textarea');
  note.rows = 2;
  note.maxLength = 4000;
  note.value = queue?.note || draft?.note || '';
  const list = el('ol', 'workflow-handoff-queue');
  const picker = el('select');
  const add = el('button', 'secondary', () => workflowText('handoff_add'));
  add.type = 'button';
  add.onclick = () => {
    if (!picker.value) return;
    profiles.push(picker.value);
    renderQueue();
  };
  function renderQueue() {
    list.replaceChildren();
    profiles.forEach((name, index) => {
      const row = el('li');
      const move = (offset) => {
        const target = index + offset;
        [profiles[index], profiles[target]] = [profiles[target], profiles[index]];
        renderQueue();
        list.children[target]?.querySelector('button:not(:disabled)')?.focus();
      };
      const up = navigationIcon(() => workflowText('handoff_up'), 'arrow-up', () => move(-1));
      const down = navigationIcon(() => workflowText('handoff_down'), 'arrow-down', () => move(1));
      const remove = navigationIcon(() => workflowText('handoff_remove'), 'close', () => {
        profiles.splice(index, 1);
        renderQueue();
        picker.focus();
      });
      up.disabled = index === 0;
      down.disabled = index === profiles.length - 1;
      row.append(el('span', '', name), up, down, remove);
      list.append(row);
    });
    picker.replaceChildren();
    for (const name of available.filter(name => !profiles.includes(name))) {
      const option = el('option', '', name);
      option.value = name;
      picker.append(option);
    }
    picker.disabled = add.disabled = picker.options.length === 0;
  }
  renderQueue();
  const modelName = current?.model || modelSettings.current.model;
  form.append(
    el('p', 'picker-hint', () => CLATI18n.msg('workflow.handoff_current', {model:modelName})),
    el('p', 'picker-hint', () => workflowText('handoff_consent')),
    makeLabel('handoff_candidates', picker), add, list,
    makeLabel('handoff_threshold', threshold), makeLabel('handoff_note', note)
  );
  const result = await CLATDialog.edit(() => workflowText('handoff_models'), form, () => {
    if (!threshold.reportValidity()) return undefined;
    return profiles.length ? {profiles:[...profiles], threshold:threshold.valueAsNumber, note:note.value} : null;
  });
  if (result !== undefined) await acceptSettings(result);
}

function renderGoalHandoff(view, goal) {
  const queue = goal.handoff;
  if (queue) {
    const current = queue.routes[queue.position].model;
    const remaining = queue.routes.slice(queue.position + 1).map(route => route.profile).join(' → ');
    view.detail.append(el('p', 'picker-hint', () => CLATI18n.msg('workflow.handoff_current', {model:current})));
    if (remaining) view.detail.append(el('p', 'picker-hint', () => CLATI18n.msg('workflow.handoff_remaining', {models:remaining})));
  }
  for (const entry of goal.handoffRecords || []) {
    view.detail.append(goalHandoffRecord(entry));
  }
  if (goal.phase !== 'complete' && builtinEnabled(view.value, 'builtin.goal')) {
    const owner = [state.sessionId, state.selectionGeneration, state.stream];
    const current = () => owner[0] === state.sessionId && owner[1] === state.selectionGeneration &&
      owner[2] === state.stream && toolViewIsOpen(view.page);
    view.detail.append(workflowViewButton(view, 'handoff_models', async () => {
      try {
        await configureGoalHandoff(goal, handoff => current() && view.act({
          action:'configure_goal_handoff', id:goal.id, revision:goal.revision, handoff
        }));
      } catch (error) {
        if (current()) CLATI18n.bind(view.status, 'textContent', () => error.message);
      }
    }, view.value.busy || view.value.goal.armed || Boolean(view.value.handoff)));
  }
}

function goalHandoffRecord(entry) {
  const result = el('details', 'workflow-handoff-record');
  result.append(el('summary', '', () => entry.to
    ? CLATI18n.msg('workflow.handoff_transferred', {from:entry.from, to:entry.to})
    : CLATI18n.msg('workflow.handoff_paused', {from:entry.from})));
  result.append(el('p', '', () => entry.allowance
    ? CLATI18n.msg(entry.allowance.windowHours ? 'workflow.handoff_window_low' : 'workflow.handoff_low', {
      percent:entry.allowance.remaining, hours:entry.allowance.windowHours
    })
    : workflowText('handoff_exhausted')));
  if (entry.summary) result.append(el('p', '', entry.summary));
  for (const [model, code] of entry.skipped) {
    result.append(el('p', '', () => CLATI18n.msg('workflow.handoff_skipped', {model,reason:workflowText('handoff_' + code.replaceAll('-', '_'))})));
  }
  return result;
}

function syncGoalHandoffTranscript() {
  const goal = composerWorkflow()?.goal?.goal;
  const records = goal?.handoffRecords || [];
  const displayed = new Map([...dom.transcript.querySelectorAll('[data-goal-handoff]')]
    .map(node => [node.dataset.goalHandoff, node]));
  for (const entry of records) {
    const id = goal.id + ':' + entry.turn;
    if (displayed.delete(id)) continue;
    const node = goalHandoffRecord(entry);
    node.dataset.goalHandoff = id;
    const turnEnd = [...dom.transcript.querySelectorAll('[data-event="turn_ended"][data-turn]')]
      .find(row => Number(row.dataset.turn) === entry.turn);
    if (turnEnd) turnEnd.after(node);
    else appendTranscript(node);
  }
  for (const node of displayed.values()) node.remove();
}
