/* Native chrome only. Every operation is checked again by Rust's origin/Host guard. */
(() => {
  'use strict';
  if (window.top !== window || !window.__TAURI__) return;
  const invoke = action => window.__TAURI__.core.invoke('desktop_window', { action });
  const ready = async () => {
    let info = { platform: window.__CLAT_NATIVE_FRAME__ };
    if (!info.platform) {
      try { info = await invoke('info'); } catch (_) { return; }
    }
    if (!['macos', 'windows'].includes(info?.platform)) return;
    document.documentElement.dataset.nativeFrame = info.platform;
    const style = document.createElement('style');
    style.textContent = `
      .native-window-controls{position:fixed;inset:0 0 auto auto;height:48px;display:flex;z-index:10001;color:inherit}
      .native-window-controls button{display:grid;place-items:center;width:46px;padding:0;border:0;border-radius:0;background:transparent;color:inherit;font:inherit}
      .native-window-controls button:hover{background:color-mix(in srgb,CanvasText 12%,transparent)}
      .native-window-controls button[data-window-action="close"]:hover{background:#c42b1c;color:#fff}
      .native-window-controls svg{width:12px;height:12px;fill:none;stroke:currentColor;stroke-width:1.2}
      .native-fallback-bar{position:fixed;inset:0 0 auto;height:48px;padding:0 150px 0 16px;display:flex;align-items:center;background:Canvas;color:CanvasText;z-index:10000;user-select:none;font:13px system-ui}
      html[data-native-frame="macos"] .native-fallback-bar{padding-left:100px}
      #native-window-status{position:fixed;inset:48px 12px auto auto;max-width:360px;padding:12px;background:Canvas;color:CanvasText;z-index:10002}
    `;
    document.head.append(style);
    if (!document.getElementById('application-toolbar')) {
      const bar = document.createElement('header'); bar.className = 'native-fallback-bar'; bar.textContent = 'CLAT';
      document.body.prepend(bar); document.body.style.paddingTop = '48px';
    }
    const act = action => invoke(action).catch(error => {
      let status = document.getElementById('native-window-status');
      if (!status) { status = document.createElement('p'); status.id = 'native-window-status'; status.setAttribute('role', 'status'); document.body.append(status); }
      CLATI18n.bind(status, 'textContent', () => CLATI18n.present(error?.key ? error : String(error)));
    });
    document.addEventListener('mousedown', event => {
      if (event.button !== 0 || !event.target.closest('.application-toolbar, .native-fallback-bar')) return;
      if (event.target.closest('button, a, input, select, textarea, [role="menu"], .workbench-tools')) return;
      event.preventDefault(); void act(event.detail === 2 ? 'toggle_maximize' : 'drag');
    });
    if (info.platform === 'windows') createControls(act);
  };
  function createControls(act) {
    const controls = document.createElement('div'); controls.className = 'native-window-controls';
    for (const [action, key, path] of [
      ['minimize', 'desktop.window_minimize', 'M1 6h10'],
      ['toggle_maximize', 'desktop.window_maximize_restore', 'M1.5 1.5h9v9h-9z'],
      ['close', 'desktop.window_close', 'm1 1 10 10M11 1 1 11'],
    ]) {
      const button = document.createElement('button'); button.type = 'button'; button.dataset.windowAction = action;
      CLATI18n.attribute(button, 'aria-label', () => CLATI18n.msg(key));
      CLATI18n.attribute(button, 'title', () => CLATI18n.msg(key));
      const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      svg.setAttribute('viewBox', '0 0 12 12'); svg.setAttribute('aria-hidden', 'true');
      const glyph = document.createElementNS('http://www.w3.org/2000/svg', 'path'); glyph.setAttribute('d', path);
      svg.append(glyph); button.append(svg); button.onclick = () => void act(action); controls.append(button);
    }
    document.body.append(controls);
  }
  const boot = () => CLATI18n.ready.then(ready);
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, { once: true }); else boot();
})();
