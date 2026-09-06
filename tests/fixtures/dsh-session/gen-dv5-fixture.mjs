// DV-5（2026-09-06）：DSH 0.1.2-alpha.4+ 三个 v0 必填事件的 golden
// generator——`model/selection`、`session-log-deepseek/delivery-accepted`、
// `subagent/model-selection-policy`（引入提交 822d735356 2026-08-25）。
//
// 出处说明：B8 时代的金样由钉靶 DSH 真实写路径产出；但 0.1.3 钉靶
// （d347e70390）的写路径已钉死 v2（`SESSION_FORMAT_VERSION = 2`，无
// 版本旋钮——session-persistence-jsonl/index.ts 构造器校验 catalog
// currentVersion），v0 字节无法再由 DSH 产出。本脚本退回 regen.mjs 的
// 原语级铸法（node:zlib zstd + JSON.stringify，帧结构/信封字段序与
// B8 金样一致：头帧 + 单体帧、envelope {type,seq,time,data[,surfaceOp]}），
// payload 形状取自 DSH 0.1.3 源三个写点：
//   - model/selection：session-controller/src/agent.ts selectForNextRequest
//     （AgentModelSelection {provider, model, reasoningEffort?}，必填落盘）
//   - subagent/model-selection-policy：tool-subagent/src/model-selection-state.ts
//     （{allowedModels:[{provider,model}]}，log-only、无 surfaceOp）
//   - session-log-deepseek/delivery-accepted：session-log-deepseek/src/types.ts
//     （{sessionId, sessionFormatVersion?, throughSeq}）
//
// 运行（在 clat 仓库根）：node tests/fixtures/dsh-session/gen-dv5-fixture.mjs
//（产物直接落本目录，提交进库；.gitattributes 已有 `-text` 覆盖。）
import { zstdCompressSync, constants } from 'node:zlib';
import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const CHECKSUM = { params: { [constants.ZSTD_c_checksumFlag]: 1 } };
const ID = '018f2a64-9d3f-7cde-8123-9a4f2b6c0b04';
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
    content: [{ type: 'text', text: 'pick a model for this turn' }],
    source: { kind: 'user' },
  }, { surfaceOp: 'append' }),
  ev('step/start', { turn: 1, step: 1 }),
  // 三个 DV-5 目标事件：必需信封（无 ignorable）、无 surfaceOp。
  ev('model/selection', {
    provider: 'deepseek',
    model: 'deepseek-chat',
    reasoningEffort: 'high',
  }),
  ev('subagent/model-selection-policy', {
    allowedModels: [
      { provider: 'deepseek', model: 'deepseek-chat' },
      { provider: 'openai', model: 'gpt-5' },
    ],
  }),
  ev('session-log-deepseek/delivery-accepted', {
    sessionId: ID,
    sessionFormatVersion: 0,
    throughSeq: 2,
  }),
  ev('assistant/message', {
    turn: 1,
    step: 1,
    message: {
      id: '018f2a64-9d3f-7cde-8123-9a4f2b6c1002',
      role: 'assistant',
      content: [{ type: 'text', text: 'model selected for the run' }],
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
const out = join(dirname(fileURLToPath(import.meta.url)), 'model-selection-session.jsonl.zstd');
writeFileSync(out, Buffer.concat([headerFrame, bodyFrame]));
console.log('generated', out, { events: events.length });
