import { expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { json, ROOT } from './common';

type Step = { uses?: string; run?: string; name?: string; id?: string; if?: string; with?: Record<string, string | number | boolean> };
type Workflow = { jobs: Record<string, { steps?: Step[]; strategy?: { matrix: { include: { os: string; target: string }[] } } }> };
function source(path: string): string { return readFileSync(join(ROOT, path), 'utf8').replaceAll('\r\n', '\n'); }
function workflow(path: string): Workflow { return Bun.YAML.parse(source(path)) as Workflow; }
function pinnedBefore(steps: Step[], command: string): void {
  const setup = steps.findIndex(step => step.uses === 'oven-sh/setup-bun@v2');
  const invoking = steps.findIndex(step => step.run?.includes(command));
  expect(setup).toBeGreaterThanOrEqual(0); expect(invoking).toBeGreaterThan(setup);
  expect(String(steps[setup]!.with?.['bun-version'])).toBe('1.3.14');
}
test('CI and Docker share the pinned Bun runtime before desktop tooling', () => {
  const desktop = workflow('.github/workflows/desktop.yml');
  const release = workflow('.github/workflows/release.yml');
  pinnedBefore(desktop.jobs.plan!.steps!, 'release.ts matrix');
  pinnedBefore(release.jobs.plan!.steps!, 'release.ts matrix');
  pinnedBefore(desktop.jobs.native!.steps!, 'bun install');
  pinnedBefore(release.jobs.build!.steps!, 'bun install');
  pinnedBefore(workflow('.github/workflows/ci.yml').jobs.check!.steps!, 'scripts/gates.sh');
  expect(source('scripts/ci-box/Dockerfile')).toContain('npm install --global bun@1.3.14');
  const gates = source('scripts/gates.sh'); expect(gates).toContain('bun test desktop/scripts --timeout 60000'); expect(gates).toContain('typescript/bin/tsc');
});
test('desktop entry points and CI no longer invoke Python desktop scripts', () => {
  for (const path of ['.github/workflows/desktop.yml', '.github/workflows/release.yml', 'publish', 'scripts/gates.sh']) expect(source(path)).not.toMatch(/python3[^\n]*(desktop\/scripts|release\.py|workflow\.py)/);
  for (const name of ['workflow.py', 'release.py', 'owner_release.py', 'protect_key.py', 'test_release.py', 'test_workflow.py', 'test_protect_key.py']) expect(existsSync(join(ROOT, 'desktop/scripts', name))).toBe(false);
});
test('desktop has one Bun lockfile and all tool dependencies stay development-only', () => {
  const root = json<any>(join(ROOT, 'package.json')), desktop = json<any>(join(ROOT, 'desktop/package.json'));
  expect(root.packageManager).toBe('bun@1.3.14'); expect(desktop.packageManager).toBe('bun@1.3.14'); expect(desktop.dependencies).toBeUndefined();
  expect(existsSync(join(ROOT, 'desktop/bun.lock'))).toBe(true); expect(existsSync(join(ROOT, 'desktop/package-lock.json'))).toBe(false);
  for (const [name, version] of Object.entries(desktop.devDependencies)) expect(source('desktop/bun.lock')).toContain(`"${name}": "${version}"`);
});
test('ordinary desktop pushes check native code without producing release installers', () => {
  const desktop = workflow('.github/workflows/desktop.yml');
  const steps = desktop.jobs.native!.steps!;
  const commands = steps.filter(s => !s.if).map(s => s.run ?? '').join('\n');
  expect(commands).toContain('cargo test -p clat-desktop');
  expect(commands).not.toContain('release.ts build');
  expect(commands).not.toContain('release.ts package');
  expect(desktop.jobs.native!.steps!.find(s => s.uses?.startsWith('Swatinem/rust-cache'))!.with!['cache-on-failure']).toBe(true);
  for (const step of steps.filter(s => s.run?.includes('release.ts build') || s.run?.includes('release.ts package'))) {
    expect(step.if).toContain("github.event_name == 'workflow_dispatch'");
    expect(step.if).toContain('inputs.installer');
    expect(step.if).toContain("startsWith(matrix.platform, 'windows')");
  }
});
test('formal CI builds Windows desktops in CLI jobs; macOS is owner-built', () => {
  const release = workflow('.github/workflows/release.yml');
  const builds = release.jobs.build!.steps!.filter(s => s.run?.includes('release.ts build'));
  expect(builds).toHaveLength(1);
  expect(builds[0]!.if).toContain("contains(matrix.target, 'windows')");
  expect(source('.github/workflows/desktop.yml')).toContain('cancel-in-progress: true');
});
test('Windows installer consumes the CLI built in its own platform job', () => {
  const release = workflow('.github/workflows/release.yml');
  expect(release.jobs.desktop).toBeUndefined();
  const steps = release.jobs.build!.steps!;
  const cli = steps.findIndex(s => s.run?.includes('cargo build --release'));
  const receipt = steps.findIndex(s => s.run?.includes('backend.ts record'));
  const installer = steps.findIndex(s => s.run?.includes('release.ts build'));
  expect(receipt).toBeGreaterThan(cli); expect(installer).toBeGreaterThan(receipt);
  expect(steps[installer]!.run).toContain('--backend dist-backend');
  expect(steps[installer]!.if).toContain("contains(matrix.target, 'windows')");
});
test('main CI and Release share native runners and dependency cache identities', () => {
  const ci = workflow('.github/workflows/ci.yml').jobs['plugin-performance']!;
  const release = workflow('.github/workflows/release.yml').jobs.build!;
  const assets = json<{ cli: { os: string; target: string }[] }>(join(ROOT, 'desktop/assets.json'));
  expect(ci.strategy!.matrix.include.map(({ os, target }) => ({ os, target }))).toEqual(assets.cli);
  const cache = (steps: Step[]) => steps.find(s => s.uses === 'Swatinem/rust-cache@v2.9.2')!;
  const warm = cache(ci.steps!), consume = cache(release.steps!);
  expect(warm.with!['shared-key']).toBe('product-${{ matrix.target }}');
  expect(consume.with!['shared-key']).toBe(warm.with!['shared-key']);
  expect(consume.with!['prefix-key']).toBe(warm.with!['prefix-key']);
  expect(warm.with!['prefix-key']).toBe('v2-product-release');
  expect(warm.with!['save-if']).toContain('github.event.repository.default_branch');
  expect(consume.with!['save-if']).toBe(false);
  expect(warm.with!['cache-on-failure']).toBe(false);
  for (const cache of [warm, consume]) {
    expect(cache.with!['add-rust-environment-hash-key']).not.toBe(false);
    expect(cache.with!['cache-workspace-crates']).not.toBe(true);
  }
});
test('release dependency warming is bounded to main cache misses without packaging', () => {
  const steps = workflow('.github/workflows/ci.yml').jobs['plugin-performance']!.steps!;
  const warm = steps.filter(s => s.name?.startsWith('Warm release'));
  expect(warm).toHaveLength(2);
  for (const step of warm) {
    expect(step.if).toContain('github.event.repository.default_branch');
    expect(step.if).toContain("steps.product-cache.outputs.cache-hit != 'true'");
    expect(step.run).toContain('--release --locked --target ${{ matrix.target }}');
    expect(step.run).not.toMatch(/release\.ts|\bpackage\b|\bpublish\b|tauri\.js/);
  }
  expect(warm[0]!.run).toContain('-p clat-core');
  expect(warm[1]!.run).toContain('-p clat-desktop');
  expect(warm[1]!.run).toContain('--features tauri/custom-protocol');
  expect(warm[1]!.if).toContain("contains(matrix.target, 'windows')");
  const pinned = steps.findIndex(s => s.name === 'Add the pinned toolchain cross target');
  expect(steps.findIndex(s => s.id === 'product-cache')).toBeGreaterThan(pinned);
  for (const step of steps.filter(s => s.name?.startsWith('Repeated SHA') || s.name === 'Test authenticated compiler cache')) {
    expect(step.run).toContain('--target ${{ matrix.target }}');
  }
});
