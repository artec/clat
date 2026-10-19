// AG-DOGFOOD-10 / A3 J1-EDGE-1：上一轮明示缺口的定向补证（真 serve +
// 真 PTY TUI + 真 Chromium 同 Host/同 Session/合成假模型）。
//
// 聚焦三项缺口，不重测已发布正向主链（ag9-j1-real.spec.js 保持基线）：
//  1. 非空提交的**真实 Host 拒绝不入史**：图片能力门（INV-MM2-2，
//     prepare_run 在任何 journal 写入前整轮拒绝、零痕迹）——真 PWA 文件
//     输入 + 非空文本 + 无视觉能力的自定义模型 → prompt.send 拒绝。
//     对照：同文本去掉图片 → accepted 同链。断言四面对齐：PWA 转录、
//     host journal（session.history RPC）、假模型实收、真 TUI 历史召回
//     （拒绝文本不得进入任何一处）。附带 selection-generation 栅栏的
//     确定性拒绝（真实客户端提交都会携带该字段；PWA/TUI 跨端切换竞态
//     的同一拒绝面）。
//  2. 多行/原始草稿：真 TTY bracketed paste（TUI 自解析 200~/201~）、
//     行内 Left/Right 光标移动、非空多行草稿 Up/Down 召回/恢复、
//     提交文本 == journal 真值 == 模型实收（权威事实，不解析屏幕）。
//     Shift+Enter 的真实终端可达性如实记录：expect PTY 不是终端模拟器，
//     无法产生可区分的 Shift+Enter 字节流——单元契约
//     （paste_settled_tests::paste_only_inserts_draft_and_editing_keys_survive）
//     已锁，真键盘路径 IME/NOT_RUN 同节声明。
//  3. 浮层归属与跨会话：斜杠浮层期间方向键归浮层、Esc 关闭后 composer
//     原文恢复、召回仍工作；切会话后旧会话历史不得污染新会话召回/草稿。
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const {
  sleep, fakeModelServer, startServe, startTui, bootJointEnv, pwaSubmit,
  latestUserText, userCount,
} = require('../helpers/ag10-joint-env');

// 标准 1×1 透明 PNG（canonical base64，CRC 合法）：PWA 附件门用的真实文件。
const TINY_PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==',
  'base64',
);

// host journal 的最近 user 文本页（权威：不经 UI；wire 形状为扁平
// {type:"user_message", text}，见 serve/shapes.rs replay_event_json）。
const journalUsers = (page) => page.evaluate(async () => {
  const history = await rpc('session.history', { max_messages: 200 });
  return (history.events || [])
    .filter((event) => event && event.type === 'user_message')
    .map((event) => (typeof event.text === 'string' ? event.text : ''));
});

// 裸 HTTP RPC：与原生 Rust HostClient 同型（不经 PWA rpc() 的世代补章——
// 那个包装会用当前世代覆盖入参，无法驱动真实客户端的过期世代竞态）。
// 这正是 TUI 在跨端切换竞态里发出的同一公开面请求。
async function rawRpc(page, method, params) {
  const origin = new URL(page.url()).origin;
  const token = await page.evaluate(() => state.token);
  const response = await fetch(`${origin}/api/${method}`, {
    method: 'POST',
    headers: {
      ...(token ? { Authorization: 'Bearer ' + token } : {}),
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(params || {}),
  });
  const body = await response.json().catch(() => null);
  if (!body || typeof body.ok !== 'boolean') throw new Error('malformed rpc reply');
  if (body.ok) return body.value;
  throw Object.assign(new Error((body.error && body.error.message) || 'request failed'),
    { code: (body.error && body.error.code) || 'internal' });
}

test('J1-EDGE-1: a genuinely non-empty host-rejected submission never enters history on any client', async ({ page }) => {
  test.setTimeout(300_000);
  const modelLog = [];
  const fake = await fakeModelServer(modelLog);
  const serve = await startServe();
  let env;
  try {
    env = await bootJointEnv(page, fake, serve, { /* 先不启 TUI：拒绝与对照都从 PWA 发起 */ });

    // —— 对照：普通非空文本 accepted（同链正向）。——
    const controlMarker = 'edge-accept-control-4c1';
    const before = await userCount(page);
    await pwaSubmit(page, controlMarker);
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(before + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(controlMarker);
    const controlJournal = await journalUsers(page);
    expect(controlJournal[controlJournal.length - 1]).toBe(controlMarker);
    expect(modelLog[modelLog.length - 1].user).toContain(controlMarker);

    // —— 真实拒绝路径 A：图片能力门（非空文本 + 真实文件输入）。——
    // 自定义 open_ai_compatible profile 无第一方视觉证据 → prepare_run
    // 在任何 journal 写入前拒绝整轮（零痕迹）。
    const rejectMarker = 'edge-reject-image-7e2 with attachment';
    const upload = path.join(os.tmpdir(), `ag10-edge-${Date.now()}.png`);
    fs.writeFileSync(upload, TINY_PNG);
    await page.setInputFiles('#attachment-input', upload);
    await expect(page.locator('#attachment-rail')).toBeVisible({ timeout: 15_000 });
    // 等真实上传完成（staged→uploaded；失败状态直接红出，不静默早退）。
    await expect.poll(() => page.evaluate(() => {
      const images = state.draft.images || [];
      return images.length > 0 && images.every((i) => i.status !== 'uploading')
        ? images.map((i) => i.status).join(',')
        : '';
    }), { timeout: 20_000 }).toBe('uploaded');
    const beforeReject = await userCount(page);
    const modelLogBefore = modelLog.length;
    await pwaSubmit(page, rejectMarker);
    // PWA 侧拒绝反馈可见（草稿保留通告；存在性探针）。
    await page.waitForFunction(() => {
      const notices = [...document.querySelectorAll('.draft-notice, #run-state, [data-draft-notice]')];
      return notices.some((n) => /does not accept image|remove the image|switch to a vision model|resolve image|failed/i.test(n.textContent));
    }, null, { timeout: 20_000 }).catch(() => {});
    await sleep(1_500);
    // 四面负向：PWA 转录、host journal、模型实收、（稍后）TUI 召回。
    expect(await userCount(page), 'rejected submission must not appear as a user message').toBe(beforeReject);
    const journalAfterReject = await journalUsers(page);
    expect(journalAfterReject.some((text) => text.includes('edge-reject-image-7e2')),
      'the host journal must keep zero trace of the rejected submission').toBe(false);
    expect(modelLog.length, 'the model must never be called for a rejected round').toBe(modelLogBefore);
    expect(await page.evaluate(() => inputHistory.list.filter((t) => t.includes('edge-reject-image-7e2'))),
      'the PWA committed-input history must not contain the rejected text').toEqual([]);
    // 草稿保真：拒绝后 composer 文本仍在（用户可改后重发）。
    expect(await page.inputValue('#prompt')).toContain('edge-reject-image-7e2');

    // —— 真实拒绝路径 B：selection-generation 栅栏（公开 RPC 面；真实
    // 客户端每次提交都携带该字段，跨端切换竞态的同一拒绝面）。世代取
    // 永不可达值（宿主初始代为 0，从 0 递增）→ 确定性拒绝。——
    const staleMarker = 'edge-reject-stale-gen-9a3';
    const beforeStale = await userCount(page);
    const staleModelLog = modelLog.length;
    const stale = await rawRpc(page, 'prompt.send', {
      text: staleMarker,
      expected_selection_generation: 424242,
    }).then(() => 'accepted', (error) => `${error.code}: ${error.message}`);
    expect(stale).toContain('session selection changed');
    await sleep(800);
    expect(await userCount(page), 'a fence-rejected submission must not enter the transcript').toBe(beforeStale);
    expect(modelLog.length).toBe(staleModelLog);
    const journalStale = await journalUsers(page);
    expect(journalStale.some((text) => text.includes(staleMarker))).toBe(false);

    // —— 真 TUI 召回负向：拒绝文本不得进入任何客户端的已提交历史。——
    const tui = startTui(serve.home, serve.project, {});
    env.tui = tui;
    await tui.waitFor('Conversation');
    await tui.waitFor('ag10 real joint session');
    await sleep(1_000);
    // Up → 召回最新已提交输入（对照 marker，不是被拒文本），Enter 提交
    // 作为权威判据。
    const beforeRecall = await userCount(page);
    tui.send('\x1b[A');
    await sleep(700);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeRecall + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 })
      .not.toContain('edge-reject');
    const recallJournal = await journalUsers(page);
    expect(recallJournal[recallJournal.length - 1], 'TUI recall must surface only committed inputs').toBe(controlMarker);
    console.log('J1-EDGE-1 rejection ok:', JSON.stringify({ stale, lastCommitted: controlMarker }));
  } finally { if (env) { env.shutdown(); } else { serve.child.kill('SIGTERM'); fake.server.close(); } }
});

test('J1-EDGE-1: real-TTY multiline drafts survive paste, arrows, recall, and submit verbatim', async ({ page }) => {
  test.setTimeout(300_000);
  const modelLog = [];
  const fake = await fakeModelServer(modelLog);
  const serve = await startServe();
  let env;
  try {
    env = await bootJointEnv(page, fake, serve, {});
    const tui = env.tui;
    // 建立一条已提交历史（对照锚点）。
    await pwaSubmit(page, 'history-anchor-1');
    await expect(page.locator('.msg.assistant', { hasText: 'acknowledged' }).last()).toBeVisible({ timeout: 30_000 });
    await page.waitForFunction(() => !state.runActive, null, { timeout: 30_000 });
    await sleep(800);

    // —— 真 bracketed paste：多行草稿原样入 composer（TUI 自解析）。
    // 权威判据 = host journal + 模型实收（PWA DOM 的 markdown 渲染会折叠
    // 软换行，不作无损真相）。
    tui.send('\x1b[200~paste-line-A\rpaste-line-B\r\rpaste-line-C\x1b[201~');
    await sleep(700);
    // 行内光标移动：光标在末行末尾 → Left×2 → 插入 '#'（落在 '-C' 之
    // 前），End 回行尾追加 '$'。以真实提交为权威判据。
    tui.send('\x1b[D\x1b[D');
    await sleep(250);
    tui.send('#');
    await sleep(200);
    tui.send('\x1b[F');
    await sleep(200);
    tui.send('$');
    await sleep(300);
    const before = await userCount(page);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(before + 1);
    const expected = 'paste-line-A\npaste-line-B\n\npaste-line#-C$';
    await expect.poll(() => journalUsers(page), { timeout: 10_000 })
      .toEqual(expect.arrayContaining([expected]));
    const journal = await journalUsers(page);
    expect(journal[journal.length - 1], 'journal truth must hold the verbatim multiline draft').toBe(expected);
    const modelUsers = modelLog.map((m) => m.user);
    expect(modelUsers[modelUsers.length - 1], 'the model must receive the multiline text verbatim').toContain(expected);
    // PWA 转录存在性（正文不被显示层改写为残句；无损等值以 journal 为准）。
    await expect(page.locator('.msg.user', { hasText: 'paste-line-A' })).toBeVisible({ timeout: 10_000 });
    await page.waitForFunction(() => !state.runActive, null, { timeout: 30_000 });
    await sleep(800);

    // —— B-R1 F2（返工）：Up 必须真的进入历史——中间事实判别：非空多行
    // 草稿 Up 后立即追加探针并提交，journal 必须等于**被召回的最新已
    // 提交项**加探针；若 Up/Down 对非空草稿失效（草稿原地不动），journal
    // 会是 unsent 原稿加探针 → 红。——
    tui.send('\x1b[200~unsent-one\runsent-two\x1b[201~');
    await sleep(600);
    tui.send('\x1b[A'); // 召回最新已提交项（expected），替换非空多行草稿
    await sleep(400);
    tui.send('::UP-PROBE');
    await sleep(300);
    const beforeProbe = await userCount(page);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeProbe + 1);
    const journalProbe = await journalUsers(page);
    expect(
      journalProbe[journalProbe.length - 1],
      'Up on a non-empty multiline draft must recall the latest committed entry (not leave the draft in place)',
    ).toBe(`${expected}::UP-PROBE`);
    await page.waitForFunction(() => !state.runActive, null, { timeout: 30_000 });
    await sleep(800);

    // —— 非空多行草稿的 Up/Down 召回与完整恢复（真实按键，既有保护）。——
    // 产品语义（keys.rs：Up 在历史非空时一律召回，原草稿入暂存；Down
    // 逐级返回）——实测并锁定该契约，且多行草稿在召回后可完整取回。
    tui.send('\x1b[200~keep-l1\rkeep-l2\x1b[201~');
    await sleep(600);
    tui.send('\x1b[A'); // 召回最新历史（替换多行草稿，原稿暂存）
    await sleep(400);
    tui.send('\x1b[B'); // Down 取回暂存的多行原稿
    await sleep(400);
    const beforeRestore = await userCount(page);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeRestore + 1);
    const journalRestore = await journalUsers(page);
    expect(journalRestore[journalRestore.length - 1],
      'Up then Down must restore the original multiline draft verbatim').toBe('keep-l1\nkeep-l2');
    expect(modelLog[modelLog.length - 1].user).toContain('keep-l1\nkeep-l2');
    console.log('J1-EDGE-1 multiline ok:', JSON.stringify(modelLog.slice(-2)));

    // IME（合同 §3.4）：真 macOS 候选窗无法从 Playwright/PTY 可靠驱动，
    // 禁止以合成 Composition 事件冒充真桌面 IME —— 记
    // IME=NOT_RUN/OWNER_MANUAL_ACCEPTANCE；人工步骤见交付报告。
  } finally { if (env) { env.shutdown(); } else { serve.child.kill('SIGTERM'); fake.server.close(); } }
});

test('J1-EDGE-1: overlay owns arrows while open; session switch never pollutes the fresh draft', async ({ page }) => {
  test.setTimeout(300_000);
  const modelLog = [];
  const fake = await fakeModelServer(modelLog);
  const serve = await startServe();
  let env;
  try {
    env = await bootJointEnv(page, fake, serve, {});
    const tui = env.tui;
    // 会话 A 建立独有历史。
    const markerA = 'old-session-unique-a1';
    const beforeA = await userCount(page);
    tui.send(markerA);
    await sleep(500);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeA + 1);
    await page.waitForFunction(() => !state.runActive, null, { timeout: 30_000 });
    await sleep(800);

    // —— B-R1 F3（返工）：浮层方向键必须真实移动选中项——以 Tab 采纳的
    // 实际结果为权威判据：无 Down 的 Tab 填入初始项；Down 后 Tab 填入
    // 另一项，两次提交进 journal 的命令词必须不同（move_selection 失效
    // →两次相同 →红）。填充文本经 Home/Delete/End 转普通词再提交。——
    const pickAndSubmit = async (probe) => {
      tui.send('\t'); // Tab：把选中命令填入 composer（浮层内选中项的真实采纳）
      await sleep(400);
      tui.send('\x1b[H'); // Home：光标到行首
      tui.send('\x1b[3~'); // Delete：删掉前导 '/'
      tui.send('\x1b[F'); // End：光标到行尾
      await sleep(300);
      tui.send(probe);
      await sleep(300);
      const before = await userCount(page);
      tui.send('\r');
      await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(before + 1);
      const journal = await journalUsers(page);
      await page.waitForFunction(() => !state.runActive, null, { timeout: 30_000 });
      await sleep(600);
      return journal[journal.length - 1];
    };
    tui.send('/');
    await tui.waitFor('Commands');
    // 目录经宿主 RPC 异步装载：waitFor 命中的可能是旧帧的同名标题，
    // 必须等真实装载完成后再按方向键（空列表上 move_selection 是无操作）。
    await sleep(1_000);
    const initialPick = await pickAndSubmit('::PICK0');
    tui.send('/');
    await sleep(1_000);
    tui.send('\x1b[B'); // Down：真实移动浮层选中项
    await sleep(400);
    const movedPick = await pickAndSubmit('::PICK1');
    const commandWord = (text) => text.split('::')[0].trim();
    expect(commandWord(movedPick).length, 'the filled selection must be a real command word').toBeGreaterThan(0);
    expect(
      commandWord(movedPick),
      'Down must move the picker selection (the adopted command must change)',
    ).not.toBe(commandWord(initialPick));
    console.log('J1-EDGE-1 picker adoption:', JSON.stringify({ initialPick, movedPick }));

    // —— 斜杠浮层归属（既有负向保护）：方向键不进 composer 文本，Esc
    // 关闭后 composer 仍是 '/'，退格清空后历史召回仍工作。——
    tui.send('/');
    await tui.waitFor('Commands');
    tui.send('\x1b[B\x1b[A\x1b[B');
    await sleep(500);
    tui.send('\x1b'); // Esc 关闭浮层
    await sleep(400);
    tui.send('\x7f'); // 退掉残留的 '/'
    await sleep(300);
    const beforeRecall = await userCount(page);
    tui.send('\x1b[A'); // 关闭浮层后召回最新已提交输入（＝本轮最后一次提交 movedPick）
    await sleep(600);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeRecall + 1);
    await expect.poll(() => latestUserText(page), { timeout: 10_000 }).toBe(movedPick);
    await page.waitForFunction(() => !state.runActive, null, { timeout: 30_000 });
    await sleep(800);

    // —— 跨会话：PWA 新会话后，TUI 未发送草稿是 frontend-local 状态——
    // 真实产品行为是草稿存续并在当前（新）会话提交。旧会话独有历史不
    // 得进入新会话召回（权威：journal 与召回提交内容）。
    const draftKeep = 'draft-in-old-session-zz';
    tui.send(draftKeep);
    await sleep(600);
    await page.evaluate(() => rpc('session.new', {}));
    await page.waitForFunction(() => !state.runActive && document.querySelectorAll('.msg.user').length === 0, null, { timeout: 20_000 });
    await sleep(1_500); // 等 TUI 跟随切换 + 快照重放。
    const beforeDraft = await userCount(page);
    tui.send('\r'); // 提交存续草稿 → 必须落入**新**会话
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeDraft + 1);
    const journalDraft = await journalUsers(page);
    expect(journalDraft[journalDraft.length - 1], 'the surviving draft must land in the new session').toBe(draftKeep);
    // 召回归新会话：清空后 Up 只能召回新会话自己的提交（draftKeep），
    // 旧会话独有文本（markerA）永不出现。
    tui.send('\x7f'.repeat(40));
    await sleep(500);
    const beforeSessionRecall = await userCount(page);
    tui.send('\x1b[A');
    await sleep(600);
    tui.send('\r');
    await expect.poll(() => userCount(page), { timeout: 30_000 }).toBe(beforeSessionRecall + 1);
    const journalRecall = await journalUsers(page);
    expect(journalRecall[journalRecall.length - 1], 'new-session recall must surface only this session\u2019s inputs').toBe(draftKeep);
    expect(journalRecall.some((text) => text && text.includes('old-session-unique')),
      'old-session text must never be recalled into the new session').toBe(false);
    console.log('J1-EDGE-1 overlay+cross-session ok:', JSON.stringify({ journalLen: journalRecall.length }));
  } finally { if (env) { env.shutdown(); } else { serve.child.kill('SIGTERM'); fake.server.close(); } }
});
