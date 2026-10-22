import { afterEach, expect, mock, spyOn, test } from 'bun:test';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { checksum, DESKTOP, digest, json, nativeTarget, ROOT } from './common';
import { OwnerRelease } from './owner-release';
import { DesktopRelease } from './release';
import { build, start, startArgs } from './workflow';
import { fixture, fixtureIndex, generateKey, remote, sign } from './test-support';

afterEach(() => mock.restore());
test('root and desktop offer short local commands', () => {
  for (const path of [join(ROOT, 'package.json'), join(DESKTOP, 'package.json')]) {
    const scripts = json<{ scripts: Record<string, string> }>(path).scripts;
    expect(scripts.start).toEndWith('workflow.ts start'); expect(scripts['app:build']).toEndWith('workflow.ts build'); expect(scripts['app:test']).toEndWith('workflow.ts test');
    expect(Object.keys(scripts).some(n => n.startsWith('release:'))).toBe(false);
  }
});
test('native architecture detection covers macOS and Windows', () => {
  for (const [system, machine, expected] of [['darwin', 'arm64', 'aarch64-apple-darwin'], ['darwin', 'x86_64', 'x86_64-apple-darwin'], ['win32', 'AMD64', 'x86_64-pc-windows-msvc'], ['win32', 'ARM64', 'aarch64-pc-windows-msvc']]) {
    expect(nativeTarget(system as NodeJS.Platform, machine!)).toBe(expected);
  }
  expect(() => nativeTarget('linux')).toThrow();
});
test('start preserves local origins and locked Cargo runner', async () => {
  const args = startArgs(new DesktopRelease(), 'aarch64-apple-darwin'); expect(args).toContain('--no-dev-server'); expect(args).toContain('--locked'); expect(args[0]).toBe(process.execPath);
  expect(args).not.toContain('python3'); expect(args).not.toContain('node');
  if (['darwin', 'win32'].includes(process.platform)) await fixture(async release => {
    const events: string[] = [];
    spyOn(release, 'stageBackend').mockImplementation(() => { events.push('stage'); });
    const result = await start(release, () => { events.push('launch'); return { exited: Promise.resolve(0), kill() {} }; });
    expect(result).toBe(0); expect(events).toEqual(['stage', 'launch']);
  });
});
test('build only makes native unsigned package', async () => {
  if (!['darwin', 'win32'].includes(process.platform)) return;
  await fixture(async release => {
    spyOn(console, 'log').mockImplementation(() => {});
    const compiling = spyOn(release, 'build').mockResolvedValue(undefined); const packaging = spyOn(release, 'package').mockImplementation(() => {});
    await build(release); expect(compiling).toHaveBeenCalledWith(nativeTarget(), { release: true, official: true }); expect(packaging).toHaveBeenCalledTimes(1);
  }, () => { throw new Error('build cannot access GitHub'); });
});
test('inventory retains six CLI and four desktop targets', () => {
  const owner = new OwnerRelease(); expect(owner.archives()).toHaveLength(12); expect(owner.payloads()).toHaveLength(18); expect(owner.files()).toHaveLength(54); expect(new Set(owner.files()).size).toBe(54);
});
async function withoutLock(owner: OwnerRelease): Promise<void> {
  spyOn(owner, 'localLock').mockImplementation(async action => action()); spyOn(console, 'log').mockImplementation(() => {});
}
test('one-command publication reuses prepared assets on retry', async () => {
  for (const prepared of [false, true]) await fixture(async (_release, owner) => {
    await withoutLock(owner); const events: string[] = [];
    if (prepared) mkdirSync(join(owner.directory(), 'prepared'), { recursive: true });
    spyOn(owner, 'authenticate').mockImplementation(() => {}); spyOn(owner, 'draft').mockImplementation(() => remote(owner));
    spyOn(owner, 'prepare').mockImplementation(async () => { events.push('sign'); }); spyOn(owner, 'verify').mockImplementation(async () => { events.push('verify'); });
    spyOn(owner, 'upload').mockImplementation(async () => { events.push('upload'); }); spyOn(owner, 'publish').mockImplementation(async () => { events.push('publish'); });
    await owner.complete(); expect(events).toEqual([prepared ? 'verify' : 'sign', 'upload', 'publish']);
  });
});
test('legacy publish still runs one owner entry without Python', () => {
  const script = readFileSync(join(ROOT, 'publish'), 'utf8').replaceAll('\r\n', '\n'); expect(script).toContain('workflow.ts" release "$@"'); expect(script).not.toContain('python');
});
test('upload never publishes', async () => {
  await fixture(async (_release, owner) => {
    await withoutLock(owner); const events: string[] = [];
    spyOn(owner, 'draft').mockImplementation(() => remote(owner)); spyOn(owner, 'verify').mockImplementation(async () => { events.push('verified'); });
    spyOn(owner, 'compareRemote').mockImplementation(async () => { events.push('remote-verified'); });
    spyOn(owner, 'gh').mockImplementation((...args) => { expect(args.slice(0, 2)).toEqual(['release', 'upload']); events.push('upload'); return ''; });
    await owner.upload(); expect(events).toEqual(['verified', 'upload', 'remote-verified']);
  });
});
test('publication requires local and remote verification', async () => {
  await fixture(async (_release, owner) => {
    await withoutLock(owner); const events: string[] = [];
    spyOn(owner, 'draft').mockImplementation(() => remote(owner)); spyOn(owner, 'verify').mockImplementation(async () => { events.push('local'); });
    spyOn(owner, 'compareRemote').mockImplementation(async () => { events.push('remote'); });
    spyOn(owner, 'gh').mockImplementation((...args) => { expect(args).toContain('--draft=false'); events.push('publish'); return ''; });
    await owner.publish(); expect(events).toEqual(['local', 'remote', 'publish']);
  });
});
test('failed verification blocks upload and publication', async () => {
  await fixture(async (_release, owner) => {
    await withoutLock(owner); spyOn(owner, 'draft').mockImplementation(() => remote(owner));
    const gh = spyOn(owner, 'gh'); const verify = spyOn(owner, 'verify').mockRejectedValue(new Error('bad signature'));
    await expect(owner.upload()).rejects.toThrow('bad signature'); await expect(owner.publish()).rejects.toThrow('bad signature'); expect(gh).not.toHaveBeenCalled();
    verify.mockResolvedValue(undefined); spyOn(owner, 'compareRemote').mockRejectedValue(new Error('remote changed'));
    await expect(owner.publish()).rejects.toThrow('remote changed'); expect(gh).not.toHaveBeenCalled();
  });
});
test('version is exact, never latest unrelated draft', () => {
  for (const [tagName, isDraft] of [['v0.0.1', true], ['v2.0.0', false]]) {
    const owner = new OwnerRelease(new DesktopRelease(ROOT, () => JSON.stringify({ tagName, isDraft })));
    expect(() => owner.draft()).toThrow();
  }
});
test('manifest binds exact filename and bytes', async () => {
  await fixture((release, owner) => {
    const path = join(release.root, 'asset.bin'); writeFileSync(path, 'fixture'); checksum(path); owner.verifyChecksum(release.root, 'asset.bin');
    for (const content of [digest(path) + '  other.bin\n', '0'.repeat(64) + '  asset.bin\n', digest(path) + '  asset.bin\n\n', digest(path) + '  asset.bin extra\n']) {
      writeFileSync(path + '.sha256', content); expect(() => owner.verifyChecksum(release.root, 'asset.bin')).toThrow();
    }
  });
});
test.skipIf(process.platform !== 'darwin')('partial draft fails before asking password or signing', async () => {
  await fixture(async (release, owner) => {
    await withoutLock(owner); spyOn(owner, 'draft').mockReturnValue({ ...remote(owner), assets: [] });
    const signing = spyOn(release, 'finalize'); await expect(owner.prepare()).rejects.toThrow('CI'); expect(signing).not.toHaveBeenCalled();
  });
});
test('changed remote bytes block publication', async () => {
  await fixture(async (release, owner) => {
    mkdirSync(join(owner.directory(), 'prepared'), { recursive: true }); writeFileSync(join(owner.directory(), 'prepared/asset'), 'signed-local');
    spyOn(owner, 'files').mockReturnValue(['asset']); spyOn(owner, 'gh').mockImplementation((...args) => { writeFileSync(join(args[args.indexOf('--dir') + 1]!, 'asset'), 'replaced-remote'); return ''; });
    await expect(owner.compareRemote({ tagName: 'v2.0.0', isDraft: true, assets: [{ name: 'asset' }] })).rejects.toThrow('不一致'); expect(release.version()).toBe('2.0.0');
  });
});
test.skipIf(!Bun.which('minisign'))('complete signed inventory and validly signed wrong index reject tampering', async () => {
  await fixture(async (release, owner) => {
    generateKey(join(release.root, 'cli.key'), join(release.root, 'release/minisign.pub'));
    generateKey(join(release.root, 'desktop.key'), join(release.desktop, 'raw.pub'));
    writeFileSync(join(release.desktop, 'updater.pub'), readFileSync(join(release.desktop, 'raw.pub')).toString('base64'));
    const folder = join(release.root, 'assets'); mkdirSync(folder);
    owner.archives().forEach(name => writeFileSync(join(folder, name), 'test-only package'));
    const updates = release.names('desktop', owner.tag()).filter(n => !n.endsWith('.dmg'));
    const signUpdate = (name: string) => {
      const file = join(folder, name); sign(file, join(release.root, 'desktop.key'), `timestamp:1\tfile:${name}\tversion:2.0.0`);
      writeFileSync(file + '.sig', readFileSync(file + '.minisig').toString('base64'));
    };
    updates.forEach(signUpdate); const index = fixtureIndex(release, owner, folder); signUpdate('desktop-latest.json');
    for (const name of owner.payloads()) { checksum(join(folder, name)); sign(join(folder, name + '.sha256'), join(release.root, 'cli.key'), 'CLAT v2.0.0 release checksum manifests'); }
    await owner.verify(folder);
    writeFileSync(join(folder, updates[0]!), 'tampered'); await expect(owner.verify(folder)).rejects.toThrow(); writeFileSync(join(folder, updates[0]!), 'test-only package');
    const value = json<any>(index); value.platforms[Object.keys(value.platforms)[0]!].url = 'https://example.invalid/substituted'; writeFileSync(index, JSON.stringify(value));
    // Remove only our disposable signature to re-sign a deliberately wrong index.
    const { unlinkSync } = await import('node:fs'); unlinkSync(index + '.minisig'); signUpdate('desktop-latest.json');
    await expect(owner.verifyDesktop(folder)).rejects.toThrow('下载目标');
  });
});
test.skipIf(process.platform === 'win32')('release lock rejects overlap and releases explicitly after failure', async () => {
  await fixture(async (_release, owner) => {
    await expect(owner.localLock(async () => {
      await expect(owner.localLock(async () => {})).rejects.toThrow('已有本机发布'); throw new Error('fixture failure');
    })).rejects.toThrow('fixture failure');
    await owner.localLock(async () => {});
  });
});
