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

function completeOneTimeTask(rule, delivery) {
  return (rule.spec.trigger.kind === 'job' || rule.spec.trigger.rule.kind === 'at')
    && delivery?.phase === 'completed' && !rule.next_at
    && (rule.phase === 'enabled' || rule.phase === 'exhausted');
}

function taskDeliveryFacts(deliveries) {
  const latest = new Map(), active = new Set();
  for (const delivery of deliveries) {
    if (delivery.finished_at == null) active.add(delivery.rule_id);
    if ((latest.get(delivery.rule_id)?.started_at ?? -Infinity) <= delivery.started_at) {
      latest.set(delivery.rule_id, delivery);
    }
  }
  return {latest, active};
}

function scheduleNeedsAttention(rule, delivery) {
  return Boolean(rule.blocked) || rule.phase === 'draft'
    || rule.phase === 'exhausted' && !completeOneTimeTask(rule, delivery);
}

function taskRows(project) {
  const {latest, active} = taskDeliveryFacts(project.deliveries);
  const schedules = project.rules.map(rule => ({
    id: rule.id, type: 'schedules', value: rule, project,
    name: rule.spec.title || rule.spec.instruction.split('\n')[0],
    running: active.has(rule.id), lastDelivery:latest.get(rule.id),
    upcoming: rule.phase === 'enabled' && !rule.blocked && (rule.next_at != null || rule.spec.trigger.kind === 'job'),
    attention: scheduleNeedsAttention(rule, latest.get(rule.id))
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
  CLATI18n.attribute(select, 'aria-label', () => pageText(label));
  for (const key of options) {
    const option = el('option', '', () => pageText(key === 'all' ? allKey : key));
    option.value = key; select.append(option);
  }
  return select;
}

function taskDetailChanged(previous, next) {
  const a = previous.value, b = next.value;
  return a.revision !== b.revision || a.state !== b.state || a.phase !== b.phase
    || a.blocked !== b.blocked || a.next_at !== b.next_at || previous.running !== next.running
    || previous.lastDelivery?.id !== next.lastDelivery?.id || previous.lastDelivery?.phase !== next.lastDelivery?.phase;
}
function taskPhaseText(rule, delivery) {
  if (completeOneTimeTask(rule, delivery)) return pageText('completed');
  return rule.phase === 'exhausted' ? pageText('task_limit_reached') : CLATI18n.msg('schedule.' + rule.phase);
}
function taskRowGroup(row) {
  if (row.type === 'jobs') return 'jobs';
  return row.value.phase === 'paused' ? 'paused' : 'schedules';
}
function makeTaskRow(row, {isSelected, choose, showProject, canManage, onMenu}) {
  const schedule = row.type === 'schedules';
  const status = () => {
    if (!schedule) return CLATI18n.msg('job.' + row.value.state);
    if (row.running) return pageText('running');
    if (row.attention && row.value.phase === 'enabled') return pageText('attention');
    return taskPhaseText(row.value, row.lastDelivery);
  };
  const meta = () => [
    showProject ? row.project.title : '',
    status(),
    schedule ? scheduleTriggerText(row.value.spec.trigger) : '',
  ].filter(Boolean).join(' · ');
  const item = el('div', 'task-directory-item');
  const button = navigationRow({title:row.name, meta, icon:schedule ? 'schedule' : 'jobs',
    selected:isSelected, action:() => void choose(row)});
  button.dataset.taskId = row.id;
  if (showProject) button.title = row.project.root;
  item.append(button);
  if (schedule && canManage) item.append(taskMenuButton(event => onMenu(event.currentTarget, row.value)));
  return item;
}
function renderTaskList(list, items, context) {
  const top = list.scrollTop;
  list.replaceChildren();
  for (const key of ['schedules', 'paused', 'jobs']) {
    const rows = items.filter(row => taskRowGroup(row) === key);
    if (!rows.length) continue;
    const section = el('section', 'task-directory-group');
    section.append(el('h3', '', () => key === 'paused' ? CLATI18n.msg('schedule.paused') : pageText(key)));
    for (const row of rows) section.append(makeTaskRow(row, {...context, isSelected:context.isSelected(row), canManage:context.canManage(row)}));
    list.append(section);
  }
  if (!items.length) list.append(pageEmpty('empty_tasks'));
  list.scrollTop = top;
}
function taskOverview(actions) {
  const overview = navigationOverview({
    title:() => pageText('task_overview_title'), description:() => pageText('task_intro'), icon:'schedule',
    items:[['example_review', 'branch'], ['example_summary', 'document']].map(([key, icon]) => ({
      title:() => pageText(key), description:() => pageText(key + '_description'), icon,
      action:() => actions.create(key),
    })),
  });
  overview.classList.add('task-overview');
  overview.querySelectorAll('button').forEach(button => disableBuiltin(button, state.workbench, 'builtin.scheduler'));
  return overview;
}

/* Lists and details belong to the task page; the global inspector is independent. */
let taskCenter = null;
function createTaskCenter() {
  const page = el('section', 'task-center'); page.id = 'jobs-dialog';
  const type = taskFilterControl('task-type', 'task_type', TASK_TYPES, 'all_types');
  const phase = taskFilterControl('task-status', 'task_status', TASK_STATUSES, 'all_statuses');
  const {element: scope, input: all, text: trustedProjects} = navigationTrustedProjects('task-all-projects');
  const fields = el('div', 'task-filter-row'); fields.append(type, phase);
  const status = el('p', 'navigation-status'); status.setAttribute('role', 'status');
  const view = createNavigationPage(page, {title: () => shellText('tasks'), searchLabel: () => pageText('search_tasks'), onOverview:returnToOverview});
  const {list, body: detail, query} = view;
  const create = shellButton('task-create', 'plus', () => pageText('create_task'), () => scheduleActions?.create(), false);
  create.dataset.action = 'create_task'; create.classList.add('task-create');
  view.head.after(create); view.filters.append(scope, fields);
  view.sidebar.insertBefore(status, list); document.body.append(page);
  const menus = createTaskMenu();
  let generation = 0, selection = 0, selected = null, restoreId = null;
  let projects = [], items = [], reader = null, scheduleActions = null, reading = 0;
  const projectRoot = () => state.workbench?.project?.root;
  const fail = error => CLATI18n.bind(status, 'textContent', () => CLATI18n.present(error?.presentation, error?.message || ''));
  view.context({
    actions: () => [viewAction('refresh', 'refresh', () => pageText('refresh'), () => refresh())],
    crumbs: () => [
      {label: () => shellText('tasks'), run: returnToOverview},
      {label: () => all.checked ? trustedProjects() : projectDisplayName(projectRoot(), state.workbench?.project?.name)},
      ...(selected && all.checked ? [{label: () => selected.project.title}] : []),
      ...(selected ? [{label: () => selected.name}] : []),
    ],
  });
  const same = (first, second) => first?.id === second.id && first?.type === second.type
    && first?.project.root === second.project.root;
  const filtered = row => (type.value === 'all' || row.type === type.value)
    && (phase.value === 'all' || row[phase.value]);
  function route(id = null) {
    const params = new URLSearchParams();
    if (type.value !== 'all') params.set('type', type.value);
    if (phase.value !== 'all') params.set('status', phase.value);
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
    menus.close();
    const term = query.value.trim().toLocaleLowerCase();
    const matches = items.filter(row => filtered(row) && (row.name.toLocaleLowerCase().includes(term)
      || all.checked && row.project.title.toLocaleLowerCase().includes(term)));
    renderTaskList(list, matches, {
      isSelected:row => same(selected, row), choose, showProject:all.checked,
      canManage:row => Boolean(scheduleActions) && row.project.root === projectRoot(),
      onMenu:(source, rule) => menus.open(source, scheduleMenuActions(rule, scheduleActions)),
    });
  }
  async function choose(row) {
    if (row.project.root === projectRoot()) { show(row); return; }
    try {
      await context.openProject({root: row.project.root}, {hash: '#/' + route(row.id)});
    } catch (error) { if (context.isActive()) fail(error); }
  }
  function clearSelection() {
    selection++; reader?.dispose(); reader = null; selected = null; menus.close();
    returnConversationPane(); detail.classList.remove('has-live-conversation'); view.reset();
    for (const child of [...view.bar.children]) {
      if (child !== view.name && !child.matches('.navigation-back, .navigation-overview-back')) child.remove();
    }
  }
  function show(row, preserve = false) {
    const top = preserve ? reading : 0;
    clearSelection(); selected = row; restoreId = row.id;
    view.show(row.name); navigate(); rows();
    const owner = selection, current = () => owner === selection && pageIsOpen(page) && context.isActive();
    if (row.type === 'schedules') {
      reader = renderScheduleTask(detail, row.value, {actionsPort:scheduleActions, deliveries:row.project.deliveries, current, menus});
    } else renderJobDetail(detail, row.value, current, fail);
    detail.querySelector(':scope > h3')?.remove();
    const actions = detail.querySelector(':scope > .page-toolbar'); if (actions) view.bar.append(actions);
    const restore = () => { if (current()) detail.scrollTop = top; };
    requestAnimationFrame(restore); if (reader?.ready) void reader.ready.then(restore);
  }
  function overview() {
    if (selected || !scheduleActions) return;
    detail.replaceChildren(taskOverview(scheduleActions));
  }
  function returnToOverview() {
    restoreId = null; reading = 0;
    clearSelection(); overview(); rows(); detail.scrollTop = 0; navigate();
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
    disableBuiltin(create, state.workbench, 'builtin.scheduler');
    const request = ++generation, active = () => request === generation && pageIsOpen(page) && context.isActive();
    try {
      const values = await load(); if (!active()) return;
      projects = values.projects; items = projects.flatMap(taskRows);
      CLATI18n.bind(status, 'textContent', () => [
        ...['builtin.jobs', 'builtin.scheduler'].filter(id => !builtinEnabled(state.workbench, id)).map(id => builtinReason(state.workbench, id)),
        ...values.errors.map(error => CLATI18n.present(error.presentation || error, error.message || '')),
      ].join(' · '));
      const latest = items.find(row => row.id === restoreId && row.project.root === projectRoot());
      if (latest && (!selected || !same(selected, latest) || taskDetailChanged(selected, latest))) {
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
  type.onchange = phase.onchange = filtersChanged;
  all.onchange = updateScope; query.oninput = rows;
  page.addEventListener('clat:page-capture', () => { reading = detail.scrollTop; });
  page.addEventListener('clat:page-enter', () => { if (selected) show(selected, true); refreshViewContext(); });
  page.addEventListener('clat:page-leave', () => { generation++; selection++; menus.close(); reader?.dispose(); returnConversationPane(); });
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
      type.value = TASK_TYPES.includes(nextType) ? nextType : 'all';
      phase.value = TASK_STATUSES.includes(nextStatus) ? nextStatus : 'all';
      const scopeChanged = all.checked !== (params.get('scope') === 'trusted');
      all.checked = params.get('scope') === 'trusted'; restoreId = params.get('task');
      if (scopeChanged) { projects = []; items = []; list.replaceChildren(); }
      if (scopeChanged || selected?.id !== restoreId) clearSelection();
      if (!restoreId) overview();
    },
    register(actions) { scheduleActions = actions; refreshViewContext(); },
    openSchedules() { type.value = 'schedules'; phase.value = 'all'; trigger.click(); },
    showSchedule(rule) {
      const local = projects.find(project => project.root === projectRoot()) || currentProject();
      const row = taskRows({...local, jobs:[], rules:[rule]})[0];
      if (!filtered(row)) { type.value = 'schedules'; phase.value = 'all'; }
      show(row);
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
      const {latest} = taskDeliveryFacts(log.deliveries);
      for(const rule of rules.rules)if(scheduleNeedsAttention(rule,latest.get(rule.id))){
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

function taskMenuButton(action) {
  const button = navigationIcon(() => pageText('actions'), 'more', action);
  button.classList.add('session-more', 'task-more');
  button.setAttribute('aria-haspopup', 'menu');
  button.setAttribute('aria-expanded', 'false');
  return button;
}
function createTaskMenu() {
  let menu = null, trigger = null;
  function close(focus = false) {
    if (!menu) return;
    menu.remove(); menu = null;
    trigger.setAttribute('aria-expanded', 'false');
    if (focus) trigger.focus();
    trigger = null;
  }
  document.addEventListener('pointerdown', event => {
    if (menu && !menu.contains(event.target) && !trigger.contains(event.target)) close();
  });
  window.addEventListener('resize', () => close(true));
  return {close, open(source, entries) {
    if (trigger === source) { close(true); return; }
    close();
    trigger = source; trigger.setAttribute('aria-expanded', 'true');
    menu = el('div', 'session-context-menu task-action-menu'); menu.setAttribute('role', 'menu');
    CLATI18n.attribute(menu, 'aria-label', () => pageText('actions'));
    for (const {key, icon, run, disabled} of entries) {
      const button = shellButton('', icon, () => pageText(key), () => { close(true); void run(); }, false);
      button.dataset.action = key; button.setAttribute('role', 'menuitem'); button.disabled = Boolean(disabled);
      menu.append(button);
    }
    menu.addEventListener('keydown', event => {
      if (event.key === 'Escape' || event.key === 'Tab') {
        if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); }
        close(true); return;
      }
      if (!['ArrowDown','ArrowUp','Home','End'].includes(event.key)) return;
      event.preventDefault();
      const buttons = [...menu.querySelectorAll('button:not(:disabled)')];
      const index = buttons.indexOf(document.activeElement), shift = event.key === 'ArrowDown' ? 1 : -1;
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + shift + buttons.length) % buttons.length;
      buttons[next]?.focus();
    });
    document.body.append(menu); positionSessionMenu(menu, 0, 0, source);
    menu.querySelector('button:not(:disabled)')?.focus();
  }};
}
function scheduleMenuActions(rule, actions) {
  const isTime = rule.spec.trigger.kind === 'time';
  const noJob = !isTime && !builtinEnabled(state.workbench, 'builtin.jobs');
  const entries = [{key:'edit', icon:'edit', run:() => actions.edit(rule), disabled:noJob}];
  if (rule.phase === 'enabled') {
    if (isTime) entries.push({key:'run_now', icon:'refresh', run:() => actions.run(rule), disabled:Boolean(rule.blocked)});
    entries.push({key:'pause', icon:'pause', run:() => actions.change(rule, 'pause')});
  }
  if (rule.phase === 'draft' || rule.phase === 'paused') {
    entries.push({key:'review', icon:'permission-write', run:() => actions.review(rule), disabled:noJob});
  }
  if (rule.phase === 'paused' && rule.authorization) {
    entries.push({key:'resume', icon:'schedule', run:() => actions.change(rule, 'resume'), disabled:noJob});
  }
  entries.push({key:'duplicate', icon:'copy', run:() => actions.duplicate(rule), disabled:noJob},
    {key:'delete', icon:'trash', run:async () => {
      if (await CLATDialog.confirm(CLATI18n.msg('schedule.delete_confirm'))) await actions.change(rule, 'delete');
    }});
  return entries;
}
function taskScheduleSummary(rule, deliveries) {
  const summary = el('section', 'task-schedule-summary');
  const icon = el('span', 'task-schedule-icon'); icon.append(svgIcon('schedule'));
  const facts = el('div', 'task-schedule-copy');
  facts.append(el('strong', '', () => scheduleTriggerText(rule.spec.trigger)));
  const next = rule.phase === 'enabled' && !rule.blocked ? rule.next_at : null;
  const records = deliveries.filter(record => record.rule_id === rule.id).reverse();
  const subtitle = () => next == null
    ? taskPhaseText(rule, records[0])
    : pageText('task_next_run', {time:new Intl.DateTimeFormat(CLATI18n.locale, {dateStyle:'medium', timeStyle:'short'}).format(next)});
  facts.append(el('span', '', subtitle)); summary.append(icon, facts);
  if (records.length) {
    const history = el('details', 'task-run-history'); history.append(el('summary', '', () => pageText('deliveries')));
    const entries = el('ul');
    for (const record of records) entries.append(el('li', 'task-delivery', () =>
      new Date(record.started_at).toLocaleString(CLATI18n.locale) + ' · ' + pageText(record.phase)));
    history.append(entries); summary.append(history);
  }
  return summary;
}
function renderScheduleTask(detail, rule, {actionsPort, deliveries, current, menus}) {
  const actions = el('div', 'page-toolbar task-detail-actions');
  const entries = scheduleMenuActions(rule, actionsPort);
  const primary = entries.find(item => ['run_now', 'review'].includes(item.key));
  if (primary) {
    const button = pageAction(primary.key, primary.run, true); button.disabled = Boolean(primary.disabled);
    actions.append(button);
  }
  const edit = entries[0], editButton = pageAction(edit.key, edit.run);
  editButton.disabled = Boolean(edit.disabled);
  actions.append(editButton, taskMenuButton(event => menus.open(event.currentTarget, entries)));
  detail.append(actions, taskScheduleSummary(rule, deliveries));
  if (rule.spec.trigger.kind === 'job' && !builtinEnabled(state.workbench, 'builtin.jobs')) {
    detail.append(el('p', 'settings-note', () => CLATI18n.msg('extension.job_source_paused')));
  }
  if (rule.blocked) detail.append(el('p', 'error', () => rule.blocked === 'budget_exhausted'
    ? pageText('task_limit_reached') : CLATI18n.msg(rule.blocked.includes('.') ? rule.blocked : 'schedule.' + rule.blocked)));
  const conversation = el('section', 'task-conversation'); detail.append(conversation);
  if (borrowConversationPane(conversation, rule.scope.session)) {
    detail.classList.add('has-live-conversation');
    return null;
  }
  return createConversationReader(conversation, {project:rule.scope.project, session:rule.scope.session, seq:null}, {editable:true, current});
}
