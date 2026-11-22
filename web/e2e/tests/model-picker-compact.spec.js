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
  // A channel with more models than fit must scroll inside its own pane,
  // never paint rows beneath the fixed thinking-intensity panel.
  for (const viewport of [{ width: 1280, height: 720 }, { width: 1008, height: 1100 }, { width: 504, height: 700 }]) {
    await page.setViewportSize(viewport);
    const layout = await page.evaluate(() => {
      const browser = document.querySelector('#model-picker-browser');
      const list = document.querySelector('#model-picker-list');
      const panel = document.querySelector('.model-thinking-panel');
      return {
        browserBottom: browser.getBoundingClientRect().bottom,
        listBottom: list.getBoundingClientRect().bottom,
        panelTop: panel.getBoundingClientRect().top,
        clientHeight: list.clientHeight,
        scrollHeight: list.scrollHeight,
        browserOverflow: getComputedStyle(browser).overflowY,
        resultsOverflow: getComputedStyle(browser.querySelector('.model-picker-results')).overflowY,
      };
    });
    expect(layout.browserOverflow).toBe('hidden');
    expect(layout.resultsOverflow).toBe('hidden');
    expect(layout.browserBottom).toBeLessThanOrEqual(layout.panelTop);
    expect(layout.listBottom).toBeLessThanOrEqual(layout.panelTop);
    expect(layout.scrollHeight).toBeGreaterThan(layout.clientHeight);
    await choices.last().scrollIntoViewIfNeeded();
    const scrolled = await page.evaluate(() => {
      const list = document.querySelector('#model-picker-list');
      const last = list.querySelector('.model-choice:last-child');
      const bottom = list.getBoundingClientRect().bottom;
      return { scrollTop: list.scrollTop, bottom, lastBottom: last.getBoundingClientRect().bottom,
        panelTop: document.querySelector('.model-thinking-panel').getBoundingClientRect().top };
    });
    expect(scrolled.scrollTop).toBeGreaterThan(0);
    expect(scrolled.lastBottom).toBeLessThanOrEqual(scrolled.bottom + 1);
    expect(scrolled.lastBottom).toBeLessThanOrEqual(scrolled.panelTop);
    await choices.first().scrollIntoViewIfNeeded();
  }
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

test('last selected OpenCode Go preset stays scrollable above thinking controls', async ({ page }) => {
  await page.route('**/api/model.settings.get', async route => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.active_profile = null;
    body.value.current.preset = 'opencode-grok-4.7';
    body.value.current.model = 'grok-4.7';
    body.value.current.vendor = 'OpenCode Go';
    body.value.current.image_input = true;
    await route.fulfill({ response, json: body });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.click('#model-picker-trigger');
  const rows = page.locator('#model-picker-list .model-choice');
  await expect(rows).toHaveCount(12);
  await expect(rows.last()).toHaveAttribute('aria-checked', 'true');
  await rows.last().scrollIntoViewIfNeeded();
  const positions = await page.evaluate(() => {
    const list = document.querySelector('#model-picker-list');
    const last = list.querySelector('.model-choice:last-child');
    return {
      listBottom: list.getBoundingClientRect().bottom,
      lastBottom: last.getBoundingClientRect().bottom,
      panelTop: document.querySelector('.model-thinking-panel').getBoundingClientRect().top,
      scrollTop: list.scrollTop,
    };
  });
  expect(positions.scrollTop).toBeGreaterThan(0);
  expect(positions.lastBottom).toBeLessThanOrEqual(positions.listBottom + 1);
  expect(positions.lastBottom).toBeLessThanOrEqual(positions.panelTop);
});

test('thinking preview stays smooth and stable while the Host saves and refreshes', async ({ page }) => {
  let savedLevel = 'high';
  let finishSave;
  const pendingSave = new Promise(resolve => { finishSave = resolve; });
  await page.route('**/api/model.settings.get', async route => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.current.thinking_levels = ['low', 'high', 'max'];
    body.value.current.thinking_level = savedLevel;
    await route.fulfill({ response, json: body });
  });
  await page.route('**/api/workbench.info', async route => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.model.thinking_level = savedLevel;
    await route.fulfill({ response, json: body });
  });
  await page.route('**/api/model.overrides.set', async route => {
    expect(route.request().postDataJSON()).toMatchObject({
      field: 'thinking_level', state: 'set', value: 'max',
    });
    await pendingSave;
    savedLevel = 'max';
    await route.fulfill({ json: { ok: true, value: {} } });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.click('#model-picker-trigger');
  const slider = page.locator('#model-thinking');
  await expect(slider).toHaveValue('1');
  const animation = await slider.evaluate(input => {
    const fill = getComputedStyle(input.parentElement, '::after');
    return { property: fill.transitionProperty, duration: fill.transitionDuration };
  });
  expect(animation.property).toContain('transform');
  expect(animation.duration).not.toBe('0s');
  await slider.press('End');
  await expect(slider).toHaveAttribute('data-saving', '');
  // Switching modes re-renders the picker while the backend still reports the
  // old value. Neither that render nor a Host refresh may rewind the preview.
  await page.locator('#model-picker-custom').click();
  await page.locator('#model-picker-builtins').click();
  await expect(slider).toHaveValue('2');
  await expect(page.locator('#model-thinking-value')).toHaveText(/max/i);
  finishSave();
  await expect(slider).not.toHaveAttribute('data-saving', '', LIVE);
  await expect(slider).toBeEnabled(LIVE);
  await expect(slider).toHaveValue('2');
  await expect(page.locator('#model-picker-label')).toContainText('max');
  await expect(page.locator('#model-thinking-ticks > span[data-current="true"]')).toHaveCount(1);
});

test('thinking preview rolls back after a rejected save', async ({ page }) => {
  await page.route('**/api/model.settings.get', async route => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.current.thinking_levels = ['low', 'high', 'max'];
    body.value.current.thinking_level = 'high';
    await route.fulfill({ response, json: body });
  });
  await page.route('**/api/model.overrides.set', async route => {
    await route.fulfill({ json: { ok: false, error: { message: 'Override rejected' } } });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.click('#model-picker-trigger');
  const slider = page.locator('#model-thinking');
  await expect(slider).toHaveValue('1');
  await slider.press('End');
  await expect(page.locator('#model-picker-error')).toContainText('Override rejected', LIVE);
  await expect(slider).toBeEnabled(LIVE);
  await expect(slider).toHaveValue('1', LIVE);
  await expect(page.locator('#model-thinking-ticks > span[data-current="true"]')).toHaveCount(1);
});

test('thinking intensity honors reduced-motion preference', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await openWorkbench(page, hostInfo('success'));
  await page.click('#model-picker-trigger');
  const style = await page.locator('#model-thinking').evaluate(input =>
    getComputedStyle(input.parentElement, '::after').transitionDuration);
  expect(parseFloat(style)).toBeLessThan(0.001);
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
