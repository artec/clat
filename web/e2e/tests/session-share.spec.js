const { test, expect } = require('@playwright/test');
const { hostInfo, openWorkbench, createWorkbenchConversation, LIVE } = require('../helpers/workbench');

async function addShare(page, source) {
  await page.click('#composer-add');
  await page.click('#add-session-share');
  const dialog = page.locator('.session-share-dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('select option[value="' + source + '"]')).toHaveCount(1, LIVE);
  await dialog.locator('select').selectOption(source);
  await expect(dialog.locator('button.primary')).toBeEnabled(LIVE);
  await dialog.locator('button.primary').click();
  await expect(page.locator('#session-share-rail .session-share-chip')).toHaveCount(1);
}

test('shares an archived conversation as a frozen user message', async ({ page }) => {
  await openWorkbench(page, hostInfo('workbench'));
  const source = await createWorkbenchConversation(page, 'Shared conversation source');
  await page.evaluate(id => rpc('session.organize', { id, archived: true, confirmed: true }), source);
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBeNull();
  await addShare(page, source);
  await page.click('#send');
  await expect(page.locator('#session-share-rail .session-share-chip')).toHaveCount(0, LIVE);
  await expect(page.locator('.msg.user').last()).toContainText('Shared conversation source', LIVE);
});

test('recalls a queued share and lets the user remove it', async ({ page }) => {
  await openWorkbench(page, hostInfo('question'));
  const newConversation = '.project-group:has(#session-list) [data-action=new-project-session]';
  await page.click(newConversation);
  await page.fill('#prompt', 'Original share body');
  await page.click('#send');
  const question = page.locator('.question-card').last();
  await question.getByRole('button', { name: 'stable', exact: true }).click(LIVE);
  await question.getByRole('button', { name: 'Send answer', exact: true }).click();
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  const source = await page.evaluate(async () => {
    const id = state.sessionId;
    await rpc('session.rename', { id, title: 'Queue share source' });
    await refreshSessions();
    return id;
  });
  await page.click(newConversation);
  await page.fill('#prompt', 'Wait for a response');
  await page.click('#send');
  await expect(question.getByRole('button', { name: 'Decline', exact: true })).toBeVisible(LIVE);
  await addShare(page, source);
  await page.click('#send');
  const trigger = page.locator('#pending-input-trigger');
  await expect(trigger).toBeEnabled(LIVE);
  await trigger.click();
  const row = page.locator('.pending-input-row').filter({ hasText: 'Queue share source' });
  await expect(row).toBeVisible();
  await expect(row).not.toContainText('Original share body');
  await row.getByRole('button', { name: 'Return to input' }).click();
  const chip = page.locator('#session-share-rail .session-share-chip');
  await expect(chip).toContainText('Queue share source', LIVE);
  await chip.getByRole('button', { name: 'Remove shared conversation' }).click();
  await expect(chip).toHaveCount(0);
  await question.getByRole('button', { name: 'Decline', exact: true }).click();
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
});
