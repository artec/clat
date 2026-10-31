/* Workflow controller: snapshots and typed intentions, independent of page rendering. */
let workflowController=null;
const workflowText=key=>CLATI18n.msg('workflow.'+key);
function builtinEnabled(value,id){return value?.builtin_availability?.[id]?.enabled!==false;}
function builtinReason(value,id){return CLATI18n.present(value?.builtin_availability?.[id]?.reason);}
function disableBuiltin(node,value,id){
  if(!node)return;
  node.disabled=!builtinEnabled(value,id);
  CLATI18n.attribute(node,'title',()=>builtinReason(value,id));
}
function retainedPlan(value){return value?.plan_mode&&!builtinEnabled(value,'builtin.plan_mode');}
function workflowGoalStatus(value){
  const goal=value.goal?.goal;if(!goal)return 'not_created';
  if(goal.blockedReason?.code==='awaiting-user-acceptance')return 'awaiting_acceptance';
  if(goal.phase==='complete')return 'complete';
  if(value.goal.armed&&value.busy)return 'running';
  return goal.phase==='blocked'?'blocked':'paused';
}
async function sendWorkflowIntent(intent,snapshot){
  const owner=[state.sessionId,state.selectionGeneration,state.stream];
  const current=()=>owner[0]===state.sessionId&&owner[1]===state.selectionGeneration&&owner[2]===state.stream;
  const value=snapshot||await rpc('workflow.details',{});if(!current()||value.session!==owner[0])return;
  const result=await rpc('workflow.action',{operationId:newOpaqueClientId('workflow'),session:owner[0],expectedVersion:value.version,expectedGeneration:value.generation,intent});
  if(owner[0]!==state.sessionId||owner[1]!==state.selectionGeneration||owner[2]!==state.stream)return;
  await refreshWorkbench();if(!current())return;await refreshShellContext();if(!current())return;
  window.dispatchEvent(new Event('clat:workflow-changed'));return result;
}
async function loadWorkflowView(view){
  const request=++view.epoch,owner=[state.stream,state.selectionGeneration];
  const current=()=>toolViewIsOpen(view.page)&&request===view.epoch&&owner[0]===state.stream&&owner[1]===state.selectionGeneration;
  try{
    const value=await rpc('workflow.details',{});if(!current())return;
    const signature=JSON.stringify(value);view.value=value;
    if(view.signature!==signature){view.signature=signature;renderWorkflowView(view);refreshViewContext();}
    CLATI18n.bind(view.status,'textContent',()=>workflowText(view.value.handoff?'handoff':view.value.plan_mode?'planning':workflowGoalStatus(view.value)));
  }catch(error){if(current())CLATI18n.bind(view.status,'textContent',()=>error.message);}
}
async function actWorkflowView(view,intent){
  if(view.acting)return false;view.acting=true;
  const owner=[state.sessionId,state.selectionGeneration],token=view.intent;
  const current=()=>token===view.intent&&owner[0]===state.sessionId&&owner[1]===state.selectionGeneration&&toolViewIsOpen(view.page);
  try{const result=await sendWorkflowIntent(intent,view.value);if(result===undefined||!current())return false;await loadWorkflowView(view);return current();}
  catch(error){if(current())CLATI18n.bind(view.status,'textContent',()=>error.message);return false;}
  finally{view.acting=false;}
}
function installWorkflowDetails(){
  const trigger=el('button','ghost',()=>CLATI18n.msg('web.plan_goal'));trigger.id='workflow-open';trigger.type='button';document.getElementById('review-open').after(trigger);
  const page=el('section','workspace-review');page.id='workflow-details';
  const ui=createWorkspaceToolView(page,'workflow'),status=ui.status,list=ui.list,detail=ui.body;document.body.append(page);
  const view={page,status,list,detail,value:null,selection:null,epoch:0,acting:false,signature:'',editing:false,intent:0};
  view.act=intent=>actWorkflowView(view,intent);
  view.select=key=>{view.intent++;view.editing=false;view.selection=key;ui.show(()=>key.startsWith('plan:')?workflowText('plans'):key==='goal'?workflowText('goals'):workflowText('todos'));renderWorkflowView(view);page.dataset.detail='true';};
  const open=()=>openWorkbenchTool(page),load=()=>loadWorkflowView(view);
  async function afterEnter(run){
    open();const token=++view.intent,owner=[state.sessionId,state.selectionGeneration];await load();
    if(token===view.intent&&toolViewIsOpen(page)&&owner[0]===state.sessionId&&owner[1]===state.selectionGeneration)run();
  }
  trigger.onclick=open;for(const id of ['plan-mode-badge','goal-badge'])document.getElementById(id).onclick=open;
  registerView(page,{sidebar:ui.sidebar,actions:()=>[viewAction('refresh','refresh',()=>pageText('refresh'),()=>load()),viewAction('create-goal','plus',()=>workflowText('create_goal'),()=>{page.dataset.detail='true';renderWorkflowGoalForm(view);},true,()=>!retainedPlan(view.value))]});
  page.addEventListener('clat:tool-enter',()=>void load());page.addEventListener('clat:tool-leave',()=>{view.epoch++;view.intent++;});
  page.addEventListener('clat:tool-refresh',()=>{if(!view.acting)void load();});
  window.addEventListener('clat:workflow-changed',()=>{if(toolViewIsOpen(page))void load();});
  page.addEventListener('clat:tool-reset',()=>{view.epoch++;view.intent++;view.value=null;view.selection=null;view.signature='';view.editing=false;});
  workflowController={open,goal:()=>afterEnter(()=>view.select('goal')),create:()=>afterEnter(()=>renderWorkflowGoalForm(view)),plan:text=>afterEnter(()=>{const doc=view.value?.documents.findLast(doc=>doc.text===text);if(doc)view.select('plan:'+doc.id+':'+doc.revision);})};
}
