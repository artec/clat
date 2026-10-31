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
  if(view.selection.startsWith('plan:'))renderWorkflowPlan(view,value.documents.find(doc=>view.selection==='plan:'+doc.id+':'+doc.revision));
  else if(view.selection.startsWith('todo:')){
    const todo=value.todos[Number(view.selection.split(':')[1])];
    if(todo)detail.append(el('h2','',()=>todo.content),el('p','',()=>CLATI18n.msg('workflow.todo_status',{status:CLATI18n.msg('workflow.todo_'+todo.status)})));
  }else renderWorkflowGoal(view,value.goal?.goal);
  list.scrollTop=top;detail.scrollTop=read;
}
function workflowViewButton(view,key,run,disabled=false){
  const node=el('button','ghost',()=>workflowText(key));node.type='button';node.disabled=disabled;node.onclick=run;return node;
}
function renderWorkflowPlan(view,doc){
  if(!doc){view.selection=null;return;}
  const {value,detail}=view,text=el('div','rich-text');renderMarkdown(text,doc.text);
  const actions=el('div','file-range'),latest=!value.documents.some(other=>other.id===doc.id&&other.revision>doc.revision);
  const reviewing=value.review?.id===doc.id&&value.review?.revision===doc.revision,ready=doc.projection_ready!==false;
  const approve=execute=>view.act({action:'approve',plan:doc.id,revision:doc.revision,execute});
  actions.append(workflowViewButton(view,doc.approved?'execute_plan':'approve_execute',()=>approve(true),!ready||!latest||Boolean(value.handoff)||value.busy&&!reviewing),
    workflowViewButton(view,'approve_only',()=>approve(false),!ready||doc.approved||!latest||value.busy&&!reviewing),
    workflowViewButton(view,'revise',async()=>{if(await view.act({action:'revise',plan:doc.id,revision:doc.revision})){workbenchShell.chat();dom.prompt.focus();}},value.busy&&!reviewing));
  if(!ready)detail.append(el('p','error',()=>workflowText('projection_unavailable')));
  detail.append(el('h2','',()=>doc.title),el('p','picker-hint',()=>CLATI18n.msg('workflow.plan_revision',{revision:doc.revision,status:workflowText(doc.approved?'approved':'draft')})),actions,text);
}
function renderWorkflowGoal(view,goal){
  const {value,detail}=view;
  if(!goal){
    const overview=navigationOverview({title:()=>workflowText('no_goal'),description:()=>workflowText('no_plans'),icon:'plan'});
    overview.append(workflowViewButton(view,'create_goal',()=>renderWorkflowGoalForm(view)));
    detail.append(overview);return;
  }
  detail.append(el('h2','',()=>goal?.objective||workflowText('no_goal')),el('p','',()=>workflowText(workflowGoalStatus(value))));
  const action=verb=>view.act({action:'goal',verb,id:goal.id,revision:goal.revision,summary:null}),actions=el('div','file-range');
  if(goal.phase!=='complete')actions.append(
    workflowViewButton(view,'continue_goal',()=>action('start'),value.plan_mode||value.busy||Boolean(value.handoff)),
    workflowViewButton(view,'pause_goal',()=>action('pause'),!value.goal.armed&&!value.busy),
    workflowViewButton(view,'accept_goal',()=>action('complete'),value.busy),
    workflowViewButton(view,'feedback_continue',()=>renderWorkflowGoalForm(view,goal),value.busy||value.plan_mode),
    workflowViewButton(view,'end_goal',()=>{if(confirm(workflowText('end_goal_confirm')))void action('clear');},value.busy));
  detail.append(actions);
  if(goal.lastResult)detail.append(el('h3','',()=>workflowText('latest_result')),el('pre','review-diff',()=>goal.lastResult));
  if(goal.blockedReason)detail.append(el('p','picker-hint',()=>goal.blockedReason.code==='awaiting-user-acceptance'?workflowText('acceptance_help'):goal.blockedReason.message));
  const linked=value.documents.findLast(doc=>doc.goal_id===goal.id);
  if(linked)detail.append(workflowViewButton(view,'view_plan',()=>view.select('plan:'+linked.id+':'+linked.revision)));
  detail.append(el('p','picker-hint',()=>CLATI18n.msg('workflow.goal_progress',{rounds:goal.roundsStarted,maxRounds:goal.limits.maxRounds,tokens:goal.tokensUsed,maxTokens:goal.limits.maxTokens})));
}
function renderWorkflowGoalForm(view,goal=null){
  view.editing=true;const form=el('form','workflow-goal-form'),input=el('textarea','');input.rows=4;
  const label=()=>workflowText(goal?'feedback_prompt':'goal_objective');CLATI18n.attribute(input,'aria-label',label);
  const submit=workflowViewButton(view,goal?'feedback_continue':'create_and_start',()=>{});submit.type='submit';
  form.append(el('label','',label),input,el('p','picker-hint',()=>workflowText('acceptance_help')),submit);
  view.detail.replaceChildren(form);view.page.dataset.detail='true';input.focus();
  form.onsubmit=async event=>{
    event.preventDefault();if(!input.value.trim())return;submit.disabled=true;
    const token=view.intent,session=state.sessionId;
    const intent=goal?{action:'goal',verb:'feedback',id:goal.id,revision:goal.revision,summary:input.value.trim()}:{action:'create_goal',objective:input.value.trim()};
    const done=await view.act(intent);
    if(token!==view.intent||session!==state.sessionId||!view.detail.contains(form))return;
    if(done){view.editing=false;renderWorkflowView(view);}else submit.disabled=false;
  };
}
