import { afterEach, expect, mock, spyOn, test } from 'bun:test';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { checksum, DESKTOP, run, withTemp } from './common';
import { LocalMacos } from './local-macos';
import { ReleaseBuild } from './release-build';
import { fixture } from './test-support';

afterEach(() => mock.restore());
const commit = 'a'.repeat(40);
test('owner draft downloads six CLI and two Windows installers only', async () => {
  await fixture((_release, owner) => {
    expect(owner.remoteArchives()).toHaveLength(8);
    expect(owner.remoteArchives().filter(n => n.startsWith('clat-desktop-'))).toHaveLength(2);
    expect(owner.remoteArchives().some(n => n.endsWith('.dmg'))).toBe(false);
    expect(owner.archives()).toHaveLength(12);
  });
});
test('matrix CLI selects one platform and rejects unsupported CI targets', () => {
  const command = [process.execPath, join(DESKTOP, 'scripts/release.ts'), 'matrix', '--kind', 'desktop'];
  const ci = JSON.parse(run([...command, '--ci'], { quiet: true })).include;
  expect(ci).toHaveLength(2); expect(ci.every((r: { platform: string }) => r.platform.startsWith('windows'))).toBe(true);
  const one = JSON.parse(run([...command, '--target', 'aarch64-apple-darwin'], { quiet: true })).include;
  expect(one).toHaveLength(1); expect(one[0].target).toBe('aarch64-apple-darwin');
  expect(() => run([...command, '--target', 'invalid'], { quiet: true })).toThrow();
  expect(() => run([...command, '--target', 'aarch64-apple-darwin', '--ci'], { quiet: true })).toThrow();
});
test('formal local macOS build omits disposable DMG creation', async () => {
  await fixture(async release => {
    spyOn(release, 'stageBackend').mockImplementation(() => {});
    const dmg = spyOn(release, 'makeDmg');
    await release.build('aarch64-apple-darwin', { release: true, official: true, appOnly: true });
    expect(dmg).not.toHaveBeenCalled();
  }, () => '');
});
test('local formal build rejects dirty source and mismatched tags before compiling', async () => {
  for (const [dirty, tag] of [[' M Cargo.toml', commit], ['', 'b'.repeat(40)], ['', commit]]) {
    await fixture(release => {
      const local = new LocalMacos(release, join(release.root, 'cache'));
      if (dirty || tag !== commit) expect(() => local.source()).toThrow();
      else expect(local.source()).toBe(commit);
    }, args => args[1] === 'status' ? dirty! : args[2] === 'HEAD' ? commit : tag!);
  }
});
test('failed notarization preserves unsigned builds; retry reuses signed targets and rejects changed bytes', async () => {
  await fixture(async release => {
    const local = new LocalMacos(release, join(release.root, 'cache'), async () => ({ folder: 'verified-fixture-backend', runId: '123' }));
    const build = spyOn(release, 'build').mockResolvedValue(undefined);
    spyOn(release, 'package').mockImplementation((target, folder, appOnly) => {
      expect(appOnly).toBe(true);
      const record = release.records('desktop').find(r => r.target === target)!;
      writeFileSync(join(folder, local.names(record, false)[0]!), 'unsigned fixture');
    });
    let fail = true;
    const sign = spyOn(release, 'signMacos').mockImplementation(async (folder, record) => {
      if (fail) throw new Error('notary offline');
      for (const name of local.names(record, true)) { writeFileSync(join(folder, name), 'signed fixture'); checksum(join(folder, name)); }
    });
    const output = join(release.root, 'out'); mkdirSync(output);
    await expect(local.prepare(output)).rejects.toThrow('notary offline');
    expect(build).toHaveBeenCalledTimes(1);
    fail = false; await local.prepare(output);
    expect(build).toHaveBeenCalledTimes(2); expect(sign).toHaveBeenCalledTimes(3);
    await local.prepare(output);
    expect(build).toHaveBeenCalledTimes(2); expect(sign).toHaveBeenCalledTimes(3);
    expect(readFileSync(join(output, local.names(release.records('desktop')[0]!, true)[0]!), 'utf8')).toBe('signed fixture');
    const record = release.records('desktop')[0]!;
    writeFileSync(join(local.directory, 'macos', commit, record.target, 'signed', local.names(record, true)[0]!), 'replaced');
    await expect(local.prepare(output)).rejects.toThrow('已改变');
    expect(sign).toHaveBeenCalledTimes(3);
  }, args => args[0] === 'git' && args[1] !== 'status' ? commit : '');
});
test('source change during compilation cannot create a reusable stage', async () => {
  await fixture(async release => {
    const local = new LocalMacos(release, join(release.root, 'cache'));
    let source = commit; spyOn(local, 'source').mockImplementation(() => source);
    const record = release.records('desktop')[0]!;
    await withTemp('stage-race-', async parent => {
      await expect(local.stage(parent, commit, record, 'unsigned', async folder => {
        writeFileSync(join(folder, local.names(record, false)[0]!), 'fixture'); source = 'b'.repeat(40);
      })).rejects.toThrow('源码发生变化');
      expect(existsSync(join(parent, 'unsigned'))).toBe(false);
    });
  });
});
test('owner assembles local macOS and remote Windows into the unchanged complete inventory', async () => {
  await fixture(async (release, owner) => {
    spyOn(owner, 'gh').mockImplementation((...args) => {
      const folder = args[args.indexOf('--dir') + 1]!;
      for (const name of owner.remoteArchives()) { writeFileSync(join(folder, name), 'CI fixture'); checksum(join(folder, name)); }
      expect(args.flatMap((value, index) => value === '--pattern' ? [args[index + 1]!] : [])).toEqual(owner.remoteArchives().flatMap(n => [n, n + '.sha256']));
      return '';
    });
    spyOn(LocalMacos.prototype, 'source').mockReturnValue(commit);
    spyOn(ReleaseBuild.prototype, 'locate').mockResolvedValue(undefined);
    spyOn(ReleaseBuild.prototype, 'ready').mockResolvedValue(undefined);
    spyOn(owner, 'draft').mockReturnValue({ tagName: owner.tag(), isDraft: true, assets: owner.remoteArchives().flatMap(n => [n, n + '.sha256']).map(name => ({ name })) });
    spyOn(LocalMacos.prototype, 'prepare').mockImplementation(async function (this: LocalMacos, folder) {
      for (const record of release.records('desktop').filter(r => r.platform!.startsWith('darwin'))) {
        for (const name of this.names(record, true)) { writeFileSync(join(folder, name), 'local signed fixture'); checksum(join(folder, name)); }
      }
    });
    const finalize = spyOn(release, 'finalize').mockImplementation(async folder => {
      for (const name of owner.archives()) owner.verifyChecksum(folder, name);
      for (const name of owner.payloads()) if (!existsSync(join(folder, name))) { writeFileSync(join(folder, name), 'signature fixture'); checksum(join(folder, name)); }
    });
    const verify = spyOn(owner, 'verify').mockResolvedValue(undefined);
    const { OwnerRelease } = await import('./owner-release');
    const assembly = new OwnerRelease(release, async () => '', () => 0);
    spyOn(assembly, 'gh').mockImplementation(owner.gh.bind(owner));
    spyOn(assembly, 'draft').mockImplementation(owner.draft.bind(owner));
    spyOn(assembly, 'verify').mockImplementation(owner.verify.bind(owner));
    await assembly.buildPrepared(owner.remoteArchives().flatMap(n => [n, n + '.sha256']));
    expect(finalize.mock.calls[0]![2]).toBe(true);
    expect(verify).toHaveBeenCalledTimes(1);
    for (const name of owner.payloads()) expect(existsSync(join(owner.directory(), 'prepared', name))).toBe(true);
  });
});
