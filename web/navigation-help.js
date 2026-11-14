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

function createTranscriptFollow(viewport, onChange) {
  let pinned = true;
  let lastTop = viewport.scrollTop;
  let frame = null;
  let touchY = null;

  function setPinned(value) {
    if (pinned === value) return;
    pinned = value;
    onChange();
  }
  function rememberPosition() { lastTop = viewport.scrollTop; }
  function scrollToEnd() {
    viewport.scrollTop = viewport.scrollHeight;
    rememberPosition();
  }
  function observeScroll() {
    if (!viewport.clientHeight) return;
    const top = viewport.scrollTop;
    const previous = lastTop;
    lastTop = top;
    if (top === previous) return;
    const bottom = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
    const atBottom = top >= bottom - 1; // scrollTop can be fractional.
    // A layout shrink can clamp scrollTop down to the new bottom.
    if (top < previous && !atBottom) setPinned(false);
    else if (top > previous && atBottom) setPinned(true);
  }
  function pause() {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
    rememberPosition();
    setPinned(false);
  }
  function resume() {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
    scrollToEnd();
    setPinned(true);
  }
  function schedule() {
    if (!pinned || frame !== null) return;
    frame = requestAnimationFrame(() => {
      frame = null;
      // A scrollbar drag or keyboard scroll can move before its scroll event is delivered.
      observeScroll();
      if (pinned && viewport.clientHeight) scrollToEnd();
    });
  }
  function beforeUpwardScroll(target) {
    if (!pinned || viewport.scrollTop <= 0) return;
    // Let scrollable reasoning and tool output handle their own gestures first.
    for (let node = target; node && node !== viewport; node = node.parentElement) {
      if (node.scrollTop > 0 && /^(auto|scroll|overlay)$/.test(getComputedStyle(node).overflowY)) return;
    }
    pause();
  }

  viewport.addEventListener('wheel', event => {
    if (!event.ctrlKey && !event.shiftKey && event.deltaY < 0 && -event.deltaY >= Math.abs(event.deltaX)) {
      beforeUpwardScroll(event.target);
    }
  }, { passive: true });
  viewport.addEventListener('touchstart', event => {
    touchY = event.touches.length === 1 ? event.touches[0].clientY : null;
  }, { passive: true });
  viewport.addEventListener('touchmove', event => {
    const nextY = event.touches.length === 1 ? event.touches[0].clientY : null;
    if (touchY !== null && nextY !== null && nextY > touchY) beforeUpwardScroll(event.target);
    touchY = nextY;
  }, { passive: true });
  viewport.addEventListener('scroll', observeScroll, { passive: true });
  return { isPinned: () => pinned, pause, resume, schedule, rememberPosition };
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
    latest.classList.toggle('hidden',isFollowingLatest());
    for(const attr of ['title','aria-label'])CLATI18n.attribute(latest,attr,()=>CLATI18n.msg(running?'history_nav.running_latest':'history_nav.latest'));
  }
  latest.onclick=()=>{snapToLatest();sync();};
  viewport.addEventListener('scroll',sync,{passive:true});
  if(typeof ResizeObserver==='function')new ResizeObserver(sync).observe(dom.transcript);
  window.addEventListener('clat:workbench-updated',sync);
  window.addEventListener('clat:activated',sync);
  sync();return {sync};
}

// Suppress native anchoring while changing transcript layout, then restore
// the reading anchor before the browser paints.
function preserveHistoryPosition(update, anchor = viewportAnchor()) {
  const viewport = dom['transcript-scroll'];
  const offset = anchor ? anchor.getBoundingClientRect().top - viewport.getBoundingClientRect().top : null;
  const anchoring = viewport.style.overflowAnchor;
  viewport.style.overflowAnchor = 'none';
  try {
    update();
    if (anchor?.isConnected && offset !== null) {
      const delta = anchor.getBoundingClientRect().top - viewport.getBoundingClientRect().top - offset;
      viewport.scrollTop = viewport.scrollTop + delta;
      transcriptFollow.rememberPosition();
    }
  } finally {
    viewport.style.overflowAnchor = anchoring;
  }
}
