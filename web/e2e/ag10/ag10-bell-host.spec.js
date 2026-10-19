// AG-DOGFOOD-10 / A1 BELL-HOST-1：真 Host-attached run 终态提醒铃。
//
// 缺陷（用户实战 + 静态核准）：`src/tui/native.rs` 的 `prompt.settled`
// 分支不调 notify——后台 Host 上 Run 结束，失焦终端永远无铃声出口；
// standalone（finish_run）与 dsh（turn/end）两腿均有通知。
//
// 验收面（合同 §1）：真随机端口 clat serve + 真 PTY TUI（expect 驱动，
// CLAT_BELL_COMMAND 写 marker 文件 = 受控无声 sink，测试不实际发声、
// 不依赖系统音量）+ 假模型独立 Session。断言以 marker 计数为权威：
//   - completed / failed：恰 +1（不重复）；
//   - cancelled、前台（DECSET 1004 FocusGained）、CLAT_NO_BELL、
//     重连重放、切会话重放、旧会话迟到结算：不响；
//   - 当前会话的 run 恢复归属后继续响。
// pre-fix（无 notify）：completed/failed 腿红；负向腿天然绿。
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const {
  sleep, fakeModelServer, startServe, startTui, bootJointEnv, pwaSubmit,
} = require('../helpers/ag10-joint-env');

// 受控无声铃 sink：单字脚本（sh -c 执行），向 marker 文件追加 `bell`。
function bellMarker(tag) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `ag10-bell-${tag}-`));
  const marker = path.join(dir, 'marker.log');
  const script = path.join(dir, 'bell.sh');
  fs.writeFileSync(script, `#!/bin/sh\nprintf bell >> ${JSON.stringify(marker)}\n`);
  fs.chmodSync(script, 0o755);
  return {
    marker, script,
    count: () => (fs.existsSync(marker)
      ? fs.readFileSync(marker, 'utf8').split('bell').length - 1
      : 0),
  };
}

test('BELL-HOST-1: attached settlement rings once per outcome; cancel/foreground/replay stay silent', async ({ page }) => {
  test.setTimeout(240_000);
  const bell = bellMarker('main');
  const modelLog = [];
  const fake = await fakeModelServer(modelLog);
  const serve = await startServe();
  let env;
  try {
    env = await bootJointEnv(page, fake, serve, { bellCommand: bell.script });
    const tui = env.tui;
    expect(bell.count(), 'no ring may happen during boot/attach').toBe(0);

    // —— 腿 A：completed 恰一声（pre-fix 红：此分支无 notify）。——
    const verdictA = await page.locator('.verdict.completed').count();
    await pwaSubmit(page, 'first completed run');
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 30_000 }).toBe(verdictA + 1);
    await expect.poll(() => bell.count(), { timeout: 10_000 }).toBe(1);

    // —— 腿 B：主动取消不响（用户自己在键盘前按了 Stop）。——
    await pwaSubmit(page, 'orderlive slow run: create the two files and report');
    await expect.poll(() => page.evaluate(() => state.runActive), { timeout: 15_000 }).toBe(true);
    await page.click('#cancel');
    const verdictC = await page.locator('.verdict.cancelled').count();
    await expect.poll(() => page.locator('.verdict.cancelled').count(), { timeout: 30_000 }).toBe(verdictC + 1);
    await sleep(1_200);
    expect(bell.count(), 'a user-cancelled run must not ring').toBe(1);

    // —— 腿 C：前台（FocusGained）静音 —— 真实 DECSET 1004 输入序列。——
    tui.send('\x1b[I');
    await sleep(500);
    const verdictF = await page.locator('.verdict.completed').count();
    await pwaSubmit(page, 'focused run stays silent');
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 30_000 }).toBe(verdictF + 1);
    await sleep(1_200);
    expect(bell.count(), 'a run settling while the terminal is focused must stay silent').toBe(1);

    // —— 腿 D：失焦（FocusLost）恢复响一声。——
    tui.send('\x1b[O');
    await sleep(500);
    const verdictU = await page.locator('.verdict.completed').count();
    await pwaSubmit(page, 'unfocused run rings');
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 30_000 }).toBe(verdictU + 1);
    await expect.poll(() => bell.count(), { timeout: 10_000 }).toBe(2);

    // —— 腿 E：重连重放不响（journal 回放不是结算）。——
    tui.child.kill('SIGTERM');
    await sleep(1_500);
    const tui2 = startTui(serve.home, serve.project, { bellCommand: bell.script });
    env.tui = tui2;
    await tui2.waitFor('Conversation');
    await tui2.waitFor('ag10 real joint session');
    await sleep(1_200);
    expect(bell.count(), 'journal replay after reconnect must never ring').toBe(2);
    // 重连后当前会话的 run 归属恢复：再响一声。
    const verdictR = await page.locator('.verdict.completed').count();
    await pwaSubmit(page, 'post reconnect run');
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 30_000 }).toBe(verdictR + 1);
    await expect.poll(() => bell.count(), { timeout: 10_000 }).toBe(3);
  } finally { if (env) { env.shutdown(); } else { serve.child.kill('SIGTERM'); fake.server.close(); } }
});

test('BELL-HOST-1: failed run rings; CLAT_NO_BELL stays silent', async ({ page }) => {
  test.setTimeout(240_000);
  const bell = bellMarker('fail');
  const modelLog = [];
  const fake = await fakeModelServer(modelLog);
  const serve = await startServe();
  let env;
  try {
    env = await bootJointEnv(page, fake, serve, { bellCommand: bell.script });
    // failed 结算恰一声（AFK 更需要知道失败）。
    const failedBefore = await page.locator('.verdict.failed').count();
    await pwaSubmit(page, 'boomplease fail this run');
    await expect.poll(() => page.locator('.verdict.failed').count(), { timeout: 30_000 }).toBe(failedBefore + 1);
    await expect.poll(() => bell.count(), { timeout: 10_000 }).toBe(1);

    // CLAT_NO_BELL=1 重开 TUI（B-R1 F1 返工）：同一 marker 命令保持武装
    // ——静音检查必须观察**被禁止的出口**；正控＝上一段同出口已响过
    // 一声。若 NO_BELL 优先级退化（Command 赢），本段 marker 会 +1 → 红。
    env.tui.child.kill('SIGTERM');
    await sleep(1_500);
    const tui2 = startTui(serve.home, serve.project, { noBell: true, bellCommand: bell.script });
    env.tui = tui2;
    await tui2.waitFor('Conversation');
    const verdict = await page.locator('.verdict.completed').count();
    await pwaSubmit(page, 'quiet run after nobell');
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 30_000 }).toBe(verdict + 1);
    await sleep(1_200);
    expect(bell.count(), 'CLAT_NO_BELL must suppress the armed command outlet').toBe(1);
  } finally { if (env) { env.shutdown(); } else { serve.child.kill('SIGTERM'); fake.server.close(); } }
});

test('BELL-HOST-1: session switch and a late cross-session settlement stay attributed', async ({ page }) => {
  test.setTimeout(240_000);
  const bell = bellMarker('cross');
  const modelLog = [];
  const fake = await fakeModelServer(modelLog);
  const serve = await startServe();
  let env;
  try {
    env = await bootJointEnv(page, fake, serve, { bellCommand: bell.script });
    // 基线：当前会话完成 → 恰一声。
    const verdict = await page.locator('.verdict.completed').count();
    await pwaSubmit(page, 'warm run in session A');
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 30_000 }).toBe(verdict + 1);
    await expect.poll(() => bell.count(), { timeout: 10_000 }).toBe(1);

    // 空闲切新会话：切换/重放不响。
    await page.evaluate(() => rpc('session.new', {}));
    await page.waitForFunction(() => !state.runActive && document.querySelectorAll('.msg.user').length === 0, null, { timeout: 20_000 });
    await sleep(1_000);
    expect(bell.count(), 'a session switch and its replay must never ring').toBe(1);

    // —— 跨会话边界（产品栅栏勒定的真实边界）——
    // 宿主拒绝在 run 活跃时切会话（"cannot switch sessions while a run
    // is active"），且 settler 在同一锁内广播 settled 后才清 active_run：
    // 切换只可能在结算广播之后完成。真实可达边界因此是：结算响一声后
    // 立即切换，切换/重放不得再补响（同锁序：settled 先于 selection
    // notice 到达同一 SSE 流）。
    await pwaSubmit(page, 'orderlive slow run: create the two files and report');
    await expect.poll(() => page.evaluate(() => state.runActive), { timeout: 15_000 }).toBe(true);
    const verdictSlow = await page.locator('.verdict.completed').count();
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 90_000 }).toBe(verdictSlow + 1);
    await expect.poll(() => bell.count(), { timeout: 10_000 }).toBe(2);
    // 结算后立即切新会话：不得因切换/重放再响。
    await page.evaluate(() => rpc('session.new', {}));
    await page.waitForFunction(() => !state.runActive && document.querySelectorAll('.msg.user').length === 0, null, { timeout: 20_000 });
    await sleep(1_500);
    expect(bell.count(), 'a switch right after settlement must not add a second ring').toBe(2);

    // 当前会话归属恢复：新会话内完成 run 再次响一声。
    const verdict2 = await page.locator('.verdict.completed').count();
    await pwaSubmit(page, 'attribution restored run');
    await expect.poll(() => page.locator('.verdict.completed').count(), { timeout: 30_000 }).toBe(verdict2 + 1);
    await expect.poll(() => bell.count(), { timeout: 10_000 }).toBe(3);
  } finally { if (env) { env.shutdown(); } else { serve.child.kill('SIGTERM'); fake.server.close(); } }
});
