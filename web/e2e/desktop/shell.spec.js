// Native commands are mocked here; WKWebView/ACL and installer claims stay separate.
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const ui = path.resolve(__dirname, '../../../desktop/ui');

async function welcome(page, busy = false, withSidebar = false) {
  await page.route('http://desktop.test/**', route => {
    const name = new URL(route.request().url()).pathname.slice(1) || 'index.html';
    if (!['index.html', 'welcome.css', 'welcome.js', 'icon.png'].includes(name)) return route.abort();
    const types = {html:'text/html',css:'text/css',js:'application/javascript',png:'image/png'};
    let body=fs.readFileSync(path.join(ui,name));
    if(withSidebar&&name==='index.html')body=Buffer.from(body.toString().replace('<footer>','<footer class="sidebar-footer">'));
    return route.fulfill({ body, contentType:types[name.split('.').pop()] });
  });
  const bridge = fs.readFileSync(path.join(ui, 'bridge.js'), 'utf8');
  const nativeMock = ({busy, catalogs,manifest}) => {
    window.desktopCalls = []; window.desktopEvents = {};
    window.desktopLanguage='zh-Hans';
    const localeInfo=()=>({manifest,preference:window.desktopLanguage,locale:window.desktopLanguage==='system'?'en':window.desktopLanguage,catalogs});
    window.desktopInfo = { desktopVersion:'2.1.2',backendVersion:'2.1.2',hostVersion:'2.1.2',hostConnected:true,autoCheck:true,
      updateAvailable:busy,downloaded:busy,updateBusy:busy,updateStatus:busy ? '等待项目空闲' : '' };
    window.__TAURI__ = { event:{listen:async(name,callback)=>{window.desktopEvents[name]=callback;return()=>{};}}, core: { invoke:async (command, args) => {
      window.desktopCalls.push({command,args});
      if (command === 'desktop_locale') return localeInfo();
      if (command === 'desktop_preferences') { if(args?.language)window.desktopLanguage=args.language; return localeInfo(); }
      if (command === 'desktop_info') return structuredClone(window.desktopInfo);
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
  await page.addInitScript({content:`(${nativeMock.toString()})(${JSON.stringify({busy,catalogs,manifest:JSON.parse(fs.readFileSync(path.resolve(ui,'../../locales/manifest.json'),'utf8'))})});\n${runtime}\n${bridge}`});
  await page.goto('http://desktop.test/index.html');
  await page.locator('#settings').click();
  await expect(page.locator('#clat-desktop-settings')).toBeVisible();
}

test('language switching keeps the desktop sidebar launcher glyph and label nodes',async({page})=>{
  await welcome(page,false,true);
  const launcher=page.locator('.sidebar-footer .sidebar-footer-button');
  for(const language of ['en','zh-Hant','zh-Hans']){
    await page.locator('#clat-desktop-settings [data-language-select]').selectOption(language);
    await expect(launcher.locator('[aria-hidden=true]')).toHaveText('▣');
    await expect(launcher.locator('.sidebar-expanded-only')).not.toBeEmpty();
  }
});

test('Host compatibility errors retain their reason across live language changes', async ({page}) => {
  await welcome(page);
  await page.evaluate(() => {
    window.desktopInfo.connectionError = 'English diagnostic';
    window.desktopInfo.connectionPresentation = {key:'host.same_version_different_build',args:{}};
  });
  const dialog = page.locator('#clat-desktop-settings');
  for (const [language,text] of [['zh-Hans','同一版本'],['zh-Hant','相同版本'],['en','same CLAT version']]) {
    await dialog.locator('[data-language-select]').selectOption(language);
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
  await expect(dialog.locator('#clat-desktop-update')).toContainText('同一版本');
  await dialog.locator('[data-action=close]').click();
  await page.locator('#open').click();
  await expect(page.locator('#status')).toContainText('同一版本');
  await page.locator('#settings').click();
  await dialog.locator('[data-language-select]').selectOption('zh-Hant');
  await expect(dialog.locator('#clat-desktop-update')).toContainText('相同版本');
  await expect(page.locator('#status')).toContainText('相同版本');
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
  await expect(dialog.locator('#clat-desktop-update')).toContainText('先发送或清空');
  const infoCalls = await page.evaluate(() => window.desktopCalls.filter(c => c.command === 'desktop_info').length);
  await page.waitForFunction(count => window.desktopCalls.filter(c => c.command === 'desktop_info').length > count, infoCalls);
  await expect(dialog.locator('#clat-desktop-update')).toContainText('先发送或清空');
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
  await expect(page.locator('#clat-desktop-versions')).toContainText('当前 Host 未连接');
  await expect(page.locator('[data-action=stop]')).toBeDisabled();
  await expect(page.locator('#clat-desktop-update')).toContainText('后台协议不兼容');
  await page.evaluate(() => Object.assign(window.desktopInfo, {
    hostVersion:'2.2.0',hostConnected:true,backendPending:false,connectionError:null,
  }));
  await expect(page.locator('#status')).toContainText('选择项目后');
  await page.locator('[data-action=close]').click();
  await page.locator('#settings').click();
  await expect(page.locator('#clat-desktop-versions')).toContainText('当前 Host 2.2.0');
  await expect(page.locator('[data-action=stop]')).toBeEnabled();
  expect(await page.evaluate(() => window.desktopCalls.some(c => c.command === 'desktop_stop_host'))).toBe(false);
});

test('three desktop languages switch without reloading or losing a draft and pending update', async ({page}) => {
  await welcome(page,true);
  await page.evaluate(()=>{ const input=document.createElement('textarea'); input.id='prompt'; input.value='中文草稿 {path}';document.body.append(input); window.marker={}; });
  const dialog=page.locator('#clat-desktop-settings');
  const language=dialog.locator('[data-language-select]');
  for(const [id,close] of [['en','Close'],['zh-Hant','關閉'],['zh-Hans','关闭']]) {
    await language.selectOption(id);
    await expect(dialog.locator('[data-action=close]')).toHaveText(close);
    await expect(page.locator('#prompt')).toHaveValue('中文草稿 {path}');
    await expect(dialog.locator('[data-action=cancel]')).toBeEnabled();
    expect(await page.evaluate(()=>!!window.marker)).toBe(true);
  }
  const commands=await page.evaluate(()=>window.desktopCalls.map(c=>c.command));
  expect(commands.filter(c=>!['desktop_info','desktop_locale','desktop_preferences'].includes(c))).toEqual([]);
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
  await expect(page.locator('#clat-desktop-update')).toContainText('网络');
  await page.locator('[data-language-select]').selectOption('en');
  await expect(page.locator('#clat-desktop-update')).toContainText('network');
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
  await expect(page.locator('#clat-desktop-update')).toContainText('先发送或清空');
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
  await expect(page.locator('#clat-desktop-update')).toContainText('正在安装');
  await page.locator('[data-language-select]').selectOption('en');
  await expect(page.locator('#clat-desktop-update')).toContainText('Installing desktop');
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
