// AG-DOGFOOD-10 / A2 TITLE-OBS-1（捕获版）：逐字复刻 app.spec.js 的
// TITLE-CUT-1 用例流程（同 success 宿主、同断言、同顺序），仅在断言
// 失败时补落可归因 DOM 观测记录（display:none 祖先链等）。用于
// --repeat-each 驱动跨测试宿主状态累积下的间歇复现抓现场。
// 诊断用途：不修改产品、不修改原用例、不加延时刷绿。
const { test, expect } = require('@playwright/test');
const { openWorkbench, hostInfo, LIVE } = require('../helpers/workbench');

function observe(page, sessionId, phase) {
  return page.evaluate(({ sessionId, phase }) => {
    const rows = [...document.querySelectorAll(`[data-session-id="${sessionId}"]`)];
    const strong = rows.length ? rows[0].querySelector('.session-copy strong') : null;
    const hiddenAncestors = [];
    if (strong) {
      let node = strong.parentElement;
      while (node && node !== document.documentElement) {
        if (getComputedStyle(node).display === 'none') {
          hiddenAncestors.push(`${node.tagName.toLowerCase()}${node.id ? '#' + node.id : ''}${node.className && typeof node.className === 'string' ? '.' + node.className.split(/\s+/).join('.') : ''}`);
        }
        node = node.parentElement;
      }
    }
    const cs = strong ? getComputedStyle(strong) : null;
    return {
      phase,
      sessionId,
      matchCount: rows.length,
      isConnected: strong ? strong.isConnected : null,
      appClass: document.querySelector('#app').className,
      dataSidebar: document.querySelector('#app').dataset.sidebar,
      dataMobileSidebar: document.querySelector('#app').dataset.mobileSidebar,
      innerWidth: window.innerWidth,
      presentation: localStorage.getItem('clat.presentation.v1'),
      sessionsCount: (window.state && state.sessions) ? state.sessions.length : null,
      hiddenAncestors,
      display: cs ? cs.display : null,
      rectWidth: strong ? Math.round(strong.getBoundingClientRect().width) : null,
      scrollWidth: strong ? strong.scrollWidth : null,
      clientWidth: strong ? strong.clientWidth : null,
      text: strong ? strong.textContent.slice(0, 18) : null,
    };
  }, { sessionId, phase });
}

test('TITLE-OBS-1 capture: original TITLE-CUT-1 flow with failure-scene observation', async ({ page }) => {
  test.setTimeout(180_000);
  let sessionId = null;
  try {
    await openWorkbench(page, hostInfo('success'));
    // 机制探针：页面在后续导航后包裹 renderSessions 计数（非侵入），
    // 验证“测量窗口内列表被重渲染、定位器量到已脱挂节点”假说。
    await page.evaluate(() => {
      window.__renderCount = 0;
      window.__renderTimes = [];
      const orig = window.renderSessions;
      if (typeof orig === 'function') {
        window.renderSessions = function wrapped(...args) {
          window.__renderCount += 1;
          window.__renderTimes.push(Date.now());
          return orig.apply(this, args);
        };
        window.__renderWrap = 'ok';
      } else {
        window.__renderWrap = 'missing';
      }
    });
    await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
    await expect(page.locator('#send')).toBeEnabled(LIVE);
    await page.fill('#prompt', 'narrow viewport title');
    await page.click('#send');
    await expect(page.locator('.msg.assistant').last()).toBeVisible(LIVE);
    await page.evaluate(() => refreshWorkbench());
    await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).not.toBeNull();
    sessionId = await page.evaluate(() => state.sessionId);

    const longTitle = '这条标题用来验证窄视口下会话标题只做视觉省略而完整保存不会把残缺词句写回存储层';
    page.once('dialog', (dialog) => dialog.accept(longTitle));
    await page.click('#session-title');
    await expect(page.locator('#session-title')).toHaveText(longTitle, LIVE);

    const rowTitle = page.locator(`[data-session-id="${sessionId}"] .session-copy strong`);
    await expect(rowTitle).toHaveText(longTitle, LIVE);
    const rowMetrics = await rowTitle.evaluate((el) => ({
      scrollWidth: el.scrollWidth,
      clientWidth: el.clientWidth,
      textOverflow: getComputedStyle(el).textOverflow,
      overflow: getComputedStyle(el).overflow,
      whiteSpace: getComputedStyle(el).whiteSpace,
    }));
    // 失败现场捕获：几何断言红时落全量观测（含 display:none 祖先链与
    // renderSessions 重渲染计数/时间戳）。
    if (rowMetrics.scrollWidth <= rowMetrics.clientWidth) {
      const scene = await observe(page, sessionId, 'row-metrics-failing');
      const renders = await page.evaluate(() => ({
        wrap: window.__renderWrap,
        count: window.__renderCount || 0,
        last: (window.__renderTimes || []).slice(-4),
        now: Date.now(),
      }));
      console.log('TITLE-OBS-1 FAILURE SCENE:', JSON.stringify({ ...scene, renders }, null, 1));
    }
    expect(rowMetrics.scrollWidth).toBeGreaterThan(rowMetrics.clientWidth);
    expect(rowMetrics.textOverflow).toBe('ellipsis');
    expect(rowMetrics.overflow).toBe('hidden');
    expect(rowMetrics.whiteSpace).toBe('nowrap');

    await page.setViewportSize({ width: 360, height: 640 });
    await expect(page.locator('#session-title')).toHaveText(longTitle, LIVE);
    const narrow = await page.evaluate(() => {
      const title = document.querySelector('#session-title');
      const rect = title.getBoundingClientRect();
      const cs = getComputedStyle(title);
      return {
        viewport: window.innerWidth,
        docScrollWidth: document.documentElement.scrollWidth,
        titleRight: Math.round(rect.right),
        titleScroll: title.scrollWidth,
        titleClient: title.clientWidth,
        textOverflow: cs.textOverflow,
        overflow: cs.overflow,
        whiteSpace: cs.whiteSpace,
      };
    });
    console.log('TITLE-CUT-1 narrow header metrics:', JSON.stringify(narrow));
    expect(narrow.docScrollWidth).toBeLessThanOrEqual(360);
    expect(narrow.titleRight).toBeLessThanOrEqual(360);
    expect(narrow.titleScroll).toBeGreaterThan(narrow.titleClient);
    expect(narrow.textOverflow).toBe('ellipsis');
    expect(narrow.overflow).toBe('hidden');
    expect(narrow.whiteSpace).toBe('nowrap');
  } catch (error) {
    if (sessionId) {
      try {
        console.log('TITLE-OBS-1 FAILURE OBSERVE:', JSON.stringify(await observe(page, sessionId, 'catch'), null, 1));
      } catch (e) { /* page may be closed */ }
    }
    throw error;
  }
});
