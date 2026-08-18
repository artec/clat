// Generate DSH-style golden bytes with Node's own zstd + JSON.stringify
// (the exact primitives deepseek-harness uses at the pinned revision).
import { zstdCompressSync, constants } from 'node:zlib';
import { writeFileSync } from 'node:fs';

const CHECKSUM = { params: { [constants.ZSTD_c_checksumFlag]: 1 } };

// Header in DSH toHeaderLine insertion order.
const header = {
  type: 'session', version: 0,
  id: '018f2a64-9d3f-7cde-8123-9a4f2b6c0001',
  createdAt: 1723980000000,
  cwd: '/Users/deng/Documents/GitHub/clat',
  delegationDepth: 0,
};
// One event with DSH envelope insertion order (type, seq, time, data, surfaceOp).
const event = {
  type: 'user/message', seq: 0, time: 1723980000001,
  data: {
    id: '018f2a64-aaaa-7000-8000-000000000002',
    role: 'user',
    content: [{ type: 'text', text: 'héllo "quoted" \\ back /slash\ttab' }],
    source: { kind: 'user' },
  },
  surfaceOp: 'append',
};
// A packed text-chunks row in DSH field order.
const row = {
  type: 'text-chunks', seq0: 1, time0: 1723980000002,
  data: { turn: 1, step: 0, index: 0, dt: [3, 2], texts: ['He', 'l', 'lo'] },
};

const headerLine = JSON.stringify(header) + '\n';
const eventLine = JSON.stringify(event);
const rowLine = JSON.stringify(row);
writeFileSync((process.env.CLAT_DSH_FIXTURES || '/tmp/clat-interop/') + 'header-line.txt', headerLine);
writeFileSync((process.env.CLAT_DSH_FIXTURES || '/tmp/clat-interop/') + 'event-line.txt', eventLine);
writeFileSync((process.env.CLAT_DSH_FIXTURES || '/tmp/clat-interop/') + 'row-line.txt', rowLine);

const headerFrame = zstdCompressSync(Buffer.from(headerLine, 'utf8'), CHECKSUM);
const bodyFrame = zstdCompressSync(Buffer.from(eventLine + '\n' + rowLine + '\n', 'utf8'), CHECKSUM);
writeFileSync((process.env.CLAT_DSH_FIXTURES || '/tmp/clat-interop/') + 'header-frame.bin', headerFrame);
writeFileSync((process.env.CLAT_DSH_FIXTURES || '/tmp/clat-interop/') + 'body-frame.bin', bodyFrame);
writeFileSync((process.env.CLAT_DSH_FIXTURES || '/tmp/clat-interop/') + 'log-two-frames.bin', Buffer.concat([headerFrame, bodyFrame]));
console.log('generated', { headerFrame: headerFrame.length, bodyFrame: bodyFrame.length });
