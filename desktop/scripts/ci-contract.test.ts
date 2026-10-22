import { expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { json, ROOT } from './common';

type Step = { uses?: string; run?: string; name?: string; with?: Record<string, string | number> };
type Workflow = { jobs: Record<string, { steps?: Step[] }> };
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
  pinnedBefore(release.jobs.desktop!.steps!, 'bun install');
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
