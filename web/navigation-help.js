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
  if (typeof globalThis.__TAURI__?.core?.invoke === 'function') {
    installDesktopNotifications();
    return;
  }
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

function buildDesktopNotificationSwitch(id, titleKey, titleFallback, hintKey, hintFallback) {
  const input = el('input', 'settings-switch');
  input.type = 'checkbox'; input.id = id; input.disabled = true;
  const copy = el('span', 'settings-row-copy');
  copy.append(
    el('strong', '', () => CLATI18n.msg(titleKey, {}, titleFallback)),
    el('small', '', () => CLATI18n.msg(hintKey, {}, hintFallback)),
  );
  const row = el('label', 'settings-row');
  row.append(copy, input);
  return { row, input };
}

function installDesktopNotifications() {
  const invoke = globalThis.__TAURI__?.core?.invoke;
  const host = document.querySelector('[data-settings-panel=appearance]');
  if (typeof invoke !== 'function' || !host || document.getElementById('desktop-notification-sound')) return;
  const banner = buildDesktopNotificationSwitch(
    'desktop-notifications-enabled',
    'desktop.notification_banner', 'Desktop notifications',
    'desktop.notification_banner_hint', 'Show system notification banners when background tasks finish or need input.',
  );
  const soundWhenOff = buildDesktopNotificationSwitch(
    'desktop-notification-sound-when-banner-off',
    'desktop.notification_sound_when_banner_off', 'Play sound when desktop notifications are off',
    'desktop.notification_sound_when_banner_off_hint', 'Still play the selected sound directly when system banners are turned off or silenced by Do Not Disturb.',
  );
  const select = el('select'); select.id = 'desktop-notification-sound'; select.disabled = true;
  CLATI18n.attribute(select, 'aria-label', () => CLATI18n.msg('desktop.notification_sound', {}, 'Completion sound'));
  const preview = el('button', 'secondary', () => CLATI18n.msg('desktop.notification_sound_preview', {}, 'Preview'));
  preview.type = 'button'; preview.id = 'desktop-notification-sound-preview'; preview.disabled = true;
  const controls = el('div', 'notification-sound-controls'); controls.append(select, preview);
  const soundCopy = el('span', 'settings-row-copy');
  soundCopy.append(
    el('strong', '', () => CLATI18n.msg('desktop.notification_sound', {}, 'Completion sound')),
    el('small', '', () => CLATI18n.msg('desktop.notification_sound_hint', {}, 'Played when a task completes while CLAT is in the background.')),
  );
  const soundRow = el('div', 'settings-row'); soundRow.append(soundCopy, controls);
  const status = el('p', 'picker-hint'); status.id = 'desktop-notification-sound-status'; status.setAttribute('role', 'status');
  const card = el('div', 'settings-card'); card.append(banner.row, soundWhenOff.row, soundRow, status);
  const section = el('section', 'settings-group-block'); section.id = 'desktop-notification-sound-section';
  section.append(el('h4', '', () => CLATI18n.msg('desktop.notification_sound_section', {}, 'Notifications')), card);
  host.append(section);

  let state = { selected: 'Funk', bannerEnabled: true, soundWhenBannerOff: true, envOverride: null, sounds: [] };
  const renderStatus = error => {
    if (error) { status.textContent = typeof error === 'string' ? error : (error?.message || String(error)); return; }
    if (state.envOverride === 'no_bell') status.textContent = CLATI18n.msg('desktop.notification_sound_env_muted', {}, 'Muted by the CLAT_NO_BELL environment variable.');
    else if (state.envOverride === 'command') status.textContent = CLATI18n.msg('desktop.notification_sound_env_command', {}, 'Overridden by the CLAT_BELL_COMMAND environment variable.');
    else status.textContent = '';
  };
  const renderControls = () => {
    banner.input.checked = state.bannerEnabled;
    soundWhenOff.input.checked = state.soundWhenBannerOff;
    select.replaceChildren();
    const offOption = document.createElement('option');
    offOption.value = 'off';
    offOption.textContent = CLATI18n.msg('desktop.notification_sound_off', {}, 'Muted');
    select.append(offOption);
    const list = state.sounds.slice();
    if (state.selected && state.selected !== 'off' && !list.includes(state.selected)) list.push(state.selected);
    for (const name of list) {
      const option = document.createElement('option');
      option.value = name; option.textContent = name; select.append(option);
    }
    select.value = state.selected || 'off';
    preview.disabled = select.disabled || select.value === 'off';
    renderStatus(null);
  };
  const applyPayload = data => {
    state = {
      sounds: Array.isArray(data?.sounds) ? data.sounds : [],
      selected: typeof data?.selected === 'string' ? data.selected : (data?.default || 'Funk'),
      bannerEnabled: data?.bannerEnabled !== false,
      soundWhenBannerOff: data?.soundWhenBannerOff !== false,
      envOverride: data?.envOverride || null,
    };
    banner.input.disabled = soundWhenOff.input.disabled = select.disabled = false;
    renderControls();
  };
  const save = async patch => {
    try { applyPayload(await invoke('desktop_set_notification_sound', patch)); }
    catch (error) { renderControls(); renderStatus(error); }
  };
  banner.input.addEventListener('change', () => save({ bannerEnabled: banner.input.checked, preview: false }));
  soundWhenOff.input.addEventListener('change', () => save({ soundWhenBannerOff: soundWhenOff.input.checked, preview: false }));
  select.addEventListener('change', () => { preview.disabled = select.value === 'off'; save({ sound: select.value, preview: true }); });
  preview.addEventListener('click', async () => {
    if (select.value === 'off') return;
    try { await invoke('desktop_preview_notification_sound', { sound: select.value }); renderStatus(null); }
    catch (error) { renderStatus(error); }
  });
  CLATI18n.onChange(() => { if (!select.disabled) renderControls(); });
  invoke('desktop_notification_sounds').then(applyPayload).catch(renderStatus);
}

function createTranscriptFollow(viewport, onChange) {
  let pinned = true;
  let lastTop, lastBottom;
  let layoutFrames = 0;
  let frame = null;
  viewport.dataset.followingLatest = 'true';
  rememberPosition();

  function setPinned(value) {
    if (pinned === value) return;
    pinned = value;
    viewport.dataset.followingLatest = String(value);
    onChange();
  }
  function bottomEdge() { return Math.max(0, viewport.scrollHeight - viewport.clientHeight); }
  function rememberPosition() {
    lastBottom = bottomEdge();
    lastTop = viewport.scrollTop;
  }
  function scrollToEnd() {
    // Let the browser clamp to its precise bottom. The extent metrics are rounded.
    viewport.scrollTop = viewport.scrollHeight;
    rememberPosition();
  }
  function observeScroll() {
    if (!viewport.clientHeight) return;
    const bottom = bottomEdge();
    const top = viewport.scrollTop;
    const previous = lastTop;
    const resized = layoutFrames > 0 || bottom !== lastBottom;
    lastTop = top;
    lastBottom = bottom;
    if (top === previous) return;
    const atBottom = top >= bottom - 1; // scrollTop can be fractional.
    // Composer shrink and reply growth can cancel out before the scroll event,
    // even though the browser already clamped scrollTop during the shrink.
    if (resized) return;
    if (top < previous && !atBottom) setPinned(false);
    else if (top > previous && atBottom) setPinned(true);
  }
  function pause() {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
    layoutFrames = 0;
    rememberPosition();
    setPinned(false);
  }
  function resume() {
    if (frame !== null) cancelAnimationFrame(frame);
    frame = null;
    layoutFrames = 0;
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
      if (layoutFrames > 0 && --layoutFrames > 0) schedule();
    });
  }
  function syncLayout() {
    if (!pinned || !viewport.clientHeight) return;
    // ResizeObserver runs after animation callbacks. Follow before this frame paints.
    // WebKit can apply a correction from that layout one frame after the scroll write.
    layoutFrames = 2;
    scrollToEnd();
    schedule();
  }
  function beforeUpwardScroll(target) {
    if (!pinned || viewport.scrollTop <= 0) return;
    // Let scrollable reasoning and tool output handle their own gestures first.
    for (let node = target; node && node !== viewport; node = node.parentElement) {
      if (node.scrollTop > 0 && /^(auto|scroll|overlay)$/.test(getComputedStyle(node).overflowY)) return;
    }
    pause();
  }

  trackUpwardGestures(viewport, beforeUpwardScroll);
  viewport.addEventListener('scroll', observeScroll, { passive: true });
  return { isPinned: () => pinned, pause, resume, schedule, syncLayout, rememberPosition };
}

function trackUpwardGestures(viewport, beforeUpwardScroll) {
  let touchY = null;
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
}

// Floating history controls share the existing scroll owner and run lifecycle.
function createHistoryNavigation(){
  const viewport=dom['transcript-scroll'],latest=el('button','back-to-latest');
  latest.id='back-to-latest';latest.type='button';
  const arrow=svgIcon('arrow-down');arrow.classList.add('history-latest-arrow');
  const dots=el('span','history-running-dots');dots.setAttribute('aria-hidden','true');
  dots.append(el('i'),el('i'),el('i'));latest.append(arrow,dots);viewport.after(latest);
  function sync(){
    const running=String(Boolean(state.connected&&state.runActive&&!state.switching&&context.isActive()));
    const following=String(isFollowingLatest());
    if(latest.dataset.following!==following)latest.dataset.following=following;
    if(latest.dataset.running===running)return;
    latest.dataset.running=running;
    for(const attr of ['title','aria-label'])CLATI18n.attribute(latest,attr,()=>CLATI18n.msg(running==='true'?'history_nav.running_latest':'history_nav.latest'));
  }
  latest.onclick=()=>{snapToLatest();sync();};
  viewport.addEventListener('scroll',sync,{passive:true});
  if(typeof ResizeObserver==='function'){
    const observer=new ResizeObserver(()=>{scrollIfFollowing(true);sync();});
    observer.observe(dom.transcript);observer.observe(viewport);
  }
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
