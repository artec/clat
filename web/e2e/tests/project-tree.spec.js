const { test, expect } = require('@playwright/test');
const { openWorkbench, hostInfo, addWorkbenchProject, fetchBrowserRequest, LIVE, createWorkbenchConversation: conversation } = require('../helpers/workbench');

const group = (page, route) => page.locator(`[data-project-owner="${route}"]`);
const entry = (page, id) => page.locator(`[data-session-id="${id}"] .session-item`);

test('project conversations open their own session and retain each project draft; expanding remains read-only', async ({ page }) => {
  const fixture = hostInfo('project-tree');
  const root = fixture.secondaryProject.root;
  await openWorkbench(page, fixture);
  const a1 = await conversation(page, 'Project A first');
  const a2 = await conversation(page, 'Project A second');
  await page.fill('#prompt', 'Unsent A second');
  const route = await addWorkbenchProject(page, root);
  const [b1, b2] = fixture.secondaryProject.sessions;
  await entry(page, b2).click();
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(b2);
  await page.fill('#prompt', 'Unsent B second');
  await entry(page, a1).click();
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(a1);
  await expect(page.locator('#prompt')).toHaveValue('');
  await entry(page, a2).click();
  await expect(page.locator('#prompt')).toHaveValue('Unsent A second', LIVE);
  await entry(page, b2).click();
  await expect(page.locator('#prompt')).toHaveValue('Unsent B second', LIVE);
  await group(page, 'default').locator('[data-action="new-project-session"]').click();
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBeNull();
  await expect(page.locator('#prompt')).toHaveValue('');
  await entry(page, a2).click();
  await expect(page.locator('#prompt')).toHaveValue('Unsent A second', LIVE);
  // A clean document has no retained B view. Browsing B metadata must not subscribe or mutate it.
  const streams = [], writes = [];
  page.on('request', request => {
    if (request.url().endsWith(`/workspace/${route}/api/events`)) streams.push(request.url());
    if (/\/api\/(session\.(switch|new)|prompt\.send)$/.test(request.url())) writes.push(request.url());
  });
  await page.reload();
  await expect(entry(page, b1)).toBeVisible(LIVE);
  await page.fill('#prompt', 'Read-only browsing keeps this draft');
  await group(page, route).locator('[data-action="toggle-project"]').click();
  await expect(entry(page, b1)).toBeHidden();
  await group(page, route).locator('[data-action="toggle-project"]').click();
  await expect(entry(page, b1)).toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue('Read-only browsing keeps this draft');
  expect(await page.evaluate(() => state.sessionId)).toBe(a2);
  expect(streams).toEqual([]); expect(writes).toEqual([]);
  await entry(page, b1).click();
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(b1);
  await expect(page.locator('#project-root')).toHaveText(root);
});

test('a conversation waiting for another project to connect cannot override a newer navigation', async ({ page }) => {
  const fixture = hostInfo('project-tree'), root = fixture.secondaryProject.root;
  let release;
  try {
    await openWorkbench(page, fixture);
    const route = await addWorkbenchProject(page, root);
    const target = fixture.secondaryProject.sessions[0];
    const other = fixture.secondaryProject.sessions[1];
    await entry(page, other).click();
    await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(other);
    await page.click('[data-project-route="default"]'); await page.click('#nav-workbench');
    await page.reload(); await expect(entry(page, target)).toBeVisible(LIVE);
    await page.fill('#prompt', 'Newer navigation wins');
    let issued;
    const started = new Promise(resolve => { issued = resolve; });
    const held = new Promise(resolve => { release = resolve; });
    const switches = [];
    page.on('request', request => { if (request.url().endsWith(`/workspace/${route}/api/session.switch`)) switches.push(request.url()); });
    await page.route(`**/workspace/${route}/api/workbench.info`, async request => {
      const response = await fetchBrowserRequest(request); issued(); await held; await request.fulfill({ response });
    });
    await entry(page, target).click(); await started;
    await page.click('[data-project-route="default"]'); release();
    await page.unrouteAll({ behavior: 'wait' });
    await expect(page.locator('#prompt')).toHaveValue('Newer navigation wins', LIVE);
    expect(new URL(page.url()).searchParams.get('workspace')).toBeNull();
    expect(switches).toEqual([]);
  } finally { release?.(); }
});
