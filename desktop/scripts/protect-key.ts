import { lstatSync, readFileSync, renameSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { cleanEnv, decodeWrapper, DESKTOP, privateWrite, readPassword, ROOT, withTemp } from './common';

export function minisign(args: string[], passwords: string[]): void {
  // A new session has no controlling tty; Minisign reads only the anonymous pipe.
  const result = Bun.spawnSync(['minisign', ...args], { stdin: Buffer.from(passwords.join('\n') + '\n'), detached: true,
    env: cleanEnv(), stdout: 'pipe', stderr: 'pipe', timeout: 60000 });
  if (!result.success) throw new Error('Minisign 操作失败；原私钥保持原样。请检查原密码。');
}
export function passwords(old: string, next: string, repeat: string): [string, string] {
  if (!next || next !== repeat) throw new Error('新密码不能为空，且两次输入必须一致；未修改私钥。');
  if (/[\r\n\0]/.test(old + next)) throw new Error('密码不能包含换行或空字符；未修改私钥。');
  return [old, next];
}
function lastLine(value: Buffer): string { return value.toString().trimEnd().split(/\r?\n/).at(-1)!; }
export async function protect(key: string, publicKey: string, prompt = readPassword): Promise<void> {
  if (!Bun.which('minisign')) throw new Error('请先安装 minisign。');
  const info = lstatSync(key, { bigint: true });
  if (!info.isFile() || info.nlink !== 1n) throw new Error('私钥必须是普通文件，不能是符号链接或硬链接。');
  if (process.platform !== 'win32' && Number(info.mode & 0o777n) !== 0o600) throw new Error('私钥权限必须是 0600；未修改私钥。');
  const original = readFileSync(key), raw = decodeWrapper(original);
  const payload = decodeWrapper(lastLine(raw)), expected = lastLine(decodeWrapper(readFileSync(publicKey)));
  const [old, next] = passwords(await prompt('原密码（首次设置直接按回车）：'), await prompt('新的桌面更新私钥密码：'), await prompt('再输入一次新密码：'));
  await withTemp('.desktop-key-', folder => {
    const secret = join(folder, 'secret'), derived = join(folder, 'public'); privateWrite(secret, raw);
    minisign(['-C', '-s', secret], payload.subarray(2, 4).toString() === 'Sc' ? [old, next, next] : [next, next]);
    minisign(['-R', '-s', secret, '-p', derived], [next]);
    if (lastLine(readFileSync(derived)) !== expected) throw new Error('公钥不匹配；原私钥保持原样。');
    const encrypted = readFileSync(secret);
    if (decodeWrapper(lastLine(encrypted)).subarray(2, 4).toString() !== 'Sc') throw new Error('未形成密码加密的私钥；原私钥保持原样。');
    const replacement = join(folder, 'replacement'); privateWrite(replacement, Buffer.from(encrypted.toString('base64') + '\n'));
    const current = lstatSync(key, { bigint: true });
    if (['dev', 'ino', 'mode', 'nlink', 'size', 'mtimeNs', 'ctimeNs'].some(k => current[k as keyof typeof current] !== info[k as keyof typeof info]) || !readFileSync(key).equals(original)) throw new Error('私钥在操作期间被修改；不会覆盖。');
    renameSync(replacement, key);
  }, dirname(key));
}
if (import.meta.main) {
  if (process.argv.includes('--help')) console.log('bun desktop/scripts/protect-key.ts：隐藏输入密码，保持已有密钥对不变。');
  else protect(join(ROOT, '.release-secrets/clat-desktop.key'), join(DESKTOP, 'updater.pub')).then(() => console.log('桌面更新私钥已设置密码，公钥保持不变。请安全备份私钥并妥善保存密码。')).catch(error => { console.error(error.message); process.exitCode = 1; });
}
