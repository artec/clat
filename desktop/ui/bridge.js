(() => {
  'use strict';
  if (window.top !== window || !window.__TAURI__) return;
  const t = (key, args) => CLATI18n.t(key,args);
  const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
  const showSettings = () => { const panel=document.getElementById('clat-desktop-settings');if(panel&&!panel.open)panel.showModal(); };
  Object.defineProperty(window, 'CLATDesktop', { value: Object.freeze({ version: 1, showSettings }) });
  const createPanel = () => {
    const host = document.createElement('div'); host.id = 'clat-desktop-controls';
    const sheet = document.createElement('style');
    sheet.textContent = '#clat-desktop-controls{font:13px ui-sans-serif,system-ui;position:fixed;right:18px;bottom:12px;z-index:10000}#clat-desktop-controls button{font:inherit;cursor:pointer;border:1px solid #78877360;border-radius:6px;padding:7px 12px;color:inherit;background:Canvas}#clat-desktop-controls dialog{color:CanvasText;background:Canvas;border:1px solid #78877360;border-radius:12px;padding:26px;width:min(520px,90vw);max-height:85vh;overflow:auto}#clat-desktop-controls dialog::backdrop{background:#10181070}#clat-desktop-controls button:disabled{opacity:.45;cursor:not-allowed}#clat-desktop-controls button:focus-visible{outline:2px solid #78ad81;outline-offset:3px}#clat-desktop-controls h2{font-size:20px;margin:0 0 22px}#clat-desktop-controls p{line-height:1.7;white-space:pre-wrap;overflow-wrap:anywhere}#clat-desktop-controls .actions{display:flex;gap:10px;flex-wrap:wrap;margin-top:18px}#clat-desktop-controls .secondary{color:#74836e}#clat-desktop-controls .danger{color:#ba554c}';
    host.append(sheet);
    const launch = document.createElement('button'); launch.onclick = showSettings;
    const footer = document.querySelector('.sidebar-footer');
    const launchLabel = document.createElement('span'); launchLabel.className = 'sidebar-expanded-only';
    if (footer) {
      launch.className = 'sidebar-footer-button'; launch.setAttribute('aria-label', 'CLAT');
      const glyph = document.createElement('span'); glyph.textContent = '▣'; glyph.setAttribute('aria-hidden', 'true');
      launch.replaceChildren(glyph, launchLabel);
    }
    const dialog = document.createElement('dialog'); dialog.id = 'clat-desktop-settings';
    dialog.innerHTML = '<h2>CLAT</h2><p id="clat-desktop-versions"></p><label><input id="clat-desktop-auto" type="checkbox"><span data-i18n="desktop.updates"></span></label><p id="clat-desktop-update" role="status" aria-live="polite"></p><progress id="clat-desktop-progress" hidden></progress><p class="secondary" data-i18n="desktop.update_hint"></p><div class="actions"><button data-action="open" data-i18n="desktop.open"></button><button data-action="check" data-i18n="desktop.check"></button><button data-action="download" data-i18n="desktop.download"></button><button data-action="install" data-i18n="desktop.install"></button><button data-action="cancel" data-i18n="desktop.cancel_wait"></button></div><p><a data-update-manual href="https://github.com/artec/clat/releases/latest" target="_blank" rel="noopener noreferrer" data-i18n="desktop.manual_update"></a></p><div class="actions"><button data-action="stop" class="danger" data-i18n="desktop.stop"></button><button data-action="quit" data-i18n="desktop.quit"></button><button data-action="close" data-i18n="common.close"></button></div><p class="secondary" data-i18n="desktop.background_hint"></p>';

    host.append(launch, dialog); document.body.append(host);
    const language = document.createElement('div'); dialog.prepend(language); CLATI18n.selector(language);
    if (footer) footer.prepend(launch);
    const status = dialog.querySelector('#clat-desktop-update');
    return {dialog,status,launch,footer,launchLabel};
  };
  const ready = () => {
    const {dialog,status,launch,footer,launchLabel}=createPanel();
    let actionError = '';
    const reportError = error => { actionError=error?.key?error:String(error); CLATI18n.bind(status,'textContent',()=>CLATI18n.present(actionError)); };
    let request = null, latestInfo = {};
    let composing = false;
    document.addEventListener('compositionstart',e=>{if(e.target.id==='prompt')composing=true;},true);
    document.addEventListener('compositionend',e=>{if(e.target.id==='prompt')composing=false;},true);
    const buttons = () => {
      const info=latestInfo, busy=info.updateBusy||request!==null;
      dialog.querySelector('[data-action="check"]').disabled = busy;
      dialog.querySelector('[data-action="download"]').disabled = busy||!info.updateAvailable||info.downloaded;
      dialog.querySelector('[data-action="install"]').disabled = busy||!info.downloaded;
      dialog.querySelector('[data-action="open"]').disabled = busy;
      dialog.querySelector('[data-action="cancel"]').disabled = !info.updateBusy||info.updatePhase==='installing'||info.updateCancellable===false;
      dialog.querySelector('[data-action="quit"]').disabled = info.updatePhase==='installing';
      dialog.querySelector('[data-action="stop"]').disabled = !info.hostConnected||info.updatePhase==='installing';
    };
    let refreshing = false;
    const refresh = async () => {
      if (refreshing) return;
      refreshing = true;
      try {
      const info = await invoke('desktop_info'); latestInfo=info;
      const background = () => info.hostStatus ? (info.hostStatus.ready ? t('desktop.idle') : t('desktop.busy',{count:info.hostStatus.busy_workspaces.length})) : '';
      CLATI18n.bind(dialog.querySelector('#clat-desktop-versions'),'textContent',()=>t('desktop.versions',{desktop:info.desktopVersion,host:info.hostConnected?info.hostVersion:t('desktop.not_connected'),backend:info.backendVersion,pending:info.backendPending?t('desktop.pending'):'',status:background()}));
      dialog.querySelector('#clat-desktop-auto').checked = info.autoCheck;
      CLATI18n.bind(status,'textContent',()=>CLATI18n.present(actionError) || CLATI18n.present(info.updatePresentation,info.updateStatus) || info.backendError || CLATI18n.present(info.connectionPresentation,info.connectionError) || '');
      const label = () => t(info.updateAvailable ? 'desktop.update_available' : 'desktop.entry');
      if (footer) { CLATI18n.bind(launchLabel,'textContent',label); CLATI18n.bind(launch,'title',label); } else CLATI18n.bind(launch,'textContent',label);
      buttons();
      CLATI18n.bind(dialog.querySelector('[data-action="install"]'),'textContent',()=>info.updateVersion?t('desktop.install_version',{version:info.updateVersion}):t('desktop.install'));
      const progress=dialog.querySelector('#clat-desktop-progress');
      progress.hidden=info.updatePhase!=='downloading';
      progress.setAttribute('aria-label',t('desktop.download'));
      if(info.updateTotal>0) {progress.max=info.updateTotal;progress.value=Math.min(info.updateReceived||0,info.updateTotal);}
      else progress.removeAttribute('value');
      } finally { refreshing = false; }
    };
    const act = async action => {
      if(action==='close'){dialog.close();return;}
      if(action!=='cancel'&&(request||latestInfo.updateBusy))return;
      const tracked=action!=='cancel';
      let composer, previousReadOnly, locked=false;
      try {
        actionError = '';
        if(['install','open'].includes(action) && (composing||document.querySelector('#prompt')?.value.trim()||document.querySelector('#attachment-rail .attachment-chip')))throw CLATI18n.msg('desktop.draft_warning');
        const command={open:'desktop_open_project',check:'desktop_check_update',download:'desktop_download_update',install:'desktop_install_update',cancel:'desktop_cancel_update',stop:'desktop_stop_host',quit:'desktop_quit'}[action];
        if(!command)return;
        if(action==='install') {
          const guard=new CustomEvent('clat-desktop-install-lock',{cancelable:true});
          if(!window.dispatchEvent(guard))throw CLATI18n.msg('desktop.draft_warning');
          locked=true; composer=document.querySelector('#prompt');
          if(composer){previousReadOnly=composer.readOnly;composer.readOnly=true;}
        }
        if(tracked)request=action;buttons();
        CLATI18n.bind(status,'textContent',()=>t('desktop.working'));
        await invoke(command);
      } catch(error) {actionError=error?.key?error:String(error);}
      finally {
        if(tracked)request=null;
        if(locked)window.dispatchEvent(new Event('clat-desktop-install-unlock'));
        if(composer)composer.readOnly=previousReadOnly;
        await refresh().catch(()=>{});buttons();
        if(actionError)CLATI18n.bind(status,'textContent',()=>CLATI18n.present(actionError));
      }
    };
    dialog.addEventListener('click',event=>{const action=event.target.closest('[data-action]')?.dataset.action;if(action)void act(action);});
    window.__TAURI__.event?.listen('desktop-check-update',()=>{showSettings();void act('check');}).catch(()=>{});
    CLATI18n.onChange(() => refresh().catch(() => {}));
    dialog.querySelector('#clat-desktop-auto').onchange = event => invoke('desktop_preferences', { autoCheck:event.target.checked }).catch(reportError);
    launch.addEventListener('click', () => refresh().catch(reportError));
    refresh().catch(() => {});
    CLATI18n.refresh();
    let ticks = 0;
    setInterval(() => { ticks += 1; if (dialog.open || ticks % 10 === 0) refresh().catch(() => {}); }, 1000);
  };
  const boot = () => CLATI18n.ready.then(ready);
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, { once:true }); else boot();
})();
