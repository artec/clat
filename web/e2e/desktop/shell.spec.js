// Native commands are mocked here; WKWebView/ACL and installer claims stay separate.
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const ui = path.resolve(__dirname, '../../../desktop/ui');

async function welcome(page, busy = false) {
  await page.route('http://desktop.test/**', route => {
    const name = new URL(route.request().url()).pathname.slice(1) || 'index.html';
    if (!['index.html', 'welcome.css', 'welcome.js', 'icon.png'].includes(name)) return route.abort();
    const types = {html:'text/html',css:'text/css',js:'application/javascript',png:'image/png'};
    return route.fulfill({ body:fs.readFileSync(path.join(ui, name)), contentType:types[name.split('.').pop()] });
  });
  const bridge = fs.readFileSync(path.join(ui, 'bridge.js'), 'utf8');
  const nativeMock = busy => {
    window.desktopCalls = [];
    window.desktopInfo = { desktopVersion:'2.1.0',backendVersion:'2.1.0',hostVersion:'2.1.0',autoCheck:true,
      updateAvailable:busy,downloaded:busy,updateBusy:busy,updateStatus:busy ? '等待项目空闲' : '' };
    window.__TAURI__ = { core: { invoke:async (command, args) => {
      window.desktopCalls.push({command,args});
      if (command === 'desktop_info') return structuredClone(window.desktopInfo);
      if (command === 'desktop_check_update') window.desktopInfo.updateAvailable = true;
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
  await page.addInitScript({content:`(${nativeMock.toString()})(${JSON.stringify(busy)});\n${bridge}`});
  await page.goto('http://desktop.test/index.html');
  await page.locator('#settings').click();
  await expect(page.locator('#clat-desktop-settings')).toBeVisible();
}

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
