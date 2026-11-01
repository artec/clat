/* One native update owner for the about panel and every project rail. */
(() => {
  'use strict';
  const quiet = new Set(['update.current','update.found','update.verified','update.cancel_check','update.cancel_download','update.cancel_install']);
  function project({info = {}, error = null, request = null, updateError = false}) {
    const phase = request === 'download' ? 'downloading' : request === 'install' && !info.updateBusy ? 'waiting' : info.updatePhase;
    const busy = Boolean(request || info.updateBusy);
    const visible = Boolean(info.updateAvailable || info.downloaded);
    const actionFailed = updateError && error && (!error.key || !quiet.has(error.key));
    const failed = visible && !busy && Boolean(actionFailed || info.updatePresentation?.key && !quiet.has(info.updatePresentation.key));
    const total = Number(info.updateTotal), received = Math.max(0, Number(info.updateReceived) || 0);
    return {visible, phase, busy, failed, received, total: total > 0 ? total : null,
      ratio: total > 0 ? Math.min(1, received / total) : null,
      action: busy ? null : info.downloaded ? 'install' : info.updateAvailable ? 'download' : null,
      cancellable: Boolean(info.updateBusy && info.updateCancellable !== false && info.updatePhase !== 'installing')};
  }
  function create({invoke, prepare = () => () => {}, visible = () => false}) {
    let info = {}, error = null, request = null, updateError = false, refreshing = null, timer = null, ticks = 0;
    const listeners = new Set();
    const snapshot = () => ({info, error, request, updateError});
    const emit = () => { for (const listener of listeners) listener(snapshot()); };
    function refresh() {
      if (refreshing) return refreshing;
      refreshing = invoke('desktop_info').then(value => { info = value; emit(); }).finally(() => { refreshing = null; });
      return refreshing;
    }
    async function act(action) {
      if (action !== 'cancel' && (request || info.updateBusy)) return;
      if (action === 'cancel' && !project(snapshot()).cancellable) return;
      const command = {open:'desktop_open_project',check:'desktop_check_update',download:'desktop_download_update',install:'desktop_install_update',cancel:'desktop_cancel_update',stop:'desktop_stop_host',quit:'desktop_quit'}[action];
      if (!command) return;
      const tracked = action !== 'cancel';
      if (tracked) request = action;
      error = null; updateError = ['check','download','install','cancel'].includes(action); emit();
      let cleanup = () => {};
      try { cleanup = prepare(action); await invoke(command); }
      catch (failure) { error = failure?.key ? failure : String(failure); }
      finally {
        cleanup();
        if (refreshing) await refreshing.catch(() => {});
        await refresh().catch(() => {}); if (tracked) request = null; emit();
      }
    }
    return {snapshot, refresh, act,
      subscribe(listener) { listeners.add(listener); listener(snapshot()); return () => listeners.delete(listener); },
      start() { if (timer !== null) return; void refresh().catch(() => {}); timer = setInterval(() => {
        ticks++; if (request || info.updateBusy || visible() || ticks % 10 === 0) void refresh().catch(() => {});
      }, 1000); },
      stop() { clearInterval(timer); timer = null; },
    };
  }
  globalThis.CLATDesktopUpdates = Object.freeze({create, project});
})();
