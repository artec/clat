import { expect, test } from 'bun:test';
import { ReleaseProgress, useColor } from './release-progress';

test('release color follows terminal support and explicit opt-outs', () => {
  expect(useColor({}, true)).toBe(true);
  expect(useColor({}, false)).toBe(false);
  expect(useColor({ FORCE_COLOR: '1' }, false)).toBe(true);
  for (const env of [{ NO_COLOR: '' }, { FORCE_COLOR: '0' }, { TERM: 'dumb' }, { NO_COLOR: '1', FORCE_COLOR: '1' }]) {
    expect(useColor(env, true)).toBe(false);
  }
});
test('release steps separate blocks and color markers and elapsed times independently', () => {
  const lines: string[] = [];
  const progress = new ReleaseProgress(line => lines.push(line), true);
  progress.line('start', '编译');
  progress.line('done', '完成，耗时 ' + progress.time('3分52秒'));
  progress.line('failed', '失败'); progress.line('reused', '复用缓存');
  // 行为断言（2026-10-26 文案不入 CI 契约）：四种步骤状态各占一行、
  // 完成行带时长——措辞/ANSI 配色可自由调整。
  expect(lines).toHaveLength(4);
  expect(lines[0]).toContain('编译');
  expect(lines[1]).toMatch(/完成.*3分52秒/);
  expect(lines[2]).toContain('失败');
  expect(lines[3]).toContain('复用缓存');
  const plain = new ReleaseProgress(line => lines.push(line), false);
  plain.line('done', '耗时 ' + plain.time('1分00秒'));
  expect(lines.at(-1)).toMatch(/^\n✓ .*1分00秒/);
});
test('release step records actual completion or failure without changing the result or original error', async () => {
  const lines: string[] = [];
  const progress = new ReleaseProgress(line => lines.push(line), false);
  const result = { verified: true };
  expect(await progress.step('签名', async () => {
    expect(lines).toEqual(['\n⟳ 签名']); return result;
  })).toBe(result);
  expect(lines).toEqual(['\n⟳ 签名', '\n✓ 签名完成']);
  lines.length = 0;
  const error = new Error('fixture failure');
  try { await progress.step('公证', async () => { throw error; }); throw new Error('expected failure'); }
  catch (caught) { expect(caught).toBe(error); }
  expect(lines).toEqual(['\n⟳ 公证', '\n✗ 公证失败']);
});
