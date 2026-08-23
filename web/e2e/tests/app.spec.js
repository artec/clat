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

function openUrl(entry) {
  return `${entry.origin}/?t=${entry.token}`;
}

const LIVE = { timeout: 30_000 };

// —— 验收①：一轮真实 run（流式 → 工具卡 → 审批 Allow → 终审）——
test.describe('acceptance ① approval + run lifecycle', () => {
  test('prompt → approval allow → tool executes → settled completed', async ({ page }) => {
    const entry = hostInfo('run-command');
    await page.goto(openUrl(entry));
    await expect(page.locator('#conn-status')).toHaveText('live', LIVE);

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
    await page.goto(openUrl(entry));
    await expect(page.locator('#conn-status')).toHaveText('live', LIVE);

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
  await page.goto(openUrl(entry));
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);

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

// —— 验收④：双标签页——同 run 双观察；首答即赢；次答者见 not-pending ——
test('dual tabs observe the same run; first answer wins', async ({ browser }) => {
  const entry = hostInfo('run-command');
  const tabA = await browser.newPage();
  const tabB = await browser.newPage();
  await tabA.goto(openUrl(entry));
  await tabB.goto(openUrl(entry));
  await expect(tabA.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(tabB.locator('#conn-status')).toHaveText('live', LIVE);

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
