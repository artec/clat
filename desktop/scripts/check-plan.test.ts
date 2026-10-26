import { expect, test } from 'bun:test';
import { mkdirSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { checksForPaths, gitChangedPaths, pathsForEvent } from './check-plan';
import { ROOT, run, withTemp } from './common';

const all = { native: true, tooling: true, web: true };
test('script and webpage edits select their own checks without four native builds', () => {
  expect(checksForPaths(['desktop/scripts/release.ts', 'publish', 'desktop/bun.lock', 'package.json']))
    .toEqual({ native: false, tooling: true, web: false });
  expect(checksForPaths(['web/i18n.js', 'web/e2e/desktop/shell.spec.js']))
    .toEqual({ native: false, tooling: false, web: true });
  expect(checksForPaths(['desktop/scripts/ci-contract.test.ts', 'desktop/ui/bridge.js']))
    .toEqual({ native: false, tooling: true, web: true });
});
test('all native build inputs, shared core changes and unknown paths retain four native faces', () => {
  for (const path of ['desktop/src/main.rs', 'desktop/build.rs',
    'desktop/Cargo.toml', 'desktop/tauri.conf.json', 'desktop/capabilities/main.json', 'desktop/assets.json',
    'src/host_client/connect.rs', 'src/application/run_execution.rs', 'crates/core/Cargo.toml',
    'sdk/wit-proposals/net-runtime/src/lib.rs', 'plugins/probe/Cargo.toml', 'Cargo.toml', 'Cargo.lock',
    'rust-toolchain.toml', '.cargo/config.toml', '.github/workflows/ci.yml', '.github/workflows/desktop.yml',
    '.github/workflows/release.yml', '.gitattributes', 'tests/fixtures/native.bin', 'new-build-input']) {
    expect(checksForPaths([path])).toEqual(all);
  }
  // A doc suffix does not make an embedded native resource documentation-only.
  expect(checksForPaths(['src/fixtures/README.md'])).toEqual(all);
});
test('known documentation and empty diffs do not add desktop work; unavailable diffs run everything', () => {
  expect(checksForPaths(['docs/testing.md', 'README.zh.md', '.agents/skills/example/SKILL.md', 'LICENSE']))
    .toEqual({ native: false, tooling: false, web: false });
  expect(checksForPaths([])).toEqual({ native: false, tooling: false, web: false });
  expect(checksForPaths(undefined)).toEqual(all);
});
test('shared language resources select Rust, native shell and web checks together',()=>{
  for(const path of ['locales/manifest.json','src/i18n/mod.rs','scripts/i18n-check.ts'])expect(checksForPaths([path])).toEqual(all);
});
test('presentation and PWA-only edits do not run unrelated Desktop bridge or native checks', () => {
  for (const path of ['web/workbench.css', 'web/app.js', 'web/project-tree.js', 'web/index.html',
    'web/e2e/tests/app.spec.js', 'web/e2e/manual/project-tree.spec.js', 'web/e2e/playwright.visual.config.js',
    'desktop/ui/welcome.css', 'desktop/icons/icon.ico', 'web/icons/icon-192.png', 'locales/zh-Hans.json']) {
    expect(checksForPaths([path])).toEqual({ native:false, tooling:false, web:false });
  }
  expect(checksForPaths(['desktop/ui/index.html', 'desktop/ui/window-bar.js']))
    .toEqual({ native:false, tooling:false, web:true });
});
test('push spans all commits and PR spans the base to the tested merge; missing history widens', () => {
  const before = 'a'.repeat(40), base = 'b'.repeat(40), head = 'c'.repeat(40), calls: string[][] = [];
  const diff = (from: string, to: string) => { calls.push([from, to]); return ['desktop/scripts/release.ts']; };
  expect(pathsForEvent('push', { before }, head, diff)).toEqual(['desktop/scripts/release.ts']);
  expect(pathsForEvent('pull_request', { before, pull_request: { base: { sha: base } } }, head, diff)).toBeDefined();
  expect(calls).toEqual([[before, head], [base, head]]);
  for (const name of ['workflow_dispatch', 'unknown']) expect(checksForPaths(pathsForEvent(name, { before }, head, diff))).toEqual(all);
  for (const value of ['0'.repeat(40), '--bad-option', 'missing', '']) expect(pathsForEvent('push', { before: value }, head, diff)).toBeUndefined();
  expect(pathsForEvent('push', { before }, 'bad', diff)).toBeUndefined();
  expect(checksForPaths(pathsForEvent('push', { before }, head, () => undefined))).toEqual(all);
  expect(checksForPaths(pathsForEvent('push', { before }, head, () => { throw new Error('shallow history'); }))).toEqual(all);
  expect(calls).toHaveLength(2);
});
test('real Git NUL diff preserves deletions and both rename paths with native filesystem names', async () => {
  await withTemp('clat-check-plan-', root => {
    const git = (...args: string[]) => run(['git', ...args], { cwd: root, quiet: true }).trim();
    // Win32 rejects newlines in filenames; spaces/Unicode still exercise the
    // NUL path contract there. POSIX also checks an embedded newline.
    const unusual = process.platform === 'win32' ? '旧 名' : '旧 名\n';
    const old = `src/${unusual}.rs`, moved = `desktop/scripts/${unusual}.ts`;
    git('init', '--quiet'); mkdirSync(join(root, dirname(old)), { recursive: true });
    writeFileSync(join(root, old), 'original\n'); git('add', '.'); const base = git('write-tree');
    mkdirSync(join(root, dirname(moved)), { recursive: true }); renameSync(join(root, old), join(root, moved));
    git('add', '-A'); const head = git('write-tree');
    const paths = gitChangedPaths(base, head, root)!;
    expect(paths.sort()).toEqual([moved, old].sort()); expect(checksForPaths(paths)).toEqual(all);
    expect(gitChangedPaths('d'.repeat(40), head, root)).toBeUndefined();
  });
});
test('CLI writes bounded booleans for malformed events instead of silently skipping checks', async () => {
  await withTemp('clat-check-output-', async root => {
    const output = join(root, 'output'), summary = join(root, 'summary'), event = join(root, 'event.json');
    writeFileSync(event, '{ invalid json');
    run([process.execPath, join(ROOT, 'desktop/scripts/check-plan.ts')], { quiet: true,
      env: { GITHUB_EVENT_PATH: event, GITHUB_EVENT_NAME: 'push', GITHUB_SHA: 'f'.repeat(40), GITHUB_OUTPUT: output, GITHUB_STEP_SUMMARY: summary } });
    expect(await Bun.file(output).text()).toBe('native=true\ntooling=true\nweb=true\n');
    expect(await Bun.file(summary).text()).toContain('unknown (full checks)');
  });
});
