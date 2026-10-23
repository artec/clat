import { lstatSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { decodeWrapper, digest, json, withTemp, writeJson } from './common';
import type { DesktopRelease } from './release';

export const PAYLOAD = 'release-payload.json';
export interface ReleasePayload {
  version: string; notes: string; pub_date: string;
  platforms: Record<string, { url: string; signature: string }>;
  artifacts: Record<string, string>;
}
export interface ReleaseIndex { schema_version: number; payload: string; signatures: { cli: string; desktop: string } }

export function checksumGroups(release: DesktopRelease, tag: string): Record<string, string[]> {
  const groups = Object.fromEntries(release.names('cli', tag).map(n => [n + '.sha256', [n]]));
  for (const record of release.records('desktop')) {
    const stem = `clat-desktop-${tag}-${record.label}`;
    const mac = record.platform!.startsWith('darwin');
    groups[stem + (mac ? '.sha256' : '-setup.exe.sha256')] = mac ? [stem + '.dmg', stem + '.app.tar.gz'] : [stem + '-setup.exe'];
  }
  return groups;
}
export function publicFiles(release: DesktopRelease, tag: string): string[] {
  return [...release.names('cli', tag), ...release.names('desktop', tag), ...Object.keys(checksumGroups(release, tag)), 'latest.json'].sort();
}
export function writePayload(release: DesktopRelease, folder: string, tag: string, platforms: ReleasePayload['platforms']): void {
  for (const [name, entries] of Object.entries(checksumGroups(release, tag))) {
    writeFileSync(join(folder, name), entries.map(n => `${digest(join(folder, n))}  ${n}\n`).join(''));
  }
  const artifacts = Object.fromEntries(publicFiles(release, tag).filter(n => n !== 'latest.json').map(n => [n, digest(join(folder, n))]));
  writeJson(join(folder, PAYLOAD), { version: release.version(), notes: 'CLAT ' + tag, pub_date: new Date().toISOString(), platforms, artifacts });
}
export function assembleIndex(folder: string): void {
  const path = join(folder, PAYLOAD);
  writeJson(join(folder, 'latest.json'), { schema_version: 1, payload: readFileSync(path, 'utf8'), signatures: {
    cli: readFileSync(path + '.minisig', 'utf8'), desktop: readFileSync(path + '.sig', 'utf8').trim(),
  } });
}
function checkComment(signature: string, expected: string): void {
  const lines = signature.trimEnd().split(/\r?\n/);
  if (lines.length !== 4 || lines[2] !== 'trusted comment: ' + expected) throw new Error('发布索引签名未绑定当前版本。');
}
export async function verifyIndex(release: DesktopRelease, folder: string, desktopOnly = false): Promise<void> {
  const path = join(folder, 'latest.json');
  if (!lstatSync(path).isFile()) throw new Error('发布索引必须是普通文件。');
  const index = json<ReleaseIndex>(path);
  if (index.schema_version !== 1 || typeof index.payload !== 'string') throw new Error('不支持的发布索引格式。');
  const tag = 'v' + release.version();
  await withTemp('clat-release-verify-', temporary => {
    const payload = join(temporary, 'payload'), signature = join(temporary, 'signature'), publicKey = join(temporary, 'public');
    writeFileSync(payload, index.payload);
    if (!desktopOnly) {
      checkComment(index.signatures.cli, `CLAT ${tag} release index`);
      writeFileSync(signature, index.signatures.cli);
      release.execute(['minisign', '-Vm', payload, '-p', join(release.root, 'release/minisign.pub'), '-x', signature, '-q']);
    }
    writeFileSync(publicKey, decodeWrapper(readFileSync(join(release.desktop, 'updater.pub'))));
    const desktopSignature = decodeWrapper(index.signatures.desktop).toString();
    const lines = desktopSignature.trimEnd().split(/\r?\n/);
    if (lines.length !== 4 || !lines[2]!.split('\t').includes('version:' + release.version())) throw new Error('桌面更新签名未绑定当前版本。');
    writeFileSync(signature, desktopSignature);
    release.execute(['minisign', '-Vm', payload, '-p', publicKey, '-x', signature, '-q']);
    const value = JSON.parse(index.payload) as ReleasePayload;
    validatePayload(release, folder, value);
    for (const record of release.records('desktop')) {
      const item = value.platforms[record.platform!]!;
      const name = item.url.split('/').at(-1)!;
      writeFileSync(signature, decodeWrapper(item.signature));
      const packageLines = decodeWrapper(item.signature).toString().trimEnd().split(/\r?\n/);
      if (packageLines.length !== 4 || !packageLines[2]!.split('\t').includes('version:' + release.version())) throw new Error('桌面更新签名未绑定当前版本。');
      release.execute(['minisign', '-Vm', join(folder, name), '-p', publicKey, '-x', signature, '-q']);
    }
  });
}
function validatePayload(release: DesktopRelease, folder: string, value: ReleasePayload): void {
  const tag = 'v' + release.version();
  const platforms = release.records('desktop').map(r => r.platform).sort();
  if (value.version !== release.version() || JSON.stringify(Object.keys(value.platforms).sort()) !== JSON.stringify(platforms)) throw new Error('桌面更新索引的版本或四平台清单不匹配。');
  for (const record of release.records('desktop')) {
    const suffix = record.platform!.startsWith('darwin') ? '.app.tar.gz' : '-setup.exe';
    const name = `clat-desktop-${tag}-${record.label}${suffix}`;
    if (value.platforms[record.platform!]!.url !== `https://github.com/artec/clat/releases/download/${tag}/${name}`) throw new Error('更新索引的下载目标不匹配。');
  }
  const names = publicFiles(release, tag).filter(n => n !== 'latest.json');
  if (JSON.stringify(Object.keys(value.artifacts).sort()) !== JSON.stringify(names)) throw new Error('发布索引的资产清单不匹配。');
  for (const name of names) if (!lstatSync(join(folder, name)).isFile() || value.artifacts[name] !== digest(join(folder, name))) throw new Error('发布文件校验失败：' + name);
  for (const [name, entries] of Object.entries(checksumGroups(release, tag))) {
    const expected = entries.map(n => `${value.artifacts[n]}  ${n}\n`).join('');
    if (readFileSync(join(folder, name), 'utf8') !== expected) throw new Error('发布校验清单不匹配：' + name);
  }
}
