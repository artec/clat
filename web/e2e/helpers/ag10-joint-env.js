// AG-DOGFOOD-10（第九轮甲方）联合环境：真 clat serve（随机端口）+
// 真 Chromium PWA 配置并激活假模型 + 真 PTY TUI（expect 驱动，可选
// CLAT_BELL_COMMAND / CLAT_NO_BELL 环境注入）。
//
// 复用第八轮 ag9-j1-real.spec.js 的已验证骨架（该文件是已发布轮次的
// 冻结证据，不改写）；本轮两份新 spec（ag10-bell-host / ag10-j1-edge）
// 共用本模块，避免两份新拷贝互相漂移。
const { expect } = require('@playwright/test');
const { spawn } = require('child_process');
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');

// 二进制：默认取仓库 target；共享 CARGO_TARGET_DIR 构建时可用
// CLAT_E2E_BIN 显式指向实际产物（红/绿四态循环节省整树重建）。
const BIN = process.env.CLAT_E2E_BIN
  || path.join(__dirname, '..', '..', '..', 'target', 'debug', 'clat');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------
// 假模型：OpenAI 兼容。分支：含 "boomplease" 回 HTTP 500（run failed 腿）；
// 无 "orderlive" 快回 "acknowledged"；含 "orderlive" 走 write_file×2 →
// read_file → 终文多步因果链（slow 变体步间 1.2s 拉宽 run 窗口）；标题
// 请求短回；建议请求回固定建议。
// ---------------------------------------------------------------------------
function fakeModelServer(log) {
  const sse = (res, chunks) => {
    res.writeHead(200, { 'Content-Type': 'text/event-stream' });
    for (const [delta, finish] of chunks) {
      const choice = { index: 0, delta, finish_reason: finish ?? null };
      res.write(`data: ${JSON.stringify({ id: 'ag10', object: 'chat.completion.chunk', created: 1, model: 'ag10-fake', choices: [choice] })}\n\n`);
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

      // 失败腿：让 provider 请求以 5xx 失败 → run failed（settled failed）。
      if (userText.includes('boomplease')) {
        res.writeHead(500, { 'Content-Type': 'text/plain' });
        return res.end('ag10 intentional model failure');
      }
      const ins = String(instructions);
      if (ins.startsWith('Generate a concise title')) { return plain('ag10 real joint session'); }
      if (ins.startsWith('Predict one useful next user message')) { return plain('suggested-next-ag10'); }
      if (!userText.includes('orderlive')) { return plain('acknowledged'); }
      const slow = userText.includes('slow');
      if (slow) { await sleep(1_200); }
      if (toolResults === 0) {
        if (!stream) { return res.end('non-stream tool step'); }
        res.writeHead(200, { 'Content-Type': 'text/event-stream' });
        res.write(`data: ${JSON.stringify({ id: 'ag10', object: 'chat.completion.chunk', created: 1, model: 'ag10-fake', choices: [{ index: 0, delta: { content: 'opening two work files' }, finish_reason: null }] })}\n\n`);
        if (slow) { await sleep(1_200); }
        res.write(`data: ${JSON.stringify({ id: 'ag10', object: 'chat.completion.chunk', created: 1, model: 'ag10-fake', choices: [{ index: 0, delta: toolDelta(0, 'call-ag10-write-x', 'write_file', { path: 'ag10-x.md', content: '# X\n\norderlive artifact one\n', overwrite: true }), finish_reason: null }] })}\n\n`);
        if (slow) { await sleep(1_200); }
        res.write(`data: ${JSON.stringify({ id: 'ag10', object: 'chat.completion.chunk', created: 1, model: 'ag10-fake', choices: [{ index: 0, delta: toolDelta(1, 'call-ag10-write-y', 'write_file', { path: 'ag10-y.md', content: '# Y\n\norderlive artifact two\n', overwrite: true }), finish_reason: null }] })}\n\n`);
        res.write(`data: ${JSON.stringify({ id: 'ag10', object: 'chat.completion.chunk', created: 1, model: 'ag10-fake', choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }] })}\n\n`);
        res.write('data: [DONE]\n\n');
        return res.end();
      }
      if (slow) { await sleep(1_200); }
      if (toolResults <= 2) {
        return stream ? sse(res, [
          [toolDelta(0, 'call-ag10-read-x', 'read_file', { path: 'ag10-x.md' })],
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
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'ag10-home-'));
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

// 真 TUI App：expect 分配 PTY（可控 winsize）+ interact 双向桥接；可选
// 注入 CLAT_BELL_COMMAND（单字脚本路径）或 CLAT_NO_BELL=1。
function startTui(home, project, opts = {}) {
  const envPairs = [`HOME=${home}`, 'TERM=xterm-256color', 'CLICOLOR=0'];
  if (opts.bellCommand) { envPairs.push(`CLAT_BELL_COMMAND=${opts.bellCommand}`); }
  if (opts.noBell) { envPairs.push('CLAT_NO_BELL=1'); }
  const tcl = `
set cwd [lindex $argv 0]
set bin [lindex $argv 1]
cd $cwd
spawn env ${envPairs.join(' ')} $bin
exec stty rows 32 columns 110 < $spawn_out(slave,name)
send_user "AG10-TUI-READY\\r"
interact
`;
  const tclPath = path.join(os.tmpdir(), `ag10-tui-${Date.now()}.exp`);
  fs.writeFileSync(tclPath, tcl);
  const child = spawn('expect', [tclPath, project, BIN], { cwd: project });
  let out = '';
  child.stdout.on('data', (d) => { out += d.toString('utf8'); });
  child.stderr.on('data', (d) => { out += d.toString('utf8'); });
  return {
    child,
    send: (bytes) => child.stdin.write(bytes),
    // 仅用于"存在性"判定（出现即真）；缺失判定一律走权威事实。
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

// PWA 侧配置并激活假模型（真实设置 UI 路径）。

// 一次联合环境：真 serve + 假模型 + PWA 真实 UI 配置并激活（可选启动
// 真 TUI）。tui 选项透传 startTui。
async function bootJointEnv(page, fake, serve, tuiOpts = null) {
  await page.goto(`http://127.0.0.1:${serve.port}/`);
  await expect(page.locator('#conn-status')).toHaveText('live', { timeout: 30_000 });
  await page.click('#settings-open');
  await page.click('[data-settings-category="models"]');
  await page.click('#model-profile-new');
  await page.fill('#model-profile-name', 'ag10-fake');
  await page.selectOption('#model-profile-protocol', 'open_ai_compatible');
  await page.fill('#model-profile-model', 'ag10-fake');
  await page.fill('#model-profile-endpoint', `http://127.0.0.1:${fake.port}`);
  await page.fill('#model-profile-path', '/chat/completions');
  await page.fill('#model-profile-key', 'ag10-fake-key');
  await page.click('#model-profile-save');
  await expect(page.locator('#model-settings-saved')).toContainText('Profile saved', { timeout: 15_000 });
  await page.keyboard.press('Escape');
  await page.click('#model-picker-trigger');
  await page.getByRole('radio', { name: 'Use ag10-fake', exact: false }).click();
  await expect(page.locator('#model-picker-label')).toContainText('ag10-fake', { timeout: 15_000 });
  let tui = null;
  if (tuiOpts) {
    tui = startTui(serve.home, serve.project, tuiOpts);
    await tui.waitFor('Conversation');
  }
  const shutdown = () => {
    if (tui) { tui.child.kill('SIGTERM'); }
    serve.child.kill('SIGTERM');
    fake.server.close();
  };
  return { tui, shutdown };
}

// PWA 提交一条提示词（真实 composer 路径）。
async function pwaSubmit(page, text) {
  await page.fill('#prompt', text);
  await page.click('#send');
}

const latestUserText = (page) => page.evaluate(() => {
  const rows = [...document.querySelectorAll('.msg.user')];
  return rows.length ? rows[rows.length - 1].textContent.trim() : null;
});
const userCount = (page) => page.locator('.msg.user').count();

module.exports = {
  BIN, sleep, fakeModelServer, startServe, startTui, bootJointEnv,
  pwaSubmit, latestUserText, userCount,
};
