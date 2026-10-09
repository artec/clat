const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('QUE-1 PWA real run queues separate next turn, recalls, dispatches, and retains cancellation', async ({ page }) => {
  await openWorkbench(page, hostInfo('queue-command'));
  await page.evaluate(() => rpc('permission.set', { mode: 'read-only' }));
  await page.fill('#prompt', 'queue first task'); await page.click('#send');
  const firstApproval = page.locator('.approval-card').first();
  await expect(firstApproval).toBeVisible(LIVE);
  await page.fill('#prompt', 'queue second task'); await page.click('#queue-enqueue');
  await expect(page.locator('#next-turn-queue')).toContainText('queue second task', LIVE);
  await expect(page.locator('#prompt')).toHaveValue('');
  await expect(page.locator('.msg.user')).toHaveCount(1);
  await page.fill('#prompt', 'existing draft'); await page.click('#queue-recall');
  await expect(page.locator('#prompt')).toHaveValue('queue second task\nexisting draft', LIVE);
  await page.waitForFunction(() => !nextTurnBusy);
  await page.fill('#prompt', 'queue second task'); await page.click('#queue-enqueue');
  await expect(page.locator('#next-turn-queue')).toContainText('queue second task', LIVE);
  await firstApproval.locator('button.primary').click();
  await expect(page.locator('.msg.user')).toHaveCount(2, LIVE);
  await expect(page.locator('.msg.user').last()).toContainText('queue second task');
  await expect(page.locator('.approval-card').nth(1)).toBeVisible(LIVE);
  await page.fill('#prompt', 'queue third task retained'); await page.click('#queue-enqueue');
  await expect(page.locator('#next-turn-queue')).toContainText('queue third task retained', LIVE);
  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect(page.locator('#next-turn-queue')).toContainText('queue third task retained');
  await page.click('#queue-recall');
  await expect(page.locator('#prompt')).toHaveValue('queue third task retained', LIVE);
  await expect(page.locator('#next-turn-queue')).toBeHidden();
});

test('QUE-1 PWA actual unclaimed pure text steering restores on cancel without overwriting editing', async ({ page }) => {
  await openWorkbench(page, hostInfo('queue-command'));
  await page.evaluate(() => rpc('permission.set', { mode: 'read-only' }));
  await page.fill('#prompt', 'hold for text steering'); await page.click('#send');
  await expect(page.locator('.approval-card').last()).toBeVisible(LIVE);
  await page.fill('#prompt', 'unclaimed text'); await page.click('#send');
  await expect(page.locator('#prompt')).toHaveValue('', LIVE);
  await page.fill('#prompt', 'new editing'); await page.click('#cancel');
  await expect(page.locator('#prompt')).toHaveValue('unclaimed text\nnew editing', LIVE);
});
