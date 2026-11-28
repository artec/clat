const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('the desktop window sends terminal keystrokes on one socket', async ({ page, context }) => {
  test.skip(process.platform === 'win32', 'The fixture uses /bin/sh.');
  const host = hostInfo('workbench');
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: host.origin });
  await page.addInitScript(() => { window.__CLAT_WEBVIEW__ = 'wkwebview'; });
  const inputPosts = [];
  const pageErrors = [];
  const channels = [];
  page.on('websocket', socket => {
    const channel = { url: socket.url(), frames: [], open: true };
    channels.push(channel);
    socket.on('framereceived', frame => {
      const payload = frame.payload;
      channel.frames.push(Buffer.isBuffer(payload) ? payload.toString('utf8') : String(payload));
    });
    socket.on('close', () => { channel.open = false; });
  });
  page.on('pageerror', error => pageErrors.push(error.message));
  page.on('request', request => {
    if (request.url().includes('/api/terminal.input')) inputPosts.push(request.url());
  });
  await openWorkbench(page, host);
  const name = 'Drive socket fixture';
  const marker = 'DRIVE_PAGE_MARKER';
  const id = await page.evaluate(async name => {
    const operationId = crypto.randomUUID();
    const row = await rpc('terminal.create', {
      operationId,
      clientId: 'pending',
      shell: '/bin/sh',
      rows: 24,
      cols: 80,
    });
    await rpc('terminal.rename', { id: row.id, name });
    return row.id;
  }, name);
  try {
    await page.reload();
    await expect(page.locator('#conn-status')).toHaveAttribute('data-state', 'live', LIVE);
    await page.evaluate(() => { document.documentElement.dataset.webview = 'wkwebview'; });
    await page.click('#nav-terminal');
    const row = page.getByRole('button', { name: new RegExp(name) });
    await expect(row).toBeVisible(LIVE);
    await row.click();
    // Selecting the row replaces the socket. Type only after the replacement is streaming.
    let stable = null;
    let since = 0;
    await expect.poll(() => {
      const live = channels.find(channel => channel.open && channel.url.endsWith('/drive') && channel.frames.length > 0) || null;
      if (live !== stable) { stable = live; since = Date.now(); return false; }
      return Boolean(live) && Date.now() - since >= 400;
    }, LIVE).toBe(true);
    const postsBefore = inputPosts.length;
    await page.locator('.xterm-helper-textarea').focus();
    await page.keyboard.type(`printf '${marker}\\n'`);
    await page.keyboard.press('Enter');
    await expect.poll(() => channels.some(channel => channel.frames.join('').includes(marker)), LIVE).toBe(true);
    expect(inputPosts.length).toBe(postsBefore);
    expect(pageErrors).toEqual([]);
    await page.getByRole('banner').getByRole('button', { name: 'More actions' }).click();
    await page.getByRole('menuitem', { name: 'Find', exact: true }).click();
    const dialog = page.locator('dialog.clat-dialog');
    await dialog.getByRole('textbox').fill(marker);
    await dialog.locator('[data-role="accept"]').click();
    await page.locator('#terminal-page').getByRole('button', { name: 'Copy', exact: true }).click();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(marker);
  } finally {
    await page.evaluate(id => rpc('terminal.remove', {
      id, operationId: crypto.randomUUID(), confirmed: true,
    }), id).catch(() => {});
  }
});
