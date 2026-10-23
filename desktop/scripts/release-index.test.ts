import { afterEach, expect, mock, spyOn, test } from 'bun:test';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { checksum, DESKTOP, json, run } from './common';
import { assembleIndex, PAYLOAD, type ReleaseIndex } from './release-index';
import { minisign } from './protect-key';
import { fixture, generateKey, remote, sign } from './test-support';

afterEach(() => mock.restore());
test.skipIf(!Bun.which('minisign'))('real finalizer publishes only compact assets and authenticates both roots after private files are removed', async () => {
  await fixture(async (release, owner) => {
    const cli = join(release.root, '.release-secrets/clat-minisign.key');
    const desktop = join(release.root, 'desktop.key');
    generateKey(cli, join(release.root, 'release/minisign.pub'));
    generateKey(desktop, join(release.desktop, 'raw.pub'));
    minisign(['-C', '-s', desktop], ['fixture-protected', 'fixture-protected']);
    writeFileSync(join(release.root, '.release-secrets/clat-desktop.key'), readFileSync(desktop).toString('base64'));
    writeFileSync(join(release.desktop, 'updater.pub'), readFileSync(join(release.desktop, 'raw.pub')).toString('base64'));
    // Actual Tauri signer, only command location changes for the isolated root.
    const execute = release.execute;
    Object.defineProperty(release, 'execute', { value: (args: string[], options: any) => execute(
      args[1]?.endsWith('tauri.js') ? [args[0]!, join(DESKTOP, 'node_modules/@tauri-apps/cli/tauri.js'), ...args.slice(2)] : args, { ...options, env: args.includes('--private-key-path') ? { TAURI_SIGNING_PRIVATE_KEY_PASSWORD: 'fixture-protected' } : options?.env, quiet: true }) });
    spyOn(release, 'preflightSigning').mockResolvedValue(undefined);
    const folder = join(release.root, 'assets'); mkdirSync(folder);
    for (const name of owner.archives()) { writeFileSync(join(folder, name), 'test-only archive'); checksum(join(folder, name)); }
    await release.finalize(folder, owner.tag(), true);
    sign(join(folder, PAYLOAD), cli, 'CLAT v2.0.0 release index'); assembleIndex(folder);
    const publicFolder = join(release.root, 'public'); mkdirSync(publicFolder);
    for (const name of owner.files()) writeFileSync(join(publicFolder, name), readFileSync(join(folder, name)));
    await owner.verify(publicFolder);
    const path = join(publicFolder, 'latest.json'), original = readFileSync(path);
    for (const root of ['cli', 'desktop'] as const) {
      const value = json<ReleaseIndex>(path); value.signatures[root] = 'invalid'; writeFileSync(path, JSON.stringify(value));
      await expect(owner.verify(publicFolder)).rejects.toThrow(); writeFileSync(path, original);
    }
    const mac = owner.files().find(n => n.endsWith('-macos-arm64.sha256'))!;
    expect(readFileSync(join(publicFolder, mac), 'utf8').trim().split('\n')).toHaveLength(2);
    writeFileSync(join(publicFolder, mac), 'substituted checksum');
    await expect(owner.verify(publicFolder)).rejects.toThrow('校验失败');
  }, (args, options) => run(args, { ...options, quiet: true }));
}, 60000);

test('remote extra legacy signature blocks publication', async () => {
  await fixture(async (_release, owner) => {
    const gh = spyOn(owner, 'gh');
    await expect(owner.compareRemote({ ...remote(owner), assets: [...remote(owner).assets, { name: 'obsolete.minisig' }] })).rejects.toThrow('完整发布资产');
    expect(gh).not.toHaveBeenCalled();
  });
});

test('release notes put Host first and link only the four human desktop installers', async () => {
  await fixture((_release, owner) => {
    const notes = owner.notes();
    expect(notes.indexOf('CLI / Host')).toBeLessThan(notes.indexOf('## Desktop'));
    for (const name of owner.archives().filter(n => n.endsWith('.dmg') || n.endsWith('-setup.exe'))) expect(notes).toContain(name);
    expect(notes).not.toContain('.app.tar.gz');
    expect(notes).toContain('手动重装一次');
  });
});

test.skipIf(process.platform === 'win32')('publication rejects a symlinked index before reading its contents', async () => {
  await fixture(async (release, owner) => {
    const { symlinkSync } = await import('node:fs');
    const target = join(release.root, 'index-target'); writeFileSync(target, '{}');
    symlinkSync(target, join(release.root, 'latest.json'));
    await expect(owner.verify(release.root)).rejects.toThrow('普通文件');
  });
});
