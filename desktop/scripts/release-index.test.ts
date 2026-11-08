import { afterEach, expect, mock, spyOn, test } from 'bun:test';
import { mkdirSync, readFileSync, writeFileSync, unlinkSync } from 'node:fs';
import { join } from 'node:path';
import { checksum, DESKTOP, digest, json, run } from './common';
import { assembleIndex, PAYLOAD, writePayload, type ReleaseIndex } from './release-index';
import { minisign } from './protect-key';
import { fixture, generateKey, remote, sign } from './test-support';

afterEach(() => mock.restore());
test('payload preserves and authenticates CI binary-mode checksum manifests', async () => {
  await fixture((release, owner) => {
    const folder = join(release.root, 'assets'); mkdirSync(folder);
    for (const name of owner.archives()) {
      writeFileSync(join(folder, name), 'archive');
      checksum(join(folder, name));
    }
    const name = release.names('cli', owner.tag()).find(name => name.includes('windows'))!;
    const manifest = join(folder, name + '.sha256');
    const binaryMode = digest(join(folder, name)) + ' *' + name + '\n';
    writeFileSync(manifest, binaryMode);
    writePayload(release, folder, owner.tag(), {});
    expect(readFileSync(manifest, 'utf8')).toBe(binaryMode);
    const payload = json<{ artifacts: Record<string, string> }>(join(folder, PAYLOAD));
    expect(payload.artifacts[name + '.sha256']).toBe(digest(manifest));
    writeFileSync(manifest, '0'.repeat(64) + ' *' + name + '\n');
    expect(() => writePayload(release, folder, owner.tag(), {})).toThrow();
  });
});
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
    const cliNames = release.names('cli', owner.tag());
    const windowsCli = cliNames.find(name => name.includes('windows'))!;
    const binaryManifest = digest(join(folder, windowsCli)) + ' *' + windowsCli + '\n';
    writeFileSync(join(folder, windowsCli + '.sha256'), binaryManifest);
    const remoteHashes = Object.fromEntries(cliNames.map(name => [name, digest(join(folder, name))]));
    for (const name of cliNames) unlinkSync(join(folder, name));
    await release.finalize(folder, owner.tag(), true, remoteHashes);
    sign(join(folder, PAYLOAD), cli, 'CLAT v2.0.0 release index'); assembleIndex(folder);
    const publicFolder = join(release.root, 'public'); mkdirSync(publicFolder);
    for (const name of owner.files().filter(name => !cliNames.includes(name))) writeFileSync(join(publicFolder, name), readFileSync(join(folder, name)));
    writeFileSync(join(publicFolder, 'remote-cli-digests.json'), JSON.stringify(remoteHashes));
    await owner.verify(publicFolder);
    expect(readFileSync(join(publicFolder, windowsCli + '.sha256'), 'utf8')).toBe(binaryManifest);
    const remoteAssets = owner.files().map(name => ({ name, state: 'uploaded', digest: 'sha256:' + (remoteHashes[name] ?? digest(join(publicFolder, name))) }));
    expect(owner.missing({tagName: owner.tag(), isDraft: true, assets: remoteAssets}, publicFolder)).toEqual([]);
    const changed = remoteAssets.map(asset => asset.name === cliNames[0] ? {...asset, digest: 'sha256:' + '0'.repeat(64)} : asset);
    expect(() => owner.missing({tagName: owner.tag(), isDraft: true, assets: changed}, publicFolder)).toThrow();
    expect(() => owner.missing({tagName: owner.tag(), isDraft: true, assets: remoteAssets.filter(asset => asset.name !== cliNames[0])}, publicFolder)).toThrow();
    writeFileSync(join(publicFolder, 'remote-cli-digests.json'), JSON.stringify({...remoteHashes, [cliNames[0]!]: '0'.repeat(64)}));
    await expect(owner.verify(publicFolder)).rejects.toThrow();
    writeFileSync(join(publicFolder, 'remote-cli-digests.json'), JSON.stringify(remoteHashes));
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
    expect(() => owner.verifyRemoteInventory({ ...remote(owner), assets: [...remote(owner).assets, { name: 'obsolete.minisig' }] })).toThrow('完整发布资产');
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
