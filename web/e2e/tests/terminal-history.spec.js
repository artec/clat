const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('an ended terminal can replay its output after the page reloads', async ({ page, context }) => {
  test.skip(process.platform === 'win32', 'The fixture uses /bin/sh.');
  const host = hostInfo('workbench');
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: host.origin });
  await openWorkbench(page, host);
  const name = 'Ended terminal replay fixture';
  const marker = 'terminal-history-output-marker';
  const { id, clientId } = await page.evaluate(async name => {
    const operationId = crypto.randomUUID(), clientId = crypto.randomUUID();
    const { id } = await rpc('terminal.create', { operationId, clientId, shell: '/bin/sh' });
    await rpc('terminal.rename', { id, name });
    return { id, clientId };
  }, name);
  try {
    await page.evaluate(({ id, clientId }) => rpc('terminal.input', {
      id, clientId, lease: 1, sequence: 0,
      data: "printf 'terminal-history-%s\\n' 'output-marker'; exit\n",
    }), { id, clientId });
    await expect.poll(() => page.evaluate(async id => {
      const { terminals } = await rpc('terminal.list', {});
      return terminals.find(row => row.id === id)?.phase;
    }, id), LIVE).toBe('ended');

    await page.reload();
    await page.click('#nav-terminal');
    await page.getByRole('button', { name: new RegExp(name) }).click();
    await expect(async () => {
      await page.getByRole('button', { name: 'Find', exact: true }).click();
      const dialog = page.locator('dialog.clat-dialog');
      await dialog.getByRole('textbox').fill(marker);
      await dialog.locator('[data-role="accept"]').click();
      await page.locator('#terminal-page').getByRole('button', { name: 'Copy', exact: true }).click();
      expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(marker);
    }).toPass(LIVE);
  } finally {
    await page.evaluate(id => rpc('terminal.remove', {
      id, operationId: crypto.randomUUID(), confirmed: true,
    }), id);
  }
});
