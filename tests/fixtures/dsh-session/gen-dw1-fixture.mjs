// DW-1（2026-09-09）：DSH 0.1.3-alpha.2 两个必填评价事件的 golden
// generator——`feedback/message-put`、`feedback/message-delete`（DV-5
// 第三次同类词汇竞争；引入见 alpha.2 known-event-types.ts +2）。
//
// 铸法与 gen-dv5-fixture.mjs 相同：钉靶写路径不产 v0 字节，退回
// 原语级铸造（node:zlib zstd + JSON.stringify，帧结构与信封字段序
// 与 B8 金样一致：头帧 + 单体帧、envelope {type,seq,time,data[,
// surfaceOp]}）。payload 形状取自 DSH 源（0.1.5-alpha.1 树，该两型
// 自 alpha.2 引入后未变）：
//   - feedback/message-put：packages/feedback/message-feedback/src/index.ts
//     putRequest 分支（{sessionId, item:{messageId,rating,note?,version,
//     createdAt,updatedAt}}，必填落盘、log-only 无 surfaceOp）
//   - feedback/message-delete：同文件 deleteRequest 分支
//     （{sessionId, messageId}，必填落盘、log-only）
//
// 运行（在 clat 仓库根）：node tests/fixtures/dsh-session/gen-dw1-fixture.mjs
//（产物直接落本目录，提交进库；.gitattributes 的
//  `tests/fixtures/dsh-session/** -text` 已覆盖。）
import { zstdCompressSync, constants } from 'node:zlib';
import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const CHECKSUM = { params: { [constants.ZSTD_c_checksumFlag]: 1 } };
const ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0b05';
const CREATED = 1787385720000;
const CWD = '/Users/deng/Documents/GitHub/clat';

const header = {
  type: 'session', version: 0, id: ID, createdAt: CREATED,
  cwd: CWD, delegationDepth: 0,
};

let seq = 0;
let time = CREATED + 1;
const ev = (type, data, extra = {}) => ({ type, seq: seq++, time: time++, data, ...extra });

const events = [
  ev('turn/start', { turn: 1 }),
  ev('user/message', {
    id: '018f2a64-9d3f-7cde-8123-9a4f2b6c1001',
    role: 'user',
    content: [{ type: 'text', text: 'answer, then the user rates and withdraws the rating' }],
    source: { kind: 'user' },
  }, { surfaceOp: 'append' }),
  ev('step/start', { turn: 1, step: 1 }),
  // 两个 DW-1 目标事件：必需信封（无 ignorable）、无 surfaceOp。
  ev('feedback/message-put', {
    sessionId: ID,
    item: {
      messageId: '018f2a64-9d3f-7cde-8123-9a4f2b6c1002',
      rating: 'positive',
      note: 'sharp and concise',
      version: '018f2a64-9d3f-7cde-8123-9a4f2b6c2001',
      createdAt: CREATED + 2,
      updatedAt: CREATED + 2,
    },
  }),
  ev('feedback/message-delete', {
    sessionId: ID,
    messageId: '018f2a64-9d3f-7cde-8123-9a4f2b6c1002',
  }),
  ev('assistant/message', {
    turn: 1,
    step: 1,
    message: {
      id: '018f2a64-9d3f-7cde-8123-9a4f2b6c1002',
      role: 'assistant',
      content: [{ type: 'text', text: 'rated, then the rating was withdrawn' }],
      source: { kind: 'model', provider: 'deepseek', model: 'deepseek-chat' },
    },
  }, { surfaceOp: 'append' }),
  ev('turn/end', { turn: 1, reason: { kind: 'completed' } }),
];

const headerFrame = zstdCompressSync(Buffer.from(JSON.stringify(header) + '\n', 'utf8'), CHECKSUM);
const bodyFrame = zstdCompressSync(
  Buffer.from(events.map((event) => JSON.stringify(event) + '\n').join(''), 'utf8'),
  CHECKSUM,
);
const out = join(dirname(fileURLToPath(import.meta.url)), 'feedback-session.jsonl.zstd');
writeFileSync(out, Buffer.concat([headerFrame, bodyFrame]));
console.log('generated', out, { events: events.length });
