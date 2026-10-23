import { expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { json, ROOT } from './common';

type Step = { uses?: string; run?: string; name?: string; id?: string; if?: string; with?: Record<string, string | number | boolean> };
type Workflow = { jobs: Record<string, { if?: string; needs?: string | string[]; env?: Record<string, string>; outputs?: Record<string, string>; steps?: Step[]; strategy?: { matrix: { include: { os: string; target: string }[] } } }> };
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
test('ordinary desktop pushes check native code once in CI without producing release installers', () => {
  const desktop = workflow('.github/workflows/desktop.yml');
  const ci = workflow('.github/workflows/ci.yml').jobs['plugin-performance']!;
  const steps = ci.steps!;
  const commands = steps.filter(s => !s.if).map(s => s.run ?? '').join('\n');
  expect(commands).not.toContain('release.ts build');
  expect(commands).not.toContain('release.ts package');
  expect(desktop.jobs.native!.if).toBe("github.event_name == 'workflow_dispatch'");
  expect(ci.needs).toContain('desktop-plan');
  for (const command of ['cargo clippy -p clat-desktop', 'cargo test -p clat-desktop']) {
    const check = steps.find(s => s.run?.startsWith(command))!;
    expect(check).toBeDefined();
    expect(check.if).toContain("needs.desktop-plan.outputs.native == 'true'");
    expect(check.if).toContain("contains(matrix.target, 'apple-darwin') || contains(matrix.target, 'windows')");
    expect(check.run).toContain('--locked'); expect(check.run).toContain('--target ${{ matrix.target }}');
    expect(check.run!.trim().split('\n')).toHaveLength(1);
  }
  for (const step of desktop.jobs.native!.steps!.filter(s => s.run?.includes('release.ts build') || s.run?.includes('release.ts package'))) {
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
  // v3 already contains immutable snapshots warmed with the wrong feature graph.
  expect(warm.with!['prefix-key']).toBe('v4-product-native');
  expect(warm.with!['save-if']).toContain('github.event.repository.default_branch');
  expect(warm.with!['save-if']).toContain("needs.desktop-plan.outputs.native == 'true'");
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
    expect(step.if).toContain("needs.desktop-plan.outputs.native == 'true'");
    expect(step.if).toContain("steps.product-cache.outputs.cache-hit != 'true'");
    expect(step.run).toContain('--release --locked --target ${{ matrix.target }}');
    expect(step.run).not.toMatch(/release\.ts|\bpackage\b|\bpublish\b|tauri\.js/);
  }
  const releaseBuild = workflow('.github/workflows/release.yml').jobs.build!.steps!.find(s => s.name === 'Build')!;
  expect(warm[0]!.name).toBe('Warm release CLI dependencies');
  expect(warm[0]!.run).toBe(releaseBuild.run);
  expect(warm[1]!.run).toContain('-p clat-desktop');
  expect(warm[1]!.run).toContain('--features tauri/custom-protocol');
  expect(warm[1]!.if).toContain("contains(matrix.target, 'windows')");
  const pinned = steps.findIndex(s => s.name === 'Add the pinned toolchain cross target');
  expect(steps.findIndex(s => s.id === 'product-cache')).toBeGreaterThan(pinned);
  for (const step of steps.filter(s => s.name?.startsWith('Repeated SHA') || s.name === 'Test authenticated compiler cache')) {
    expect(step.run).toContain('--target ${{ matrix.target }}');
  }
});
test('only the four desktop platforms use shared Release caches and extra warming', () => {
  const ci = workflow('.github/workflows/ci.yml').jobs['plugin-performance']!;
  const release = workflow('.github/workflows/release.yml').jobs.build!;
  const predicate = "contains(matrix.target, 'apple-darwin') || contains(matrix.target, 'windows')";
  for (const job of [ci, release]) {
    const caches = job.steps!.filter(s => s.with?.['prefix-key'] === 'v4-product-native');
    expect(caches).toHaveLength(1); expect(caches[0]!.if).toContain(predicate);
  }
  const cliWarm = ci.steps!.find(s => s.name === 'Warm release CLI dependencies')!;
  expect(cliWarm.if).toContain(predicate);
  expect(ci.strategy!.matrix.include.filter(r => r.target.includes('unknown-linux-gnu'))).toHaveLength(2);
  expect(ci.steps!.find(s => s.name === 'Compile the complete target surface')!.if).toBeUndefined();
  expect(ci.steps!.find(s => s.name === 'Repeated SHA comparison (release)')!.if).toBe('matrix.measure');
});
test('native checking and Release share explicit targets and compact CI profiles with one cache writer', () => {
  const ci = workflow('.github/workflows/ci.yml').jobs['plugin-performance']!;
  const manual = workflow('.github/workflows/desktop.yml').jobs.native!;
  const release = workflow('.github/workflows/release.yml').jobs.build!;
  const writer = ci.steps!.find(s => s.id === 'product-cache')!;
  for (const job of [ci, manual, release]) {
    expect(job.env?.CARGO_PROFILE_DEV_DEBUG).toBe('0');
    expect(job.env?.CARGO_PROFILE_TEST_DEBUG).toBe('0');
    const cache = job.steps!.find(s => s.uses === 'Swatinem/rust-cache@v2.9.2')!;
    expect(cache.with!['prefix-key']).toBe(writer.with!['prefix-key']);
    expect(cache.with!['shared-key']).toBe(writer.with!['shared-key']);
    expect(cache.with!['add-rust-environment-hash-key']).not.toBe(false);
    expect(cache.with!['cache-workspace-crates']).not.toBe(true);
    if (job !== ci) expect(cache.with!['save-if']).toBe(false);
    for (const check of job.steps!.filter(s => s.run?.startsWith('cargo clippy -p clat-desktop') || s.run?.startsWith('cargo test -p clat-desktop'))) {
      expect(check.run).toContain('--target ${{ matrix.target }}');
      expect(check.run).not.toContain('--release');
    }
  }
  expect(source('Cargo.toml')).not.toContain('[profile.dev]\ndebug = 0');
});
test('path planning guards native checks and retains cross-platform tooling without native compilation', () => {
  const ci = workflow('.github/workflows/ci.yml');
  const desktop = workflow('.github/workflows/desktop.yml');
  for (const plan of [ci.jobs['desktop-plan']!, desktop.jobs.plan!]) {
    expect(plan.outputs!.native).toContain('steps.changes.outputs.native');
    const checkout = plan.steps!.find(s => s.uses === 'actions/checkout@v5')!;
    expect(checkout.with!['fetch-depth']).toBe(0);
    expect(plan.steps!.find(s => s.id === 'changes')!.run).toBe('bun desktop/scripts/check-plan.ts');
  }
  const native = ci.jobs['plugin-performance']!.steps!;
  pinnedBefore(native, 'bun install');
  const scripts = native.find(s => s.run === 'bun test desktop/scripts --timeout 60000')!;
  expect(scripts.if).toContain("needs.desktop-plan.outputs.tooling == 'true'");
  expect(scripts.if).toContain("contains(matrix.target, 'apple-darwin') || contains(matrix.target, 'windows')");
  expect(desktop.jobs['web-shell']!.if).toContain("needs.plan.outputs.web == 'true'");
  expect(desktop.jobs.tooling!.if).toContain("needs.plan.outputs.tooling == 'true'");
});
