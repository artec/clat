function installTaskReview() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("web.tasks", {}, "Tasks"))); trigger.type = 'button'; trigger.id = 'tasks-open';
  document.getElementById('review-open').after(trigger);
  const dialog = el('section', 'workspace-review'); dialog.id = 'task-review'; CLATI18n.attribute(dialog,'aria-label',()=>CLATI18n.msg("web.current_run_processes", {}, "Current run processes"));
  const header = el('div', 'review-header'); const refresh = el('button', 'ghost', () => (CLATI18n.msg("web.refresh", {}, "Refresh")));
  const close = el('button', 'icon-button', () => ('×')); CLATI18n.attribute(close,'aria-label',()=>CLATI18n.msg("web.close_tasks", {}, "Close tasks"));
  header.append(el('h2', '', () => (CLATI18n.msg("web.current_run_processes", {}, "Current run processes"))), refresh, close);
  const status = el('p', 'picker-hint'); status.setAttribute('role', 'status');
  const body = el('div', 'review-body'); const list = el('nav', 'review-files'); const detail = el('section', 'review-patch');
  body.append(list, detail); dialog.append(header, status, body); document.body.append(dialog);
  let generation=0,logGeneration=0,selected=null;
  const owner=()=>[state.stream,state.selectionGeneration,workspacePrefix];
  const owns=captured=>captured.every((v,i)=>v===owner()[i]);
  async function logs(task,batch,captured){
    const request=++logGeneration;selected=task.id;
    for(const button of list.querySelectorAll('.review-file'))button.classList.toggle('is-selected',button.dataset.task===String(task.id));
    try{
      const value=await rpc('tasks.logs',{generation:batch,id:task.id});
      if(!toolViewIsOpen(dialog)||request!==logGeneration||!owns(captured))return;
      detail.replaceChildren(el('h3','',()=>value.command),el('p','picker-hint',()=>CLATI18n.msg('web.dynamic_session_value_generation_value_value_exit_value',{value1:value.session,value2:value.generation,value3:value.state,value4:value.exit_code??'—'})));
      for(const name of ['stdout','stderr','pty'])detail.append(el('h4','',()=>name+(value[name].truncated?CLATI18n.msg('web.logs_tail'):'')),el('pre','review-diff',()=>value[name].text||CLATI18n.msg('web.empty_log')));
    }catch(error){if(toolViewIsOpen(dialog)&&request===logGeneration&&owns(captured))detail.replaceChildren(el('p','error',()=>error.message));}
  }
  async function load(){
    const request=++generation,captured=owner(),top=list.scrollTop,read=detail.scrollTop;logGeneration++;
    list.replaceChildren();detail.replaceChildren();
    CLATI18n.bind(status,'textContent',()=>CLATI18n.msg('web.reading_host_process_facts'));
    try{
      const result=await rpc('tasks.list',{});
      if(!toolViewIsOpen(dialog)||request!==generation||!owns(captured))return;
      CLATI18n.bind(status,'textContent',()=>CLATI18n.present(result.presentation,result.note));
      if(!result.tasks.length)list.append(el('p','picker-hint',()=>CLATI18n.msg('web.no_retained_processes_for_the_selected_current_run_not_a_history_or_background_job')));
      for(const task of result.tasks){
        const button=el('button','review-file');button.type='button';button.dataset.task=String(task.id);
        button.append(el('span','',()=>CLATI18n.msg('web.dynamic_value_value_exit_value',{value1:task.id,value2:task.state,value3:task.exit_code??'—'})),el('small','',()=>task.command));
        button.classList.toggle('is-selected',task.id===selected);
        button.onclick=()=>void logs(task,result.generation,captured);list.append(button);
      }
      const prior=result.tasks.find(task=>task.id===selected);if(prior)await logs(prior,result.generation,captured);
      if(toolViewIsOpen(dialog)&&request===generation&&owns(captured)){list.scrollTop=top;detail.scrollTop=read;}
    }catch(error){if(toolViewIsOpen(dialog)&&request===generation&&owns(captured))CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  trigger.onclick=()=>openWorkbenchTool(dialog);refresh.onclick=()=>void load();close.onclick=()=>closeWorkbenchTool();
  dialog.addEventListener('clat:tool-enter',()=>void load());
  dialog.addEventListener('clat:tool-leave',()=>{generation++;logGeneration++;});
  dialog.addEventListener('clat:tool-reset',()=>{selected=null;generation++;logGeneration++;});
}

function focusWorkbenchTool(trigger) {
  const target = document.getElementById('workspace-chat-tab');
  if (target?.getClientRects().length) target.focus({preventScroll:true});
  else document.getElementById('tools-open')?.focus({preventScroll:true});
}

function installWorkbenchTools() {
  const trigger = el('button', 'icon-button'); trigger.type = 'button'; trigger.id = 'tools-open';
  trigger.append(svgIcon('tools'));
  CLATI18n.attribute(trigger, 'aria-label', () => CLATI18n.msg('tui.tools'));
  CLATI18n.attribute(trigger, 'title', () => CLATI18n.msg('tui.tools'));
  trigger.setAttribute('aria-expanded', 'false'); trigger.setAttribute('aria-controls', 'workbench-tools');
  const menu = el('div', 'workbench-tools hidden'); menu.id = 'workbench-tools'; CLATI18n.attribute(menu,'aria-label',()=>CLATI18n.msg("web.workbench_tools", {}, "Workbench tools"));
  document.getElementById('review-open').before(trigger, menu);
  for (const [id,icon,key] of [
    ['files-open','folder','experience.files'],
    ['review-open','tool','experience.changes'],
    ['turn-review-open','check','experience.turn'],
    ['tasks-open','jobs','experience.tasks'],
    ['workflow-open','plan','experience.workflow'],
    ['help-open','help','web.help'],
  ]) {
    const button = document.getElementById(id);
    button.className = 'workbench-tool-action';
    // Retire the trigger's old text binding; translations now belong to its label.
    CLATI18n.bind(button,'textContent',()=> '');
    const label=()=>CLATI18n.msg(key);
    button.append(svgIcon(icon),el('span','',label));
    CLATI18n.attribute(button,'aria-label',label);
    CLATI18n.attribute(button,'title',label);
    if(id==='help-open') {const divider=el('hr','menu-separator');divider.setAttribute('aria-hidden','true');menu.append(divider);}
    menu.append(button);
  }
  const close = () => { menu.classList.add('hidden'); trigger.setAttribute('aria-expanded', 'false'); };
  trigger.addEventListener('click', () => { const opened = menu.classList.toggle('hidden'); trigger.setAttribute('aria-expanded', String(!opened)); });
  menu.addEventListener('click', (event) => { if (event.target.closest('button')) close(); });
  menu.addEventListener('keydown', (event) => { if (event.key === 'Escape') { event.preventDefault(); close(); trigger.focus(); } });
  document.addEventListener('click', (event) => { if (!menu.contains(event.target) && !trigger.contains(event.target)) close(); });
}
