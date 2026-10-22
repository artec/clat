import { appendFileSync, chmodSync, closeSync, copyFileSync, lstatSync, mkdirSync, openSync, readSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { digest, json, ROOT, run, type Runner, writeJson } from './common';

export interface BackendIdentity { commit: string; version: string; target: string; runId: string }
interface Receipt extends BackendIdentity { official: true; sha256: string; size: number }
export function executable(target: string): string { return target.includes('windows') ? 'clat.exe' : 'clat'; }
export function sourceCommit(root: string, execute: Runner = run): string {
  const commit = execute(['git', 'rev-parse', 'HEAD'], { cwd: root, quiet: true }).trim();
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('invalid source commit');
  return commit;
}
export function verifyArchitecture(path: string, target: string): void {
  if (!lstatSync(path).isFile()) throw new Error('backend must be a regular file');
  const fd = openSync(path, 'r'), header = Buffer.alloc(64);
  try {
    if (readSync(fd, header, 0, 64, 0) !== 64) throw new Error('truncated backend');
    if (target.endsWith('apple-darwin')) {
      const cpu = target.startsWith('aarch64-') ? 0x0100000c : 0x01000007;
      if (header.readUInt32LE(0) !== 0xfeedfacf || header.readUInt32LE(4) !== cpu) throw new Error('backend Mach-O architecture mismatch');
    } else if (target.endsWith('pc-windows-msvc')) {
      const offset = header.readUInt32LE(60), pe = Buffer.alloc(6);
      if (header.readUInt16LE(0) !== 0x5a4d || offset < 64 || offset > 1024 ** 2
        || readSync(fd, pe, 0, 6, offset) !== 6 || pe.readUInt32LE(0) !== 0x4550
        || pe.readUInt16LE(4) !== (target.startsWith('aarch64-') ? 0xaa64 : 0x8664)) throw new Error('backend PE architecture mismatch');
    } else throw new Error('unsupported desktop backend target');
  } finally { closeSync(fd); }
}
export function verifyBackend(folder: string, expected: BackendIdentity): string {
  const receiptPath = join(folder, 'receipt.json');
  if (!lstatSync(receiptPath).isFile()) throw new Error('backend receipt must be a regular file');
  const receipt = json<Receipt>(receiptPath), binary = join(folder, executable(expected.target));
  for (const field of ['commit', 'version', 'target', 'runId'] as const) if (!expected[field] || receipt[field] !== expected[field]) throw new Error('backend identity mismatch: ' + field);
  verifyArchitecture(binary, expected.target);
  if (receipt.official !== true || !/^[a-f0-9]{40}$/.test(receipt.commit) || !/^\d+$/.test(receipt.runId)
    || receipt.size !== lstatSync(binary).size || receipt.sha256 !== digest(binary)) throw new Error('backend digest or official identity mismatch');
  return binary;
}
export function recordBackend(source: string, output: string, identity: BackendIdentity, execute: Runner = run): void {
  verifyArchitecture(source, identity.target);
  if (execute([source, '--version'], { quiet: true }).trim() !== 'clat ' + identity.version) throw new Error('backend executable version mismatch');
  mkdirSync(output, { recursive: true });
  const binary = join(output, executable(identity.target));
  copyFileSync(source, binary); chmodSync(binary, 0o755);
  writeJson(join(output, 'receipt.json'), { ...identity, official: true, size: lstatSync(binary).size, sha256: digest(binary) });
  verifyBackend(output, identity);
}
export async function backendMain(args = process.argv.slice(2)): Promise<void> {
  const { positionals, values } = parseArgs({ args, allowPositionals: true, options: { target: { type: 'string' }, output: { type: 'string', default: 'dist-backend' } } });
  const { DesktopRelease } = await import('./release'); const release = new DesktopRelease();
  if (!values.target || !release.records('desktop').some(r => r.target === values.target)) throw new Error('supported desktop target required');
  const identity = { target: values.target, version: release.version(), commit: sourceCommit(ROOT), runId: process.env.GITHUB_RUN_ID ?? '' };
  if (process.env.CLAT_OFFICIAL_RELEASE !== '1' || !/^\d+$/.test(identity.runId) || identity.commit !== process.env.GITHUB_SHA) throw new Error('official CI build identity required');
  if (positionals[0] === 'probe') {
    if (process.env.GITHUB_REPOSITORY !== 'artec/clat' || !process.env.GITHUB_TOKEN || !process.env.GITHUB_OUTPUT) throw new Error('CI artifact query configuration missing');
    const response = await fetch(`https://api.github.com/repos/artec/clat/actions/runs/${identity.runId}/artifacts?name=backend-${identity.target}`, {
      headers: { Authorization: 'Bearer ' + process.env.GITHUB_TOKEN, Accept: 'application/vnd.github+json' }, signal: AbortSignal.timeout(30000),
    });
    if (!response.ok) throw new Error('artifact query failed: ' + response.status);
    const data = await response.json() as { artifacts: { name: string; expired: boolean }[] };
    const found = data.artifacts.some(a => a.name === 'backend-' + identity.target && !a.expired);
    appendFileSync(process.env.GITHUB_OUTPUT, `found=${found}\n`); return;
  }
  const binary = join(release.targetDirectory(), identity.target, 'release', executable(identity.target));
  const output = resolve(values.output!);
  if (positionals[0] === 'record') recordBackend(binary, output, identity);
  else if (positionals[0] === 'restore') { const source = verifyBackend(output, identity); mkdirSync(join(binary, '..'), { recursive: true }); copyFileSync(source, binary); chmodSync(binary, 0o755); }
  else throw new Error('backend.ts record|restore --target TARGET --output DIR');
}
if (import.meta.main) backendMain().catch(error => { console.error(error.message); process.exitCode = 1; });
