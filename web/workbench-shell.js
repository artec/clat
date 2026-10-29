/* Shared workbench navigation and view composition. Business actions use existing ports. */
let workbenchShell = null;

function shellText(key, args = {}) { return CLATI18n.msg('workbench.' + key, args); }
function shellButton(id, icon, label, action, tooltip = true) {
  const button = el('button', 'shell-button');
  button.id = id; button.type = 'button';
  button.append(svgIcon(icon), el('span', 'shell-label', label));
  CLATI18n.attribute(button, 'aria-label', label);
  if (tooltip) CLATI18n.attribute(button, 'title', label);
  button.addEventListener('click', action);
  return button;
}

function createShellRail(shell) {
  const rail = el('nav', 'global-rail'); rail.id = 'global-nav';
  CLATI18n.attribute(rail, 'aria-label', () => shellText('navigation'));
  const links = el('div', 'global-links');
  const home = shellButton('nav-workbench', 'agent', () => shellText('workbench'), () => { closeMobileSidebar(); shell.chat(); });
  const logo = el('span', 'clat-status-icon'); logo.setAttribute('aria-hidden', 'true');
  home.querySelector('svg').replaceWith(logo); links.append(home);
  for (const [id, icon, label, run] of [
    ['nav-terminal', 'terminal', 'terminal', () => shell.open('terminal-page')],
    ['nav-resources', 'folder', 'resources', () => shell.open('resources-dialog')],
    ['nav-tasks', 'schedule', 'tasks', () => shell.open('jobs-dialog')],
    ['nav-history', 'history', 'history', () => shell.open('history-dialog')],
    ['nav-extensions', 'wasm', 'extensions', () => openMarket()],
  ]) links.append(shellButton(id, icon, () => shellText(label), () => { closeMobileSidebar(); run(); }));
  const bottom = el('div', 'global-bottom sidebar-footer');
  bottom.append(shellButton('nav-settings', 'settings', () => shellText('settings'), () => openSettings()),
    shellButton('nav-help', 'help', () => shellText('help'), () => shell.open('help-dialog')));
  rail.append(links, bottom); dom.app.prepend(rail);
  return rail;
}

function createShellPages(shell) {
  const host = el('section', 'shell-pages hidden'); host.id = 'page-host';
  const taskNav = el('nav', 'page-tabs'); taskNav.id = 'task-page-tabs';
  taskNav.append(shellButton('task-page-jobs', 'jobs', () => CLATI18n.msg('job.open'), () => shell.open('jobs-dialog')),
    shellButton('task-page-schedules', 'schedule', () => CLATI18n.msg('schedule.open'), () => shell.open('schedules-dialog')));
  taskNav.hidden = true; host.append(taskNav); document.getElementById('content-area').append(host);
  for (const [id, trigger, route] of [
    ['settings-dialog', 'settings-open', 'settings'], ['market-dialog', 'market-open', 'extensions'],
    ['jobs-dialog', 'jobs-open', 'tasks'],
    ['history-dialog', 'history-open', 'history'], ['help-dialog', 'help-open', 'help'],
    ['resources-dialog', 'resources-open', 'resources'],
    ['terminal-page', 'terminal-open', 'terminal'],
  ]) shell.mount(id, trigger, route, host, 'page');
  return host;
}


function createShellInspector(shell) {
  const scroll = dom.inspector.querySelector('.inspector-scroll');
  const details = el('details', 'session-technical');
  details.append(el('summary', '', () => shellText('session_details')));
  while (scroll.firstChild) details.append(scroll.firstChild);
  const overview = el('section', 'context-overview'); overview.id = 'context-overview';
  const connection = el('div', 'context-status'); connection.id = 'context-connection'; connection.hidden=true; connection.setAttribute('role','status');
  const status = el('div', 'context-status'); status.id = 'context-status';
  const plan = el('div', 'context-plan'); plan.id = 'context-plan';
  const resources = el('section', 'context-resources'); resources.id = 'context-resources';
  overview.append(connection, status, plan, resources, details);
  const back = shellButton('context-overview-open', 'previous', () => experienceText('overview'), () => shell.overview());
  back.classList.add('extension-back'); back.hidden = true;
  const cards = el('nav', 'extension-modules');
  for (const {entry:id,icon,key,trigger} of WORKSPACE_TOOLS) {
    const button = shellButton(id,icon,() => experienceText(key),() => document.getElementById(trigger).click());
    const content = el('span', 'extension-module-content');
    const copy = el('span', 'extension-module-copy');
    copy.append(button.querySelector('.shell-label'), el('small','',() => experienceText(key+'_hint')));
    content.append(button.querySelector('svg'), copy);
    button.append(content); cards.append(button);
  }
  overview.insertBefore(cards,resources); scroll.append(back,overview);
  for (const {id,key} of WORKSPACE_TOOLS) shell.tools.register(id,key);
}

function mountShellDialog(shell, id, trigger, route, host, type) {
  const dialog = document.getElementById(id);
  if (!dialog) return;
  if (type === 'page') {
    host.append(dialog); dialog.hidden = true; dialog.classList.add('shell-surface', 'hidden');
    dialog.setAttribute('role', 'region');
    const seat = { dialog, trigger, route, type, active: false };
    shell.surfaces.set(id, seat); navigationPages.set(id, seat); return;
  }
  const nativeClose = dialog.close.bind(dialog);
  host.append(dialog); dialog.classList.add('shell-surface', 'hidden');
  dialog.setAttribute('aria-modal', 'false');
  dialog.setAttribute('role', 'region');
  shell.surfaces.set(id, { dialog, trigger, route, type });
  dialog.showModal = () => shell.show(id);
  dialog.close = value => {
    nativeClose(value); dialog.classList.add('hidden');
    if (type === 'page') shell.chat(); else shell.overview();
  };
  dialog.addEventListener('close', () => {
    if (dialog.open) return;
    if (type === 'page' && shell.page === id) shell.chat();
    if (type === 'side' && shell.side === id) shell.overview();
  });
  dialog.addEventListener('keydown', event => {
    if (event.key === 'Escape' && !event.defaultPrevented) { event.preventDefault(); dialog.close(); }
  });
}

function shellShow(shell, id, updateRoute = true) {
  const seat = shell.surfaces.get(id); if (!seat) return;
  shell[seat.type] = id;
  for (const [key, other] of shell.surfaces) {
    if (other.type !== seat.type || key === id) continue;
    if (other.type === 'page') leavePage(other.dialog);
    else if (other.dialog.open) HTMLDialogElement.prototype.close.call(other.dialog);
    other.dialog.classList.add('hidden');
  }
  if (seat.type === 'page') {
    captureProjectReading(); shell.tools?.global();
    closeMobileSidebar();
    shell.pageHost.classList.remove('hidden');
    dom.app.dataset.page = seat.route;
    document.getElementById('conversation-page').classList.add('hidden');
    syncPanelAccessibility();
    document.getElementById('task-page-tabs').hidden = true;
    markShellNavigation(seat.route.split('/')[0]);
    if (context.isActive() && updateRoute && !shell.routing) context.navigatePage(seat.route);
  } else {
    document.getElementById('context-overview').hidden = true;
    document.getElementById('context-overview-open').hidden = false;
    setInspector('open');
  }
  seat.dialog.classList.remove('hidden');
  // Full pages retain navigation focus; dialog.show() would focus the first control.
  if (seat.type === 'page') {
    const entering = !seat.active; seat.active = true; seat.dialog.hidden = false;
    if (entering) {seat.dialog.dispatchEvent(new Event('clat:page-enter'));shell.pageHost.scrollTop = seat.scrollTop || 0;}
  }
  else seat.dialog.show();
  refreshApplicationToolbar(shell);
}

function shellChat(shell, updateRoute = true) {
  returnConversationPane(); shell.page = null;
  for (const seat of shell.surfaces.values()) if (seat.type === 'page') leavePage(seat.dialog);
  shell.pageHost.classList.add('hidden'); delete dom.app.dataset.page;
  document.getElementById('conversation-page').classList.remove('hidden');
  restoreProjectReading();
  syncPanelAccessibility();
  markShellNavigation('workbench');
  if (context.isActive() && updateRoute && !shell.routing) context.navigatePage('workbench');
  shell.tools?.chat(false);
  refreshApplicationToolbar(shell);
}

function shellOverview(shell) {
  shell.side = null;
  for (const seat of shell.surfaces.values()) if (seat.type === 'side') {
    if (seat.dialog.open) HTMLDialogElement.prototype.close.call(seat.dialog);
    seat.dialog.classList.add('hidden');
  }
  document.getElementById('context-overview').hidden = false;
  document.getElementById('context-overview-open').hidden = true;
  setInspector('open'); void refreshShellContext();
}

function markShellNavigation(route) {
  for (const button of document.querySelectorAll('#global-nav .shell-button')) {
    if (button.id === 'nav-' + route) button.setAttribute('aria-current', 'page');
    else button.removeAttribute('aria-current');
  }
}

function installShellDesktopSettings() {
  window.addEventListener('clat-desktop-settings-mount', event => {
    const { panel } = event.detail;
    const category = document.querySelector('[data-settings-category="desktop"]') || el('button', '', () => shellText('updates_about'));
    category.type = 'button'; category.dataset.settingsCategory = 'desktop';
    category.onclick = () => selectSettingsCategory('desktop');
    const nav=document.querySelector('.settings-nav');if(!nav.contains(category))nav.append(category);
    const section = document.querySelector('[data-settings-panel="desktop"]') || el('section', 'settings-section desktop-settings-section');
    section.dataset.settingsPanel = 'desktop'; section.hidden = currentSettingsCategory !== 'desktop';
    section.replaceChildren(panel); document.querySelector('.settings-content').append(section);
    panel.classList.add('desktop-settings-integrated');
    decorateDesktopSettings(section,panel);
    panel.querySelector('[data-action="close"]').hidden = true;
    panel.querySelector('.desktop-language').hidden = true;
    if (!panel.open) panel.show();
    event.detail.show = () => openSettings('desktop');
    event.detail.handled = true;
  });
}

async function refreshShellContext() {
  const shell = workbenchShell;
  if (!shell || !state.connected) return;
  const generation = ++shell.contextRequest;
  const owner = [state.stream, state.selectionGeneration];
  const summary=document.getElementById('context-plan'),session=state.sessionId||'';
  if(summary.dataset.session!==session){summary.replaceChildren();summary.dataset.session=session;}
  try {
    const value = await rpc('workflow.details', {});
    if (generation !== shell.contextRequest || owner[0] !== state.stream || owner[1] !== state.selectionGeneration) return;
    shell.workflow = value;
    const status = document.getElementById('context-status');
    status.hidden = !value.busy;
    CLATI18n.bind(status, 'textContent', () => CLATI18n.msg(value.busy ? 'web.running_short' : 'web.idle'));
    renderShellWorkSummary(value);
    const badge = document.getElementById('composer-plan-toggle');
    badge?.setAttribute('aria-pressed', String(Boolean(value.plan_mode)));
    if (badge) badge.disabled = false;
    document.getElementById('add-plan').disabled = value.busy || shell.planPending;
    refreshShellResources();
  } catch (_) {
    if (generation !== shell.contextRequest || owner[0] !== state.stream || owner[1] !== state.selectionGeneration) return;
    const status=document.getElementById('context-status');status.hidden=false;
    CLATI18n.bind(status,'textContent',()=>experienceText('unavailable'));
  }
}

function renderShellWorkSummary(value){
  const target=document.getElementById('context-plan'),session=state.sessionId||'',expanded=target.dataset.session===session&&target.querySelector('details')?.open;
  target.dataset.session=session;target.replaceChildren();const todos=value.todos||[];
  if(!todos.length&&!value.approved_plan&&!value.plan_mode)return;
  const open=shellButton('context-plan-open','open',()=>shellText('plan_details'),()=>document.getElementById('workflow-open').click());open.classList.add('context-work-open');
  if(!todos.length){const row=el('div','context-work-heading');row.append(svgIcon('plan'),el('span','',()=>shellText(value.approved_plan?'approved_plan':'planning')),open);target.append(row);return;}
  const done=todos.filter(todo=>todo.status==='completed').length,details=el('details','context-work-details'),summary=el('summary','context-work-heading');details.open=Boolean(expanded);
  summary.append(svgIcon('task-list'),el('strong','',()=>shellText('run_tasks')),el('small','',()=>shellText('todo_progress',{done,total:todos.length})));details.append(summary);
  const current=todos.find(todo=>todo.status==='in_progress')||todos.find(todo=>todo.status!=='completed'),preview=current?el('p','context-work-preview',current.content):null;if(preview)CLATI18n.attribute(preview,'title',()=>current.content);
  const list=el('div','context-work-list');for(const todo of todos){const row=el('div','context-work-item');row.classList.toggle('is-completed',todo.status==='completed');row.classList.toggle('is-current',todo.status==='in_progress');row.append(svgIcon(todo.status==='completed'?'check':'task-list'),el('span','',todo.content));list.append(row);}details.append(list,open);target.append(details);if(preview)target.append(preview);
}

function refreshShellResources() {
  const target = document.getElementById('context-resources');
  target.replaceChildren(el('h2', '', () => shellText('resources')));
  const sources = [...dom.transcript.querySelectorAll('.message-attachment')];
  target.hidden = sources.length === 0; if(!sources.length)return;
  for (const source of sources.slice(-8)) {
    const button = shellButton('', 'vision', () => source.querySelector('.message-attachment-label').textContent, () => source.click());
    target.append(button);
  }
  target.append(shellButton('context-resources-files', 'folder', () => shellText('reference_file'), () => document.getElementById('files-open').click()));
}

function createShellComposer(shell) {
  const trigger = shellButton('composer-add', 'open', () => shellText('add'), () => popover.toggle(), false);
  trigger.classList.add('composer-add');
  trigger.replaceChildren(dom['attachment-open'].querySelector('svg').cloneNode(true));
  const menu = el('section', 'composer-add-menu'); menu.id = 'composer-add-menu'; menu.hidden = true;
  menu.setAttribute('role', 'dialog'); trigger.setAttribute('aria-haspopup', 'dialog');
  trigger.setAttribute('aria-controls', menu.id); trigger.setAttribute('aria-expanded', 'false');
  CLATI18n.attribute(menu, 'aria-label', () => shellText('add'));
  const group = (label, rows) => {
    const block = el('div', 'composer-add-group'); block.append(el('h3', '', label));
    for (const [id, icon, text, run] of rows) block.append(shellButton(id, icon, text, () => { popover.close(); run(); }));
    menu.append(block);
  };
  group(() => shellText('add_content'), [
    ['add-image', 'vision', () => CLATI18n.msg('web.add_images'), () => dom['attachment-input'].click()],
    ['add-project-file', 'open', () => shellText('reference_file'), () => document.getElementById('files-open').click()],
  ]);
  group(() => shellText('work_mode'), [
    ['add-plan', 'plan', () => shellText('plan_mode'), () => toggleShellPlan()],
    ['add-goal', 'check', () => shellText('goal'), () => document.getElementById('workflow-open').click()],
    ['add-suggestion','trace',()=>experienceText('manual_suggestion'),()=>requestSuggestion()],
  ]);
  group(() => shellText('capabilities'), [
    ['add-skills', 'tool', () => shellText('skills'), () => { dom.prompt.focus(); dom.prompt.value += (dom.prompt.value ? '\n' : '') + '/'; state.composerGeneration++; resizePrompt(); dom.prompt.dispatchEvent(new Event('input', {bubbles:true})); }],
    ['add-extensions', 'wasm', () => shellText('extensions'), () => openMarket()],
  ]);
  const plan = shellButton('composer-plan-toggle', 'plan', () => workflowText('work_mode'), () => {}, false);
  installWorkModeMenu(plan);
  dom['attachment-open'].classList.add('hidden');
  dom['attachment-open'].before(trigger);
  (dom['composer-permission'].closest('.permission-seat') || dom['composer-permission']).after(plan);
  const popover = attachComposerPopup(trigger, trigger, menu, () => {}, 'left');
}

async function toggleShellPlan() {
  if (state.runActive || state.compactionActive || state.switching || workbenchShell.planPending) return;
  workbenchShell.planPending = true;
  const owner = [state.selectionGeneration, state.stream];
  const current = () => owner[0] === state.selectionGeneration && owner[1] === state.stream;
  try {
    await sendWorkflowIntent({action:'mode',mode:'plan'});
    if (current()) { await refreshWorkbench(); await refreshShellContext(); }
  } catch (error) { if (current()) updateRunState(error.message); }
  finally { workbenchShell.planPending = false; void refreshShellContext(); }
}

function installWorkbenchShell() {
  const shell = { surfaces: new Map(), contextRequest: 0, workflow: null };
  workbenchShell = shell;
  shell.mount = (...args) => mountShellDialog(shell, ...args);
  shell.show = (id, route) => shellShow(shell, id, route);
  shell.chat = route => shellChat(shell, route);
  shell.overview = () => shellOverview(shell);
  shell.open = id => { if(id === 'schedules-dialog') { taskCenter.openSchedules(); return; } const seat = shell.surfaces.get(id); if (seat) document.getElementById(seat.trigger)?.click(); };
  createShellRail(shell); createResourceLibrary(shell); shell.pageHost = createShellPages(shell);
  shell.tools = installWorkspaceTools(shell);
  createShellInspector(shell); createShellComposer(shell);
  registerGlobalViewContexts(shell);createApplicationToolbar(shell);
  installShellDesktopSettings(); installInteractionDock(); installInspectorResize(); markShellNavigation('workbench');
  const route = () => {
    const raw=location.hash.slice(2), path=raw.split('?')[0];
    const target = [...shell.surfaces].find(([, seat]) => seat.type === 'page' && seat.route === path);
    if(path==='tasks/schedules'){taskCenter.restore(new URLSearchParams('type=schedules'));}
    else if(path==='tasks/jobs'){taskCenter.restore(new URLSearchParams('type=jobs'));}
    else if(path==='tasks')taskCenter.restore(new URLSearchParams(raw.split('?')[1]||''));
    shell.routing = true;
    try { if (target) shell.open(target[0]); else if(path.startsWith('tasks/'))shell.open('jobs-dialog'); else { shell.chat(false); shell.tools.restore(new URLSearchParams(raw.split('?')[1] || '')); } }
    finally { shell.routing = false; }
  };
  window.addEventListener('popstate', route);
  window.addEventListener('clat:activated', route);
  window.addEventListener('clat:workbench-updated', () => { refreshApplicationToolbar(shell); void refreshShellContext(); });
  if (context.isActive() && location.hash && location.hash !== '#/workbench') route();
  context.renderDirectory();
}
