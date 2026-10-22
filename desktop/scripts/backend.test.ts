import { afterEach, expect, mock, test } from 'bun:test';
import { readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { recordBackend, verifyBackend } from './backend';
import { digest, json, withTemp, writeJson } from './common';
import { DesktopRelease } from './release';
import { backendArtifact as artifact, backendBytes as binary, backendIdentity as identity, fixture } from './test-support';

afterEach(() => mock.restore());
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
