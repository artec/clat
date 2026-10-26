// Manual layout review only. Widths/colors/radii are deliberately not CI contracts.
const { test, expect } = require('@playwright/test');
const { hostInfo, openWorkbench, LIVE } = require('../helpers/workbench');

test('review rounded content surfaces and map visibility at actual conversation widths', async ({ page }, info) => {
  await page.setViewportSize({ width: 1487, height: 1058 });
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
  await page.route('https://pi.at.cn/catalog.json', route => route.fulfill({
    contentType: 'application/json',
    body: require('node:fs').readFileSync(require('node:path').resolve(__dirname, '../../../market/catalog.json')),
  }));
  await openWorkbench(page, hostInfo('history'));
  await page.evaluate(() => CLATI18n.setLanguage('zh-Hans'));
  await expect(page.locator('#transcript > .msg').first()).toBeAttached(LIVE);
  const observations = [];
  async function shot(name) {
    await page.screenshot({ path: info.outputPath(name + '.png'), animations: 'disabled' });
    observations.push(await page.evaluate(name => ({
      name, conversationWidth: document.querySelector('.conversation').clientWidth,
      mapDisplayed: Boolean(document.querySelector('#message-map').getClientRects().length)
        && getComputedStyle(document.querySelector('#message-map')).display !== 'none',
      outlineEntries: state.history.outline.length,
      viewportWidth: innerWidth, scrollWidth: document.documentElement.scrollWidth,
    }), name));
  }
  await page.click('#inspector-toggle');
  await shot('conversation-narrow-map-hidden');
  await page.click('#project-list-toggle');
  await shot('conversation-wide-map-visible');
  const map = page.locator('.message-map-item').last();
  await map.hover(); await shot('map-preview');
  await page.mouse.move(40, 100);
  async function dragWorkArea(delta) {
    const handle = await page.locator('#inspector-resize').boundingBox();
    await page.mouse.move(handle.x + 3, handle.y + 200);
    await page.mouse.down();
    await page.mouse.move(handle.x + 3 + delta, handle.y + 200, { steps: 8 });
    await page.mouse.up();
  }
  await dragWorkArea(-240); await shot('conversation-work-area-wider');
  await dragWorkArea(240); await shot('conversation-work-area-restored');
  await page.click('#inspector-toggle');
  await page.click('#nav-extensions');
  await expect(page.locator('.market-item h3').first()).toBeVisible(LIVE);
  await expect(page.locator('.plugin-installed .market-summary').first()).toBeVisible(LIVE);
  await shot('extensions-dark');
  await page.click('#nav-settings'); await shot('settings-dark');
  await page.evaluate(() => CLATI18n.setLanguage('en'));
  await page.evaluate(() => { state.theme = 'light'; applyPresentation(); });
  await shot('settings-light');
  await page.click('#nav-extensions'); await shot('extensions-light');
  await page.evaluate(() => { state.theme = 'system'; applyPresentation(); });
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.click('#nav-workbench'); await shot('conversation-phone');
  await page.click('#mobile-sidebar-open'); await shot('conversation-list-phone');
  await page.click('#mobile-sidebar-close');
  await page.click('#inspector-toggle'); await shot('extension-phone');
  await info.attach('layout-observations', { body: JSON.stringify(observations, null, 2), contentType: 'application/json' });
  require('node:fs').writeFileSync(info.outputPath('observations.json'), JSON.stringify(observations, null, 2));
});
