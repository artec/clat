function installTaskReview() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("web.tasks", {}, "Tasks"))); trigger.type = 'button'; trigger.id = 'tasks-open';
  document.getElementById('review-open').after(trigger);
  const dialog = el('section', 'workspace-review'); dialog.id = 'task-review'; CLATI18n.attribute(dialog,'aria-label',()=>CLATI18n.msg("web.current_run_processes", {}, "Current run processes"));
  const ui=createWorkspaceToolView(dialog,'tasks'),status=ui.status,list=ui.list,detail=ui.body;document.body.append(dialog);
  let generation=0,logGeneration=0,selected=null;
  const owner=()=>[state.stream,state.selectionGeneration,workspacePrefix];
  const owns=captured=>captured.every((v,i)=>v===owner()[i]);
  async function logs(task,batch,captured){
    const request=++logGeneration;selected=task.id;ui.show(task.command||String(task.id));
    for(const button of list.querySelectorAll('.navigation-row'))button.setAttribute('aria-current',String(button.dataset.task===String(task.id)));
    try{
      const value=await rpc('tasks.logs',{generation:batch,id:task.id});
      if(!toolViewIsOpen(dialog)||request!==logGeneration||!owns(captured))return;
      detail.replaceChildren(el('h3','',()=>value.command),el('p','picker-hint',()=>CLATI18n.msg('web.dynamic_session_value_generation_value_value_exit_value',{value1:value.session,value2:value.generation,value3:value.state,value4:value.exit_code??'—'})));
      for(const name of ['stdout','stderr','pty'])detail.append(el('h4','',()=>name+(value[name].truncated?CLATI18n.msg('web.logs_tail'):'')),el('pre','review-diff',()=>value[name].text||CLATI18n.msg('web.empty_log')));
    }catch(error){if(toolViewIsOpen(dialog)&&request===logGeneration&&owns(captured))detail.replaceChildren(el('p','error',()=>error.message));}
  }
  async function load(){
    const request=++generation,captured=owner(),top=list.scrollTop,read=detail.scrollTop;logGeneration++;
    list.replaceChildren();if(!selected)ui.overview();
    CLATI18n.bind(status,'textContent',()=>CLATI18n.msg('web.reading_host_process_facts'));
    try{
      const result=await rpc('tasks.list',{});
      if(!toolViewIsOpen(dialog)||request!==generation||!owns(captured))return;
      CLATI18n.bind(status,'textContent',()=>CLATI18n.present(result.presentation,result.note));
      if(!result.tasks.length){ui.overview();list.append(el('p','picker-hint',()=>CLATI18n.msg('web.no_retained_processes_for_the_selected_current_run_not_a_history_or_background_job')));}
      for(const task of result.tasks){
        const button=navigationRow({title:task.command,meta:()=>CLATI18n.msg('web.dynamic_value_value_exit_value',{value1:task.id,value2:task.state,value3:task.exit_code??'—'}),icon:'terminal',selected:task.id===selected,action:()=>void logs(task,result.generation,captured)});button.dataset.task=String(task.id);list.append(button);
      }
      const prior=result.tasks.find(task=>task.id===selected);if(prior)await logs(prior,result.generation,captured);else{selected=null;ui.overview();}
      if(toolViewIsOpen(dialog)&&request===generation&&owns(captured)){list.scrollTop=top;detail.scrollTop=read;}
    }catch(error){if(toolViewIsOpen(dialog)&&request===generation&&owns(captured))CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  registerView(dialog,{sidebar:ui.sidebar,actions:()=>[viewAction('refresh','refresh',()=>pageText('refresh'),()=>load())]});
  trigger.onclick=()=>openWorkbenchTool(dialog);
  dialog.addEventListener('clat:tool-enter',()=>void load());
  dialog.addEventListener('clat:tool-refresh',()=>void load());
  dialog.addEventListener('clat:tool-leave',()=>{generation++;logGeneration++;});
  dialog.addEventListener('clat:tool-reset',()=>{selected=null;generation++;logGeneration++;});
}

function focusWorkbenchTool(){document.getElementById('project-list-toggle')?.focus({preventScroll:true});}
// Legacy trigger nodes retain their action contracts; chrome owns their presentation.
function installWorkbenchTools(){for(const tool of WORKSPACE_TOOLS)document.getElementById(tool.trigger).hidden=true;}
