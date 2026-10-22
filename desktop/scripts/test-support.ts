import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { cleanEnv, run, type Runner, withTemp, writeJson } from './common';
import { DesktopRelease } from './release';
import { OwnerRelease, type RemoteRelease } from './owner-release';

export async function fixture<T>(action: (release: DesktopRelease, owner: OwnerRelease) => T | Promise<T>, execute: Runner = run): Promise<T> {
  return withTemp('clat-tool-test-', async root => {
    mkdirSync(join(root, 'desktop')); mkdirSync(join(root, 'release')); mkdirSync(join(root, '.release-secrets'));
    writeFileSync(join(root, 'Cargo.toml'), '[workspace.package]\nversion = "2.0.0"\n');
    const release = new DesktopRelease(root, execute), owner = new OwnerRelease(release);
    return action(release, owner);
  });
}
export function remote(owner: OwnerRelease): RemoteRelease {
  return { tagName: owner.tag(), isDraft: true, assets: owner.files().map(name => ({ name })) };
}
export function generateKey(secret: string, publicKey: string): void {
  const result = Bun.spawnSync(['minisign', '-G', '-W', '-s', secret, '-p', publicKey], { env: cleanEnv(), stdout: 'pipe', stderr: 'pipe' });
  if (!result.success) throw new Error('fixture key generation failed');
}
export function sign(path: string, key: string, comment: string): void {
  const result = Bun.spawnSync(['minisign', '-S', '-s', key, '-m', path, '-t', comment], { env: cleanEnv(), stdout: 'pipe', stderr: 'pipe', stdin: 'ignore' });
  if (!result.success) throw new Error('fixture signing failed');
}
export function fixtureIndex(release: DesktopRelease, owner: OwnerRelease, folder: string): string {
  const platforms: Record<string, { url: string; signature: string }> = {};
  for (const record of release.records('desktop')) {
    const suffix = record.platform!.startsWith('darwin') ? '.app.tar.gz' : '-setup.exe';
    const name = `clat-desktop-${owner.tag()}-${record.label}${suffix}`;
    platforms[record.platform!] = { url: `https://github.com/artec/clat/releases/download/${owner.tag()}/${name}`, signature: readFileSync(join(folder, name + '.sig'), 'utf8').trim() };
  }
  const index = join(folder, 'desktop-latest.json'); writeJson(index, { version: release.version(), platforms }); return index;
}
