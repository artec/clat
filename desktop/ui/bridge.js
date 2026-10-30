(() => {
  'use strict';
  if (window.top !== window || !window.__TAURI__) return;
  // Cancel only the WebView default; application contextmenu listeners still run.
  window.addEventListener('contextmenu', event => event.preventDefault(), { capture:true });
  const t = (key, args) => CLATI18n.t(key,args);
  const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
  let recentKey=null, recentSignature=null, recentLane=Promise.resolve(), recentError=null;
  window.addEventListener('clat-desktop-conversation',event=>{
    const row=event.detail,key=JSON.stringify([row.root,row.session]),opened=key!==recentKey;
    const signature=JSON.stringify(row);if(!opened&&signature===recentSignature)return;
    recentSignature=signature;
    recentKey=key;
    recentLane=recentLane.catch(()=>{}).then(()=>invoke('desktop_remember_conversation',{row,opened})).catch(error=>{if(recentSignature===signature){recentSignature=null;recentKey=null;}throw error;});
    void recentLane.catch(error=>{
      recentError=error;
      console.error('[CLAT] tray conversation update failed',error);
      window.dispatchEvent(new CustomEvent('clat-desktop-tray-error',{detail:error}));
    });
  });
  let integratedShow = null;
  const showSettings = () => { if(integratedShow){integratedShow();return;}const panel=document.getElementById('clat-desktop-settings');if(panel&&!panel.open)panel.showModal(); };
  Object.defineProperty(window, 'CLATDesktop', { value: Object.freeze({ version: 1, showSettings }) });
  const createPanel = () => {
    const host = document.createElement('div'); host.id = 'clat-desktop-controls';
    const launch = document.createElement('button'); launch.onclick = showSettings;
    const footer = document.querySelector('.sidebar-footer');
    const launchLabel = document.createElement('span'); launchLabel.className = 'sidebar-expanded-only';
    if (footer) {
      launch.className = 'sidebar-footer-button'; launch.setAttribute('aria-label', 'CLAT');
      const glyph = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      for (const [name, value] of Object.entries({viewBox:'0 0 24 24','aria-hidden':'true',fill:'none',stroke:'currentColor','stroke-width':'1.7','stroke-linejoin':'round'})) glyph.setAttribute(name,value);
      const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
      path.setAttribute('d','M4 5h16v14H4zM8 5v14'); glyph.append(path);
      launch.replaceChildren(glyph, launchLabel);
    }
    const dialog = document.createElement('dialog'); dialog.id = 'clat-desktop-settings';
    dialog.innerHTML = `
      <button class="desktop-dialog-close" data-action="close" type="button" data-i18n-aria-label="common.close" ><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18"/></svg></button>
      <header class="desktop-product"><strong class="desktop-wordmark">CLAT</strong><span data-i18n="desktop.about_tagline"></span></header>
      <section class="desktop-card"><h3 data-i18n="desktop.product_info"></h3><dl id="clat-desktop-versions">
        <div><dt data-i18n="desktop.app_version"></dt><dd data-version="desktop"></dd></div>
        <div><dt data-i18n="desktop.host_version"></dt><dd data-version="host"></dd></div>
        <div><dt data-i18n="desktop.backend_version"></dt><dd data-version="backend"></dd></div>
      </dl></section>
      <section class="desktop-card"><h3 data-i18n="desktop.software_update"></h3>
        <label class="desktop-preference"><span data-i18n="desktop.updates"></span><input id="clat-desktop-auto" type="checkbox"></label>
        <p id="clat-desktop-update" role="status" aria-live="polite"></p><progress id="clat-desktop-progress" hidden></progress>
        <p class="secondary" data-i18n="desktop.update_hint"></p>
        <div class="actions"><button data-action="check" data-i18n="desktop.check"></button><button data-action="download" data-i18n="desktop.download"></button><button data-action="install" data-i18n="desktop.install"></button><button data-action="cancel" data-i18n="desktop.cancel_wait"></button><a data-update-manual href="https://github.com/artec/clat/releases/latest" target="_blank" rel="noopener noreferrer" data-i18n="desktop.manual_update"></a></div>
      </section>
      <section class="desktop-card"><div class="desktop-card-heading"><h3 data-i18n="desktop.background_title"></h3><span id="clat-desktop-background"></span></div>
        <p class="secondary" data-i18n="desktop.background_hint"></p><div class="actions"><button data-action="open" data-i18n="desktop.open"></button><button data-action="stop" class="danger" data-i18n="desktop.stop"></button><button data-action="quit" data-i18n="desktop.quit"></button></div>
      </section>`;
    const style=document.createElement('style');
    style.textContent=`
      #clat-desktop-settings .desktop-product{display:flex;flex-direction:column;gap:4px;margin:0 0 24px;padding:0;border:0;background:transparent}
      #clat-desktop-settings .desktop-wordmark{font-size:32px;font-weight:650;letter-spacing:.04em;line-height:1.3;color:var(--text)}
      #clat-desktop-settings .desktop-product>span{font-size:13px;color:var(--text-muted)}
      #clat-desktop-settings .desktop-card{margin:0 0 16px;padding:20px;border:1px solid var(--line);border-radius:12px;background:var(--surface-input)}
      #clat-desktop-settings .desktop-card h3{margin:0;font-size:14px;font-weight:600;line-height:1.5}
      #clat-desktop-settings #clat-desktop-versions{margin:12px 0 0}
      #clat-desktop-settings #clat-desktop-versions>div{display:flex;justify-content:space-between;align-items:baseline;gap:20px;padding:10px 0;border-bottom:1px solid var(--line)}
      #clat-desktop-settings #clat-desktop-versions>div:last-child{border:0;padding-bottom:0}
      #clat-desktop-settings dt{color:var(--text-muted)}#clat-desktop-settings dd{margin:0;text-align:end;overflow-wrap:anywhere}
      #clat-desktop-settings .desktop-preference,#clat-desktop-settings .desktop-card-heading{display:flex;align-items:center;justify-content:space-between;gap:16px}
      #clat-desktop-settings .desktop-preference{margin:16px 0;padding:0;border:0;background:transparent}
      #clat-desktop-settings .desktop-card-heading>span{font-size:12px;color:var(--text-muted);text-align:end}
      #clat-desktop-settings .desktop-card p{margin:12px 0;font-size:13px;line-height:1.7;white-space:normal}
      #clat-desktop-settings .desktop-card .secondary{color:var(--text-muted)}
      #clat-desktop-settings .desktop-card .actions{display:flex;align-items:center;flex-wrap:wrap;gap:8px;margin:16px 0 0}
      #clat-desktop-settings .actions a{margin-inline-start:auto;font-size:12px;color:var(--accent)}
      #clat-desktop-settings progress{width:100%;height:5px;accent-color:var(--accent)}
      #clat-desktop-settings [hidden]{display:none!important}
      @media(max-width:480px){#clat-desktop-settings .desktop-card{padding:16px}#clat-desktop-settings .actions a{margin-inline-start:0;width:100%}}
    `;document.head.append(style);

    for(const button of dialog.querySelectorAll('button'))button.type='button';
    host.append(launch, dialog); document.body.append(host);
    // Critical native-shell placement must also work with a Host's older stylesheet.
    Object.assign(dialog.querySelector('[data-action="close"]').style, {
      position:'absolute',top:'12px',right:'12px',width:'32px',height:'32px',padding:'0',
      display:'flex',alignItems:'center',justifyContent:'center',border:'0',background:'transparent',
    });
    const language = document.createElement('div'); language.className='desktop-language'; dialog.prepend(language); CLATI18n.selector(language);
    const placement = { panel:dialog, handled:false, show:null };
    window.dispatchEvent(new CustomEvent('clat-desktop-settings-mount', { detail:placement }));
    if(placement.handled){integratedShow=placement.show;launch.remove();}
    window.addEventListener('clat:project-activated', () => {
      const next = { panel:dialog, handled:false, show:null };
      window.dispatchEvent(new CustomEvent('clat-desktop-settings-mount', { detail:next }));
      if(next.handled)integratedShow=next.show;
    });
    if (footer&&!placement.handled) footer.prepend(launch);
    const status = dialog.querySelector('#clat-desktop-update');
    return {dialog,status,launch,footer,launchLabel};
  };
  const ready = () => {
    const {dialog,status,launch,footer,launchLabel}=createPanel();
    let actionError = '';
    const reportError = error => { actionError=error?.key?error:String(error); CLATI18n.bind(status,'textContent',()=>CLATI18n.present(actionError)); };
    window.addEventListener('clat-desktop-tray-error',event=>reportError(event.detail));
    if(recentError)reportError(recentError);
    const trayIntent=async()=>{
      const intent=await invoke('desktop_take_tray_intent');if(!intent)return;
      if(intent.settings)showSettings();
      if(intent.checkUpdate)await act('check');
      if(intent.conversation)window.dispatchEvent(new CustomEvent('clat-desktop-open-conversation',{detail:intent.conversation}));
    };
    let request = null, latestInfo = {};
    let composing = false;
    document.addEventListener('compositionstart',e=>{if(e.target.id==='prompt')composing=true;},true);
    document.addEventListener('compositionend',e=>{if(e.target.id==='prompt')composing=false;},true);
    const buttons = () => {
      const info=latestInfo, busy=info.updateBusy||request!==null;
      {
        const visible={check:!info.updateAvailable&&!info.downloaded&&!info.updateBusy,download:info.updateAvailable&&!info.downloaded&&!info.updateBusy,install:info.downloaded&&!info.updateBusy,cancel:info.updateBusy&&info.updateCancellable!==false,open:!dialog.classList.contains('desktop-settings-integrated')};
        for(const [action,show] of Object.entries(visible))dialog.querySelector('[data-action="'+action+'"]').hidden=!show;
      }
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
      const versions={desktop:()=>info.desktopVersion,host:()=>info.hostConnected?info.hostVersion:t('desktop.not_connected'),backend:()=>info.backendVersion+(info.backendPending?t('desktop.pending'):'')};
      for(const [key,value]of Object.entries(versions))CLATI18n.bind(dialog.querySelector('[data-version="'+key+'"]'),'textContent',value);
      CLATI18n.bind(dialog.querySelector('#clat-desktop-background'),'textContent',()=>background()||t('desktop.not_connected'));
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
        if (['install','open','quit'].includes(action)) {
          const request = new CustomEvent('clat-desktop-action-request', { cancelable:true, detail:{action} });
          if (!window.dispatchEvent(request)) throw CLATI18n.msg('desktop.draft_warning');
        }
        if(['install','open','quit'].includes(action) && (composing||document.querySelector('#prompt')?.value.trim()||document.querySelector('#attachment-rail .attachment-chip')))throw CLATI18n.msg('desktop.draft_warning');
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
    window.__TAURI__.event?.listen('desktop-tray-intent',()=>void trayIntent().catch(reportError)).then(()=>trayIntent()).catch(reportError);
    window.__TAURI__.event?.listen('desktop-check-update',()=>{showSettings();void act('check');}).catch(()=>{});
    CLATI18n.onChange(() => refresh().catch(() => {}));
    dialog.querySelector('#clat-desktop-auto').onchange = event => invoke('desktop_preferences', { autoCheck:event.target.checked }).catch(reportError);
    launch.addEventListener('click', () => refresh().catch(reportError));
    refresh().catch(() => {});
    CLATI18n.refresh();
    let ticks = 0;
    setInterval(() => { ticks += 1; if ((dialog.classList.contains('desktop-settings-integrated') ? !dialog.closest('[data-settings-panel]')?.hidden && !dialog.closest('#settings-dialog')?.hidden : dialog.open) || ticks % 10 === 0) refresh().catch(() => {}); }, 1000);
  };
  const boot = () => CLATI18n.ready.then(ready);
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, { once:true }); else boot();
})();
