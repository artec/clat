import { afterEach, expect, mock, test } from 'bun:test';
import { mkdirSync, readFileSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { recordBackend, verifyBackend } from './backend';
import { digest, json, withTemp, writeJson } from './common';
import { DesktopRelease } from './release';
import { backendArtifact as artifact, backendBytes as binary, backendIdentity as identity, fixture } from './test-support';

afterEach(() => mock.restore());
test('unchanged development staging preserves resource timestamps but refreshes changed bytes', async () => {
  await fixture(release => {
    const target = identity.target, directory = join(release.root, 'target');
    const source = join(directory, target, 'debug/clat');
    mkdirSync(join(directory, target, 'debug'), { recursive: true });
    writeFileSync(source, binary(target));
    let builds = 0;
    const staging = new DesktopRelease(release.root, args => {
      if (args[1] === 'build') { builds++; return ''; }
      if (args[1] === 'metadata') return JSON.stringify({ target_directory: directory });
      throw new Error('unexpected command');
    });
    staging.stageBackend(target);
    const backend = join(release.desktop, 'resources/clat-backend');
    const manifest = join(release.desktop, 'resources/backend.json');
    const old = new Date('2000-01-01T00:00:00Z');
    for (const file of [backend, manifest]) utimesSync(file, old, old);
    staging.stageBackend(target);
    expect(builds).toBe(2); // Cargo must still check every source/dependency input.
    for (const file of [backend, manifest]) expect(statSync(file).mtimeMs).toBe(old.getTime());
    const changed = Buffer.from(binary(target));
    changed[changed.length - 1] = changed[changed.length - 1]! ^ 1;
    writeFileSync(source, changed); // Equal size must not masquerade as a cache hit.
    staging.stageBackend(target);
    expect(readFileSync(backend)).toEqual(readFileSync(source));
    expect(json<{ sha256: string }>(manifest).sha256).toBe(digest(source));
  });
});
test('CLI artifact binds source, version, platform, run, official identity and bytes', async () => {
  await withTemp('backend-contract-', folder => {
    artifact(folder); expect(verifyBackend(folder, identity)).toBe(join(folder, 'clat'));
    for (const field of ['commit', 'version', 'target', 'runId'] as const) expect(() => verifyBackend(folder, { ...identity, [field]: 'wrong' })).toThrow('identity mismatch');
    const receipt = json<Record<string, unknown>>(join(folder, 'receipt.json'));
    for (const change of [{ official: false }, { size: 1 }, { sha256: '0'.repeat(64) }]) {
      writeJson(join(folder, 'receipt.json'), { ...receipt, ...change }); expect(() => verifyBackend(folder, identity)).toThrow();
    }
    writeJson(join(folder, 'receipt.json'), receipt);
    writeFileSync(join(folder, 'clat'), binary('x86_64-apple-darwin'));
    writeJson(join(folder, 'receipt.json'), { ...receipt, sha256: digest(join(folder, 'clat')) });
    expect(() => verifyBackend(folder, identity)).toThrow('architecture mismatch');
  });
});
test('receipt creation validates executable version and each native architecture', async () => {
  await withTemp('backend-record-', folder => {
    for (const target of new DesktopRelease().records('desktop').map(r => r.target)) {
      const source = join(folder, 'input'); writeFileSync(source, binary(target)); const output = join(folder, target);
      expect(() => recordBackend(source, output, { ...identity, target }, () => 'clat 1.0.0')).toThrow('version mismatch');
      recordBackend(source, output, { ...identity, target }, () => 'clat 2.0.0\n'); verifyBackend(output, { ...identity, target });
    }
  });
});
test('desktop stages prebuilt CLI unchanged without invoking cargo build', async () => {
  await fixture(release => {
    const folder = join(release.root, 'artifact'); artifact(folder); const calls: string[][] = [];
    const staging = new DesktopRelease(release.root, args => { calls.push(args); if (args[0] === 'git') return identity.commit; throw new Error('unexpected compiler invocation'); });
    staging.stageBackend(identity.target, { official: true, release: true, backend: folder, runId: '123' });
    expect(readFileSync(join(release.desktop, 'resources/clat-backend')).equals(binary(identity.target))).toBe(true);
    if (process.platform !== 'win32') expect(statSync(join(release.desktop, 'resources/clat-backend')).mode & 0o111).toBe(0o111);
    expect(calls.every(args => args[0] === 'git')).toBe(true);
    const before = readFileSync(join(release.desktop, 'resources/backend.json'));
    writeFileSync(join(folder, 'clat'), 'corrupt');
    expect(() => staging.stageBackend(identity.target, { official: true, release: true, backend: folder, runId: '123' })).toThrow();
    expect(readFileSync(join(release.desktop, 'resources/backend.json'))).toEqual(before);
    expect(() => staging.stageBackend(identity.target, { backend: folder })).toThrow('official release');
  });
});
