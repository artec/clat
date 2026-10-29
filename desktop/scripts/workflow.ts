import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { cleanEnv, DESKTOP, developmentText, nativeTarget, ROOT, run, type Environment, type Runner } from './common';
import type { DesktopRelease } from './release';

export function ensureTools(execute: Runner = run): void {
  if (!['@tauri-apps/cli/tauri.js', 'tar/package.json', 'typescript/bin/tsc'].every(n => existsSync(join(DESKTOP, 'node_modules', n)))) {
    execute([process.execPath, 'install', '--cwd', DESKTOP, '--frozen-lockfile', '--ignore-scripts']);
  }
}
export function startArgs(release: DesktopRelease, target: string): string[] {
  return [process.execPath, join(release.desktop, 'node_modules/@tauri-apps/cli/tauri.js'), 'dev', '--no-dev-server', '--target', target,
    '--config', JSON.stringify({ version: release.version() }), '--', '--locked'];
}
export interface AppProcess { exited: Promise<number>; kill(signal: 'SIGTERM'): void }
export type Launcher = (args: string[], cwd: string, env: Environment) => AppProcess;
export const launch: Launcher = (args, cwd, env) => Bun.spawn(args, { cwd, env: cleanEnv(env), stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' });
export async function stopDevelopmentHost(release: DesktopRelease, target: string): Promise<void> {
  const backend = join(release.desktop, 'resources', target.includes('windows') ? 'clat-backend.exe' : 'clat-backend');
  const status = () => { try { release.execute([backend, 'host', 'status'], { cwd: release.root, quiet: true }); return true; } catch { return false; } };
  if (!status()) return;
  console.log(developmentText('desktop.dev_stop_host'));
  release.execute([backend, 'host', 'stop'], { cwd: release.root });
  const deadline = Date.now() + 30000;
  while (status()) {
    if (Date.now() >= deadline) throw new Error(developmentText('desktop.dev_stop_timeout'));
    await new Promise(resolve => setTimeout(resolve, 100));
  }
}
export async function start(release: DesktopRelease, launchApp: Launcher = launch): Promise<number> {
  const target = nativeTarget(); release.stageBackend(target);
  await stopDevelopmentHost(release, target);
  const child = launchApp(startArgs(release, target), release.desktop, release.buildEnv(target));
  const terminate = () => child.kill('SIGTERM');
  process.once('SIGINT', terminate); process.once('SIGTERM', terminate);
  try { return await child.exited; } finally { process.off('SIGINT', terminate); process.off('SIGTERM', terminate); }
}
export async function build(release: DesktopRelease): Promise<void> {
  const target = nativeTarget(); await release.build(target, { release: true, official: true }); release.package(target, join(ROOT, 'dist-desktop'));
  console.log('本机测试安装包已生成：' + join(ROOT, 'dist-desktop'));
  console.log('尚未完成发布签名公证；正式发布仍使用仓库根目录 ./publish。');
}
export async function main(args = process.argv.slice(2)): Promise<void> {
  if (args.includes('--help') || args.includes('-h')) { console.log('CLAT Desktop: bun start | bun app:build | bun app:test | ./publish'); return; }
  const command = args[0];
  if (args.length !== 1 || !['start', 'build', 'test', 'release'].includes(command!)) throw new Error('unknown desktop command');
  ensureTools();
  if (command === 'test') {
    const result = Bun.spawnSync([process.execPath, 'test', join(DESKTOP, 'scripts'), '--timeout', '60000'], { cwd: ROOT, env: cleanEnv(), stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' });
    process.exitCode = result.exitCode; return;
  }
  const { DesktopRelease } = await import('./release'); const release = new DesktopRelease();
  if (command === 'start') process.exitCode = await start(release);
  else if (command === 'build') await build(release);
  else { const { OwnerRelease } = await import('./owner-release'); await new OwnerRelease(release).complete(); }
}
if (import.meta.main) main().catch(error => { console.error('CLAT Desktop: ' + error.message); process.exitCode = 1; });
