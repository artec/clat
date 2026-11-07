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
// Startup availability is authoritative; an unreceived snapshot cannot grant capability I/O.
function builtinReady(id){return (state.builtinAvailability||state.workbench?.builtin_availability)?.[id]?.enabled===true;}
function builtinUnavailableReason(id){return builtinReason({builtin_availability:state.builtinAvailability||state.workbench?.builtin_availability},id)||CLATI18n.msg('extension.capability_pending');}
function requireBuiltin(id){if(!builtinReady(id))throw CLATI18n.error(CLATI18n.view(()=>builtinUnavailableReason(id)));}
const optionalResourceControlState=new WeakMap();
function syncOptionalResourceControls(){
  for(const [id,names] of [['builtin.development',['files-open','review-open','terminal-open','nav-terminal','context-resources-files','context-files-open','context-changes-open','add-project-file']],['builtin.draft_images',['attachment-open','attachment-input','add-image']]]){
    for(const name of names){const node=document.getElementById(name);if(node){node.disabled=!builtinReady(id)||(id==='builtin.draft_images'&&Boolean(state.switching||state.compactionActive||state.desktopUpdateLocked));CLATI18n.attribute(node,'title',()=>builtinReady(id)?'':builtinUnavailableReason(id));}}
  }
  for(const name of ['file-browser','workspace-review','terminal-page']){
    for(const node of document.getElementById(name)?.querySelectorAll?.('button,input,textarea,select')||[]){
      if(!builtinReady('builtin.development')){if(!optionalResourceControlState.has(node))optionalResourceControlState.set(node,node.disabled);node.disabled=true;}
      else if(optionalResourceControlState.has(node)){node.disabled=optionalResourceControlState.get(node);optionalResourceControlState.delete(node);}
    }
  }
}
function syncUtilityAvailability(){
  const ready=builtinReady('builtin.utility_model');
  let note=document.getElementById('utility-availability');
  const form=document.getElementById('utility-settings-save')?.parentElement;
  if(!note&&form){note=el('p','picker-hint');note.id='utility-availability';form.append(note);}
  if(note){note.hidden=ready;CLATI18n.bind(note,'textContent',()=>CLATI18n.msg('extension.utility_unavailable_settings',{reason:builtinUnavailableReason('builtin.utility_model')}));}
  if(!ready){clearRoundFollowups();invalidateSuggestion();}
  syncSuggestionControl();
}
window.addEventListener('clat:workbench-updated',syncUtilityAvailability);
window.addEventListener('clat:workbench-updated',syncOptionalResourceControls);
window.addEventListener('clat:builtin-availability',syncUtilityAvailability);
window.addEventListener('clat:builtin-availability',syncOptionalResourceControls);
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
  if(current()){
    await refreshWorkbench();
    if(current())await refreshShellContext();
    if(current())window.dispatchEvent(new Event('clat:workflow-changed'));
  }
  return result;
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
  view.select=key=>{view.intent++;view.editing=false;view.selection=key;ui.show(()=>key.startsWith('plan:')?workflowText('plans'):key==='goal'?workflowText('goals'):key==='task'?CLATI18n.msg('extension.builtin.task.name'):workflowText('todos'));renderWorkflowView(view);page.dataset.detail='true';};
  const open=()=>openWorkbenchTool(page),load=()=>loadWorkflowView(view);
  async function afterEnter(run){
    open();const token=++view.intent,owner=[state.sessionId,state.selectionGeneration];await load();
    if(token===view.intent&&toolViewIsOpen(page)&&owner[0]===state.sessionId&&owner[1]===state.selectionGeneration)run();
  }
  trigger.onclick=open;document.getElementById('plan-mode-badge').onclick=open;
  registerView(page,{sidebar:ui.sidebar,actions:()=>[viewAction('refresh','refresh',()=>pageText('refresh'),()=>load())]});
  page.addEventListener('clat:tool-enter',()=>void load());page.addEventListener('clat:tool-leave',()=>{view.epoch++;view.intent++;});
  page.addEventListener('clat:tool-refresh',()=>{if(!view.acting)void load();});
  window.addEventListener('clat:workflow-changed',()=>{if(toolViewIsOpen(page))void load();});
  page.addEventListener('clat:tool-reset',()=>{view.epoch++;view.intent++;view.value=null;view.selection=null;view.signature='';view.editing=false;});
  workflowController={open,goal:()=>afterEnter(()=>view.select('goal')),plan:text=>afterEnter(()=>{const doc=view.value?.documents.findLast(doc=>doc.text===text);if(doc)view.select('plan:'+doc.id+':'+doc.revision);})};
}
