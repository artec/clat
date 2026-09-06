// clat web e2e — worklist PWA-4 验收①②③④。
// 宿主：global-setup 拉起的门控 cargo 测试（真 clat serve + TestProvider）。
const { test, expect } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');

function hostInfo(key) {
  const stateDir = process.env.CLAT_E2E_RUN_DIR || path.join(__dirname, '..');
  return JSON.parse(
    fs.readFileSync(path.join(stateDir, `.serve-${key}.json`), 'utf8'),
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

function currentRssBytes(pid) {
  const kib = Number.parseInt(
    execFileSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' }).trim(),
    10,
  );
  if (!Number.isFinite(kib)) throw new Error(`invalid RSS for pid ${pid}`);
  return kib * 1024;
}

async function measureRss(pid, action) {
  const idleRssBytes = currentRssBytes(pid);
  let peakRssBytes = idleRssBytes;
  const started = Date.now();
  const sampler = setInterval(() => {
    peakRssBytes = Math.max(peakRssBytes, currentRssBytes(pid));
  }, 50);
  try {
    await action();
    await new Promise((resolve) => setTimeout(resolve, 100));
    peakRssBytes = Math.max(peakRssBytes, currentRssBytes(pid));
  } finally {
    clearInterval(sampler);
  }
  return { idleRssBytes, peakRssBytes, elapsedMs: Date.now() - started };
}

function crc32(parts) {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = (value >>> 1) ^ ((value & 1) ? 0xedb88320 : 0);
    }
    table[index] = value >>> 0;
  }
  let crc = 0xffffffff;
  for (const part of parts) {
    for (const byte of part) crc = table[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const name = Buffer.from(type, 'ascii');
  const chunk = Buffer.allocUnsafe(data.length + 12);
  chunk.writeUInt32BE(data.length, 0);
  name.copy(chunk, 4);
  data.copy(chunk, 8);
  chunk.writeUInt32BE(crc32([name, data]), chunk.length - 4);
  return chunk;
}

function solidPng(width, height, rgb) {
  const rowBytes = 1 + (width * 3);
  const raw = Buffer.alloc(rowBytes * height);
  for (let y = 0; y < height; y += 1) {
    const row = y * rowBytes;
    raw[row] = 0;
    for (let x = 0; x < width; x += 1) {
      const offset = row + 1 + (x * 3);
      raw[offset] = rgb[0];
      raw[offset + 1] = rgb[1];
      raw[offset + 2] = rgb[2];
    }
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 2;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk('IHDR', header),
    pngChunk('IDAT', zlib.deflateSync(raw)),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

function makeLiveColorFixtures() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'clat-live-glm-pwa-'));
  const green = path.join(root, 'green.png');
  const yellow = path.join(root, 'yellow.png');
  fs.writeFileSync(green, solidPng(256, 256, [0, 210, 0]));
  fs.writeFileSync(yellow, solidPng(256, 256, [235, 225, 0]));
  return { root, green, yellow };
}

function makeNearLimitPngFixtures() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'clat-mm5-pwa-'));
  const source = fs.readFileSync(path.join(__dirname, '..', '..', 'icons', 'icon-512.png'));
  if (source.subarray(source.length - 8, source.length - 4).toString('ascii') !== 'IEND') {
    throw new Error('fixture source does not end in IEND');
  }
  const targetBytes = (8 * 1024 * 1024) - 4096;
  const payloadBytes = targetBytes - source.length - 12;
  const type = Buffer.from('rNDm', 'ascii'); // ancillary, private, reserved bit valid
  const paths = [];
  for (let imageIndex = 0; imageIndex < 4; imageIndex += 1) {
    const payload = Buffer.allocUnsafe(payloadBytes);
    let state = (0x9e3779b9 ^ imageIndex) >>> 0;
    for (let offset = 0; offset < payload.length; offset += 1) {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      payload[offset] = state & 0xff;
    }
    const chunk = Buffer.allocUnsafe(payload.length + 12);
    chunk.writeUInt32BE(payload.length, 0);
    type.copy(chunk, 4);
    payload.copy(chunk, 8);
    chunk.writeUInt32BE(crc32([type, payload]), chunk.length - 4);
    const output = Buffer.concat([source.subarray(0, source.length - 12), chunk, source.subarray(source.length - 12)]);
    const fixturePath = path.join(root, `near-limit-${imageIndex + 1}.png`);
    fs.writeFileSync(fixturePath, output);
    paths.push(fixturePath);
  }
  return { root, paths, rawBytes: targetBytes * paths.length };
}

async function dispatchImageFileGesture(page, selector, eventName, fixturePath) {
  const base64 = fs.readFileSync(fixturePath).toString('base64');
  await page.evaluate(({ targetSelector, type, bytes }) => {
    const binary = atob(bytes);
    const data = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) data[index] = binary.charCodeAt(index);
    const transfer = new DataTransfer();
    transfer.items.add(new File([data], 'gesture.png', { type: 'image/png' }));
    const event = type === 'paste'
      ? new ClipboardEvent(type, { bubbles: true, cancelable: true, clipboardData: transfer })
      : new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: transfer });
    document.querySelector(targetSelector).dispatchEvent(event);
  }, { targetSelector: selector, type: eventName, bytes: base64 });
}

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

test('settings expose the default-deny WeChat binding surface', async ({ page }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);

  await page.click('#settings-open');
  await expect(page.locator('#settings-dialog')).toBeVisible(LIVE);
  await expect(page.locator('.wechat-settings h3')).toHaveText('WeChat remote control');
  await expect(page.locator('#wechat-status')).toHaveText('Not bound', LIVE);
  await expect(page.locator('#wechat-counts')).toHaveText('0 paired · 0 chats');
  await expect(page.locator('#wechat-bind')).toHaveText('Bind WeChat');
  await expect(page.locator('#wechat-bind')).toBeEnabled();
  await expect(page.locator('#wechat-pair')).toBeDisabled();
  await expect(page.locator('#wechat-unbind')).toBeDisabled();
  await expect(page.locator('#wechat-qr')).toBeHidden();
  await expect(page.locator('#wechat-pairing')).toBeHidden();
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
  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
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

  // PU-9（附加工单 2026-09-06 负责人令）：assistant 标记换品牌像素机
  // 器人剪影——fill 剪影（evenodd 挖出面部），不走全局描边管线。
  const agentGlyph = page.locator('.msg.assistant .marker svg path').first();
  await expect(agentGlyph).toHaveAttribute('d', /^M6 4h12v4/);
  await expect(agentGlyph).toHaveAttribute('fill-rule', 'evenodd');
  await expect(agentGlyph).toHaveAttribute('stroke', 'none');
});

test('image-capable model is marked in the top bar and model route row', async ({ page }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);

  for (const selector of ['#header-model', '#detail-model']) {
    const identity = page.locator(selector);
    await expect(identity.locator('.vision-capability')).toHaveCount(1);
    await expect(identity.locator('.vision-capability')).toHaveAttribute('aria-label', 'Accepts images');
    await expect(identity.locator('svg')).toHaveCount(1);
    const colors = await identity.locator('.vision-capability').evaluate((badge) => ({
      color: getComputedStyle(badge).color,
      stroke: getComputedStyle(badge.querySelector('svg')).stroke,
    }));
    expect(colors.stroke).toBe(colors.color);
  }
});

// FE-1：斜杠桥仍只返回 core 事实；PWA 负责格式化 context，并让 Plan Mode
// 在普通 notice 退场后仍有持续、可撤销的视觉状态。
test('context is readable and the plan-mode marker appears and clears', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
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
    'Images:',
    'Images before projection:',
    'Older images omitted:',
    'Image bytes:',
    'Visual token estimate:',
    'Visual safety factor:',
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
  await expect(badge).toHaveText('Plan');
  await expect(badge).toHaveCSS('color', 'rgb(240, 189, 90)');

  // 另一条命令完成后仍常显，不是转录区里一闪而过的 status notice。
  await page.fill('#prompt', '/context');
  await page.click('#send');
  await expect(page.locator('.context-notice')).toHaveCount(2, LIVE);
  await expect(badge).toBeVisible();

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(badge).toBeVisible(LIVE);
  // The second no-argument /plan toggles off, including after a cold reload.
  await page.fill('#prompt', '/plan');
  await page.click('#send');
  await expect(badge).toBeHidden(LIVE);
});

test('PU content notices and armed Goal badge follow core workflow state', async ({ page }, testInfo) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await openWorkbench(page, hostInfo('run-command'));
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await page.click('#new-session');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
  await expect(page.locator('#plan-mode-badge')).toBeHidden();
  await expect(page.locator('#goal-badge')).toBeHidden();
  const command = async (text) => {
    await expect(page.locator('#send')).toBeEnabled(LIVE);
    await page.fill('#prompt', text);
    await page.click('#send');
    await expect(page.locator('#prompt')).toHaveValue('', LIVE);
  };
  await command('/mem add project PU first line\nPU second line');
  await command('/mem list');
  const memory = page.locator('.content-notice[aria-label="memory"]').last();
  await expect(memory).toContainText('PU first line\nPU second line');
  await expect(memory.locator('span')).toHaveCSS('white-space', 'pre-wrap');
  await expect(memory).toContainText('Source:');
  await command('/sub on');
  await command('/sub status');
  const sub = page.locator('.content-notice[aria-label="subagent status"]').last();
  await expect(sub).toContainText('enabled');
  await expect(sub).toContainText('/sub off');
  await expect(sub).toContainText('delegate_readonly');
  await command('/goal create inspect PU changes --rounds 1');
  await command('/goal');
  await expect(page.locator('.content-notice[aria-label="goal"]').last()).toContainText('inspect PU changes');
  await expect(page.locator('#goal-badge')).toBeHidden();

  // Materialize before entering durable Plan Mode; an ordinary run disarms.
  await command('materialize PU workflow session');
  await expect(page.locator('.approval-card').last()).toBeVisible(LIVE);
  await page.locator('.approval-card').last().locator('button.ghost').click();
  // run_completed updates the UI before core settlement releases the run.
  // Wait for prompt.settled, not the transient Idle presentation alone.
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await command('/plan on');
  await page.fill('#prompt', '/goal run');
  await page.click('#send');
  await expect(page.locator('#run-state')).toContainText('exit plan mode first', LIVE);
  await expect(page.locator('#plan-mode-badge')).toBeVisible();
  await expect(page.locator('#goal-badge')).toBeHidden();
  await command('/plan off');
  await command('/goal run');
  const goalBadge = page.locator('#goal-badge');
  await expect(goalBadge).toBeVisible(LIVE);
  await expect(goalBadge).toHaveText('Goal');
  await expect(goalBadge).toHaveCSS('color', 'rgb(92, 240, 125)');
  await expect(page.locator('#plan-mode-badge')).toBeHidden();
  await page.emulateMedia({ colorScheme: 'light' });
  await expect(goalBadge).toHaveCSS('color', 'rgb(33, 114, 59)');
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.screenshot({ path: testInfo.outputPath('pu-goal-workflow.png'), fullPage: true });
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(goalBadge).toBeVisible(LIVE);
  await page.click('#cancel');
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await expect(goalBadge).toBeHidden(LIVE);
  await command('/goal');
  const goal = page.locator('.content-notice[aria-label="goal"]').last();
  await expect(goal).toContainText('paused');
  await expect(goal).toContainText('disarmed');
  await page.click('#new-session');
  await expect(page.locator('#goal-badge')).toBeHidden();
});

// PU-5（F-1 返工）：权限 pill 是 composer 行的左锚点，徽标按
// 「权限 · Plan · Goal」镜像序向右递进（TUI 锚右，序恰相反）。
// 徽标激活时不得把 pill 锚点向右推：间距只能长在 margin-left。
test('permission pill stays the left anchor before the Plan and Goal badges', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  const order = await page.evaluate(() => {
    const ids = ['composer-permission', 'plan-mode-badge', 'goal-badge'];
    const nodes = ids.map((id) => document.getElementById(id));
    if (nodes.some((node) => node === null)) return null;
    const follows = (a, b) =>
      Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
    return {
      permissionBeforePlan: follows(nodes[0], nodes[1]),
      permissionBeforeGoal: follows(nodes[0], nodes[2]),
      planBeforeGoal: follows(nodes[1], nodes[2]),
    };
  });
  expect(order).toEqual({
    permissionBeforePlan: true,
    permissionBeforeGoal: true,
    planBeforeGoal: true,
  });
  for (const id of ['plan-mode-badge', 'goal-badge']) {
    const badge = page.locator(`#${id}`);
    await expect(badge).toHaveCSS('margin-left', '7px');
    await expect(badge).toHaveCSS('margin-right', '0px');
  }
});

// 附加工单（2026-09-06 负责人验收附加）：侧栏连接指示与插件/设置
// 图标同尺寸同列位——前身 7px 圆点既不对齐也不随状态变色。状态色沿用
// 页眉 conn-status 同一套 success/warning/danger 语义。
test('sidebar connection indicator matches footer icon geometry and follows conn state', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  const entry = hostInfo('success');
  const footnote = page.locator('#sidebar-footnote');
  const footnoteIcon = page.locator('#sidebar-footnote > svg');

  // 无 token：HTML 初始态 connecting，警告色图标已在 17px 网格上。
  await page.goto(`${entry.origin}/`);
  await expect(page.locator('#landing')).toBeVisible(LIVE);
  await expect(footnote).toHaveAttribute('data-state', 'connecting');
  await expect(footnoteIcon).toHaveCSS('width', '17px');
  await expect(footnoteIcon).toHaveCSS('color', 'rgb(240, 189, 90)');

  // 假 token：自动重连被 401 拒绝 → failed 态（真实断线路径）。
  await page.evaluate((token) => localStorage.setItem('clat.auth.v1', token), 'not-a-real-token');
  await page.reload();
  await expect(page.locator('#landing')).toBeVisible(LIVE);
  await expect(footnote).toHaveAttribute('data-state', 'failed', LIVE);
  await expect(footnoteIcon).toHaveCSS('color', 'rgb(255, 123, 112)');

  // 真 token：live 态 + 与上方两枚操作图标同宽同列、行高同网格。
  await page.fill('#connect-token', entry.token);
  await page.click('#connect-form button[type="submit"]');
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(footnote).toHaveAttribute('data-state', 'live');
  await expect(footnoteIcon).toHaveCSS('color', 'rgb(127, 219, 152)');
  // Solid inner dot + stroked outer ring: both track the state color.
  const [dotFill, ringStroke] = await footnoteIcon.evaluate((svg) => {
    const [dot, ring] = svg.querySelectorAll('circle');
    const dotStyle = getComputedStyle(dot);
    const ringStyle = getComputedStyle(ring);
    return [dotStyle.fill + ' ' + dotStyle.stroke, ringStyle.stroke + ' ' + ringStyle.fill];
  });
  expect(dotFill).toBe('rgb(127, 219, 152) none');
  expect(ringStroke).toBe('rgb(127, 219, 152) none');
  await expect(footnote).toHaveCSS('min-height', '36px');
  const [footBox, setBox, mktBox] = await Promise.all([
    footnoteIcon.boundingBox(),
    page.locator('#settings-open > svg').boundingBox(),
    page.locator('#market-open > svg').boundingBox(),
  ]);
  // The settings icon is the unbordered baseline column; the market entry's
  // own 1px border shifts its glyph right by a pixel by design.
  expect(footBox.x).toBeCloseTo(setBox.x, 1);
  expect(Math.abs(footBox.x - mktBox.x)).toBeLessThanOrEqual(1);
  expect(footBox.width).toBeCloseTo(setBox.width, 1);
  expect(footBox.width).toBeCloseTo(mktBox.width, 1);
});

// PU-7（附加工单 2026-09-06 负责人验收附加）：收起态侧栏是单一图标
// 网格——logo 与展开钮对轨心，新建会话/会话块/底部按钮同列同宽，
// 会话字块在块内双向居中且与新建按钮同高。
test('collapsed rail centers the brand and toggle and unifies the icon grid', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  // Seed one real session so the tile row exists before collapsing.
  // The success host completes deterministically without a tool approval.
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'collapsed rail seed');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(page.locator('.session-item').first()).toBeVisible(LIVE);

  await page.click('#sidebar-toggle');
  const probe = () => page.evaluate(() => {
    const box = (selector) => {
      const node = document.querySelector(selector);
      if (!node) throw new Error(`missing node: ${selector}`);
      const rect = node.getBoundingClientRect();
      return {
        left: rect.left,
        width: rect.width,
        height: rect.height,
        cx: rect.left + rect.width / 2,
        cy: rect.top + rect.height / 2,
      };
    };
    return {
      rail: box('#sidebar'),
      head: box('.sidebar-head'),
      brand: box('.brand-icon'),
      toggle: box('#sidebar-toggle'),
      fresh: box('#new-session'),
      tile: box('.session-item'),
      glyph: box('.session-glyph'),
      settings: box('#settings-open'),
    };
  });
  // The rail resizes over a 180ms grid transition; retry until it settles.
  // Every icon shares one vertical axis (±1px absorbs the rail's 1px right
  // border shifting the content-box center by half a pixel). The toggle's
  // inner svg is measured too: UA button padding + the 180° rotation once
  // threw the glyph 5px off the shared axis while the button box stayed put.
  // The toggle also straddles the header's bottom border: its vertical
  // center sits on the divider line (box bottom minus the 1px border half).
  await expect(async () => {
    const g = await probe();
    const glyph = await page.locator('#sidebar-toggle > svg').boundingBox();
    const axis = g.brand.cx;
    const dividerY = g.head.cy + g.head.height / 2 - 0.5;
    const near = (a, b) => expect(Math.abs(a - b)).toBeLessThanOrEqual(1);
    near(axis, g.rail.left + g.rail.width / 2);
    near(g.toggle.cx, axis);
    near(glyph.x + glyph.width / 2, axis);
    near(g.toggle.cy, dividerY);
    near(g.fresh.cx, axis);
    near(g.tile.cx, axis);
    near(g.settings.cx, axis);
    expect(Math.abs(g.tile.width - g.fresh.width)).toBeLessThanOrEqual(1);
    expect(Math.abs(g.tile.width - g.settings.width)).toBeLessThanOrEqual(1);
    expect(Math.abs(g.tile.height - g.fresh.height)).toBeLessThanOrEqual(1);
    near(g.glyph.cx, g.tile.cx);
    near(g.glyph.cy, g.tile.cy);
  }).toPass();
});

// PU-8（附加工单 2026-09-06 负责人验收附加）：Full Access 与 TUI 同语——
// 权限文字（页眉徽标 + 输入框上方 pill）转警示黄；其余模式保持默认色。
test('full access paints the permission text warning yellow', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await openWorkbench(page, hostInfo('success'));
  const pill = page.locator('#composer-permission');
  const badge = page.locator('#header-permission');

  await page.click('#settings-open');
  await expect(page.locator('#settings-dialog')).toBeVisible(LIVE);
  // The dialog is a live view: an async workbench refresh re-renders the
  // radios from host state and can land between our clicks. Retry the whole
  // select→confirm→save unit until the save actually lands.
  const grantFullAccess = async () => {
    await page.click('#permission-options label:has(input[value="workspace-write"])', { timeout: 3000 });
    await page.click('#permission-options label:has(input[value="danger-full-access"])', { timeout: 3000 });
    await page.check('#full-access-confirm', { timeout: 3000 });
    await page.click('#permission-save', { timeout: 3000 });
  };
  await expect(grantFullAccess).toPass({ timeout: 30_000 });
  await expect(page.locator('#settings-saved')).toHaveText('Permission mode updated.', LIVE);
  await expect(pill).toHaveCSS('color', 'rgb(205, 205, 0)', LIVE);
  await expect(badge).toHaveCSS('color', 'rgb(205, 205, 0)', LIVE);

  // Restore the host's default mode; colors follow the state, not the dialog.
  const restoreWrite = async () => {
    await page.click('#permission-options label:has(input[value="workspace-write"])', { timeout: 3000 });
    await page.click('#permission-save', { timeout: 3000 });
  };
  await expect(restoreWrite).toPass({ timeout: 30_000 });
  await expect(page.locator('#settings-saved')).toHaveText('Permission mode updated.', LIVE);
  await expect(pill).toHaveCSS('color', 'rgb(116, 128, 127)', LIVE);
  await expect(badge).toHaveCSS('color', 'rgb(174, 183, 182)', LIVE);
});

// Manual history compaction is a first-class PWA surface, not a slash-command
// dead end. It must lock conflicting UI, persist the replacement family, and
// remain visible after a cold browser projection rebuild.
test('history compaction completes, cold-replays, and allows the next run', async ({ page }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await page.fill('#prompt', 'history seed 0');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);

  for (let turn = 1; turn < 5; turn += 1) {
    await page.fill('#prompt', `history seed ${turn}`);
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(turn + 1, LIVE);
    await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  }

  const compact = page.locator('#compact-session');
  await expect(compact).toBeEnabled(LIVE);
  await compact.click();
  await expect(
    page.locator('.notice-line.trace-event', { hasText: 'History compacted' }).last(),
  ).toBeVisible(LIVE);
  await expect(compact).toHaveText('Compact history', LIVE);
  await expect(compact).toBeEnabled(LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(
    page.locator('.notice-line.trace-event', { hasText: 'History compacted' }).last(),
  ).toBeVisible(LIVE);
  await expect(compact).toBeEnabled(LIVE);

  await page.fill('#prompt', 'continue after compact');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
});

test('active history compaction survives refresh and remains cancellable', async ({ page }) => {
  const entry = hostInfo('compact-slow');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  for (let turn = 0; turn < 5; turn += 1) {
    await page.fill('#prompt', `cancellable history ${turn}`);
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(turn + 1, LIVE);
    await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  }

  const compact = page.locator('#compact-session');
  await compact.click();
  await expect(compact).toHaveText('Cancel compaction', LIVE);
  await expect(page.locator('#prompt')).toBeDisabled(LIVE);
  await expect(page.locator('#new-session')).toBeDisabled(LIVE);

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(compact).toHaveText('Cancel compaction', LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Compacting', LIVE);
  await compact.click();

  await expect(compact).toHaveText('Compact history', LIVE);
  await expect(compact).toBeEnabled(LIVE);
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  await expect(page.locator('.notice-line', { hasText: 'compaction' }).last()).toBeVisible(LIVE);
  await expect(page.locator('.notice-line.trace-event', { hasText: 'History compacted' })).toHaveCount(0);

  await page.fill('#prompt', 'continue after cancelled compaction');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
});

// MM-4：浏览器文件不以路径进入 RPC；先上传到 server-minted draft scope，
// prompt 只携带 opaque upload id。回放/实时消息再经受 Bearer 保护的
// attachment endpoint 取回 blob URL，页面不把 token 塞进图片 URL。
test('image draft stages, sends image-only, and rebuilds a protected history preview', async ({ page }) => {
  const entry = hostInfo('run-command');
  const attachmentRequests = [];
  page.on('request', (request) => {
    if (request.url().includes('/api/attachments/')) attachmentRequests.push(request);
  });
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await expect(page.locator('#attachment-summary')).toContainText('ready', LIVE);

  await page.click('#send');
  const preview = page.locator('.msg.user .message-attachment-preview').last();
  await expect(preview).toBeVisible(LIVE);
  await expect(preview).toHaveAttribute('src', /^blob:/, LIVE);
  await preview.click();
  await expect(page.locator('#image-lightbox')).toBeVisible(LIVE);
  await expect(page.locator('#image-lightbox .image-lightbox-image')).toHaveAttribute('src', /^blob:/, LIVE);
  await page.locator('#image-lightbox .image-lightbox-close').click();
  await expect(page.locator('#image-lightbox')).toBeHidden(LIVE);
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  expect(attachmentRequests).not.toHaveLength(0);
  expect(attachmentRequests[0].headers().authorization).toBe(`Bearer ${entry.token}`);
  expect(attachmentRequests[0].url()).not.toContain(entry.token);

  // 服务端 TestProvider 会请求 run_command；拒绝即可收束本用例，不让
  // 图片 UI 验收依赖副作用获批。
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  const replayPreview = page.locator('.msg.user .message-attachment-preview').last();
  await expect(replayPreview).toHaveAttribute('src', /^blob:/, LIVE);
});

test('active run accepts an image steering draft without exposing a host path', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'hold at an approval boundary');
  await page.click('#send');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.fill('#prompt', 'use this screenshot for the next step');
  await page.click('#send');
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('steering queued · held for recovery', LIVE);
  await expect(page.locator('.notice-line', { hasText: 'steering queued' })).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  await expect(page.locator('.msg.user .message-attachment-preview').last()).toHaveAttribute('src', /^blob:/, LIVE);
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('durable image steering claim can beat its RPC acknowledgement without restoring the draft', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'hold at an approval boundary for an early claim');
  await page.click('#send');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);

  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.fill('#prompt', 'claim this image before the HTTP acknowledgement');

  let markSteerProcessed;
  const steerProcessed = new Promise((resolve) => { markSteerProcessed = resolve; });
  let releaseSteerResponse;
  const steerResponseReleased = new Promise((resolve) => { releaseSteerResponse = resolve; });
  await page.route('**/api/steer.send', async (route) => {
    const response = await route.fetch();
    markSteerProcessed();
    await steerResponseReleased;
    await route.fulfill({ response });
  });

  await page.click('#send');
  await steerProcessed;
  try {
    // Release the approval while the successful steer.send response remains
    // withheld. The next model turn claims and durably emits the steering SSE
    // first; pre-fix, settleQueuedDraft ignored that event because the local
    // state had not yet advanced from sending → queued.
    await approval.locator('button.ghost').click();
    await expect(page.locator('.msg.user .message-attachment-preview').last()).toHaveAttribute('src', /^blob:/, LIVE);
    await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  } finally {
    releaseSteerResponse();
  }
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
  await expect(page.locator('.attachment-chip')).toHaveCount(0);
});

test('cancelled unclaimed image steering restores the draft for a normal retry', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await page.fill('#prompt', 'hold at an approval boundary');
  await page.click('#send');
  await expect(page.locator('.approval-card').first()).toBeVisible(LIVE);

  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.fill('#prompt', 'keep this image for the next run');
  await page.click('#send');
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('steering queued · held for recovery', LIVE);

  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await expect(page.locator('#attachment-summary')).toContainText('restored', LIVE);

  // 输入框已在 steering 入队时清空；这里以图片-only prompt 验证恢复的
  // opaque upload 可直接复用，而不是要求浏览器重新读本地文件。
  await page.click('#send');
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  await expect(page.locator('.msg.user .message-attachment-preview').last()).toHaveAttribute('src', /^blob:/, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('multiple image draft preserves ordering through image-only admission', async ({ page }) => {
  const entry = hostInfo('run-command');
  const image = path.join(__dirname, '..', '..', 'icons', 'icon-192.png');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await page.setInputFiles('#attachment-input', [image, image]);
  await expect(page.locator('.attachment-chip')).toHaveCount(2, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText(['staged locally', 'staged locally'], LIVE);
  await page.locator('.attachment-chip').nth(1).locator('button[title="Move image earlier"]').click();
  await expect(page.locator('.attachment-index')).toHaveText(['01', '02'], LIVE);

  await page.click('#send');
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  const user = page.locator('.msg.user').last();
  await expect(user.locator('.message-attachment-preview')).toHaveCount(2, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('drop and clipboard image paste enter the same staged draft pipeline', async ({ page }) => {
  const entry = hostInfo('run-command');
  const image = path.join(__dirname, '..', '..', 'icons', 'icon-192.png');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await dispatchImageFileGesture(page, '#composer-shell', 'drop', image);
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.locator('.attachment-remove').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);

  await dispatchImageFileGesture(page, '#prompt', 'paste', image);
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.click('#send');
  await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(1, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('failed image staging keeps the draft and retry reuses the original file', async ({ page }) => {
  const entry = hostInfo('run-command');
  let rejectOnce = true;
  await page.route('**/api/drafts/**/images', async (route) => {
    if (rejectOnce) {
      rejectOnce = false;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ ok: false, error: { message: 'temporary staging outage' } }),
      });
    } else {
      await route.continue();
    }
  });
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toContainText('not staged', LIVE);
  await expect(page.locator('#attachment-summary')).toContainText('need attention', LIVE);

  await page.locator('button.attachment-retry').click();
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.click('#send');
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('switching sessions revokes the local image draft instead of carrying it across', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);

  const previous = page.locator('#session-list li:not(.active) .session-item').first();
  await expect(previous).toBeVisible(LIVE);
  await previous.click();
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  await expect(page.locator('#attachment-rail')).toBeHidden(LIVE);
});

// 对抗式性能腿：长流正在持续写入转录区时，浏览器仍须能完成本地预览、
// draft scope RPC 与 raw upload；上传不能被 O(n²) 文本渲染饿死。
test('image staging stays responsive while a long stream is updating the transcript', async ({ page }) => {
  const entry = hostInfo('long-stream');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'long stream with an image draft');
  await page.click('#send');
  await page.waitForFunction(
    () => {
      const body = document.querySelector('.msg.assistant .body');
      return body && body.textContent.length > 4_000;
    },
    null,
    LIVE,
  );

  const before = await page.locator('.msg.assistant .body').textContent();
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-512.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', { timeout: 5_000 });
  await page.waitForFunction(
    (previousLength) => {
      const body = document.querySelector('.msg.assistant .body');
      return body && body.textContent.length > previousLength;
    },
    (before || '').length,
    { timeout: 5_000 },
  );

  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
});

// MM-5 手工性能腿：真实 Chromium → PWA → streaming upload → core
// admission，再经 F5/SSE 重连恢复四张受保护历史图。默认跳过，避免把
// 近 32 MiB fixture 与高频 RSS 采样加入普通前端回归。
test('MM-5 PWA near-limit upload and reconnect RSS profile', async ({ page }) => {
  test.skip(process.env.CLAT_MM5_PERF !== '1', 'set CLAT_MM5_PERF=1 for the manual RSS profile');
  test.setTimeout(300_000);
  const entry = hostInfo('run-command');
  const fixtures = makeNearLimitPngFixtures();
  try {
    await openWorkbench(page, entry);
    await page.click('#new-session');
    await expect(page.locator('#send')).toBeEnabled(LIVE);

    const upload = await measureRss(entry.pid, async () => {
      await page.setInputFiles('#attachment-input', fixtures.paths);
      await expect(page.locator('.attachment-chip')).toHaveCount(4, { timeout: 120_000 });
      await expect(page.locator('.attachment-chip .attachment-state')).toHaveText(
        ['staged locally', 'staged locally', 'staged locally', 'staged locally'],
        { timeout: 120_000 },
      );
    });

    await page.click('#send');
    await expect(page.locator('.attachment-chip')).toHaveCount(0, { timeout: 120_000 });
    await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(4, LIVE);
    const approval = page.locator('.approval-card').last();
    await expect(approval).toBeVisible(LIVE);
    await approval.locator('button.ghost').click();
    await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });

    const reconnect = await measureRss(entry.pid, async () => {
      await page.reload();
      await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
      await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(4, {
        timeout: 60_000,
      });
      for (const preview of await page.locator('.msg.user').last().locator('.message-attachment-preview').all()) {
        await expect(preview).toHaveAttribute('src', /^blob:/, LIVE);
      }
    });

    console.log(`MM5_PWA_PERF ${JSON.stringify({
      profile: process.env.CLAT_E2E_RELEASE === '1' ? 'release' : 'test',
      images: fixtures.paths.length,
      rawBytes: fixtures.rawBytes,
      upload,
      reconnect,
    })}`);
  } finally {
    fs.rmSync(fixtures.root, { recursive: true, force: true });
  }
});

// MM-5 paid product campaign: real Chromium/PWA, real CLAT Application and
// real GLM provider. It remains default-off and requires a process-local key.
test('MM-5 live GLM PWA multi-image, image-only history, and replay', async ({ page }) => {
  test.skip(process.env.CLAT_LIVE_GLM_E2E !== '1', 'set CLAT_LIVE_GLM_E2E=1 and provide the live key');
  test.setTimeout(300_000);
  const entry = hostInfo('live-glm');
  const fixtures = makeLiveColorFixtures();
  try {
    await openWorkbench(page, entry);
    await page.click('#new-session');
    await expect(page.locator('#send')).toBeEnabled(LIVE);

    await page.setInputFiles('#attachment-input', [fixtures.green, fixtures.yellow]);
    await expect(page.locator('.attachment-chip .attachment-state')).toHaveText(
      ['staged locally', 'staged locally'],
      LIVE,
    );
    await page.fill('#prompt', "Two solid-color images are attached in order. Reply exactly '1=green;2=yellow' and nothing else.");
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(1, { timeout: 180_000 });
    const orderedReply = (await page.locator('.msg.assistant .body').last().textContent() || '').toLowerCase();
    expect(orderedReply).toMatch(/1\s*=\s*green/);
    expect(orderedReply).toMatch(/2\s*=\s*yellow/);
    await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(2, LIVE);

    await page.reload();
    await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
    await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
    await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(2, LIVE);
    const replayedReply = (await page.locator('.msg.assistant .body').last().textContent() || '').toLowerCase();
    expect(replayedReply).toContain('green');
    expect(replayedReply).toContain('yellow');

    await page.setInputFiles('#attachment-input', fixtures.green);
    await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
    await page.click('#send');
    await expect(page.locator('.verdict.completed, .verdict.failed')).toHaveCount(1, {
      timeout: 180_000,
    });
    await expect(page.locator('.verdict.completed')).toHaveCount(1);
    await expect(page.locator('#send')).toBeEnabled(LIVE);

    await page.fill(
      '#prompt',
      'What solid color filled the image in my immediately previous image-only message? Reply exactly HISTORY_OK_GREEN and nothing else.',
    );
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(2, { timeout: 180_000 });
    await expect(page.locator('.msg.assistant .body').last()).toContainText('HISTORY_OK_GREEN', {
      timeout: 180_000,
    });
  } finally {
    fs.rmSync(fixtures.root, { recursive: true, force: true });
  }
});
