import { createHash } from 'node:crypto';
import { closeSync, fsyncSync, mkdirSync, mkdtempSync, openSync, readFileSync, readSync, rmSync, writeFileSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { StringDecoder } from 'node:string_decoder';

export const ROOT = resolve(import.meta.dir, '../..');
export const DESKTOP = join(ROOT, 'desktop');
export type Environment = Record<string, string | undefined>;
export interface RunOptions { cwd?: string; env?: Environment; timeout?: number; quiet?: boolean }
export type Runner = (args: string[], options?: RunOptions) => string;

export function cleanEnv(extra: Environment = {}, source: Environment = process.env): Environment {
  const env = Object.fromEntries(Object.entries(source).filter(([key]) =>
    !/PASSWORD|PRIVATE_KEY|TOKEN/.test(key) && !key.startsWith('APPLE_') && key !== 'CLAT_OFFICIAL_RELEASE'));
  return { ...env, ...extra };
}

export function redact(value: string, sources: Environment[] = [process.env]): string {
  for (const env of sources) for (const [key, secret] of Object.entries(env)) {
    if (secret && /PASSWORD|PRIVATE_KEY|TOKEN/.test(key)) value = value.replaceAll(secret, '[redacted]');
  }
  return value;
}

export const run: Runner = (args, options = {}) => {
  // Never print arguments: notarytool arguments may contain Apple credentials.
  const result = Bun.spawnSync(args, { cwd: options.cwd ?? ROOT, env: cleanEnv(options.env),
    stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', timeout: options.timeout });
  const output = redact(result.stdout.toString() + result.stderr.toString(), [process.env, options.env ?? {}]);
  if (!options.quiet && output) process.stdout.write(output.slice(-16000));
  if (!result.success) throw new Error(`${basename(args[0]!)} failed (${result.exitCode})`);
  return output;
};

export function digest(path: string): string {
  const hash = createHash('sha256');
  const fd = openSync(path, 'r');
  try {
    const buffer = Buffer.alloc(1024 * 1024);
    for (let count; (count = readSync(fd, buffer)) > 0;) hash.update(buffer.subarray(0, count));
    return hash.digest('hex');
  } finally { closeSync(fd); }
}

export function checksum(path: string): void { writeFileSync(path + '.sha256', `${digest(path)}  ${basename(path)}\n`); }
export function json<T = unknown>(path: string): T { return JSON.parse(readFileSync(path, 'utf8')); }
export function writeJson(path: string, value: unknown): void { writeFileSync(path, JSON.stringify(value, null, 2) + '\n'); }

export function privateWrite(path: string, value: Uint8Array): void {
  const fd = openSync(path, 'wx', 0o600);
  try { writeFileSync(fd, value); fsyncSync(fd); } finally { closeSync(fd); }
}

export async function withTemp<T>(prefix: string, action: (folder: string) => T | Promise<T>, parent = tmpdir()): Promise<T> {
  mkdirSync(parent, { recursive: true });
  const folder = mkdtempSync(join(parent, prefix));
  try { return await action(folder); } finally { rmSync(folder, { recursive: true, force: true }); }
}

export function decodeWrapper(value: string | Buffer): Buffer {
  const text = value.toString().replace(/\s/g, '');
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(text)) throw new Error('Invalid base64 wrapper');
  return Buffer.from(text, 'base64');
}

export async function readPassword(prompt: string): Promise<string> {
  const input = process.stdin;
  if (!input.isTTY || typeof input.setRawMode !== 'function') throw new Error('请在本机交互终端隐藏输入密码。');
  return new Promise((accept, reject) => {
    const wasRaw = input.isRaw;
    let value = '';
    const decoder = new StringDecoder('utf8');
    const finish = (error?: Error) => {
      input.off('data', receive); input.off('end', ended);
      input.setRawMode(wasRaw); input.pause(); process.stdout.write('\n');
      if (error) reject(error); else accept(value);
    };
    const ended = () => finish(new Error('已取消。'));
    const receive = (chunk: Buffer) => {
      for (const character of decoder.write(chunk)) {
        if (character === '\r' || character === '\n') return finish();
        if (character === '\u0003' || character === '\u0004') return ended();
        if (character === '\u007f' || character === '\b') value = Array.from(value).slice(0, -1).join('');
        else if (character >= ' ' && character !== '\u007f') value += character;
      }
    };
    process.stdout.write(prompt); input.setRawMode(true); input.resume();
    input.on('data', receive); input.once('end', ended);
  });
}

export function nativeTarget(system = process.platform, machine: string = process.arch): string {
  const architecture = ({ arm64: 'aarch64', amd64: 'x86_64', x64: 'x86_64' } as Record<string, string>)[machine.toLowerCase()] ?? machine;
  const suffix = ({ darwin: 'apple-darwin', win32: 'pc-windows-msvc' } as Record<string, string>)[system];
  if (!['aarch64', 'x86_64'].includes(architecture) || !suffix) throw new Error('桌面版目前支持 macOS 与 Windows 的 x64/ARM64。');
  return `${architecture}-${suffix}`;
}
