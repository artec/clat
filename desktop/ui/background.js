/* Native background only: the Host, model and transcript never see this preference. */
(() => {
  'use strict';
  if (window.top !== window || !window.__TAURI__) return;
  const invoke = args => window.__TAURI__.core.invoke('desktop_background', args);
  const ready = async () => {
    let saved;
    try { saved = await invoke({}); } catch (error) { console.error('[CLAT] background', error); return; }
    const root = document.documentElement, style = document.createElement('style');
    style.textContent = `
      html[data-window-transparency] { background:transparent!important; }
      /* The outer canvas owns gaps and the pixels outside panel corners.
         Paint panel backgrounds only inside their own clipped boxes. */
      html[data-window-transparency] body { background:color-mix(in srgb,var(--chrome-bg,var(--bg)) var(--window-background-opacity),transparent)!important; }
      html[data-window-transparency] :is(.app,.application-toolbar,.global-rail,.shell-content,.conversation-header,.shell-pages,.shell-surface,.workspace-tool,.landing-card,.native-fallback-bar,.composer) { background:transparent!important; }
      html[data-window-transparency] :is(.conversation,.navigation-detail,.settings-content,.dev-editor-detail,.git-body,.landing) { background:color-mix(in srgb,var(--surface,var(--bg)) var(--window-background-opacity),transparent)!important; }
      html[data-window-transparency] :is(.sidebar,.navigation-sidebar,.editor-sidebar,.settings-nav,.settings-panel>header,.inspector) { background:color-mix(in srgb,var(--sidebar-bg,var(--surface)) var(--window-background-opacity),transparent)!important; }
      .desktop-background-control { display:flex;align-items:center;gap:10px;min-width:0; }
      .desktop-background-control input[type=range] { width:150px;max-width:30vw;min-height:24px;padding:0;accent-color:var(--accent); }
      .desktop-background-control output { min-width:4ch;text-align:end;font-variant-numeric:tabular-nums; }
      @media(forced-colors:active) { html[data-window-transparency] body { background:Canvas!important; } }
    `;
    document.head.append(style);
    const apply = value => {
      root.toggleAttribute('data-window-transparency', value > 0);
      root.style.setProperty('--window-background-opacity', (100 - value) + '%');
    };
    apply(saved.transparency);
    const card = document.createDocumentFragment();
    const t = key => CLATI18n.msg('desktop.' + key);
    const text = (node, key) => CLATI18n.bind(node, 'textContent', () => t(key));
    const row = (key, hint, control) => {
      const label = document.createElement('label'); label.className = 'settings-row';
      const copy = document.createElement('span'); copy.className = 'settings-row-copy';
      const title = document.createElement('strong'), detail = document.createElement('small');
      text(title, key); text(detail, hint); copy.append(title, detail); label.append(copy, control); card.append(label);
    };
    const controls = document.createElement('span'); controls.className = 'desktop-background-control';
    const slider = document.createElement('input'), output = document.createElement('output');
    slider.type = 'range'; slider.min = '0'; slider.max = '100'; slider.step = '1'; slider.value = saved.transparency;
    CLATI18n.bind(slider, 'ariaLabel', () => t('background_transparency'));
    output.textContent = slider.value + '%'; controls.append(slider, output);
    row('background_transparency', 'background_transparency_hint', controls);
    const blur = document.createElement('input'), blurOutput = document.createElement('output');
    const blurControls = document.createElement('span'); blurControls.className = 'desktop-background-control';
    blur.type = saved.blurAdjustable ? 'range' : 'checkbox';
    if (saved.blurAdjustable) { blur.min = '0'; blur.max = '100'; blur.step = '1'; blur.value = saved.blurStrength; }
    else { blur.className = 'settings-switch'; blur.checked = saved.blur; }
    CLATI18n.bind(blur, 'ariaLabel', () => t('background_blur'));
    blurOutput.textContent = blur.value + '%'; blurControls.append(blur);
    if (saved.blurAdjustable) blurControls.append(blurOutput);
    row('background_blur', saved.blurAdjustable ? 'background_blur_strength_hint' : 'background_blur_hint', blurControls);
    const status = document.createElement('p'); status.className = 'settings-note error'; status.setAttribute('role', 'alert');
    let errorText = '';
    CLATI18n.bind(status, 'textContent', () => errorText);
    let lane = Promise.resolve();
    const save = (preview = false) => {
      const requested = { transparency:Number(slider.value), preview, ...(saved.blurAdjustable ? { blurStrength:Number(blur.value) } : { blur:blur.checked }) };
      lane = lane.then(async () => {
        try { const result = await invoke(requested); if (!preview) saved = result; errorText = ''; status.textContent = ''; }
        catch (error) {
          slider.value = saved.transparency; blur.checked = saved.blur; blur.value = saved.blurStrength;
          blurOutput.textContent = blur.value + '%';
          output.textContent = slider.value + '%'; apply(saved.transparency);
          errorText = CLATI18n.present(error, String(error)); status.textContent = errorText;
        }
      });
    };
    slider.oninput = () => { output.textContent = slider.value + '%'; apply(Number(slider.value)); };
    let blurPreview = null;
    blur.oninput = () => {
      blurOutput.textContent = blur.value + '%';
      if (saved.blurAdjustable && blurPreview === null) blurPreview = setTimeout(() => { blurPreview = null; save(true); }, 80);
    };
    slider.onchange = () => save();
    blur.onchange = () => { clearTimeout(blurPreview); blurPreview = null; save(); };
    const rows = [...card.childNodes];
    const mount = () => {
      const target = document.querySelector('[data-settings-panel="appearance"] .settings-card');
      if (target && !target.contains(slider)) { target.append(...rows); target.after(status); }
    };
    window.addEventListener('clat:project-activated', mount);
    mount();
  };
  const boot = () => CLATI18n.ready.then(ready);
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, { once:true });
  else void boot();
})();
