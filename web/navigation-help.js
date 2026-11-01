let historyNavigation = null;
function syncHistoryNavigation(){historyNavigation?.sync();}

const pageNotificationSeen = new Set();
let pageNotificationsEnabled = false;

function observePageNotification(kind, ctl) {
  if (!ctl || !state.connected) return;
  let id, text;
  if (kind === 'prompt.settled' && ['completed', 'failed', 'cancelled'].includes(ctl.outcome?.type)) {
    id = ctl.prompt_rpc_id; text = ctl.outcome.type === 'failed' ? CLATI18n.msg("web.agent_run_failed", {}, "Agent run failed") : ctl.outcome.type === 'cancelled' ? CLATI18n.msg("web.agent_run_cancelled", {}, "Agent run cancelled") : CLATI18n.msg("web.agent_run_completed", {}, "Agent run completed");
  } else if (kind === 'approval.requested') { id = ctl.rpc_id; text = CLATI18n.msg("web.permission_approval_needed", {}, "Permission approval needed"); }
  else if (kind === 'notice' && ctl.kind === 'question_requested') { id = ctl.payload?.rpc_id; text = CLATI18n.msg("web.agent_needs_your_answer", {}, "Agent needs your answer"); }
  if (!id || !text) return;
  const key = `${workspacePrefix}:${state.sessionId}:${kind}:${id}`;
  if (pageNotificationSeen.has(key)) return;
  pageNotificationSeen.add(key);
  if (pageNotificationSeen.size > 512) pageNotificationSeen.delete(pageNotificationSeen.values().next().value);
  if (!pageNotificationsEnabled || typeof Notification === 'undefined' || Notification.permission !== 'granted' || document.hasFocus()) return;
  try {
    const notification = new Notification('CLAT · ' + text, { body: CLATI18n.msg("web.open_the_online_clat_page_to_inspect_host_facts", {}, "Open the online CLAT page to inspect host facts."), tag: key });
    notification.onclick = () => { window.focus(); notification.close(); };
  } catch (_) { /* Some mobile browsers prohibit page Notification despite permission. */ }
}

function installNavigationHelp() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("web.help", {}, "Help"))); trigger.type = 'button'; trigger.id = 'help-open';
  document.getElementById('review-open').after(trigger);
  const project = el('button', 'icon-button'); project.type = 'button'; project.id = 'project-navigation';
  project.append(svgIcon('open'));
  for (const name of ['title','aria-label']) CLATI18n.attribute(project,name,()=>CLATI18n.msg('web.open_project_navigation'));
  document.querySelector('.project-heading').append(project); project.addEventListener('click', () => { closeMobileSidebar(); openSettings('projects'); });
  historyNavigation = createHistoryNavigation();
  installPageNotifications();
  helpCenter = createHelpCenter();
  trigger.addEventListener('click', () => openHelp());
}

function installPageNotifications() {
  const notifications = el('button', 'ghost', () => (CLATI18n.msg("web.enable_online_page_notifications", {}, "Enable online page notifications"))); notifications.type = 'button'; notifications.id = 'page-notifications';
  const status = el('p', 'picker-hint', () => (CLATI18n.msg("web.notification_limits",{},"Opt-in only · online page only · no Web Push · closed browser/background mobile restrictions apply. No prompt or file contents in notifications."))); status.setAttribute('role', 'status');
  try { pageNotificationsEnabled = localStorage.getItem('clat.page-notifications.v1') === 'true'; } catch (_) {}
  const render = () => {
    notifications.disabled = typeof Notification === 'undefined';
    CLATI18n.bind(notifications, "textContent", () => pageNotificationsEnabled ? CLATI18n.msg("web.disable_online_page_notifications", {}, "Disable online page notifications") : CLATI18n.msg("web.enable_online_page_notifications", {}, "Enable online page notifications"));
    notifications.setAttribute('aria-pressed', String(pageNotificationsEnabled));
    if (notifications.disabled) CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.page_notifications_unsupported_by_this_browser_no_background_push_service", {}, "Page notifications unsupported by this browser. No background push service."));
  };
  notifications.addEventListener('click', async () => {
    if (pageNotificationsEnabled) pageNotificationsEnabled = false;
    else {
      try { pageNotificationsEnabled = await Notification.requestPermission() === 'granted'; }
      catch (_) { pageNotificationsEnabled = false; }
      if (!pageNotificationsEnabled) CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.notification_permission_not_granted_enable_it_in_browser_settings_if_desired", {}, "Notification permission not granted. Enable it in browser settings if desired."));
    }
    try { localStorage.setItem('clat.page-notifications.v1', String(pageNotificationsEnabled)); } catch (_) {}
    render();
  });
  const notificationGroup = el('section', 'notification-settings');
  render(); notificationGroup.append(el('h3', '', () => (CLATI18n.msg("web.online_page_notifications", {}, "Online page notifications"))), notifications, status);
  document.querySelector('[data-settings-panel=appearance]').append(notificationGroup);
}

// Floating history controls share the existing scroll owner and run lifecycle.
function createHistoryNavigation(){
  const viewport=dom['transcript-scroll'],latest=el('button','back-to-latest hidden');
  latest.id='back-to-latest';latest.type='button';
  const arrow=svgIcon('arrow-down');arrow.classList.add('history-latest-arrow');
  const dots=el('span','history-running-dots');dots.setAttribute('aria-hidden','true');
  dots.append(el('i'),el('i'),el('i'));latest.append(arrow,dots);viewport.after(latest);
  function sync(){
    const running=Boolean(state.connected&&state.runActive&&!state.switching&&context.isActive());
    latest.dataset.running=String(running);
    latest.classList.toggle('hidden',!running&&nearEnd());
    for(const attr of ['title','aria-label'])CLATI18n.attribute(latest,attr,()=>CLATI18n.msg(running?'history_nav.running_latest':'history_nav.latest'));
  }
  latest.onclick=()=>{viewport.scrollTop=viewport.scrollHeight;sync();};
  viewport.addEventListener('scroll',sync,{passive:true});
  if(typeof ResizeObserver==='function')new ResizeObserver(sync).observe(dom.transcript);
  window.addEventListener('clat:workbench-updated',sync);
  window.addEventListener('clat:activated',sync);
  sync();return {sync};
}

// Prepending history has one scroll owner: suppress native anchoring while
// changing layout, then restore the visible row in the same paint transaction.
function preserveHistoryPosition(update) {
  const viewport = dom['transcript-scroll'];
  const anchor = viewportAnchor();
  const offset = anchor ? anchor.getBoundingClientRect().top - viewport.getBoundingClientRect().top : null;
  const behavior = viewport.style.scrollBehavior;
  const anchoring = viewport.style.overflowAnchor;
  viewport.style.scrollBehavior = 'auto';
  viewport.style.overflowAnchor = 'none';
  try {
    update();
    if (anchor?.isConnected && offset !== null) {
      const delta = anchor.getBoundingClientRect().top - viewport.getBoundingClientRect().top - offset;
      viewport.scrollTop = viewport.scrollTop + delta;
    }
  } finally {
    viewport.style.overflowAnchor = anchoring;
    viewport.style.scrollBehavior = behavior;
  }
}
