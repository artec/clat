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
  const latest = el('button', 'ghost back-to-latest hidden', () => (CLATI18n.msg("web.latest_messages",{},"↓ Latest messages"))); latest.id = 'back-to-latest'; latest.type = 'button';
  document.getElementById('transcript-scroll').after(latest);
  const viewport = document.getElementById('transcript-scroll');
  const updateLatest = () => latest.classList.toggle('hidden', nearEnd());
  viewport.addEventListener('scroll', updateLatest, { passive: true });
  if (typeof ResizeObserver === 'function') new ResizeObserver(updateLatest).observe(dom.transcript);
  latest.addEventListener('click', () => { viewport.scrollTop = viewport.scrollHeight; latest.classList.add('hidden'); });
  const dialog = el('section', 'help-page'); dialog.id = 'help-dialog'; CLATI18n.attribute(dialog,'aria-label',()=>CLATI18n.msg("web.help_and_keyboard_shortcuts", {}, "Help and keyboard shortcuts"));
  const header = el('div', 'review-header'); const close = el('button', 'icon-button', () => ('×')); CLATI18n.attribute(close,'aria-label',()=>CLATI18n.msg("web.close_help", {}, "Close help"));
  header.append(el('h2', '', () => (CLATI18n.msg("web.help", {}, "Help")))); const content = el('section', '');
  content.append(el('h3', '', () => (CLATI18n.msg("web.keyboard_and_navigation", {}, "Keyboard and navigation"))), el('p', '', () => (CLATI18n.msg("web.enter_sends_shift_enter_adds_a_line_ctrl_f_finds_original_conversation_text_opens_", {}, "Enter sends; Shift+Enter adds a line. Ctrl/⌘ F finds original conversation text. / opens command discovery (selection fills draft, never auto-executes). Escape closes overlays. Browser Ctrl/⌘ L and IME composition remain unchanged."))),
    el('p', '', () => (CLATI18n.msg("web.find_works_on_loaded_messages_earlier_extends_the_search_latest_messages_returns_t", {}, "Find works on loaded messages; “Earlier” extends the search. Latest messages returns to the bottom. Files quotes fixed text snapshots. Captured operations is not the entire Git working tree."))));
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
  const keyboard = el('article','help-card'); while(content.firstChild) keyboard.append(content.firstChild);
  const grid = el('div','help-grid');
  for(const [key, target] of [['getting_started','projects'],['model_help','models'],['permission_help','permissions']]) {
    const card=el('article','help-card');card.append(el('h3','',()=>pageText(key)),el('p','',()=>pageText(key+'_body')),pageAction('open_settings',()=>openSettings(target)));grid.append(card);
  }
  grid.append(keyboard);
  const troubleshoot=el('article','help-card');troubleshoot.append(el('h3','',()=>pageText('troubleshooting')),el('p','',()=>pageText('troubleshooting_body')),pageAction('history_help',()=>document.getElementById('nav-history').click()),pageAction('tasks_help',()=>document.getElementById('nav-tasks').click()));grid.append(troubleshoot);content.append(grid);
  dialog.append(header, content); document.body.append(dialog);
  trigger.addEventListener('click', () => presentPage(dialog));

}
