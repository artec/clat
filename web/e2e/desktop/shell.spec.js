// Native commands are mocked here; WKWebView/ACL and installer claims stay separate.
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const ui = path.resolve(__dirname, '../../../desktop/ui');
const localized = (page, key, args = {}) => page.evaluate(({key,args}) => CLATI18n.t(key,args), {key,args});

async function welcome(page, busy = false, withSidebar = false, loading = false, platform = 'macos') {
  await page.route('http://desktop.test/**', route => {
    const name = new URL(route.request().url()).pathname.slice(1) || 'index.html';
    if (!['index.html', 'welcome.css', 'welcome.js', 'icon.png'].includes(name)) return route.abort();
    const types = {html:'text/html',css:'text/css',js:'application/javascript',png:'image/png'};
    let body=fs.readFileSync(path.join(ui,name));
    if(withSidebar&&name==='index.html')body=Buffer.from(body.toString().replace('<footer>','<footer class="sidebar-footer">'));
    return route.fulfill({ body, contentType:types[name.split('.').pop()] });
  });
  const bridge = fs.readFileSync(path.join(ui, 'bridge.js'), 'utf8');
  const nativeMock = ({busy, catalogs,manifest,loading,platform}) => {
    // Mirrors the platform marker injected by the native window builder.
    window.__CLAT_NATIVE_FRAME__ = platform;
    window.desktopCalls = []; window.desktopEvents = {};
    window.desktopLanguage='zh-Hans';
    const localeInfo=()=>({manifest,preference:window.desktopLanguage,locale:window.desktopLanguage==='system'?'en':window.desktopLanguage,catalogs});
    window.desktopInfo = { desktopVersion:'2.1.2',backendVersion:'2.1.2',hostVersion:'2.1.2',hostConnected:false,autoCheck:true,
      updateAvailable:busy,downloaded:busy,updateBusy:busy,updateStatus:busy ? '等待项目空闲' : '' };
    window.__TAURI__ = { event:{listen:async(name,callback)=>{window.desktopEvents[name]=callback;return()=>{};}}, core: { invoke:async (command, args) => {
      window.desktopCalls.push({command,args});
      if (command === 'desktop_window') return {platform};
      if (command === 'desktop_locale') return localeInfo();
      if (command === 'desktop_preferences') { if(args?.language)window.desktopLanguage=args.language; return localeInfo(); }
      if (command === 'desktop_info') return {...structuredClone(window.desktopInfo),startupLoading:window.startupLoading ?? loading};
      if (command === 'desktop_open_project' && window.desktopOpenError) throw window.desktopOpenError;
      if (command === 'desktop_check_update') {
        if(window.deferDesktopCheck) await new Promise(resolve=>{window.finishDesktopCheck=resolve;});
        if(window.desktopCheckError) throw window.desktopCheckError;
        window.desktopInfo.updateAvailable = true;
      }
      if (command === 'desktop_download_update') window.desktopInfo.downloaded = true;
      if (command === 'desktop_install_update' && window.deferDesktopInstall) {
        window.desktopInfo.updateBusy = true;
        await new Promise(resolve => { window.finishDesktopInstall = resolve; });
      }
      if (command === 'desktop_cancel_update') {
        window.desktopInfo.updateBusy = false;
        window.finishDesktopInstall?.();
      }
    }}};
  };
  // One script pins initialization order, including the native API shim.
  const runtime = fs.readFileSync(path.resolve(ui,'../../web/i18n.js'),'utf8');
  const catalogs=Object.fromEntries(['en','zh-Hans','zh-Hant'].map(id=>[id,JSON.parse(fs.readFileSync(path.resolve(ui,'../../locales',id+'.json'),'utf8'))]));
  const windowBar = fs.readFileSync(path.join(ui, 'window-bar.js'), 'utf8');
  await page.addInitScript({content:`(${nativeMock.toString()})(${JSON.stringify({busy,catalogs,loading,platform,manifest:JSON.parse(fs.readFileSync(path.resolve(ui,'../../locales/manifest.json'),'utf8'))})});\n${runtime}\n${windowBar}\n${bridge}`});
  await page.goto('http://desktop.test/index.html');
  if (loading) return;
  await page.locator('#settings').click();
  await expect(page.locator('#clat-desktop-settings')).toBeVisible();
}

test('native titlebar controls dispatch window operations without quitting or stopping the Host', async ({ page }) => {
  await welcome(page, false, false, false, 'windows');
  await page.locator('#clat-desktop-settings [data-action="close"]').click();
  for (const action of ['minimize', 'toggle_maximize', 'close']) {
    const button = page.locator(`[data-window-action="${action}"]`);
    await expect(button).toHaveAccessibleName(/.+/);
    await button.click();
    await expect.poll(() => page.evaluate(action => window.desktopCalls.some(call =>
      call.command === 'desktop_window' && call.args.action === action), action)).toBe(true);
  }
  await page.locator('.native-fallback-bar').dispatchEvent('mousedown', { button: 0, detail: 1 });
  await expect.poll(() => page.evaluate(() => window.desktopCalls.some(call => call.args?.action === 'drag'))).toBe(true);
  expect(await page.evaluate(() => window.desktopCalls.some(call => ['desktop_quit','desktop_stop_host'].includes(call.command)))).toBe(false);
});

test('startup shows only the logo until restoration settles, then reveals welcome only when needed', async ({page}) => {
  await welcome(page, false, false, true);
  await expect(page.locator('#startup-loading')).toBeVisible();
  await expect(page.locator('main')).toBeHidden();
  for (const locale of ['en','zh-Hans','zh-Hant']) {
    const label = await page.evaluate(async locale => { await CLATI18n.setLanguage(locale); return CLATI18n.t('desktop.opening_workbench'); },locale);
    await expect(page.locator('#startup-loading')).toContainText(label);
    await expect(page.locator('main')).toBeHidden();
  }
  await page.emulateMedia({colorScheme:'dark',reducedMotion:'reduce'});

  await page.waitForTimeout(2200); // Cross a status poll while restoration is still pending.
  await expect(page.locator('main')).toBeHidden();
  await page.evaluate(() => { window.startupLoading=false; window.desktopInfo.hostConnected=true; });
  await page.waitForTimeout(2200);
  await expect(page.locator('main')).toBeHidden();
  await page.evaluate(() => { window.desktopInfo.hostConnected=false; });
  await expect(page.locator('main')).toBeVisible();
  await expect(page.locator('#startup-loading')).toBeHidden();
});

test('startup failure replaces the logo with a translated recovery page',async({page})=>{
  await welcome(page,false,false,true);
  await page.evaluate(()=>{
    window.desktopInfo.connectionError='English diagnostic';
    window.desktopInfo.connectionPresentation={key:'desktop.restore_needs_trust',args:{}};
  });
  await expect(page.locator('main')).toBeVisible();
  await expect(page.locator('#startup-loading')).toBeHidden();
  await expect(page.locator('#status')).toContainText(await localized(page, 'desktop.restore_needs_trust'));
  await expect(page.locator('#open')).toBeEnabled();
});


test('Host compatibility errors retain their reason across live language changes', async ({page}) => {
  await welcome(page);
  await page.evaluate(() => {
    window.desktopInfo.connectionError = 'English diagnostic';
    window.desktopInfo.connectionPresentation = {key:'host.same_version_different_build',args:{}};
  });
  const dialog = page.locator('#clat-desktop-settings');
  for (const language of ['zh-Hans','zh-Hant','en']) {
    await dialog.locator('[data-language-select]').selectOption(language);
    const text = await localized(page, 'host.same_version_different_build');
    await expect(dialog.locator('#clat-desktop-update')).toContainText(text);
    await expect(page.locator('#status')).toContainText(text);
    await expect(page.locator('#status')).toContainText('clat host stop');
  }
  expect(await page.evaluate(() => window.desktopCalls.filter(c => ['desktop_stop_host','desktop_open_project'].includes(c.command)))).toEqual([]);
});

test('opening a project keeps a rejected Host reason localizable in welcome and settings', async ({page}) => {
  await welcome(page);
  await page.evaluate(() => { window.desktopOpenError = {key:'host.same_version_different_build',args:{}}; });
  const dialog = page.locator('#clat-desktop-settings');
  await dialog.locator('[data-action=open]').click();
  await expect(dialog.locator('#clat-desktop-update')).toContainText(await localized(page, 'host.same_version_different_build'));
  await dialog.locator('[data-action=close]').click();
  await page.locator('#open').click();
  await expect(page.locator('#status')).toContainText(await localized(page, 'host.same_version_different_build'));
  await page.locator('#settings').click();
  await dialog.locator('[data-language-select]').selectOption('zh-Hant');
  await expect(dialog.locator('#clat-desktop-update')).toContainText(await localized(page, 'host.same_version_different_build'));
  await expect(page.locator('#status')).toContainText(await localized(page, 'host.same_version_different_build'));
  await expect(page.locator('#status')).toContainText('clat host stop');
  expect(await page.evaluate(() => window.desktopCalls.filter(c => c.command==='desktop_stop_host'))).toEqual([]);
});

test('install requires a downloaded update and never discards unsent input', async ({page}) => {
  await welcome(page);
  const dialog = page.locator('#clat-desktop-settings');
  await expect(dialog.locator('[data-action=download]')).toBeDisabled();
  await expect(dialog.locator('[data-action=install]')).toBeDisabled();
  await dialog.locator('[data-action=check]').click();
  await expect(dialog.locator('[data-action=download]')).toBeEnabled();
  await expect(dialog.locator('[data-action=install]')).toBeDisabled();
  await dialog.locator('[data-action=download]').click();
  await expect(dialog.locator('[data-action=install]')).toBeEnabled();
  await page.evaluate(() => {
    const input = document.createElement('textarea'); input.id = 'prompt'; input.value = '不能丢失的中文草稿'; document.body.append(input);
  });
  await dialog.locator('[data-action=install]').click();
  await expect(dialog.locator('#clat-desktop-update')).toContainText(await localized(page, 'desktop.draft_warning'));
  const infoCalls = await page.evaluate(() => window.desktopCalls.filter(c => c.command === 'desktop_info').length);
  await page.waitForFunction(count => window.desktopCalls.filter(c => c.command === 'desktop_info').length > count, infoCalls);
  await expect(dialog.locator('#clat-desktop-update')).toContainText(await localized(page, 'desktop.draft_warning'));
  expect(await page.evaluate(() => document.querySelector('#prompt').value)).toBe('不能丢失的中文草稿');
  expect(await page.evaluate(() => window.desktopCalls.filter(c => c.command === 'desktop_install_update'))).toHaveLength(0);
  await page.evaluate(() => { document.querySelector('#prompt').value = ''; });
  await dialog.locator('[data-action=install]').click();
  expect(await page.evaluate(() => window.desktopCalls.filter(c => c.command === 'desktop_install_update'))).toHaveLength(1);
});

test('busy update can be cancelled without stopping the shared Host', async ({page}) => {
  await welcome(page, true);
  const dialog = page.locator('#clat-desktop-settings');
  await expect(dialog.locator('[data-action=install]')).toBeDisabled();
  await expect(dialog.locator('[data-action=cancel]')).toBeEnabled();
  await dialog.locator('[data-action=cancel]').click();
  await expect(dialog.locator('[data-action=install]')).toBeEnabled();
  const commands = await page.evaluate(() => window.desktopCalls.map(c => c.command));
  expect(commands).toContain('desktop_cancel_update');
  expect(commands).not.toContain('desktop_stop_host');
});

test('waiting for installation prevents new unsaved text and cancellation restores editing', async ({page}) => {
  await welcome(page);
  const dialog = page.locator('#clat-desktop-settings');
  await dialog.locator('[data-action=check]').click();
  await expect(dialog.locator('[data-action=download]')).toBeEnabled();
  await dialog.locator('[data-action=download]').click();
  await expect(dialog.locator('[data-action=install]')).toBeEnabled();
  await page.evaluate(() => {
    window.deferDesktopInstall = true;
    const input = document.createElement('textarea'); input.id = 'prompt'; document.body.append(input);
  });
  await dialog.locator('[data-action=install]').click();
  await expect(page.locator('#prompt')).not.toBeEditable();
  await expect(dialog.locator('[data-action=cancel]')).toBeEnabled();
  await dialog.locator('[data-action=cancel]').click();
  await expect(page.locator('#prompt')).toBeEditable();
});

test('connection failure and recovery refresh on welcome and in settings', async ({page}) => {
  await welcome(page);
  await page.evaluate(() => {
    Object.assign(window.desktopInfo, { hostVersion:null, hostConnected:false, backendPending:false,
      connectionError:'后台协议不兼容，请从当前客户端重新启动后台。' });
  });
  await expect(page.locator('#status')).toContainText('后台协议不兼容');
  // Reopen to refresh the about panel immediately, without changing native state.
  await page.locator('[data-action=close]').click();
  await page.locator('#settings').click();
  await expect(page.locator('#clat-desktop-versions')).toContainText(await localized(page, 'desktop.not_connected'));
  await expect(page.locator('[data-action=stop]')).toBeDisabled();
  await expect(page.locator('#clat-desktop-update')).toContainText('后台协议不兼容');
  await page.evaluate(() => Object.assign(window.desktopInfo, {
    hostVersion:'2.2.0',hostConnected:true,backendPending:false,connectionError:null,
  }));
  await expect(page.locator('#status')).toContainText(await localized(page, 'desktop.welcome'));
  await page.locator('[data-action=close]').click();
  await page.locator('#settings').click();
  await expect(page.locator('#clat-desktop-versions')).toContainText('2.2.0');
  await expect(page.locator('[data-action=stop]')).toBeEnabled();
  expect(await page.evaluate(() => window.desktopCalls.some(c => c.command === 'desktop_stop_host'))).toBe(false);
});

test('three desktop languages switch without reloading or losing a draft and pending update', async ({page}) => {
  await welcome(page,true);
  await page.evaluate(()=>{ const input=document.createElement('textarea'); input.id='prompt'; input.value='中文草稿 {path}';document.body.append(input); window.marker={}; });
  const dialog=page.locator('#clat-desktop-settings');
  const language=dialog.locator('[data-language-select]');
  for(const id of ['en','zh-Hant','zh-Hans']) {
    await language.selectOption(id);
    const close = await localized(page, 'common.close');
    const button=dialog.locator('[data-action=close]');
    await expect(button).toHaveAccessibleName(close);
    await expect(button).toHaveAttribute('title',close);

    await expect(page.locator('#prompt')).toHaveValue('中文草稿 {path}');
    await expect(dialog.locator('[data-action=cancel]')).toBeEnabled();
    expect(await page.evaluate(()=>!!window.marker)).toBe(true);
  }
  const commands=await page.evaluate(()=>window.desktopCalls.map(c=>c.command));
  expect(commands.filter(c=>!['desktop_info','desktop_locale','desktop_preferences','desktop_take_tray_intent'].includes(c))).toEqual([]);
});

test('native menu opens update panel and starts exactly one check',async({page})=>{
  await welcome(page);
  await page.locator('[data-action=close]').click();
  await page.evaluate(()=>window.desktopEvents['desktop-check-update']?.({payload:null}));
  await expect(page.locator('#clat-desktop-settings')).toBeVisible();
  await expect.poll(()=>page.evaluate(()=>window.desktopCalls.filter(c=>c.command==='desktop_check_update').length)).toBe(1);
  await expect(page.locator('[data-action=download]')).toBeEnabled();
});
test('slow check excludes repeated page and native menu actions',async({page})=>{
  await welcome(page); await page.evaluate(()=>{window.deferDesktopCheck=true;});
  await page.locator('[data-action=check]').click();
  await expect(page.locator('[data-action=check]')).toBeDisabled();
  await page.evaluate(()=>window.desktopEvents['desktop-check-update']?.({payload:null}));
  expect(await page.evaluate(()=>window.desktopCalls.filter(c=>c.command==='desktop_check_update').length)).toBe(1);
  await page.evaluate(()=>window.finishDesktopCheck());
  await expect(page.locator('[data-action=download]')).toBeEnabled();
});
test('failed update explanation changes language and offers manual recovery without losing state',async({page})=>{
  await welcome(page);
  await page.evaluate(()=>{window.desktopCheckError={key:'update.index_network',args:{},fallback:'network failure'};});
  await page.locator('[data-action=check]').click();
  await expect(page.locator('#clat-desktop-update')).toContainText(await localized(page, 'update.index_network'));
  await page.locator('[data-language-select]').selectOption('en');
  await expect(page.locator('#clat-desktop-update')).toContainText(await localized(page, 'update.index_network'));
  await expect(page.locator('a[data-update-manual]')).toHaveAttribute('href','https://github.com/artec/clat/releases/latest');
  await page.evaluate(()=>{window.desktopCheckError=null;});
  await page.locator('[data-action=check]').click();
  await expect(page.locator('[data-action=download]')).toBeEnabled();
});
test('image-only and composing drafts cannot start installation',async({page})=>{
  await welcome(page); await page.locator('[data-action=check]').click();await page.locator('[data-action=download]').click();
  await page.evaluate(()=>{
    const input=document.createElement('textarea');input.id='prompt';document.body.append(input);
    const rail=document.createElement('div');rail.id='attachment-rail';rail.innerHTML='<article class="attachment-chip">image.png</article>';document.body.append(rail);
  });
  await page.locator('[data-action=install]').click();
  await expect(page.locator('#clat-desktop-update')).toContainText(await localized(page, 'desktop.draft_warning'));
  await page.evaluate(()=>{document.querySelector('#attachment-rail').remove();document.querySelector('#prompt').dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true}));});
  await page.locator('[data-action=install]').click();
  expect(await page.evaluate(()=>window.desktopCalls.filter(c=>c.command==='desktop_install_update').length)).toBe(0);
  await page.evaluate(()=>document.querySelector('#prompt').dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})));
  await page.locator('[data-action=install]').click();
  expect(await page.evaluate(()=>window.desktopCalls.filter(c=>c.command==='desktop_install_update').length)).toBe(1);
});

test('native installation disables cancellation and quit without blocking status refresh',async({page})=>{
  await welcome(page,true);
  await page.evaluate(()=>Object.assign(window.desktopInfo,{updatePhase:'installing',updateCancellable:false,updatePresentation:{key:'update.installing',args:{},fallback:'Installing'}}));
  await expect(page.locator('[data-action=cancel]')).toBeDisabled();
  await expect(page.locator('[data-action=quit]')).toBeDisabled();
  await expect(page.locator('[data-action=stop]')).toBeDisabled();
  await expect(page.locator('#clat-desktop-update')).toContainText(await localized(page, 'update.installing'));
  await page.locator('[data-language-select]').selectOption('en');
  await expect(page.locator('#clat-desktop-update')).toContainText(await localized(page, 'update.installing'));
});

test('verified package remains installable after a native install error',async({page})=>{
  await welcome(page);
  await page.locator('[data-action=check]').click();await page.locator('[data-action=download]').click();
  await page.evaluate(()=>{
    const invoke=window.__TAURI__.core.invoke;let first=true;
    window.__TAURI__.core.invoke=async(command,args)=>{
      if(command==='desktop_install_update'&&first){first=false;throw {key:'update.install_failed',args:{detail:'fixture disk full'},fallback:'Install failed'};}
      return invoke(command,args);
    };
  });
  await page.locator('[data-action=install]').click();
  await expect(page.locator('#clat-desktop-update')).toContainText('fixture disk full');
  await expect(page.locator('[data-action=install]')).toBeEnabled();
  await expect(page.locator('[data-action=download]')).toBeDisabled();
  await page.locator('[data-action=install]').click();
  expect(await page.evaluate(()=>window.desktopCalls.filter(c=>c.command==='desktop_install_update').length)).toBe(1);
});

test('native navigation respects unsaved work owned outside the visible composer', async ({page}) => {
  await welcome(page);
  await page.evaluate(() => {
    window.addEventListener('clat-desktop-action-request', event => event.preventDefault());
    window.desktopInfo.updateAvailable = true;
    window.desktopInfo.downloaded = true;
  });
  await expect(page.locator('[data-action=install]')).toBeEnabled();
  await page.locator('[data-action=install]').click();
  await page.locator('[data-action=open]').click();
  const actions = await page.evaluate(() => window.desktopCalls.filter(call =>
    ['desktop_install_update','desktop_open_project'].includes(call.command)));
  expect(actions).toEqual([]);
});
