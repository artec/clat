/* Scoped presentation lifecycle, navigation metadata, and guarded conversation actions. */
let projectReading = null;

function captureProjectReading() {
  const viewport = dom['transcript-scroll'];
  if (!context.isActive() || !viewport.clientHeight) return;
  const edge = viewport.getBoundingClientRect().top;
  const anchor = [...dom.transcript.children].find(node => node.getBoundingClientRect().bottom > edge);
  projectReading = {
    top: viewport.scrollTop, follow: nearEnd(), anchor, seq: anchor?.dataset.seq,
    offset: anchor ? anchor.getBoundingClientRect().top - edge : 0,
  };
}

function restoreProjectReading() {
  const viewport = dom['transcript-scroll'];
  if (!projectReading || !context.isActive() || !viewport.clientHeight) return;
  const saved = projectReading;
  const anchor = dom.transcript.contains(saved.anchor) ? saved.anchor
    : saved.seq ? dom.transcript.querySelector('[data-seq="' + CSS.escape(saved.seq) + '"]') : null;
  const behavior = viewport.style.scrollBehavior;
  viewport.style.scrollBehavior = 'auto';
  if (saved.follow) viewport.scrollTop = viewport.scrollHeight;
  else if (anchor) viewport.scrollTop += anchor.getBoundingClientRect().top - viewport.getBoundingClientRect().top - saved.offset;
  else viewport.scrollTop = saved.top;
  viewport.style.scrollBehavior = behavior;
}

function projectLifecycle() {
  let saved = null;
  const navigationWaiters = new Set();
  const ready = () => state.connected && !state.switching && state.workbench && Number.isSafeInteger(state.selectionGeneration);
  window.addEventListener('clat:workbench-updated', () => { for (const check of navigationWaiters) check(); });
  function waitForNavigation(current) {
    return new Promise((resolve, reject) => {
      const finish = value => { navigationWaiters.delete(check); clearTimeout(timeout); resolve(value); };
      const check = () => { if (!current() || ready()) finish(current()); };
      const timeout = setTimeout(() => {
        navigationWaiters.delete(check);
        if (current()) reject(CLATI18n.error(CLATI18n.msg('workbench.project_not_ready'))); else resolve(false);
      }, 30000);
      navigationWaiters.add(check); check();
    });
  }
  async function navigateConversation({ session, fresh }, current) {
    // A fresh conversation has no target message/selection to await. Lock it
    // immediately, as the former dedicated new-session action did.
    if (!(fresh && state.connected && !state.switching) && !await waitForNavigation(current)) return;
    if (!current()) return;
    workbenchShell.chat();
    if (fresh) await newSession(); else if (session) await switchSession(session);
  }
  function deactivate() {
    saveComposerText();
    window.dispatchEvent(new Event('clat:development-deactivated'));
    workbenchShell?.tools?.suspend();
    activeComposerPopup?.close();
    for (const seat of navigationPages.values()) if (seat.active) {
      leavePage(seat.dialog);
    }
    captureProjectReading();
    saved = {
      scrolls: [...context.root.querySelectorAll('*')].filter(node => node.scrollHeight > node.clientHeight)
        .map(node => [node, node.scrollTop, node.scrollLeft]),
      selection: [dom.prompt.selectionStart, dom.prompt.selectionEnd, dom.prompt.selectionDirection],
      focus: document.activeElement,
    };
    // Temporary top-layer previews cannot remain above the next project's view.
    for (const dialog of context.root.querySelectorAll('dialog[open]:not(.shell-surface)')) {
      if (dialog.id !== 'clat-desktop-settings') dialog.close();
    }
  }
  function activate() {
    const preference = readPresentationPreference();
    if (['system', 'light', 'dark'].includes(preference.theme)) state.theme = preference.theme;
    applyPresentation();
    CLATI18n.hydrate(context.root);
    window.dispatchEvent(new Event('clat:activated'));
    renderMessageMap();
    resizePrompt();
    if (!saved) return;
    for (const [node, top, left] of saved.scrolls) { node.scrollTop = top; node.scrollLeft = left; }
    restoreProjectReading();
    dom.prompt.setSelectionRange(...saved.selection);
    if (saved.focus?.isConnected && saved.focus.getClientRects().length) saved.focus.focus({ preventScroll: true });
  }
  return {
    activate, deactivate, navigateConversation,
    openTerminal: id => developmentTerminal.selectById(id),
    createTerminal: (directory,shell) => developmentTerminal.create(directory,shell),
    route:()=>dom.app.dataset.page||workbenchShell?.tools?.location()||'workbench',
    navigation: () => ({ items: state.sessions, selected: state.sessionId, ready: Boolean(ready()),
      locked: state.switching || state.compactionActive || state.desktopUpdateLocked,
      query: dom['session-search'].value, archived: showArchivedSessions }),
    summary: () => ({
      root: state.workbench?.project?.root || dom['project-root'].textContent,
      running: state.runActive || state.compactionActive,
      pending: context.root.querySelectorAll('.question-card:not(.resolved), .approval-card:not(.resolved)').length,
    }),
  };
}
