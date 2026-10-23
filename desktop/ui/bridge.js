(() => {
  'use strict';
  if (window.top !== window || !window.__TAURI__) return;
  const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
  const showSettings = () => document.getElementById('clat-desktop-settings')?.showModal();
  Object.defineProperty(window, 'CLATDesktop', { value: Object.freeze({ version: 1, showSettings }) });
  const ready = () => {
    const host = document.createElement('div'); host.id = 'clat-desktop-controls';
    const sheet = document.createElement('style');
    sheet.textContent = '#clat-desktop-controls{font:13px ui-sans-serif,system-ui;position:fixed;right:18px;bottom:12px;z-index:10000}#clat-desktop-controls button{font:inherit;cursor:pointer;border:1px solid #78877360;border-radius:6px;padding:7px 12px;color:inherit;background:Canvas}#clat-desktop-controls dialog{color:CanvasText;background:Canvas;border:1px solid #78877360;border-radius:12px;padding:26px;width:min(520px,90vw);max-height:85vh;overflow:auto}#clat-desktop-controls dialog::backdrop{background:#10181070}#clat-desktop-controls button:disabled{opacity:.45;cursor:not-allowed}#clat-desktop-controls button:focus-visible{outline:2px solid #78ad81;outline-offset:3px}#clat-desktop-controls h2{font-size:20px;margin:0 0 22px}#clat-desktop-controls p{line-height:1.7;white-space:pre-wrap;overflow-wrap:anywhere}#clat-desktop-controls .actions{display:flex;gap:10px;flex-wrap:wrap;margin-top:18px}#clat-desktop-controls .secondary{color:#74836e}#clat-desktop-controls .danger{color:#ba554c}';
    host.append(sheet);
    const launch = document.createElement('button'); launch.textContent = '桌面'; launch.onclick = showSettings;
    const footer = document.querySelector('.sidebar-footer');
    const launchLabel = document.createElement('span'); launchLabel.className = 'sidebar-expanded-only';
    if (footer) {
      launch.className = 'sidebar-footer-button'; launch.setAttribute('aria-label', 'CLAT');
      const glyph = document.createElement('span'); glyph.textContent = '▣'; glyph.setAttribute('aria-hidden', 'true');
      launch.replaceChildren(glyph, launchLabel);
    }
    const dialog = document.createElement('dialog'); dialog.id = 'clat-desktop-settings';
    dialog.innerHTML = '<h2>CLAT</h2><p id="clat-desktop-versions"></p><label><input id="clat-desktop-auto" type="checkbox"> 自动检查更新</label><p id="clat-desktop-update" role="status" aria-live="polite"></p><div class="actions"><button data-action="open">打开项目</button><button data-action="check">检查更新</button><button data-action="download">下载更新</button><button data-action="install">安装更新</button><button data-action="cancel">取消等待</button></div><div class="actions"><button data-action="stop" class="danger">停止共享后台</button><button data-action="quit">退出桌面</button><button data-action="close">关闭</button></div><p class="secondary">关闭窗口后任务继续。退出桌面不会停止共享后台。</p>';
    host.append(launch, dialog); document.body.append(host);
    if (footer) footer.prepend(launch);
    const status = dialog.querySelector('#clat-desktop-update');
    let actionError = '';
    let refreshing = false;
    const refresh = async () => {
      if (refreshing) return;
      refreshing = true;
      try {
      const info = await invoke('desktop_info');
      const background = info.hostStatus ? (info.hostStatus.ready ? '共享后台空闲' : `${info.hostStatus.busy_workspaces.length} 个项目有活动工作`) : '';
      dialog.querySelector('#clat-desktop-versions').textContent = `桌面 ${info.desktopVersion}\n当前 Host ${info.hostConnected ? info.hostVersion : '未连接'}\n包内后端 ${info.backendVersion}${info.backendPending ? '（下次启动或打开项目时启用）' : ''}\n${background}`;
      dialog.querySelector('#clat-desktop-auto').checked = info.autoCheck;
      status.textContent = actionError || info.updateStatus || info.backendError || info.connectionError || '';
      const label = info.updateAvailable ? '桌面 · 有更新' : '桌面';
      if (footer) { launchLabel.textContent = label; launch.title = label; } else launch.textContent = label;
      dialog.querySelector('[data-action="check"]').disabled = info.updateBusy;
      dialog.querySelector('[data-action="download"]').disabled = info.updateBusy || !info.updateAvailable || info.downloaded;
      dialog.querySelector('[data-action="install"]').disabled = info.updateBusy || !info.downloaded;
      dialog.querySelector('[data-action="cancel"]').disabled = !info.updateBusy;
      dialog.querySelector('[data-action="stop"]').disabled = !info.hostConnected;
      } finally { refreshing = false; }
    };
    dialog.addEventListener('click', async event => {
      const action = event.target.dataset.action; if (!action) return;
      let composer, previousReadOnly;
      try {
        if (action === 'close') { dialog.close(); return; }
        actionError = '';
        if (['install', 'open'].includes(action) && document.querySelector('#prompt')?.value.trim()) throw new Error('请先发送或清空输入框内容，避免重启丢失草稿。');
        const command = { open:'desktop_open_project',check:'desktop_check_update',download:'desktop_download_update',install:'desktop_install_update',cancel:'desktop_cancel_update',stop:'desktop_stop_host',quit:'desktop_quit' }[action];
        if (action === 'install') {
          composer = document.querySelector('#prompt');
          if (composer) { previousReadOnly = composer.readOnly; composer.readOnly = true; }
        }
        if (command) { status.textContent = '处理中…'; await invoke(command); await refresh(); }
      } catch (error) { actionError = String(error); status.textContent = actionError; }
      finally { if (composer) composer.readOnly = previousReadOnly; }
    });
    dialog.querySelector('#clat-desktop-auto').onchange = event => invoke('desktop_preferences', { autoCheck:event.target.checked }).catch(error => { status.textContent = String(error); });
    launch.addEventListener('click', () => refresh().catch(error => { status.textContent = String(error); }));
    refresh().catch(() => {});
    let ticks = 0;
    setInterval(() => { ticks += 1; if (dialog.open || ticks % 10 === 0) refresh().catch(() => {}); }, 1000);
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', ready, { once:true }); else ready();
})();
