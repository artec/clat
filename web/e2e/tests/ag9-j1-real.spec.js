// AG-DOGFOOD-9 / J1-REAL（R1 返工版）：真 TUI App（PTY 中的 clat 二进制）
// + 真 Chromium PWA 连同一个真 clat serve 进程与同一 Session 的联合验收。
//
// R1 返工要点（docs/audit/2026-10-18-ag-dogfood9-pickerhit1-j1real-review-r1.md
// + docs/todo/ag-dogfood9-pickerhit1-j1real-rework-hints-2026-10-18.md，
// 归属 CLAT_WITH_HINTS）：
// - 草稿/召回断言一律用**权威事实**（真实提交后的 user 消息内容、假模型
//   实际收到的 user 文本、PWA composer 值/宿主 inputHistory），不再把
//   ratatui 增量流剥控制序列当"当前屏幕"（F1 假阴性）；屏幕流仅用于
//   **存在性** waitFor（出现即真）。
// - 两控制校准：正常组与"同 Session 远端 Run 后"组，各用独立 Session
//   与唯一草稿标记，以真实 Enter 提交核对两处权威内容。
// - 两端补 Up/Down 草稿恢复完整序列（原草稿→Up(beta)→Up(alpha)→
//   Down(beta)→Down(原草稿)），TUI 以最终真实提交确认恢复值。
// - 负向拆分小用例：空白提交拒绝不入史（真实拒绝路径）、建议浮层
//   方向键归属、切 Session 防串史（旧 Session 独有文本不泄漏）。
// - IME：NOT_RUN（PTY 无 IME 注入层，如实声明）。
const { test, expect } = require('@playwright/test');
const { spawn } = require('child_process');
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');

const BIN = path.join(__dirname, '..', '..', '..', 'target', 'debug', 'clat');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------
// 假模型：OpenAI 兼容。分支：无 "orderlive" 快回 "acknowledged"；含
// "orderlive" 走 write_file×2 → read_file → 终文多步因果链；标题请求短回；
// 建议请求（prompt.suggest）回固定建议（R1 返工新增）。
// ---------------------------------------------------------------------------
function fakeModelServer(log) {
  const sse = (res, chunks) => {
    res.writeHead(200, { 'Content-Type': 'text/event-stream' });
    for (const [delta, finish] of chunks) {
      const choice = { index: 0, delta, finish_reason: finish ?? null };
      res.write(`data: ${JSON.stringify({ id: 'j1', object: 'chat.completion.chunk', created: 1, model: 'j1-fake', choices: [choice] })}\n\n`);
    }
    res.write('data: [DONE]\n\n');
    res.end();
  };
  const toolDelta = (index, id, name, args) => ({
    tool_calls: [{ index, id, type: 'function', function: { name, arguments: JSON.stringify(args) } }],
  });
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', async () => {
      let payload = {};
      try { payload = JSON.parse(body || '{}'); } catch (e) { /* ignore */ }
      const messages = payload.messages || [];
      const instructions = messages.find((m) => m.role === 'system')?.content
        || payload.instructions || '';
      const lastUser = [...messages].reverse().find((m) => m.role === 'user');
      const userText = typeof lastUser?.content === 'string' ? lastUser.content : '';
      const lastUserIdx = messages.map((m) => m.role).lastIndexOf('user');
      const toolResults = lastUserIdx >= 0 ? messages.slice(lastUserIdx).filter((m) => m.role === 'tool').length : 0;
      log.push({ user: userText, toolResults });
      const stream = payload.stream !== false;
      const plain = (text) => (stream
        ? sse(res, [[{ content: text }], [{}, 'stop']])
        : res.end(JSON.stringify({ choices: [{ message: { content: text }, finish_reason: 'stop' }] })));

      const ins = String(instructions);
      if (ins.startsWith('Generate a concise title')) { return plain('j1 real joint session'); }
      if (ins.startsWith('Predict one useful next user message')) { return plain('suggested-next-42'); }
      if (!userText.includes('orderlive')) { return plain('acknowledged'); }
      // orderlive 多步：步间延迟拉宽 run 窗口（host-rejected 用例需要在
      // run 进行中提交；本地即时响应会让窗口塌缩到毫秒级）。
      const slow = userText.includes('slow');
      if (slow) { await sleep(1_200); }
      if (toolResults === 0) {
        if (!stream) { return res.end('non-stream tool step'); }
        res.writeHead(200, { 'Content-Type': 'text/event-stream' });
        res.write(`data: ${JSON.stringify({ id: 'j1', object: 'chat.completion.chunk', created: 1, model: 'j1-fake', choices: [{ index: 0, delta: { content: 'opening two work files' }, finish_reason: null }] })}\n\n`);
        if (slow) { await sleep(1_200); }
        res.write(`data: ${JSON.stringify({ id: 'j1', object: 'chat.completion.chunk', created: 1, model: 'j1-fake', choices: [{ index: 0, delta: toolDelta(0, 'call-j1-write-x', 'write_file', { path: 'j1-x.md', content: '# X\n\norderlive artifact one\n', overwrite: true }), finish_reason: null }] })}\n\n`);
        if (slow) { await sleep(1_200); }
        res.write(`data: ${JSON.stringify({ id: 'j1', object: 'chat.completion.chunk', created: 1, model: 'j1-fake', choices: [{ index: 0, delta: toolDelta(1, 'call-j1-write-y', 'write_file', { path: 'j1-y.md', content: '# Y\n\norderlive artifact two\n', overwrite: true }), finish_reason: null }] })}\n\n`);
        res.write(`data: ${JSON.stringify({ id: 'j1', object: 'chat.completion.chunk', created: 1, model: 'j1-fake', choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }] })}\n\n`);
        res.write('data: [DONE]\n\n');
        return res.end();
      }
      if (slow) { await sleep(1_200); }
      if (toolResults <= 2) {
        return stream ? sse(res, [
          [toolDelta(0, 'call-j1-read-x', 'read_file', { path: 'j1-x.md' })],
          [{}, 'tool_calls'],
        ]) : res.end('non-stream tool step');
      }
      return plain('final answer after tools');
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port }));
  });
}

// 真 clat serve 二进制宿主：临时 HOME + 项目，--trust 首启授权，随机端口。
async function startServe() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'j1-real-home-'));
  const project = path.join(home, 'project');
  fs.mkdirSync(project, { recursive: true });
  const child = spawn(BIN, ['serve', '--trust', '--port', '0', '--no-open'], {
    env: { ...process.env, HOME: home, CLICOLOR: '0' }, cwd: project, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let out = '';
  child.stdout.on('data', (d) => { out += d; });
  child.stderr.on('data', (d) => { out += d; });
  const endpointPath = path.join(home, '.clat', 'host-endpoint.json');
  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline) {
    if (fs.existsSync(endpointPath)) { break; }
    await sleep(200);
  }
  if (!fs.existsSync(endpointPath)) {
    child.kill('SIGTERM');
    throw new Error('serve never published host-endpoint.json; output:\n' + out.slice(-2000));
  }
  const endpoint = JSON.parse(fs.readFileSync(endpointPath, 'utf8'));
  for (;;) {
    const ok = await fetch(`http://127.0.0.1:${endpoint.port}/`).then((r) => r.ok).catch(() => false);
    if (ok) { break; }
    if (Date.now() > deadline) { throw new Error('serve http never ready'); }
    await sleep(200);
  }
  return { child, home, project, port: endpoint.port, output: () => out };
}

// 真 TUI App：expect 分配 PTY（可控 winsize）+ interact 双向桥接。
const EXPECT_TCL = `
set home [lindex $argv 0]
set cwd [lindex $argv 1]
set bin [lindex $argv 2]
cd $cwd
spawn env HOME=$home TERM=xterm-256color CLICOLOR=0 $bin
exec stty rows 32 columns 110 < $spawn_out(slave,name)
send_user "J1-TUI-READY\\r"
interact
`;

function startTui(home, project) {
  const tcl = path.join(os.tmpdir(), `j1-tui-${Date.now()}.exp`);
  fs.writeFileSync(tcl, EXPECT_TCL);
  const child = spawn('expect', [tcl, home, project, BIN], { cwd: project });
  let out = '';
  child.stdout.on('data', (d) => { out += d.toString('utf8'); });
  child.stderr.on('data', (d) => { out += d.toString('utf8'); });
  return {
    child,
    send: (bytes) => child.stdin.write(bytes),
    // 仅用于"存在性"判定（出现即真）；缺失判定一律走权威事实（R1-F1）。
    waitFor: async (needle, timeout = 25_000) => {
      const visible = () => out.includes(needle);
      const deadline = Date.now() + timeout;
      while (Date.now() < deadline) {
        if (visible()) { return true; }
        await sleep(150);
      }
      throw new Error(`TUI never showed ${JSON.stringify(needle)} (presence probe); tail=\n` + out.slice(-900));
    },
  };
}

// 一次联合环境：真 serve + 假模型 + PWA 真实 UI 配置并激活 + 真 TUI attach。
async function bootJointEnv(page) {
  const modelLog = [];
  const fake = await fakeModelServer(modelLog);
  const serve = await startServe();
  await page.goto(`http://127.0.0.1:${serve.port}/`);
  await expect(page.locator('#conn-status')).toHaveText('live', { timeout: 30_000 });
  await page.click('#settings-open');
  await page.click('[data-settings-category="models"]');
  await page.click('#model-profile-new');
  await page.fill('#model-profile-name', 'j1-fake');
  await page.selectOption('#model-profile-protocol', 'open_ai_compatible');
  await page.fill('#model-profile-model', 'j1-fake');
  await page.fill('#model-profile-endpoint', `http://127.0.0.1:${fake.port}`);
  await page.fill('#model-profile-path', '/chat/completions');
  await page.fill('#model-profile-key', 'j1-fake-key');
  await page.click('#model-profile-save');
  await expect(page.locator('#model-settings-saved')).toContainText('Profile saved', { timeout: 15_000 });
  await page.keyboard.press('Escape');
  await page.click('#model-picker-trigger');
  await page.getByRole('radio', { name: 'Use j1-fake', exact: false }).click();
  await expect(page.locator('#model-picker-label')).toContainText('j1-fake', { timeout: 15_000 });
  const tui = startTui(serve.home, serve.project);
  await tui.waitFor('Conversation');
  const shutdown = () => {
    tui.child.kill('SIGTERM');
    serve.child.kill('SIGTERM');
    fake.server.close();
  };
  return { serve, fake, tui, modelLog, shutdown };
}

// PWA 侧最新一条 user 消息文本（权威：journal 投影）。
const latestUserText = (page) => page.evaluate(() => {
  const rows = [...document.querySelectorAll('.msg.user')];
  return rows.length ? rows[rows.length - 1].textContent.trim() : null;
});
const userCount = (page) => page.locator('.msg.user').count();

// ---------------------------------------------------------------------------
// 校准 1（正常控制，独立 Session）：TUI 输入唯一草稿标记，不做任何远端
// 操作，真实 Enter——新增 user 消息与假模型收到的 user 都必须等于标记。
// ---------------------------------------------------------------------------
test('J1 draft calibration A: normal control submits the draft verbatim', async ({ page }) => {
  test.setTimeout(180_000);
  const env = await bootJointEnv(page);
  try {
    const marker = 'calib-normal-7f3a';
    const before = await userCount(page);
    env.tui.send(marker);
    await sleep(500);
    env.tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(before + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(marker);
    const modelUsers = env.modelLog.map((m) => m.user);
    expect(modelUsers[modelUsers.length - 1], 'the fake model must receive the draft verbatim (inside its bounded context)').toContain(marker);
    console.log('J1 calibration A ok:', JSON.stringify({ marker, session: await page.evaluate(() => state.sessionId) }));
  } finally { env.shutdown(); }
});

// ---------------------------------------------------------------------------
// 校准 2（实验组，独立 Session）：TUI 先提交一轮建立会话，输入唯一草稿
// 标记；同 Session 由 PWA 触发真实多工具 Run 并等待终态（事件驱动：工具
// 卡+终文+verdict），再在 TUI 输入 Z 并真实 Enter——提交内容必须等于
// 标记+Z（草稿在远端 Run 全程保留于 composer，权威事实而非屏幕解析）。
// ---------------------------------------------------------------------------
test('J1 draft calibration B: draft survives a same-session remote multi-tool run', async ({ page }) => {
  test.setTimeout(240_000);
  const env = await bootJointEnv(page);
  try {
    env.tui.send('alpha');
    await sleep(400);
    env.tui.send('\r');
    await expect(page.locator('.msg.assistant', { hasText: 'acknowledged' })).toBeVisible({ timeout: 30_000 });
    const marker = 'calib-remote-9d1c';
    env.tui.send(marker);
    await sleep(500);
    const verdictBefore = await page.locator('.verdict.completed').count();
    await page.fill('#prompt', 'orderlive run: create the two files and report');
    await page.click('#send');
    await expect.poll(async () => page.locator('.verdict.completed').count(), { timeout: 60_000 }).toBe(verdictBefore + 1);
    await expect(page.locator('.tool-card')).toHaveCount(3, { timeout: 30_000 });
    await expect(page.locator('.msg.assistant', { hasText: 'final answer after tools' })).toBeVisible({ timeout: 30_000 });
    await page.waitForFunction(() => !state.runActive, null, { timeout: 15_000 });
    const before = await userCount(page);
    env.tui.send('Z');
    await sleep(500);
    env.tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(before + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(`${marker}Z`);
    const modelUsers = env.modelLog.map((m) => m.user);
    expect(modelUsers[modelUsers.length - 1], 'the fake model must receive draft+Z verbatim after the remote run').toContain(`${marker}Z`);
    console.log('J1 calibration B ok:', JSON.stringify({ marker: `${marker}Z`, session: await page.evaluate(() => state.sessionId) }));
  } finally { env.shutdown(); }
});

// ---------------------------------------------------------------------------
// 主链：双向历史召回（含真实 reconnect）+ orderlive live/replay 同序 +
// 两端 Up/Down 草稿恢复 + 斜杠菜单 + 切 Session 防串史。
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// R2-B 审 F1 返工（2026-10-18）：Up/Down 恢复的**中间路径**强判别。
// 乙方变异证明"只断言最终值==原草稿"对历史浏览完全失效不敏感
// （history_previous 非空文本直接 return 时主链仍绿）。本用例在
// **每个分支独立 Session**（真实提交会改变历史，不复用被污染的会话）
// 建立精确 [alpha, beta] 两条历史后分别验证：
//   a) 非空草稿 Up 直接提交 == beta（带草稿的 Up 必须真实召回）；
//   b) Up Up 提交 == alpha（深入第二条历史）；
//   c) Up Up Down 提交 == beta（向下移动）；
//   d) Up Up Down Down 提交 == 原草稿（完整恢复）。
// 每步都以**真实提交内容**为权威判据（CLAT_WITH_HINTS：分支设计来自
// B 审 R2 最小返工提示）。
// ---------------------------------------------------------------------------
test('J1 recall paths: every arrow step reaches the expected history entry', async ({ page }) => {
  test.setTimeout(360_000);
  const env = await bootJointEnv(page);
  const { tui } = env;
  const freshHistory = async () => {
    // R3-F3：正文可见 ≠ Run 结束——发送 beta 前必须等到**真实空闲**
    //（!runActive 且 Send 可提交），否则 beta 撞进 steering 窗口；发完
    // beta 同样等空闲，并确认已提交历史确为 [alpha, beta]。
    const waitIdle = async () => {
      await page.waitForFunction(() => !state.runActive, null, { timeout: 30_000 });
      await expect(page.locator('#send')).toBeEnabled({ timeout: 10_000 });
    };
    await page.click('#new-session');
    await page.waitForFunction(() => !state.runActive && document.querySelectorAll('.msg.user').length === 0, null, { timeout: 20_000 });
    await sleep(800);
    tui.send('alpha');
    await sleep(400);
    tui.send('\r');
    await expect.poll(async () => page.locator('.msg.user', { hasText: 'alpha' }).count(), { timeout: 30_000 }).toBe(1);
    await expect(page.locator('.msg.assistant', { hasText: 'acknowledged' })).toBeVisible({ timeout: 30_000 });
    await waitIdle();
    await page.fill('#prompt', 'beta');
    await page.click('#send');
    await expect.poll(async () => page.locator('.msg.user', { hasText: 'beta' }).count(), { timeout: 30_000 }).toBe(1);
    await expect(page.locator('.msg.assistant', { hasText: 'acknowledged' }).last()).toBeVisible({ timeout: 30_000 });
    await waitIdle();
    // 准备断言：该 Session 的已提交历史（journal 投影）确为 [alpha, beta]。
    const committed = await page.evaluate(() => [...document.querySelectorAll('.msg.user')].map((n) => (n.querySelector(':scope > .body') ? n.querySelector(':scope > .body').textContent : n.textContent).trim()));
    expect(committed, 'prepared history must be exactly [alpha, beta]').toEqual(['alpha', 'beta']);
    await sleep(600);
  };
  const typeDraft = async (draft) => {
    tui.send(draft);
    await sleep(500);
  };
  const press = async (key, count = 1) => {
    for (let i = 0; i < count; i++) {
      tui.send(key);
      await sleep(350);
    }
  };
  const submitExpect = async (expected, label) => {
    const before = await userCount(page);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(before + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(expected);
    console.log(`J1 recall path ok: ${label} -> ${expected}`);
  };

  try {
    await freshHistory();
    await typeDraft('d-a1');
    await press('\x1b[A');
    await submitExpect('beta', 'non-empty draft + Up');

    await freshHistory();
    await typeDraft('d-b2');
    await press('\x1b[A', 2);
    await submitExpect('alpha', 'Up,Up');

    await freshHistory();
    await typeDraft('d-c3');
    await press('\x1b[A', 2);
    await press('\x1b[B', 1);
    await submitExpect('beta', 'Up,Up,Down');

    await freshHistory();
    await typeDraft('d-d4');
    await press('\x1b[A', 2);
    await press('\x1b[B', 2);
    await submitExpect('d-d4', 'Up,Up,Down,Down restores the original draft');
  } finally { env.shutdown(); }
});

// ---------------------------------------------------------------------------
// R2-B 审 F2 返工（R3 措辞收敛）：**非空提交的宿主拒绝**在本轮真实双 UI
// 键盘路径上**未能稳定建立指定拒绝场景**——实测 TUI 在 run 活跃时 Enter
// 被接纳为 **steering**（"Steering applied" + 转向消息入 journal + 模型
// 下一轮可见），未触达 prompt.send 的 run-busy 拒绝；这不证明所有非空
// 拒绝路径在所有 UI 时序都不可达。按 B 审提示如实处置：拒绝入史保护标
// **NOT_VERIFIED（本轮未稳定建立，原因如上）**；本用例验证**运行中
// 转向**这一真实联合行为（marker 到达模型、之后可召回），另含迟到刷新
// （PWA 冷 reload 期间 TUI 草稿保留，权威提交验证）与历史对照。
// ---------------------------------------------------------------------------
test('J1 steering accepts a mid-run TUI input; late reload keeps the draft', async ({ page }) => {
  test.setTimeout(300_000);
  const env = await bootJointEnv(page);
  const { tui } = env;
  try {
    // 对照历史。
    tui.send('alpha');
    await sleep(400);
    tui.send('\r');
    await expect(page.locator('.msg.assistant', { hasText: 'acknowledged' })).toBeVisible({ timeout: 30_000 });
    await page.waitForFunction(() => !state.runActive, null, { timeout: 20_000 });

    // 远端多步 Run 进行中，TUI 输入 marker 并 Enter → 被接纳为 steering。
    const marker = 'steering-marker-5e8b';
    tui.send(marker);
    await sleep(500);
    const verdictBefore = await page.locator('.verdict.completed').count();
    await page.fill('#prompt', 'orderlive slow run: create the two files and report');
    await page.click('#send');
    await expect(page.locator('.msg.assistant', { hasText: 'opening two work files' })).toBeVisible({ timeout: 30_000 });
    await expect.poll(() => page.evaluate(() => state.runActive), { timeout: 10_000 }).toBe(true);
    tui.send('\r');
    // steering 被接纳：marker 以转向消息出现在 journal，且模型下一轮可见。
    await expect(page.locator('.msg.user', { hasText: marker })).toBeVisible({ timeout: 30_000 });
    await expect(page.getByText('Steering applied')).toBeVisible({ timeout: 15_000 });
    await expect.poll(async () => page.locator('.verdict.completed').count(), { timeout: 90_000 }).toBe(verdictBefore + 1);
    const modelUsers = env.modelLog.map((m) => m.user).join('\n');
    expect(modelUsers, 'the steered marker must reach the model in a later turn').toContain(marker);
    console.log('J1 steering ok: mid-run TUI Enter was accepted as steering and reached the model');

    // 历史对照：steering 后普通召回仍工作（清空→Up→Enter==最新提交）。
    tui.send('\x7f'.repeat(30));
    await sleep(400);
    tui.send('\x1b[A');
    await sleep(600);
    const beforeControl = await userCount(page);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeControl + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(marker);
    console.log('J1 history control ok: recall still reaches the latest input after steering');

    // 迟到刷新：TUI 新草稿在 PWA 冷刷新期间保留（reload 不污染本端草稿）。
    const lateMarker = 'late-reload-3f7k';
    tui.send('\x7f'.repeat(30));
    tui.send(lateMarker);
    await sleep(500);
    await page.reload();
    await expect(page.locator('#conn-status')).toHaveText('live', { timeout: 30_000 });
    const beforeLate = await userCount(page);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeLate + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(lateMarker);
    console.log('J1 late-reload ok: TUI draft survived a PWA cold reload');
  } finally { env.shutdown(); }
});

test('J1-REAL: recall both ways, live/replay order, Up/Down draft restore, session isolation', async ({ page }) => {
  test.setTimeout(360_000);
  const env = await bootJointEnv(page);
  const { tui } = env;
  try {
    console.log('J1 receipt:', JSON.stringify({ serve_pid: env.serve.child.pid, port: env.serve.port, fake_model_port: env.fake.port }));
    // TUI alpha → PWA ↑ 召回。
    tui.send('alpha');
    await sleep(400);
    tui.send('\r');
    await tui.waitFor('j1 real joint session', 30_000);
    await expect(page.locator('.msg.assistant', { hasText: 'acknowledged' })).toBeVisible({ timeout: 30_000 });
    await page.waitForFunction(() => inputHistory.list.length >= 1 && !state.runActive, null, { timeout: 30_000 });
    await page.locator('#prompt').press('ArrowUp');
    await expect(page.locator('#prompt')).toHaveValue('alpha');

    // —— PWA Up/Down 草稿恢复（composer 权威值逐步断言）——
    await page.fill('#prompt', 'draft-pwa-Y7');
    await page.locator('#prompt').press('ArrowUp');
    await expect(page.locator('#prompt')).toHaveValue('alpha');
    await page.fill('#prompt', 'draft-pwa-Y7'); // 重置原草稿（重走完整序列）
    await page.locator('#prompt').press('ArrowUp');
    await expect(page.locator('#prompt')).toHaveValue('alpha');
    await page.locator('#prompt').press('ArrowDown');
    await expect(page.locator('#prompt'), 'ArrowDown must walk back toward the original draft').toHaveValue('draft-pwa-Y7');
    await page.fill('#prompt', '');

    // —— orderlive：同 Session live 因果顺序 === 冷刷新 replay 同序 ——
    const verdictBefore = await page.locator('.verdict.completed').count();
    await page.fill('#prompt', 'orderlive run: create the two files and report');
    await page.click('#send');
    await expect.poll(async () => page.locator('.verdict.completed').count(), { timeout: 60_000 }).toBe(verdictBefore + 1);
    await expect(page.locator('.tool-card')).toHaveCount(3, { timeout: 30_000 });
    await expect(page.locator('.msg.assistant', { hasText: 'final answer after tools' })).toBeVisible({ timeout: 30_000 });
    const domShape = () => page.evaluate(() => [...document.querySelectorAll('.msg.user, .msg.assistant, .tool-card')].map((n) => {
      const kind = n.classList.contains('msg') ? (n.classList.contains('user') ? 'user' : 'assistant') : 'tool';
      // R3-F1：结构化读取正文——assistant/user 消息的真实正文在直接子节点
      // `.body` 内；卡内其余是 reasoning 折叠区（`Think` 标题、
      // `visually-hidden` 的 "Thinking in progress" 可访问性文案）等 UI
      // 装饰节点，textContent 全量会把隐藏文字混进"正文"。只对 .body
      // 取文本，不全局删词（用户正文可能合法包含 Think/Copy）。
      // tool 卡无隐藏节点，textContent 即工具身份+参数+输出。
      const body = n.querySelector(':scope > .body');
      const text = (body ? body.textContent : n.textContent).trim();
      return { kind, text };
    }));
    const causal = (rows) => {
      const idx = (kind, needle) => rows.findIndex((e) => e.kind === kind && e.text.includes(needle));
      const promptIdx = idx('user', 'orderlive run');
      const openingIdx = idx('assistant', 'opening two work files');
      const finalIdx = idx('assistant', 'final answer after tools');
      // R4-F1：双边界——首个工具的位置约束 toolsAfterOpening（首工具是
      // 最早的工具，它在 opening 之后 ⇒ 全部工具都在其后；此前误用
      // lastTool 只证明"还剩一个工具在后面"，首工具提前会假绿）；
      // 最后工具的位置约束 finalAfterLastTool。
      const firstTool = rows.findIndex((e) => e.kind === 'tool');
      const lastTool = rows.map((e) => e.kind).lastIndexOf('tool');
      return {
        promptIdx, openingIdx, finalIdx, firstTool, lastTool,
        tools: rows.filter((e) => e.kind === 'tool').length,
        // R3-F2：因果真值独立断言（先存在、后顺序；findIndex=-1 不得参与
        // 比较），不以"两边相等"替代"正确"——两个排列都错时相等仍成立。
        exists: promptIdx >= 0 && openingIdx >= 0 && finalIdx >= 0 && firstTool >= 0 && lastTool >= 0,
        openingAfterPrompt: openingIdx > promptIdx,
        toolsAfterOpening: firstTool > openingIdx,
        finalAfterLastTool: finalIdx > lastTool,
      };
    };
    const liveRows = await domShape();
    const liveCausal = causal(liveRows);
    expect(liveCausal.exists, 'prompt/opening/final/tool anchors must all exist').toBe(true);
    expect(liveCausal.openingAfterPrompt, 'live: opening text after the orderlive prompt').toBe(true);
    expect(liveCausal.toolsAfterOpening, 'live: tools after the opening text').toBe(true);
    expect(liveCausal.finalAfterLastTool, 'live: final answer after the last tool card').toBe(true);
    expect(liveCausal.tools).toBe(3);
    await page.reload();
    await expect(page.locator('#conn-status')).toHaveText('live', { timeout: 30_000 });
    await expect(page.locator('.tool-card')).toHaveCount(3, { timeout: 30_000 });
    // replay 逐项序列相等（kind+结构化正文），且与已验证正确的 live 因果一致。
    const replayRows = await domShape();
    const replayCausal = causal(replayRows);
    expect(replayRows.length, 'same item count after reload').toBe(liveRows.length);
    for (let i = 0; i < liveRows.length; i++) {
      expect(replayRows[i].kind, `item ${i} kind matches`).toBe(liveRows[i].kind);
      expect(replayRows[i].text, `item ${i} body text matches (structural .body read)`).toBe(liveRows[i].text);
    }
    expect(replayCausal).toEqual(liveCausal);
    console.log('J1 orderlive ok: structural body equality + independent live causal truth both hold');

    // —— PWA beta → TUI 真实 reconnect 后召回 beta ——
    tui.child.kill('SIGTERM');
    await sleep(1_500);
    await page.fill('#prompt', 'beta');
    await page.click('#send');
    await expect.poll(async () => page.locator('.msg.user', { hasText: 'beta' }).count(), { timeout: 30_000 }).toBe(1);
    await page.waitForFunction(() => inputHistory.list.length >= 2 && !state.runActive, null, { timeout: 30_000 });
    const tui2 = startTui(env.serve.home, env.serve.project);
    env.tui = tui2;
    await tui2.waitFor('Conversation');
    await tui2.waitFor('j1 real joint session');
    const alphaCount = async () => page.locator('.msg.user', { hasText: 'alpha' }).count();
    const betaCount = async () => page.locator('.msg.user', { hasText: 'beta' }).count();
    tui2.send('\x1b[A');
    await sleep(600);
    tui2.send('\r');
    await expect.poll(betaCount, { timeout: 30_000 }).toBeGreaterThanOrEqual(2);

    // —— TUI Up/Down 草稿恢复（最终真实提交确认恢复值）——
    // 序列：原草稿 → Up(最新 beta) → Up(beta/alpha 层) → Down → Down 回
    // 原草稿；以 Enter 提交内容 == 原草稿 为权威判据。
    const draftMarker = 'draft-tui-R2x';
    tui2.send(draftMarker);
    await sleep(500);
    tui2.send('\x1b[A');
    await sleep(400);
    tui2.send('\x1b[A');
    await sleep(400);
    tui2.send('\x1b[B');
    await sleep(400);
    tui2.send('\x1b[B');
    await sleep(500);
    const beforeRestore = await userCount(page);
    tui2.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeRestore + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(draftMarker);
    console.log('J1 Up/Down restore ok: TUI submitted the original draft verbatim');

    // —— 斜杠菜单真键盘冒烟（开/↑↓/Esc 后历史召回仍工作）——
    tui2.send('\x7f'.repeat(24));
    tui2.send('/');
    await tui2.waitFor('Commands');
    tui2.send('\x1b[A\x1b[B');
    await sleep(500);
    tui2.send('\x1b');
    await sleep(400);
    tui2.send('\x7f');
    await sleep(300);
    const beforeMenuRecall = await userCount(page);
    tui2.send('\x1b[A');
    await sleep(600);
    tui2.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeMenuRecall + 1);
    console.log('J1 slash-menu smoke ok: recall works after menu open/arrows/Esc');

    // —— 切 Session 防串史：旧 Session 提交独有文本，新 Session 两端
    // 不得召回它（PWA inputHistory 权威为空 + TUI ↑+Enter 不产生提交）。——
    const uniqueOld = 'unique-old-4e2f';
    tui2.send(uniqueOld);
    await sleep(500);
    tui2.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeMenuRecall + 2);
    await page.click('#new-session');
    await page.waitForFunction(() => document.querySelectorAll('.tool-card').length === 0 && !state.runActive, null, { timeout: 20_000 });
    await page.locator('#prompt').press('ArrowUp');
    await expect(page.locator('#prompt')).toHaveValue('');
    expect(await page.evaluate(() => inputHistory.list), 'fresh session must start with empty history').toEqual([]);
    await sleep(1_500);
    const usersInNew = await userCount(page);
    tui2.send('\x1b[A');
    await sleep(600);
    tui2.send('\r');
    await sleep(1_200);
    expect(await userCount(page), 'an empty composer must not submit anything').toBe(usersInNew);
    console.log('J1 session isolation ok: unique old-session text not recalled on either client');
    console.log('J1 model log tail:', JSON.stringify(env.modelLog.slice(-5)));
  } finally { env.shutdown(); }
});

// ---------------------------------------------------------------------------
// 负向小件：**空白输入前端过滤**（R2-B 审 F2 勘误命名：此路径是 TUI
// actions.rs 提交前 trim() 为空直接 return，不产生宿主请求；非空提交
// 的宿主拒绝保护见 host-rejected 用例）不入史；建议浮层（Ctrl+G）打开
// 期间方向键不污染 composer，Esc 忽略后 Enter 不产生提交（权威判据）。
// ---------------------------------------------------------------------------
test('J1 negatives: whitespace-only input filtered client-side, suggestion overlay owns arrows', async ({ page }) => {
  test.setTimeout(240_000);
  const env = await bootJointEnv(page);
  try {
    // 手动建议默认关闭（生产默认）——先经真实设置 UI 启用（utility 面板）。
    // TUI 对策略的感知来自宿主快照：真实用户路径是重开 TUI（重启 attach
    // 必拉最新快照），与 calibration 的 reconnect 腿同型。
    await page.click('#settings-open');
    await page.click('[data-settings-category="utility"]');
    await page.locator('#utility-suggestions-enabled').check();
    await page.click('#utility-settings-save');
    await expect(page.locator('#utility-settings-saved')).toContainText('policy saved', { timeout: 15_000 });
    await page.keyboard.press('Escape');
    await sleep(400);
    env.tui.child.kill('SIGTERM');
    await sleep(1_200);
    env.tui = startTui(env.serve.home, env.serve.project);
    await env.tui.waitFor('Conversation');

    // 建立一条真实对话（建议与历史召回都要求有活跃对话上下文）。
    env.tui.send('alpha');
    await sleep(400);
    env.tui.send('\r');
    await expect(page.locator('.msg.assistant', { hasText: 'acknowledged' })).toBeVisible({ timeout: 30_000 });

    // 空白拒绝：真实键盘输入三个空格 + Enter —— 不得产生新 user 消息，
    // 也不得进入已提交历史（宿主拒绝空白文本为生产路径）。
    const before = await userCount(page);
    env.tui.send('   ');
    await sleep(400);
    env.tui.send('\r');
    await sleep(1_500);
    expect(await userCount(page), 'a whitespace-only submit must be rejected (no new user message)').toBe(before);
    const history = await page.evaluate(() => inputHistory.list);
    expect(history.filter((t) => t.trim() === ''), 'rejected whitespace must never enter committed history').toHaveLength(0);

    // 建议浮层：Ctrl+G（ASCII 0x07；0x06 是 Ctrl+F=Find，曾误开查找框）
    // 生成（假模型 suggest 分支）→ 预览出现（存在性探针）→ 期间 ↑↓ →
    // Esc 忽略 → Enter（composer 必须仍为空，不产生提交）。
    env.tui.send('\x07');
    await env.tui.waitFor('suggested-next-42', 30_000);
    env.tui.send('\x1b[A\x1b[B');
    await sleep(500);
    env.tui.send('\x1b');
    await sleep(500);
    const beforeDismiss = await userCount(page);
    env.tui.send('\r');
    await sleep(1_500);
    expect(await userCount(page), 'after dismissing the suggestion the empty composer must not submit').toBe(beforeDismiss);
    const historyAfter = await page.evaluate(() => inputHistory.list);
    expect(historyAfter.filter((t) => t.includes('suggested')), 'the suggestion preview must not enter committed history').toHaveLength(0);
    console.log('J1 negatives ok: whitespace rejection + suggestion overlay arrow ownership');
  } finally { env.shutdown(); }
});
