// AG-DOGFOOD-10 / A2 TITLE-OBS-1：侧栏标题行 scrollWidth==clientWidth==0
// 间歇红的可归因观测（诊断用例，不改产品）。
//
// 病历（docs/audit/2026-10-18-ag-dogfood9-title-layout-observer-case.md）：
// `app.spec.js:1367` 行度量在未注入缺陷的候选执行中至少两次 0/0 红，
// 单项复跑绿，CAUSE_UNKNOWN。本用例在同一真实链（success 宿主 + 真 Chromium）
// 上循环复刻 TITLE-CUT-1 的材料化/改名/度量序列，每次落一条**可归因
// DOM 观测记录**（会话身份、侧栏挂载/展开态、匹配数、isConnected、
// display:none 祖先链、布局盒、scroll/client 宽、computed style、
// localStorage 呈现偏好、refreshWorkbench 前后时序），并保留原始断言
// ——只诊断，不修复、不加无限重试、不删几何契约。
const { test, expect } = require('@playwright/test');
const { openWorkbench, hostInfo } = require('../helpers/workbench');

const OBS = (sel) => document.querySelectorAll(sel).length;

function observe(page, sessionId, phase) {
  return page.evaluate(({ sessionId, phase }) => {
    const rows = [...document.querySelectorAll(`[data-session-id="${sessionId}"]`)];
    const strong = rows.length ? rows[0].querySelector('.session-copy strong') : null;
    // display:none 祖先链：0/0 的唯一充分条件族。
    const hiddenAncestors = [];
    if (strong) {
      let node = strong.parentElement;
      while (node && node !== document.documentElement) {
        const display = getComputedStyle(node).display;
        if (display === 'none') {
          hiddenAncestors.push(`${node.tagName.toLowerCase()}${node.id ? '#' + node.id : ''}${node.className && typeof node.className === 'string' ? '.' + node.className.split(/\s+/).join('.') : ''}`);
        }
        node = node.parentElement;
      }
    }
    const cs = strong ? getComputedStyle(strong) : null;
    const rect = strong ? strong.getBoundingClientRect() : null;
    return {
      phase,
      sessionId,
      matchCount: rows.length,
      isConnected: strong ? strong.isConnected : null,
      appClass: document.querySelector('#app').className,
      dataSidebar: document.querySelector('#app').dataset.sidebar,
      dataMobileSidebar: document.querySelector('#app').dataset.mobileSidebar,
      innerWidth: window.innerWidth,
      visibilityState: document.visibilityState,
      presentation: localStorage.getItem('clat.presentation.v1'),
      sessionsCount: (window.state && state.sessions) ? state.sessions.length : null,
      switching: (window.state && state.switching) ?? null,
      hiddenAncestors,
      display: cs ? cs.display : null,
      textOverflow: cs ? cs.textOverflow : null,
      rectWidth: rect ? Math.round(rect.width) : null,
      scrollWidth: strong ? strong.scrollWidth : null,
      clientWidth: strong ? strong.clientWidth : null,
      text: strong ? strong.textContent : null,
    };
  }, { sessionId, phase });
}

test('TITLE-OBS-1 diagnostic: attributable DOM observation across repeated materializations', async ({ page }) => {
  test.setTimeout(300_000);
  const entry = hostInfo('success');
  const browser = page.context().browser();
  const records = [];
  let reproduced = null;
  for (let round = 1; round <= 8; round++) {
    // 每轮全新 context（镜像真实测试隔离：localStorage/视口全部重置），
    // 同一真实 success 宿主（与当年红象同型：宿主状态在同轮 run 内共享）。
    const context = await browser.newContext();
    const roundPage = await context.newPage();
    try {
      await openWorkbench(roundPage, entry);
      await roundPage.click('.project-group:has(#session-list) [data-action=new-project-session]');
      await expect(roundPage.locator('#send')).toBeEnabled({ timeout: 30_000 });
      await roundPage.fill('#prompt', `title obs round ${round}`);
      await roundPage.click('#send');
      await expect(roundPage.locator('.msg.assistant').last()).toBeVisible({ timeout: 30_000 });
      await roundPage.evaluate(() => refreshWorkbench());
      await expect.poll(() => roundPage.evaluate(() => state.sessionId), { timeout: 30_000 }).not.toBeNull();
      const sessionId = await roundPage.evaluate(() => state.sessionId);

      const longTitle = `第${round}轮这条标题用来验证窄视口下会话标题只做视觉省略而完整保存不会把残缺词句写回存储层`;
      roundPage.once('dialog', (dialog) => dialog.accept(longTitle));
      await roundPage.click('#session-title');
      await expect(roundPage.locator('#session-title')).toHaveText(longTitle, { timeout: 30_000 });

      const rowTitle = roundPage.locator(`[data-session-id="${sessionId}"] .session-copy strong`);
      await expect(rowTitle).toHaveText(longTitle, { timeout: 30_000 });
      // 改名后立即观测 + 双重绘稳定后观测（时序归因）。
      records.push(await observe(roundPage, sessionId, 'after-toHaveText'));
      await roundPage.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const stable = await observe(roundPage, sessionId, 'after-2raf');
      records.push(stable);
      // 原始几何契约断言（不加延时刷绿）：记录是否复现 0/0。
      if (stable.scrollWidth === 0 && stable.clientWidth === 0 && !reproduced) {
        reproduced = stable;
      }
    } finally {
      await context.close();
    }
  }
  console.log('TITLE-OBS-1 observation records:', JSON.stringify(records, null, 1));
  if (reproduced) {
    console.log('TITLE-OBS-1 REPRODUCED zero-width:', JSON.stringify(reproduced, null, 1));
  } else {
    console.log('TITLE-OBS-1 zero-width NOT reproduced in this run (bounded honest negative)');
  }
  // 诊断用例不做通过判定性断言（NOT_REPRODUCED 也是有效证据）；仅断言
  // 观测记录完整可归因。
  expect(records.length).toBe(16);
  for (const record of records) {
    expect(record.matchCount, 'exactly one row per session id').toBe(1);
    expect(record.text, 'stored title stays whole').toContain('存储层');
  }
});
