const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

for (const viewport of [{ width: 1280, height: 720 }, { width: 390, height: 640 }]) {
  test(`plugin dialog scrolls to the last installed and catalog plugins at ${viewport.width}px`, async ({ page }) => {
    await page.setViewportSize(viewport);
    const installed = Array.from({ length: 8 }, (_, i) => ({
      id: `dev.scroll.installed-${i}`, name: `Installed plugin ${i}`, version: '1.0.0',
      enabled: true, trust: 'publisher-verified',
    }));
    const catalog = Array.from({ length: 24 }, (_, i) => ({
      id: `dev.scroll.catalog-${i}`, name: `Catalog plugin ${i}`, runtime: 'wasm-component',
      status: 'preview', summary: 'Long plugin list scroll fixture', tags: ['WASM'],
    }));
    await page.route('https://pi.at.cn/catalog.json', route => route.fulfill({
      contentType: 'application/json', body: JSON.stringify({ schemaVersion: 1, packages: catalog }),
    }));
    await page.route('**/api/plugin.list', route => route.fulfill({
      contentType: 'application/json', body: JSON.stringify({ ok: true, value: { installed } }),
    }));
    await openWorkbench(page, hostInfo('success'));
    if (viewport.width <= 760) await page.click('#mobile-sidebar-open');
    await page.click('#market-open');
    await expect(page.locator('.plugin-installed .market-item')).toHaveCount(8, LIVE);
    await expect(page.locator('#market-list .market-item')).toHaveCount(24, LIVE);
    const dialog = page.locator('#market-dialog');
    const bounds = await dialog.boundingBox();
    await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
    // Use actual wheel input; scrollIntoView would bypass the user's broken scroll path.
    const lastInstalled = page.locator('.plugin-installed .market-item').last();
    const lastBounds = await lastInstalled.boundingBox();
    await page.mouse.wheel(0, lastBounds.y + lastBounds.height / 2 - bounds.y - bounds.height / 2);
    await expect(lastInstalled).toBeInViewport(LIVE);
    await page.mouse.wheel(0, 100000);
    await expect(page.locator('#market-list .market-item').last()).toBeInViewport(LIVE);
    await page.screenshot({ path: `../../output/playwright/plugin-scroll-${viewport.width}.png` });
    await page.mouse.wheel(0, -100000);
    await expect(page.locator('#market-close')).toBeInViewport(LIVE);
    await expect(page.locator('.plugin-installed .market-item').first()).toBeInViewport(LIVE);
    await page.keyboard.press('Escape');
    await expect(dialog).not.toBeVisible();
  });
}
