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
    await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  } catch (error) {
    console.error('Workbench startup diagnostics:', JSON.stringify(diagnostics));
    throw error;
  } finally {
    page.off('pageerror', onError);
    page.off('requestfailed', onFailure);
  }
}

async function openWorkbenchTool(page, id) {
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

module.exports = { LIVE, hostInfo, openWorkbench, openWorkbenchTool, fetchBrowserRequest };
