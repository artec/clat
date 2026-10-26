/* Lists and details belong to the task page; the global inspector is independent. */
let taskCenter = null;
function createTaskCenter() {
  const page = el('section', 'task-center'); page.id = 'jobs-dialog';
  const head = pageHeader(page, () => shellText('tasks'), () => pageText('project_scope'));
  const filter = el('select'); filter.id = 'task-type';
  for (const key of ['all', 'schedules', 'jobs', 'attention', 'running', 'upcoming']) { const o = el('option', '', () => pageText(key)); o.value = key; filter.append(o); }
  const query = el('input'); query.type = 'search';CLATI18n.bind(query,'placeholder',()=>pageText('search_tasks')); CLATI18n.attribute(query, 'aria-label', () => pageText('search_tasks'));
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const list = el('nav', 'page-object-list'); const detail = el('section', 'page-detail task-detail');
  const body = el('div', 'page-list-detail'); body.append(list, detail);
  const toolbar = el('div', 'page-toolbar'); toolbar.append(filter, query, pageAction('refresh', () => void refresh()));
  const project=el('select');project.id='task-project';CLATI18n.attribute(project,'aria-label',()=>pageText('project_scope'));toolbar.prepend(project);
  project.onchange=()=>{const row=project.options[project.selectedIndex];void context.openProject({route_id:row.value,root:row.dataset.root},{hash:'#/tasks'}).then(target=>target?.root.querySelector('#nav-tasks')?.click()).catch(fail);};
  page.addEventListener('clat:page-enter',()=>{void rpc('workspace.directory',{}).then(value=>{project.replaceChildren();for(const row of value.projects){const o=el('option','',row.title);o.value=row.route_id||'';o.dataset.root=row.root;o.selected=row.root===state.workbench?.project?.root;project.append(o);}}).catch(()=>{});});
  page.append(toolbar, status, body); document.body.append(page);
  let generation = 0, selection = 0, selected = null, restoreId=null, rules = [], jobs = [], deliveries = [], reader = null, scheduleActions = null;
  const fail = error => CLATI18n.bind(status, 'textContent', () => error.message);
  function rows() {
    list.replaceChildren();
    const objects = [...rules.map(r => ({id:r.id, name:r.spec.title || r.spec.instruction.split('\n')[0], phase:r.blocked ? 'attention' : r.phase, type:'schedules', value:r})), ...jobs.map(j => ({id:j.id, name:j.label, phase:j.state, type:'jobs', value:j}))];
    const visible = objects.filter(row => (filter.value === 'all' || row.type === filter.value || filter.value==='running'&&(['running','starting'].includes(row.phase)||row.type==='schedules'&&deliveries.some(d=>d.rule_id===row.id&&d.finished_at===null)) || filter.value==='upcoming'&&row.type==='schedules'&&row.value.phase==='enabled' || filter.value === 'attention' && ['attention','failed','interrupted','unknown','exhausted','draft'].includes(row.phase)) && row.name.toLowerCase().includes(query.value.toLowerCase()));
    for (const row of visible) {
      const button = el('button', 'page-object'); button.type = 'button'; button.dataset.taskId = row.id;
      button.setAttribute('aria-current', String(selected?.id === row.id));
      button.append(el('strong', '', row.name), el('small', '', () => row.type === 'schedules' ? CLATI18n.msg('schedule.' + row.value.phase) + ' · ' + scheduleTriggerText(row.value.spec.trigger) : CLATI18n.msg('job.' + row.value.state)));
      button.onclick = () => show(row); list.append(button);
    }
    if (!visible.length) list.append(pageEmpty('empty_tasks'));
  }
  function show(row) {
    selected = row; restoreId=row.id;
    if(!workbenchShell.routing){const route='tasks?type='+encodeURIComponent(filter.value)+'&task='+encodeURIComponent(row.id);dom.app.dataset.page=route;context.navigatePage(route);}
    selection++; reader?.dispose(); returnConversationPane(); detail.classList.remove('has-live-conversation'); detail.replaceChildren(); rows();
    const owner = selection, current = () => owner === selection && pageIsOpen(page) && context.isActive();
    if (row.type === 'schedules') {
      const rule = row.value; const actions = el('div', 'page-toolbar');
      actions.append(pageAction('edit', () => scheduleActions.edit(rule)));
      if(rule.phase==='enabled'){
        if(rule.spec.trigger.kind==='time'){const run=pageAction('run_now',()=>void scheduleActions.run(rule),true);run.disabled=Boolean(rule.blocked);actions.append(run);}
        actions.append(pageAction('pause',()=>void scheduleActions.change(rule,'pause')));
      }else if(rule.phase==='draft'||rule.phase==='paused')actions.append(pageAction('review',()=>void scheduleActions.review(rule),true));
      const menu=el('details','task-actions-menu');menu.append(el('summary','',()=>pageText('actions')));
      const commands=el('div','task-menu-items');commands.append(pageAction('duplicate',()=>{menu.open=false;scheduleActions.duplicate(rule);}));
      if(rule.phase==='paused')commands.append(pageAction('resume',()=>void scheduleActions.change(rule,'resume')));
      commands.append(pageAction('delete',()=>{if(window.confirm(CLATI18n.msg('schedule.delete_confirm')))void scheduleActions.change(rule,'delete');}));menu.append(commands);actions.append(menu);
      detail.append(el('h3', '', rule.spec.title || rule.spec.instruction.split('\n')[0]),el('p','page-scope',()=>pageText('task_scope',{session:state.sessions.find(s=>s.id===rule.scope.session)?.title||rule.scope.session,runs:String(rule.used.runs),limit:String(rule.spec.budget.max_runs)})), actions);
      if(rule.blocked)detail.append(el('p','error',()=>CLATI18n.msg(rule.blocked.includes('.')?rule.blocked:'schedule.'+rule.blocked)));
      const history = el('details'); history.append(el('summary', '', () => pageText('deliveries')));
      for (const record of deliveries.filter(d => d.rule_id === rule.id).reverse()) history.append(el('p', 'task-delivery', () => new Date(record.started_at).toLocaleString(CLATI18n.locale) + ' · ' + pageText(record.phase)));
      detail.append(history);
      const conversation = el('section', 'task-conversation'); detail.append(conversation);
      if (!borrowConversationPane(conversation, rule.scope.session)) reader = createConversationReader(conversation, {project:rule.scope.project, session:rule.scope.session, seq:null}, {editable:true,current});
    } else renderJobDetail(detail, row.value, current, fail);
  }
  async function refresh() {
    const request = ++generation;
    try {
      const values = await Promise.all([rpc('jobs.list', {}), rpc('schedule.list', {}), rpc('schedule.deliveries', {})]);
      if (request !== generation || !pageIsOpen(page)) return;
      [jobs, rules, deliveries] = [values[0].jobs, values[1].rules, values[2].deliveries]; status.textContent='';rows();
      if(restoreId && selected?.id!==restoreId){const rule=rules.find(r=>r.id===restoreId),job=jobs.find(j=>j.id===restoreId);if(rule)show({id:rule.id,type:'schedules',value:rule});else if(job)show({id:job.id,type:'jobs',value:job});}
      if(selected){const latest=(selected.type==='schedules'?rules:jobs).find(item=>item.id===selected.id);if(!latest){selected=null;returnConversationPane();detail.replaceChildren();}else if(latest.revision!==selected.value.revision || latest.state!==selected.value.state)show({...selected,value:latest});}
      if (!selected) {detail.replaceChildren(pageEmpty('task_intro', pageAction('create_task', () => scheduleActions.create(), true)));const overview=el('div','task-overview');for(const key of ['attention','running','upcoming']){const button=pageAction(key,()=>{filter.value=key;rows();});overview.append(button);}detail.prepend(overview);const examples=el('div','task-examples');for(const key of ['example_review','example_summary']){const button=pageAction(key,()=>scheduleActions.create(key));button.classList.add('task-example');button.append(el('small','',()=>pageText(key+'_description')));examples.append(button);}detail.append(examples);}
    } catch (error) { if (request === generation) fail(error); }
  }
  filter.onchange=()=>{rows();const route='tasks?type='+filter.value+(selected?'&task='+encodeURIComponent(selected.id):'');dom.app.dataset.page=route;context.navigatePage(route);};query.oninput=rows;
  page.addEventListener('clat:page-enter',()=>{if(selected)show(selected);});
  page.addEventListener('clat:page-leave', () => { generation++; selection++; reader?.dispose(); returnConversationPane(); });
  for (const type of ['clat:job-change','clat:background-run']) window.addEventListener(type, () => { if (pageIsOpen(page)) void refresh(); });
  window.addEventListener('clat:workbench-updated',()=>{if(borrowedConversation && borrowedConversation.session!==state.sessionId && selected)show(selected);});
  const trigger = sidebarToolButton('jobs-open', 'jobs', () => shellText('tasks')); trigger.hidden = true;
  trigger.onclick = () => { presentPage(page); void refresh(); };
  document.querySelector('.sidebar-footer').append(trigger);
  return {page, refresh, fail, restore(params){filter.value=params.get('type')||'all';restoreId=params.get('task');}, register(actions) { scheduleActions = actions; head.append(pageAction('create_task', ()=>actions.create(), true)); }, openSchedules() { filter.value = 'schedules'; trigger.click(); }, showSchedule(rule) { show({id:rule.id, name:rule.spec.title || rule.spec.instruction, type:'schedules', value:rule}); }};
}

function installTaskNotifications() {
  const seen=new Set();let initialized=false,pending=false;
  async function refresh(){
    if(pending||!state.connected)return;pending=true;
    try{const [rules,log]=await Promise.all([rpc('schedule.list',{}),rpc('schedule.deliveries',{})]);
      for(const delivery of log.deliveries.filter(d=>d.finished_at!=null)){
        const key=workspacePrefix+':'+delivery.id+':'+delivery.phase;if(seen.has(key))continue;seen.add(key);if(seen.size>1024)seen.delete(seen.values().next().value);
        if(!initialized)continue;const rule=rules.rules.find(r=>r.id===delivery.rule_id);if(!rule)continue;
        const attention=['failed','interrupted'].includes(delivery.phase),policy=rule.spec.notification||'attention';
        if(attention)document.getElementById('nav-tasks').dataset.pending='true';
        if(policy==='off'||policy==='attention'&&!attention||!pageNotificationsEnabled||typeof Notification==='undefined'||Notification.permission!=='granted'||document.hasFocus())continue;
        const notification=new Notification('CLAT · '+pageText(delivery.phase),{body:pageText('task_notification'),tag:key});
        notification.onclick=()=>{window.focus();taskCenter.openSchedules();taskCenter.showSchedule(rule);notification.close();};
      }
      let needsAttention=log.deliveries.some(d=>['failed','interrupted'].includes(d.phase));
      for(const rule of rules.rules)if(rule.blocked||rule.phase==='exhausted'||rule.phase==='draft'){
        needsAttention=true;const key=workspacePrefix+':rule:'+rule.id+':'+rule.revision+':'+rule.phase+':'+String(rule.blocked);
        if(seen.has(key))continue;seen.add(key);if(seen.size>1024)seen.delete(seen.values().next().value);
        if(!initialized||rule.spec.notification==='off'||!pageNotificationsEnabled||typeof Notification==='undefined'||Notification.permission!=='granted'||document.hasFocus())continue;
        const notification=new Notification('CLAT · '+pageText('attention'),{body:pageText('task_notification'),tag:key});notification.onclick=()=>{window.focus();taskCenter.openSchedules();taskCenter.showSchedule(rule);notification.close();};
      }
      document.getElementById('nav-tasks').dataset.pending=String(needsAttention);
      initialized=true;
    }catch(_){}finally{pending=false;}
  }
  window.addEventListener('clat:background-run',event=>{if(event.detail.phase!=='event')void refresh();});
  window.addEventListener('clat:activated',()=>void refresh());window.addEventListener('clat:workbench-updated',()=>void refresh());void refresh();
}
