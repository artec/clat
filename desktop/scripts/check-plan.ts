import { appendFileSync, readFileSync } from 'node:fs';
import { ROOT } from './common';

export interface CheckPlan { native: boolean; tooling: boolean; web: boolean }
type ChangeEvent = { before?: string; pull_request?: { base?: { sha?: string } } };
type Diff = (base: string, head: string) => string[] | undefined;

// Only known non-native inputs may skip native validation. New/unrecognized
// build inputs take the conservative path, including deletions and renames.
export function checksForPaths(paths: string[] | undefined): CheckPlan {
  if (!paths) return { native: true, tooling: true, web: true };
  let native = false, tooling = false, web = false;
  for (const path of paths) {
    // Presentation is reviewed by the owner. Catalog structure is checked in plan.
    if (/^(web\/[^/]+\.css|desktop\/ui\/[^/]+\.(css|png|svg|ico)|(web|desktop)\/icons\/[^/]+\.(png|svg|ico|icns))$/.test(path)
      || path.startsWith('web/e2e/manual/') || path === 'web/e2e/playwright.visual.config.js'
      || /^locales\/(en|zh-Hans|zh-Hant)\.json$/.test(path)) continue;
    if (path.startsWith('locales/') || path.startsWith('src/i18n/') || path.startsWith('scripts/i18n-')) { native=true;tooling=true;web=true; }
    // This job runs the native bridge suite, which does not load the PWA app.
    else if (/^desktop\/ui\/[^/]+\.(js|html)$/.test(path) || path === 'web/i18n.js'
      || path.startsWith('web/e2e/desktop/') || path === 'web/e2e/playwright.desktop.config.js'
      || /^web\/e2e\/package(?:-lock)?\.json$/.test(path)) web = true;
    else if (path.startsWith('web/')) continue;
    else if (path.startsWith('desktop/scripts/') || /^(desktop\/(package\.json|bun\.lock|tsconfig\.json)|package\.json|publish)$/.test(path)) tooling = true;
    else if (!/^(docs\/|\.agents\/|\.claude\/|README(?:\.[^/]+)?$|LICENSE$|\.gitignore$|\.editorconfig$)/.test(path)) native = true;
  }
  return { native, tooling: tooling || native, web: web || native };
}

export function gitChangedPaths(base: string, head: string, root = ROOT): string[] | undefined {
  const result = Bun.spawnSync(['git', 'diff', '--no-renames', '--name-only', '-z', base, head, '--'],
    { cwd: root, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', timeout: 30000 });
  return result.success ? result.stdout.toString().split('\0').filter(Boolean) : undefined;
}

export function pathsForEvent(name: string, event: ChangeEvent | undefined, head: string, diff: Diff = gitChangedPaths): string[] | undefined {
  const base = name === 'pull_request' ? event?.pull_request?.base?.sha : name === 'push' ? event?.before : undefined;
  const sha = /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/;
  if (!base || !sha.test(base) || !sha.test(head) || /^0+$/.test(base)) return undefined;
  // PR checkout is the tested merge commit. Diff from its base, not HEAD^ or
  // just the last push commit; missing history must widen checks, never skip.
  try { return diff(base, head); } catch { return undefined; }
}

if (import.meta.main) {
  let event: ChangeEvent | undefined;
  try { event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH!, 'utf8')); } catch { /* Full checks without an event. */ }
  const paths = pathsForEvent(process.env.GITHUB_EVENT_NAME ?? '', event, process.env.GITHUB_SHA ?? '');
  const plan = checksForPaths(paths);
  const message = `Desktop checks: native=${plan.native}, tooling=${plan.tooling}, web=${plan.web}; changed paths=${paths?.length ?? 'unknown (full checks)'}`;
  console.log(message);
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(plan).map(([name, value]) => `${name}=${value}\n`).join(''));
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, message + '\n');
}
