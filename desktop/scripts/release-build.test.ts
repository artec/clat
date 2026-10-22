import { afterEach, expect, mock, test } from 'bun:test';
import { join } from 'node:path';
import { ReleaseBuild } from './release-build';
import { backendArtifact, backendIdentity as identity } from './test-support';
import { withTemp } from './common';

afterEach(() => mock.restore());
const state = { databaseId: 123, headSha: identity.commit, headBranch: 'v2.0.0', event: 'push', status: 'in_progress', conclusion: null };
test('owner accepts only the exact version tag and source Release run', async () => {
  const selected = new ReleaseBuild(() => JSON.stringify([{ ...state, databaseId: 999, headBranch: 'other-tag' }, state]), identity.commit, '2.0.0', '', async () => {}, 2);
  await selected.locate(); expect(selected.runId).toBe('123');
  const foreign = new ReleaseBuild(() => JSON.stringify([{ ...state, headSha: 'b'.repeat(40) }]), identity.commit, '2.0.0', '', async () => {}, 2);
  await expect(foreign.locate()).rejects.toThrow('身份不匹配');
});
test('backend is consumed while other platforms are still running and cached downloads are verified', async () => {
  await withTemp('release-backend-', async folder => {
    const calls: string[] = []; let lookups = 0;
    const build = new ReleaseBuild((...args) => {
      calls.push(args.slice(0, 2).join(' '));
      if (args[1] === 'list') return JSON.stringify([state]);
      if (args[1] === 'view') return JSON.stringify(state);
      if (args[0] === 'api') return JSON.stringify({ artifacts: ++lookups > 1 ? [{ name: 'backend-' + identity.target, expired: false }] : [] });
      if (args[1] === 'download') { backendArtifact(args[args.indexOf('--dir') + 1]!); return ''; }
      throw new Error('unexpected remote call');
    }, identity.commit, '2.0.0', folder, async () => {}, 3);
    await build.locate(); const result = await build.backend(identity.target);
    expect(result.runId).toBe('123'); expect(calls.filter(c => c === 'run download')).toHaveLength(1);
    await build.backend(identity.target); expect(calls.filter(c => c === 'run download')).toHaveLength(1);
    backendArtifact(result.folder, { ...identity, runId: '456' });
    await expect(build.backend(identity.target)).rejects.toThrow('runId');
    expect(calls.filter(c => c === 'run download')).toHaveLength(1);
  });
});
test('missing artifact fails on remote failure or bounded timeout without starting a local backend build', async () => {
  for (const failed of [true, false]) await withTemp('release-wait-', async folder => {
    let waited = 0;
    const build = new ReleaseBuild((...args) => args[0] === 'api' ? '{"artifacts":[]}' : JSON.stringify(failed ? { ...state, status: 'completed', conclusion: 'failure' } : state), identity.commit, '2.0.0', folder, async () => { waited++; }, 2);
    build.runId = '123';
    await expect(build.backend(identity.target)).rejects.toThrow(failed ? '远程 Release 失败' : '超时');
    expect(waited).toBe(failed ? 0 : 2);
  });
});
test('publication waits for complete successful Release and rejects failure or changed source', async () => {
  let calls = 0;
  const build = new ReleaseBuild(() => JSON.stringify(++calls > 1 ? { ...state, status: 'completed', conclusion: 'success' } : state), identity.commit, '2.0.0', '', async () => {}, 3);
  build.runId = '123'; await build.ready(); expect(calls).toBe(2);
  for (const run of [{ ...state, status: 'completed', conclusion: 'failure' }, { ...state, headSha: 'b'.repeat(40) }]) {
    const rejected = new ReleaseBuild(() => JSON.stringify(run), identity.commit, '2.0.0', '', async () => {}, 2); rejected.runId = '123';
    await expect(rejected.ready()).rejects.toThrow();
  }
});
test('expired artifact is not downloaded', async () => {
  await withTemp('expired-backend-', async folder => {
    const build = new ReleaseBuild((...args) => {
      if (args[0] === 'api') return JSON.stringify({ artifacts: [{ name: 'backend-' + identity.target, expired: true }] });
      expect(args[1]).toBe('view'); return JSON.stringify(state);
    }, identity.commit, '2.0.0', join(folder, 'cache'), async () => {}, 1);
    build.runId = '123'; await expect(build.backend(identity.target)).rejects.toThrow('超时');
  });
});
