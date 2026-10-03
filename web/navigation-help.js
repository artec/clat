const pageNotificationSeen = new Set();
let pageNotificationsEnabled = false;

function observePageNotification(kind, ctl) {
  if (!ctl || !state.connected) return;
  let id, text;
  if (kind === 'prompt.settled' && ['completed', 'failed', 'cancelled'].includes(ctl.outcome?.type)) {
    id = ctl.prompt_rpc_id; text = ctl.outcome.type === 'failed' ? 'Agent run failed' : ctl.outcome.type === 'cancelled' ? 'Agent run cancelled' : 'Agent run completed';
  } else if (kind === 'approval.requested') { id = ctl.rpc_id; text = 'Permission approval needed'; }
  else if (kind === 'notice' && ctl.kind === 'question_requested') { id = ctl.payload?.rpc_id; text = 'Agent needs your answer'; }
  if (!id || !text) return;
  const key = `${workspacePrefix}:${state.sessionId}:${kind}:${id}`;
  if (pageNotificationSeen.has(key)) return;
  pageNotificationSeen.add(key);
  if (pageNotificationSeen.size > 512) pageNotificationSeen.delete(pageNotificationSeen.values().next().value);
  if (!pageNotificationsEnabled || typeof Notification === 'undefined' || Notification.permission !== 'granted' || document.hasFocus()) return;
  try {
    const notification = new Notification('CLAT · ' + text, { body: 'Open the online CLAT page to inspect host facts.', tag: key });
    notification.onclick = () => { window.focus(); notification.close(); };
  } catch (_) { /* Some mobile browsers prohibit page Notification despite permission. */ }
}

function installNavigationHelp() {
  const trigger = el('button', 'ghost', 'Help'); trigger.type = 'button'; trigger.id = 'help-open';
  document.getElementById('review-open').after(trigger);
  const project = el('button', 'ghost', 'Open another project ↗'); project.type = 'button'; project.id = 'project-navigation';
  document.getElementById('project-name').after(project); project.addEventListener('click', () => { closeMobileSidebar(); openSettings('projects'); });
  const latest = el('button', 'ghost back-to-latest hidden', '↓ Latest messages'); latest.id = 'back-to-latest'; latest.type = 'button';
  document.getElementById('transcript-scroll').after(latest);
  const viewport = document.getElementById('transcript-scroll');
  const updateLatest = () => latest.classList.toggle('hidden', nearEnd());
  viewport.addEventListener('scroll', updateLatest, { passive: true });
  if (typeof ResizeObserver === 'function') new ResizeObserver(updateLatest).observe(dom.transcript);
  latest.addEventListener('click', () => { viewport.scrollTop = viewport.scrollHeight; latest.classList.add('hidden'); });
  const dialog = el('dialog', 'workflow-details'); dialog.id = 'help-dialog'; dialog.setAttribute('aria-label', 'Help and keyboard shortcuts');
  const header = el('div', 'review-header'); const close = el('button', 'icon-button', '×'); close.setAttribute('aria-label', 'Close help');
  header.append(el('h2', '', 'Help'), close); const content = el('section', '');
  content.append(el('h3', '', 'Keyboard and navigation'), el('p', '', 'Enter sends; Shift+Enter adds a line. Ctrl/⌘ F finds original conversation text. / opens command discovery (selection fills draft, never auto-executes). Escape closes overlays. Browser Ctrl/⌘ L and IME composition remain unchanged.'),
    el('p', '', 'Find works on loaded messages; “Earlier” extends the search. Latest messages returns to the bottom. Files quotes fixed text snapshots. Captured operations is not the entire Git working tree.'));
  const notifications = el('button', 'ghost', 'Enable online page notifications'); notifications.type = 'button'; notifications.id = 'page-notifications';
  const status = el('p', 'picker-hint', 'Opt-in only · online page only · no Web Push · closed browser/background mobile restrictions apply. No prompt or file contents in notifications.'); status.setAttribute('role', 'status');
  try { pageNotificationsEnabled = localStorage.getItem('clat.page-notifications.v1') === 'true'; } catch (_) {}
  const render = () => {
    notifications.disabled = typeof Notification === 'undefined';
    notifications.textContent = pageNotificationsEnabled ? 'Disable online page notifications' : 'Enable online page notifications';
    notifications.setAttribute('aria-pressed', String(pageNotificationsEnabled));
    if (notifications.disabled) status.textContent = 'Page notifications unsupported by this browser. No background push service.';
  };
  notifications.addEventListener('click', async () => {
    if (pageNotificationsEnabled) pageNotificationsEnabled = false;
    else {
      try { pageNotificationsEnabled = await Notification.requestPermission() === 'granted'; }
      catch (_) { pageNotificationsEnabled = false; }
      if (!pageNotificationsEnabled) status.textContent = 'Notification permission not granted. Enable it in browser settings if desired.';
    }
    try { localStorage.setItem('clat.page-notifications.v1', String(pageNotificationsEnabled)); } catch (_) {}
    render();
  });
  render(); content.append(el('h3', '', 'Online page notifications'), notifications, status);
  dialog.append(header, content); document.body.append(dialog);
  trigger.addEventListener('click', () => dialog.showModal()); close.addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', () => focusWorkbenchTool(trigger));
}
