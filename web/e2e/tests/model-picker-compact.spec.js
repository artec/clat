const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench, fetchBrowserRequest } = require('../helpers/workbench');

test('picker groups built-ins by route, preserves custom profiles and aligns to composer', async ({ page }) => {
  await page.route('**/api/model.settings.get', async route => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.current.preset = 'glm-5.3-flash';
    body.value.active_profile = null;
    body.value.current.vendor = 'GLM Coding Plan';
    body.value.current.model = 'glm-5.3-flash';
    body.value.current.image_input = true;
    await route.fulfill({ response, json: body });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.click('#model-picker-trigger');
  const popup = page.locator('#model-picker');
  await expect(popup).toBeVisible(LIVE);
  await expect(page.locator('#model-picker-current')).toContainText('GLM · GLM 5.3 Flash');
  await expect(page.locator('#model-picker-current .vision-capability')).toHaveCount(1);
  const activeVisionModel = page.locator('#model-picker-list .model-choice[aria-checked="true"]');
  await expect(activeVisionModel.locator(':scope > :last-child')).toHaveClass(/model-choice-vision/);
  await expect(activeVisionModel.locator(':scope > :nth-last-child(2)')).toHaveClass(/model-choice-mark/);
  await expect(page.locator('#model-picker-sources .model-picker-source')).toHaveCount(6);
  await page.locator('#model-picker-sources [data-model-source="OpenCode Go"]').click();
  const choices = page.locator('#model-picker-list .model-choice');
  await expect(choices).toHaveCount(12);
  await expect(choices.first().locator('strong')).toHaveText('DeepSeek V4 Pro');
  await expect(choices.first()).not.toContainText('built-in preset');
  await expect(choices.nth(1).locator(':scope > :last-child')).toHaveClass(/model-choice-vision/);
  await page.locator('#model-picker-custom').click();
  await expect(page.locator('#model-picker-sources')).toBeHidden();
  await page.locator('#model-picker-builtins').click();
  await expect(page.locator('#model-picker-sources')).toBeVisible();
  await expect(page.locator('#model-picker-key')).toBeHidden();
  await expect(page.locator('#model-picker-manage')).toBeVisible();
  await expect(popup.locator('input[type="search"]')).toHaveCount(0);
  const bounds = await page.evaluate(() => ({
    popup: document.querySelector('#model-picker').getBoundingClientRect().right,
    composer: document.querySelector('#composer-shell').getBoundingClientRect().right,
  }));
  expect(bounds.popup).toBeLessThanOrEqual(bounds.composer + 1);
});

test('thinking intensity retains host-provided levels and updates visual endpoints', async ({ page }) => {
  await page.route('**/api/model.settings.get', async route => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.current.thinking_levels = ['low', 'medium', 'high', 'max'];
    body.value.current.thinking_level = 'medium';
    await route.fulfill({ response, json: body });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.click('#model-picker-trigger');
  const slider = page.locator('#model-thinking');
  await expect(slider).toHaveValue('1');
  await expect(page.locator('#model-thinking-ticks > span')).toHaveCount(4);
  await expect(page.locator('#model-thinking-min')).not.toBeEmpty();
  await expect(page.locator('#model-thinking-max')).not.toBeEmpty();
  const centers = await slider.evaluate(input => {
    const control = input.closest('.model-thinking-control');
    const frame = control.getBoundingClientRect();
    const range = input.getBoundingClientRect();
    const track = getComputedStyle(control, '::before');
    return {
      control: frame.top + frame.height / 2,
      input: range.top + range.height / 2,
      track: frame.top + parseFloat(track.top) + parseFloat(track.height) / 2,
    };
  });
  expect(Math.abs(centers.control - centers.input)).toBeLessThan(1);
  expect(Math.abs(centers.control - centers.track)).toBeLessThan(1);
  await slider.press('End');
  await expect(slider).toHaveValue('3');
  await expect(page.locator('#model-thinking-ticks > span[data-current="true"]')).toHaveCount(1);
});
