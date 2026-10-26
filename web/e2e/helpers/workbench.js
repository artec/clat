const { expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const LIVE = { timeout: 30_000 };

function hostInfo(key) {
  const stateDir = process.env.CLAT_E2E_RUN_DIR || path.join(__dirname, '..');
  return JSON.parse(fs.readFileSync(path.join(stateDir, `.serve-${key}.json`), 'utf8'));
}

async function openWorkbench(page, entry) {
  const diagnostics = [];
  const record = (kind, detail) => {
    diagnostics.push({ kind, detail: String(detail).slice(0, 500) });
    if (diagnostics.length > 30) diagnostics.shift();
  };
  const onError = (error) => record('pageerror', error.message);
  const onFailure = (request) => record('requestfailed',
    new URL(request.url()).pathname + ': ' + request.failure()?.errorText);
  page.on('pageerror', onError);
  page.on('requestfailed', onFailure);
  try {
    await page.goto(`${entry.origin}/`);
    await expect(page).toHaveURL(`${entry.origin}/`);
    await expect(page.locator('#landing')).toBeHidden(LIVE);
    await expect(page.locator('#conn-status')).toHaveAttribute('data-state','live', LIVE);
  } catch (error) {
    console.error('Workbench startup diagnostics:', JSON.stringify(diagnostics));
    throw error;
  } finally {
    page.off('pageerror', onError);
    page.off('requestfailed', onFailure);
  }
}

async function openWorkbenchTool(page, id) {
  const pages = { 'settings-open':'nav-settings', 'market-open':'nav-extensions', 'history-open':'nav-history', 'jobs-open':'nav-tasks', 'help-open':'nav-help' };
  if (pages[id]) return page.click('#' + pages[id]);
  if (id === 'schedules-open') { await page.click('#nav-tasks'); return page.selectOption('#task-type','schedules'); }
  if (!await page.locator('#' + id).isVisible()) await page.click('#tools-open');
  await page.click('#' + id);
}

// route.fetch uses an API client; forward the original complete browser
// headers explicitly, including Fetch Metadata required by the CSRF fence.
async function fetchBrowserRequest(route) {
  const request = route.request();
  const headers = await request.allHeaders();
  // Chromium omits Fetch Metadata on intercepted requests. Restore same-origin
  // metadata only when both the actual frame and Origin agree with the target.
  const target = new URL(request.url()).origin;
  if (!headers['sec-fetch-site'] && headers.origin === target
      && new URL(request.frame().url()).origin === target) {
    headers['sec-fetch-site'] = 'same-origin';
  }
  return route.fetch({ headers });
}

async function addWorkbenchProject(page, root) {
  await page.click('#nav-settings');
  await page.click('[data-settings-category="projects"]');
  await page.fill('#workspace-root', root);
  await page.check('#workspace-trust');
  await page.click('#workspace-open');
  await expect(page.locator('#project-root')).toHaveText(root, LIVE);
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  return new URL(page.url()).searchParams.get('workspace');
}

async function createWorkbenchConversation(page, title) {
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBeNull();
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', title); await page.click('#send');
  await expect(page.locator('.msg.assistant').last()).toBeVisible(LIVE);
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).not.toBeNull();
  return page.evaluate(async title => {
    const id = state.sessionId;
    await rpc('session.rename', { id, title }); await refreshWorkbench(); await refreshSessions(); return id;
  }, title);
}
module.exports = { LIVE, hostInfo, openWorkbench, openWorkbenchTool, fetchBrowserRequest, addWorkbenchProject, createWorkbenchConversation };
