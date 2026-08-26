// clat web e2e — worklist PWA-4 验收①②③④。
// 宿主：global-setup 拉起的门控 cargo 测试（真 clat serve + TestProvider）。
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');

function hostInfo(key) {
  return JSON.parse(
    fs.readFileSync(path.join(__dirname, '..', `.serve-${key}.json`), 'utf8'),
  );
}

async function openWorkbench(page, entry) {
  await page.goto(`${entry.origin}/`);
  await expect(page).toHaveURL(`${entry.origin}/`);
  await expect(page.locator('#landing')).toBeVisible(LIVE);
  await page.fill('#connect-token', entry.token);
  await page.click('#connect-form button[type="submit"]');
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
}

const LIVE = { timeout: 30_000 };

// —— 验收①：一轮真实 run（流式 → 工具卡 → 审批 Allow → 终审）——
test.describe('acceptance ① approval + run lifecycle', () => {
  test('prompt → approval allow → tool executes → settled completed', async ({ page }) => {
    const entry = hostInfo('run-command');
    await openWorkbench(page, entry);

    await page.fill('#prompt', 'run echo please');
    await page.click('#send');
    await expect(page.locator('.msg.user .body')).toHaveText('run echo please', LIVE);

    const card = page.locator('.approval-card').first();
    await expect(card).toBeVisible(LIVE);
    await expect(card.locator('.title')).toContainText('run_command', LIVE);
    await card.locator('button.primary').click(); // Allow

    await expect(
      page.locator('.tool-card', { hasText: 'run_command' }),
    ).toBeVisible(LIVE);
    await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  });

  // —— 验收①的 Deny 腿 + 验收②：会话管理（新会话 → 拒绝 → 侧栏/重命名）
  test('fresh session → approval deny → no tool → sessions + rename', async ({ page }) => {
    const entry = hostInfo('run-command');
    await openWorkbench(page, entry);

    // 新会话（上一条测试的会话已含 run_command 结果——模型不再请求
    // 工具；新会话保证审批确定性）。等待重建完成的确定性屏障：历史
    // user 消息被清（重放骨架清空）+ composer 解锁（切换防抖）——
    // 点击不等待异步处理器，直接发 prompt 会与 session.new 跨连接
    // 竞态（e2e 实锤：prompt.send 先落地 → run 活跃 → new 被拒）。
    await page.click('#new-session');
    await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
    await expect(page.locator('#send')).toBeEnabled(LIVE);
    await page.fill('#prompt', 'try a command');
    await page.click('#send');

    const card = page.locator('.approval-card').first();
    await expect(card).toBeVisible(LIVE);
    await card.locator('button.ghost').click(); // Deny

    await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
    // 被拒调用「从不执行」：只有模型请求卡（1 张），没有执行结果卡
    //（tool_finished 才带结果体）。
    await expect(page.locator('.tool-card')).toHaveCount(1);

    // 侧栏两个会话（同一 journal 事实源——验收②）。
    await expect(page.locator('#session-list li')).toHaveCount(2, LIVE);

    // 重命名（对话窗 handle prompt 对话框）。
    page.once('dialog', (dialog) => dialog.accept('renamed by e2e'));
    await page.click('#session-title');
    await expect(page.locator('#session-title')).toHaveText('renamed by e2e', LIVE);
  });
});

// —— 验收③：run 进行中 F5 → 视图完整恢复、流式继续（INV-W3）——
test('refresh mid-run rebuilds the view and streaming continues', async ({ page }) => {
  const entry = hostInfo('long-stream');
  await openWorkbench(page, entry);

  await page.fill('#prompt', 'long stream');
  await page.click('#send');
  await page.waitForFunction(
    () => {
      const bodies = document.querySelectorAll('.msg.assistant .body');
      return bodies.length > 0 && bodies[bodies.length - 1].textContent.length > 4_000;
    },
    null,
    LIVE,
  );

  // F5：视图只从重放 + 活流重建（不依赖任何本地会话状态）。
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('.msg.user .body')).toHaveText('long stream', LIVE);
  await expect(page.locator('.msg.assistant .body').first()).toBeVisible(LIVE);

  // 流式继续：重建后的正文仍在增长，最终 settled。
  await page.waitForFunction(
    () => {
      const bodies = document.querySelectorAll('.msg.assistant .body');
      return bodies.length > 0 && bodies[bodies.length - 1].textContent.length > 8_000;
    },
    null,
    LIVE,
  );
  await expect(page.locator('.verdict.completed')).toBeVisible({
    timeout: 60_000,
  });
});

// —— 回归：应用外壳高度约束——转录区在视口内滚动、composer 永不出屏 ——
// 2026-08-24 bug：.app 网格只定义列未定义行，唯一隐式行按内容 auto 计高；
// 转录一长整个会话列被撑出视口——转录无法滚动、composer（输入框）被推
// 出屏。修复：.app { grid-template-rows: minmax(0, 1fr) } 把行钉在容器高。
test('app shell stays bounded under long content', async ({ page }) => {
  const entry = hostInfo('long-stream');
  await openWorkbench(page, entry);

  await page.fill('#prompt', 'long stream');
  await page.click('#send');
  await page.waitForFunction(
    () => {
      const bodies = document.querySelectorAll('.msg.assistant .body');
      return bodies.length > 0 && bodies[bodies.length - 1].textContent.length > 4_000;
    },
    null,
    LIVE,
  );

  const shell = await page.evaluate(() => {
    const sc = document.querySelector('.transcript-scroll');
    const composer = document.querySelector('.composer').getBoundingClientRect();
    return {
      transcriptClient: sc.clientHeight,
      transcriptScroll: sc.scrollHeight,
      composerBottom: composer.bottom,
      viewport: window.innerHeight,
    };
  });
  expect(shell.transcriptClient).toBeLessThan(shell.viewport);
  expect(shell.transcriptScroll).toBeGreaterThan(shell.transcriptClient);
  expect(shell.composerBottom).toBeLessThanOrEqual(shell.viewport);
});

// —— 验收④：双标签页——同 run 双观察；首答即赢；次答者见 not-pending ——
test('dual tabs observe the same run; first answer wins', async ({ browser }) => {
  const entry = hostInfo('run-command');
  const tabA = await browser.newPage();
  const tabB = await browser.newPage();
  await openWorkbench(tabA, entry);
  await openWorkbench(tabB, entry);

  // 新会话（宿主历史里已有 run_command 结果，保证审批触发）；
  // 等待重建完成（同上：切换防抖屏障）。
  await tabA.click('#new-session');
  await expect(tabA.locator('.msg.user')).toHaveCount(0, LIVE);
  await expect(tabA.locator('#send')).toBeEnabled(LIVE);
  await tabA.fill('#prompt', 'dual tab run');
  await tabA.click('#send');

  const cardA = tabA.locator('.approval-card').first();
  const cardB = tabB.locator('.approval-card').first();
  await expect(cardA).toBeVisible(LIVE);
  await expect(cardB).toBeVisible(LIVE); // 双观察：两端都收到审批卡

  await cardA.locator('button.primary').click(); // A 先答
  await expect(tabA.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(tabB.locator('.verdict.completed')).toBeVisible(LIVE); // B 收敛

  await cardB.locator('button.primary').click(); // B 迟到应答 → not-pending
  await expect(cardB.locator('.note.resolution')).toHaveText('already answered elsewhere', LIVE);

  await tabA.close();
  await tabB.close();
});

// —— Phase 4：公开市场只读投影；跨源请求绝不携带本地 Bearer token。——
test('plugin index panel is searchable, SVG-led, and never leaks the local token', async ({ page }) => {
  const entry = hostInfo('run-command');
  let catalogRequest;
  await page.route('https://pi.at.cn/catalog.json', async (route) => {
    catalogRequest = route.request();
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        schemaVersion: 1,
        market: { name: 'CLAT Plugin Index' },
        packages: [
          {
            id: 'dev.clat.digest',
            name: 'Digest Lab',
            runtime: 'wasm-component',
            status: 'preview',
            summary: 'Deterministic local digests.',
            tags: ['WASM', 'Rust'],
          },
          {
            id: 'cn.at.clat.dsh-port',
            name: 'DSH Porting Bridge',
            runtime: 'mcp-stdio',
            status: 'preview',
            summary: 'Cordis compatibility bridge.',
            tags: ['DSH', 'MCP'],
          },
        ],
      }),
    });
  });
  await openWorkbench(page, entry);
  await page.click('#market-open');
  await expect(page.locator('#market-dialog')).toBeVisible(LIVE);
  await expect(page.locator('.market-item')).toHaveCount(2);
  expect(catalogRequest).toBeTruthy();
  expect(catalogRequest.headers().authorization).toBeUndefined();
  expect(catalogRequest.headers().cookie).toBeUndefined();
  await page.fill('#market-search', 'DSH');
  await expect(page.locator('.market-item')).toHaveCount(1);
  await expect(page.locator('.market-item h3')).toHaveText('DSH Porting Bridge');
  await expect(page.locator('#market-dialog svg')).not.toHaveCount(0);
});

// 模型协议 ID 保留在 title 供诊断，视觉层显示可理解的事件名称。
test('model trace renders human-readable event names instead of raw protocol ids', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'trace labels');
  await page.click('#send');
  const trace = page.locator('.trace-event', { hasText: 'Model request started' }).first();
  await expect(trace).toBeVisible(LIVE);
  await expect(trace).toHaveAttribute('title', 'Event ID: model_requested');
  await expect(trace).not.toContainText('model_requested');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
});

// FE-1：斜杠桥仍只返回 core 事实；PWA 负责格式化 context，并让 Plan Mode
// 在普通 notice 退场后仍有持续、可撤销的视觉状态。
test('context is readable and the plan-mode marker appears and clears', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);

  // /context 可在 Fresh 状态读取，但 durable Plan Mode 需要已物化会话。
  // 新建后跑一轮并拒绝 Execute，既得到真实 session，也不产生命令副作用。
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await page.click('#new-session');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'materialize a session for plan mode');
  await page.click('#send');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(page.locator('#detail-session')).not.toHaveText('Fresh', LIVE);

  await page.fill('#prompt', '/context');
  await page.click('#send');
  const context = page.locator('.context-notice').last();
  await expect(context).toBeVisible(LIVE);
  await expect(context).toContainText('Context estimate · tokens');
  for (const label of [
    'Base prompt:',
    'Project instructions:',
    'Plan policy:',
    'Skill catalog:',
    'Goal policy:',
    'Memory injection:',
    'Tool schemas:',
    'History / compaction view:',
    'Output reserve:',
    'Input estimate:',
    'Total estimate:',
  ]) {
    await expect(context).toContainText(label);
  }
  await expect(context).toContainText(/Goal policy: \d+ tokens · (injected|not injected)/);
  await expect(context).toContainText(/Memory injection: \d+ \/ \d+ bytes · (injected|not injected)/);
  await expect(context).not.toContainText('{"estimator"');

  await page.fill('#prompt', '/plan');
  await page.click('#send');
  const badge = page.locator('#plan-mode-badge');
  await expect(badge).toBeVisible(LIVE);
  await expect(badge).toHaveText('Plan mode');

  // 另一条命令完成后仍常显，不是转录区里一闪而过的 status notice。
  await page.fill('#prompt', '/context');
  await page.click('#send');
  await expect(page.locator('.context-notice')).toHaveCount(2, LIVE);
  await expect(badge).toBeVisible();

  await page.fill('#prompt', '/plan off');
  await page.click('#send');
  await expect(badge).toBeHidden(LIVE);
});
