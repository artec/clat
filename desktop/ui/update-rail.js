/* Native-only update presentation; all operations go through the shared controller. */
(() => {
  'use strict';
  const downloadPath = 'M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5';
  const installPath = 'M4 8l3-4h10l3 4v12H4zM4 8h16m-12 6 3 3 5-5';
  function glyph(path) {
    const svg = document.createElementNS('http://www.w3.org/2000/svg','svg');
    for (const [key,value] of Object.entries({viewBox:'0 0 24 24',fill:'none',stroke:'currentColor','stroke-width':'1.5','stroke-linecap':'round','stroke-linejoin':'round','aria-hidden':'true'})) svg.setAttribute(key,value);
    const shape = document.createElementNS(svg.namespaceURI,'path'); shape.setAttribute('d',path); svg.append(shape); return svg;
  }
  function installStyle() {
    const style = document.createElement('style'); style.textContent = `
      #nav-update{position:relative;color:var(--accent)}#nav-update[hidden],#desktop-update-popup[hidden]{display:none!important}
      #nav-update>svg{width:22px;height:22px}#nav-update>svg[hidden]{display:none!important}
      #nav-update[data-failed=true]::after{content:'';position:absolute;right:3px;top:3px;width:6px;height:6px;border-radius:50%;background:var(--danger)}
      #nav-update:focus-visible,#desktop-update-popup button:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
      #desktop-update-popup p{margin:8px 0}#desktop-update-popup .update-popup-actions{display:flex;gap:8px;flex-wrap:wrap}#desktop-update-popup button{min-height:32px}
    `; document.head.append(style);
  }
  function mount(controller) {
    installStyle();
    const t = (key,args) => CLATI18n.t(key,args), button = document.createElement('button');
    button.id = 'nav-update'; button.type = 'button'; button.className = 'shell-button'; button.hidden = true;
    button.setAttribute('aria-haspopup','dialog'); button.setAttribute('aria-controls','desktop-update-popup');
    const icon = glyph(downloadPath), ring = CLATRailPopover.progressRing();
    button.append(icon,ring.element);
    const popup = document.createElement('section'); popup.id = 'desktop-update-popup'; popup.hidden = true; popup.tabIndex = -1; popup.setAttribute('role','dialog');
    const title = document.createElement('strong'), status = document.createElement('p'), detail = document.createElement('p'), facts=document.createElement('div'), actions = document.createElement('div'); actions.className = 'update-popup-actions';
    const primary = document.createElement('button'), cancel = document.createElement('button'), close = document.createElement('button');
    for (const node of [primary,cancel,close]) node.type = 'button'; actions.append(primary,cancel,close); popup.append(title,facts,status,detail,actions);
    let snapshot = controller.snapshot(), previousError = null;
    const card=CLATRailPopover.attach(button,popup);
    const position=card.position,hide=card.hide,show=()=>card.show(true);
    function render(value) {
      snapshot = value; const {info,error,updateError} = value, view = CLATDesktopUpdates.project(value);
      button.hidden = !view.visible; button.disabled = view.phase === 'installing'; button.dataset.failed = String(view.failed);
      icon.toggleAttribute('hidden',view.busy); ring.element.hidden = !view.busy;
      const percent = view.phase === 'downloading' && view.ratio !== null ? Math.min(99,view.ratio * 100) : null;
      if (view.busy) ring.set(percent);
      icon.firstChild.setAttribute('d', info.downloaded ? installPath : downloadPath);
      const heading = info.updateVersion ? t('desktop.install_version',{version:info.updateVersion}) : t('desktop.software_update');
      const message = updateError && error ? CLATI18n.present(error) : CLATI18n.present(info.updatePresentation,info.updateStatus);
      const progress = view.phase === 'downloading' ? t(view.total ? 'desktop.rail_progress' : 'desktop.rail_received',{received:(view.received/1048576).toFixed(1),...(view.total ? {total:(view.total/1048576).toFixed(1),percent:Math.floor(view.ratio*100)} : {})}) : '';
      button.removeAttribute('title');
      facts.replaceChildren();
      const fact=(key,value)=>CLATRailPopover.fact(facts,t('rail.'+key),value);
      if(info.desktopVersion)fact('current_version',info.desktopVersion);
      if(info.updateVersion)fact('latest_version',info.updateVersion);
      if(view.total)fact('package_size',(view.total/1048576).toLocaleString(CLATI18n.locale,{maximumFractionDigits:1})+' MiB');
      button.setAttribute('aria-label', [heading, view.busy ? t('desktop.working') : view.failed ? t('desktop.rail_failed') : t(view.action === 'install' ? 'desktop.install' : 'desktop.download')].join(' · '));
      title.textContent = heading; popup.setAttribute('aria-label',heading); status.textContent = message || ''; detail.textContent = progress;
      primary.hidden = !view.action; primary.disabled = view.busy; primary.textContent = t(view.failed ? 'desktop.rail_retry' : view.action === 'install' ? 'desktop.install' : 'desktop.download');
      cancel.hidden = !view.cancellable; cancel.textContent = t(view.phase === 'downloading' ? 'desktop.rail_cancel_download' : 'desktop.cancel_wait'); close.textContent = t('common.close');
      if (!view.visible) hide(); else if (view.failed && updateError && error && error !== previousError) show();
      previousError = error; if (!popup.hidden) position();
    }
    const attach = () => { hide(); document.getElementById('nav-settings')?.before(button); render(controller.snapshot()); };
    button.onclick = () => { const view = CLATDesktopUpdates.project(snapshot); if (view.failed || view.busy) show(); else if (view.action) void controller.act(view.action); };
    primary.onclick = () => { const view = CLATDesktopUpdates.project(controller.snapshot()); if (view.action) void controller.act(view.action); };
    cancel.onclick = () => void controller.act('cancel'); close.onclick = () => hide(true);
    window.addEventListener('clat:project-activated',attach); CLATI18n.onChange(() => render(controller.snapshot()));
    attach(); controller.subscribe(render);
  }
  globalThis.CLATDesktopUpdateRail = Object.freeze({mount});
})();
