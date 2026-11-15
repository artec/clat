/* Independent sections: configured absence skips RPC; genuine failures stay visible. */
async function loadTaskCenterContent(availability,call=rpc){
  const requests=[['builtin.jobs','jobs.list','jobs'],['builtin.scheduler','schedule.list','rules'],['builtin.scheduler','schedule.deliveries','deliveries']];
  const results=await Promise.allSettled(requests.map(([id,method,key])=>builtinEnabled(availability,id)?call(method,{}):Promise.resolve({[key]:[]})));
  const content={jobs:[],rules:[],deliveries:[],errors:[]};
  results.forEach((result,index)=>{const key=requests[index][2];if(result.status==='fulfilled')content[key]=result.value[key];else content.errors.push(result.reason);});
  return content;
}
const TASK_TYPES = ['all', 'schedules', 'jobs'];
const TASK_STATUSES = ['all', 'running', 'upcoming', 'attention'];

function taskRows(project) {
  const latest = new Map(), active = new Set();
  for (const delivery of project.deliveries) {
    if (delivery.finished_at == null) active.add(delivery.rule_id);
    if ((latest.get(delivery.rule_id)?.started_at ?? -Infinity) <= delivery.started_at) {
      latest.set(delivery.rule_id, delivery);
    }
  }
  const schedules = project.rules.map(rule => ({
    id: rule.id, type: 'schedules', value: rule, project,
    name: rule.spec.title || rule.spec.instruction.split('\n')[0],
    running: active.has(rule.id),
    upcoming: rule.phase === 'enabled' && !rule.blocked && (rule.next_at != null || rule.spec.trigger.kind === 'job'),
    attention: Boolean(rule.blocked) || ['draft', 'exhausted'].includes(rule.phase)
      || ['failed', 'interrupted'].includes(latest.get(rule.id)?.phase),
  }));
  const jobs = project.jobs.map(job => ({
    id: job.id, type: 'jobs', value: job, project, name: job.label,
    running: ['starting', 'running'].includes(job.state),
    attention: ['failed', 'interrupted', 'unknown'].includes(job.state)
      || Boolean(job.persistence_error || job.cleanup_error),
  }));
  return [...schedules, ...jobs];
}

function taskFilterControl(id, label, options, allKey) {
  const select = el('select'); select.id = id;
  for (const key of options) {
    const option = el('option', '', () => pageText(key === 'all' ? allKey : key));
    option.value = key; select.append(option);
  }
  const field = el('label', 'task-filter-field');
  const control = el('span', 'task-filter-select');
  control.append(select, svgIcon('chevron-down'));
  field.append(el('span', '', () => pageText(label)), control);
  return { select, field };
}

/* Lists and details belong to the task page; the global inspector is independent. */
let taskCenter = null;
function createTaskCenter() {
  const page = el('section', 'task-center'); page.id = 'jobs-dialog';
  const type = taskFilterControl('task-type', 'task_type', TASK_TYPES, 'all_types');
  const phase = taskFilterControl('task-status', 'task_status', TASK_STATUSES, 'all_statuses');
  const {element: scope, input: all, text: trustedProjects} = navigationTrustedProjects('task-all-projects');
  const fields = el('div', 'task-filter-row'); fields.append(type.field, phase.field);
  const status = el('p', 'navigation-status'); status.setAttribute('role', 'status');
  const view = createNavigationPage(page, {title: () => shellText('tasks'), searchLabel: () => pageText('search_tasks')});
  const {list, body: detail, query} = view;
  view.filters.append(scope, fields);
  view.sidebar.insertBefore(status, list); document.body.append(page);
  let generation = 0, selection = 0, selected = null, restoreId = null;
  let projects = [], items = [], reader = null, scheduleActions = null, reading = 0;
  const projectRoot = () => state.workbench?.project?.root;
  const fail = error => CLATI18n.bind(status, 'textContent', () => CLATI18n.present(error?.presentation, error?.message || ''));
  view.context({
    actions: () => [viewAction('refresh', 'refresh', () => pageText('refresh'), () => refresh()),
      ...(scheduleActions ? [viewAction('create', 'plus', () => pageText('create_task'),
        () => scheduleActions.create(), true, () => builtinEnabled(state.workbench, 'builtin.scheduler'))] : [])],
    crumbs: () => [
      {label: () => shellText('tasks'), run: () => { delete page.dataset.detail; }},
      {label: () => all.checked ? trustedProjects() : projectDisplayName(projectRoot(), state.workbench?.project?.name)},
      ...(selected && all.checked ? [{label: () => selected.project.title}] : []),
      ...(selected ? [{label: () => selected.name}] : []),
    ],
  });
  const same = (first, second) => first?.id === second.id && first?.type === second.type
    && first?.project.root === second.project.root;
  const filtered = row => (type.select.value === 'all' || row.type === type.select.value)
    && (phase.select.value === 'all' || row[phase.select.value]);
  function route(id = null) {
    const params = new URLSearchParams();
    if (type.select.value !== 'all') params.set('type', type.select.value);
    if (phase.select.value !== 'all') params.set('status', phase.select.value);
    if (all.checked) params.set('scope', 'trusted');
    if (id) params.set('task', id);
    const search = params.toString();
    return search ? 'tasks?' + search : 'tasks';
  }
  function navigate() {
    if (workbenchShell.routing || !context.isActive()) return;
    const address = route(restoreId); dom.app.dataset.page = address;
    context.navigatePage(address);
  }
  function rows() {
    const top = list.scrollTop, term = query.value.trim().toLocaleLowerCase();
    list.replaceChildren();
    for (const row of items) {
      if (!filtered(row) || !(row.name.toLocaleLowerCase().includes(term)
          || all.checked && row.project.title.toLocaleLowerCase().includes(term))) continue;
      const meta = () => [
        all.checked ? row.project.title : '',
        row.type === 'schedules'
          ? (row.running ? pageText('running') : row.attention ? pageText('attention') : CLATI18n.msg('schedule.' + row.value.phase))
            + ' · ' + scheduleTriggerText(row.value.spec.trigger)
          : CLATI18n.msg('job.' + row.value.state),
      ].filter(Boolean).join(' · ');
      const button = navigationRow({title: row.name, meta, icon: row.type === 'schedules' ? 'schedule' : 'jobs',
        selected: same(selected, row), action: () => void choose(row)});
      button.dataset.taskId = row.id;
      if (all.checked) button.title = row.project.root;
      list.append(button);
    }
    if (!list.childElementCount) list.append(pageEmpty('empty_tasks'));
    list.scrollTop = top;
  }
  async function choose(row) {
    if (row.project.root === projectRoot()) { show(row); return; }
    try {
      await context.openProject({root: row.project.root}, {hash: '#/' + route(row.id)});
    } catch (error) { if (context.isActive()) fail(error); }
  }
  function clearSelection() {
    selection++; reader?.dispose(); reader = null; selected = null;
    returnConversationPane(); detail.classList.remove('has-live-conversation'); view.reset();
    for (const child of [...view.bar.children]) {
      if (child !== view.name && !child.classList.contains('navigation-back')) child.remove();
    }
  }
  function show(row, preserve = false) {
    const top = preserve ? reading : 0;
    clearSelection(); selected = row; restoreId = row.id;
    view.show(row.name); navigate(); rows();
    const owner = selection, current = () => owner === selection && pageIsOpen(page) && context.isActive();
    if (row.type === 'schedules') {
      reader = renderScheduleTask(detail, row.value, {actionsPort: scheduleActions, deliveries: row.project.deliveries, current});
    } else renderJobDetail(detail, row.value, current, fail);
    detail.querySelector(':scope > h3')?.remove();
    const actions = detail.querySelector(':scope > .page-toolbar'); if (actions) view.bar.append(actions);
    const restore = () => { if (current()) detail.scrollTop = top; };
    requestAnimationFrame(restore); if (reader?.ready) void reader.ready.then(restore);
  }
  function overview() {
    if (selected) return;
    const create = pageAction('create_task', () => scheduleActions.create(), true);
    disableBuiltin(create, state.workbench, 'builtin.scheduler');
    detail.replaceChildren(pageEmpty('task_intro', create));
    const examples = el('div', 'task-examples');
    for (const key of ['example_review', 'example_summary']) {
      const button = pageAction(key, () => scheduleActions.create(key));
      disableBuiltin(button, state.workbench, 'builtin.scheduler');
      button.classList.add('task-example');
      button.append(el('small', '', () => pageText(key + '_description'))); examples.append(button);
    }
    detail.append(examples);
  }
  function currentProject(values = {jobs: [], rules: [], deliveries: []}) {
    return {...values, root: projectRoot(), title: projectDisplayName(projectRoot(), state.workbench?.project?.name)};
  }
  async function load() {
    if (all.checked) return rpc('task.directory', {});
    const local = await loadTaskCenterContent(state.workbench);
    return {projects: [currentProject(local)], errors: local.errors};
  }
  async function refresh() {
    if (!pageIsOpen(page) || !context.isActive()) return;
    const request = ++generation, active = () => request === generation && pageIsOpen(page) && context.isActive();
    try {
      const values = await load(); if (!active()) return;
      projects = values.projects; items = projects.flatMap(taskRows);
      CLATI18n.bind(status, 'textContent', () => [
        ...['builtin.jobs', 'builtin.scheduler'].filter(id => !builtinEnabled(state.workbench, id)).map(id => builtinReason(state.workbench, id)),
        ...values.errors.map(error => CLATI18n.present(error.presentation || error, error.message || '')),
      ].join(' · '));
      const latest = items.find(row => row.id === restoreId && row.project.root === projectRoot());
      if (latest && (!selected || !same(selected, latest) || latest.value.revision !== selected.value.revision
          || latest.value.state !== selected.value.state)) {
        reading = detail.scrollTop; show(latest, Boolean(selected));
      } else if (!latest && selected) { restoreId = null; clearSelection(); navigate(); }
      rows(); overview();
    } catch (error) { if (active()) { fail(error); rows(); overview(); } }
  }
  function filtersChanged() {
    if (selected && !filtered(selected)) { restoreId = null; clearSelection(); overview(); }
    rows(); navigate();
  }
  function updateScope() {
    projects = []; items = []; list.replaceChildren(); restoreId = null; clearSelection();
    CLATI18n.bind(status, 'textContent', () => pageText('loading'));
    navigate(); overview(); void refresh();
  }
  type.select.onchange = phase.select.onchange = filtersChanged;
  all.onchange = updateScope; query.oninput = rows;
  page.addEventListener('clat:page-capture', () => { reading = detail.scrollTop; });
  page.addEventListener('clat:page-enter', () => { if (selected) show(selected, true); refreshViewContext(); });
  page.addEventListener('clat:page-leave', () => { generation++; selection++; reader?.dispose(); returnConversationPane(); });
  for (const event of ['clat:job-change', 'clat:background-run']) window.addEventListener(event, () => void refresh());
  window.addEventListener('clat:workbench-updated', () => {
    if (borrowedConversation && borrowedConversation.session !== state.sessionId && selected && pageIsOpen(page)) show(selected);
    void refresh();
  });
  const trigger = sidebarToolButton('jobs-open', 'jobs', () => shellText('tasks')); trigger.hidden = true;
  trigger.onclick = () => { presentPage(page); navigate(); void refresh(); };
  document.querySelector('.sidebar-footer').append(trigger);
  return {
    page, interactionHost: view.detail, refresh, fail,
    restore(params) {
      const nextType = params.get('type'), nextStatus = params.get('status') || nextType;
      type.select.value = TASK_TYPES.includes(nextType) ? nextType : 'all';
      phase.select.value = TASK_STATUSES.includes(nextStatus) ? nextStatus : 'all';
      const scopeChanged = all.checked !== (params.get('scope') === 'trusted');
      all.checked = params.get('scope') === 'trusted'; restoreId = params.get('task');
      if (scopeChanged) { projects = []; items = []; list.replaceChildren(); }
      if (scopeChanged || selected?.id !== restoreId) clearSelection();
    },
    register(actions) { scheduleActions = actions; refreshViewContext(); },
    openSchedules() { type.select.value = 'schedules'; phase.select.value = 'all'; trigger.click(); },
    showSchedule(rule) {
      const local = projects.find(project => project.root === projectRoot()) || currentProject();
      show(taskRows({...local, rules: [rule]})[0]);
    },
  };
}

function installTaskNotifications() {
  const seen=new Set();let initialized=false,pending=false;
  async function refresh(){
    if(pending||!state.connected||!builtinEnabled(state.workbench,'builtin.scheduler'))return;pending=true;
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

function renderScheduleTask(detail,rule,{actionsPort,deliveries,current}){
      const actions = el('div', 'page-toolbar');
      actions.append(pageAction('edit', () => actionsPort.edit(rule)));
      if(rule.phase==='enabled'){
        if(rule.spec.trigger.kind==='time'){const run=pageAction('run_now',()=>void actionsPort.run(rule),true);run.disabled=Boolean(rule.blocked);actions.append(run);}
        actions.append(pageAction('pause',()=>void actionsPort.change(rule,'pause')));
      }else if(rule.phase==='draft'||rule.phase==='paused')actions.append(pageAction('review',()=>void actionsPort.review(rule),true));
      const menu=el('details','task-actions-menu');menu.append(el('summary','',()=>pageText('actions')));
      const commands=el('div','task-menu-items');commands.append(pageAction('duplicate',()=>{menu.open=false;actionsPort.duplicate(rule);}));
      if(rule.phase==='paused')commands.append(pageAction('resume',()=>void actionsPort.change(rule,'resume')));
      commands.append(pageAction('delete',async()=>{if(await CLATDialog.confirm(CLATI18n.msg('schedule.delete_confirm')))void actionsPort.change(rule,'delete');}));menu.append(commands);actions.append(menu);
      if(rule.spec.trigger.kind==='job'&&!builtinEnabled(state.workbench,'builtin.jobs')){
        for(const button of actions.querySelectorAll('button'))if(['edit','duplicate','review','resume'].some(key=>button.dataset.action===key))disableBuiltin(button,state.workbench,'builtin.jobs');
        detail.append(el('p','settings-note',()=>CLATI18n.msg('extension.job_source_paused')));
      }
      detail.append(el('h3', '', rule.spec.title || rule.spec.instruction.split('\n')[0]),el('p','page-scope',()=>pageText('task_scope',{session:state.sessions.find(s=>s.id===rule.scope.session)?.title||rule.scope.session,runs:String(rule.used.runs),limit:String(rule.spec.budget.max_runs)})), actions);
      if(rule.blocked)detail.append(el('p','error',()=>CLATI18n.msg(rule.blocked.includes('.')?rule.blocked:'schedule.'+rule.blocked)));
      const history = el('details'); history.append(el('summary', '', () => pageText('deliveries')));
      for (const record of deliveries.filter(d => d.rule_id === rule.id).reverse()) history.append(el('p', 'task-delivery', () => new Date(record.started_at).toLocaleString(CLATI18n.locale) + ' · ' + pageText(record.phase)));
      detail.append(history);
      const conversation = el('section', 'task-conversation'); detail.append(conversation);
      if(borrowConversationPane(conversation,rule.scope.session)){detail.classList.add('has-live-conversation');return null;}return createConversationReader(conversation, {project:rule.scope.project, session:rule.scope.session, seq:null}, {editable:true,current});
}
