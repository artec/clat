import { existsSync, renameSync } from 'node:fs';
import { join } from 'node:path';
import { verifyBackend } from './backend';
import { withTemp } from './common';

interface RunState { databaseId: number; headSha: string; headBranch: string; event: string; status: string; conclusion: string | null }
type Gh = (...args: string[]) => string;
export type Pause = (ms: number) => Promise<void>;
const pause: Pause = ms => new Promise(resolve => setTimeout(resolve, ms));

// Only artifacts of this repository's version-tag Release run are admitted.
export class ReleaseBuild {
  runId = '';
  constructor(readonly gh: Gh, readonly commit: string, readonly version: string, readonly directory: string, readonly sleep: Pause = pause, readonly attempts = 180) {}
  validate(run: RunState): void {
    if (run.headSha !== this.commit || run.headBranch !== 'v' + this.version || !['push', 'workflow_dispatch'].includes(run.event)) throw new Error('远程 Release 源码或标签身份不匹配。');
  }
  async locate(): Promise<void> {
    for (let attempt = 0; attempt < this.attempts; attempt++) {
      const runs = JSON.parse(this.gh('run', 'list', '--repo', 'artec/clat', '--workflow', 'release.yml', '--commit', this.commit, '--limit', '50', '--json', 'databaseId,headSha,headBranch,event,status,conclusion')) as RunState[];
      const run = runs.filter(r => r.headBranch === 'v' + this.version && ['push', 'workflow_dispatch'].includes(r.event)).sort((a, b) => b.databaseId - a.databaseId)[0];
      if (run) { this.validate(run); this.runId = String(run.databaseId); return; }
      if (!attempt) console.log('⟳ 等待版本标签触发 Release 构建');
      await this.sleep(30000);
    }
    throw new Error('等待 Release 启动超时；可重新运行 ./publish。');
  }
  state(): RunState {
    if (!/^\d+$/.test(this.runId)) throw new Error('Release run is not selected');
    const run = JSON.parse(this.gh('run', 'view', this.runId, '--repo', 'artec/clat', '--json', 'headSha,headBranch,event,status,conclusion')) as RunState;
    this.validate(run); return run;
  }
  failed(run: RunState): boolean { return run.status === 'completed' && run.conclusion !== 'success'; }
  async backend(target: string): Promise<{ folder: string; runId: string }> {
    const expected = { commit: this.commit, version: this.version, target, runId: this.runId };
    const parent = join(this.directory, 'backends', this.commit, this.runId), folder = join(parent, target);
    if (existsSync(folder)) { verifyBackend(folder, expected); return { folder, runId: this.runId }; }
    for (let attempt = 0; attempt < this.attempts; attempt++) {
      const run = this.state();
      const artifacts = JSON.parse(this.gh('api', `repos/artec/clat/actions/runs/${this.runId}/artifacts?name=backend-${target}`)) as { artifacts: { name: string; expired: boolean }[] };
      if (artifacts.artifacts.some(a => a.name === 'backend-' + target && !a.expired)) {
        await withTemp('downloading-', temporary => {
          this.gh('run', 'download', this.runId, '--repo', 'artec/clat', '--name', 'backend-' + target, '--dir', temporary);
          verifyBackend(temporary, expected); renameSync(temporary, folder);
        }, parent);
        return { folder, runId: this.runId };
      }
      if (this.failed(run)) throw new Error('远程 Release 失败，且尚无所需 CLI；重跑失败任务后再次 ./publish。');
      if (!attempt) console.log('⟳ 等待远程 CLI：' + target + '（其他平台继续并行）');
      await this.sleep(30000);
    }
    throw new Error('等待 CLI 产物超时；本机已完成阶段保留。');
  }
  async ready(): Promise<void> {
    for (let attempt = 0; attempt < this.attempts; attempt++) {
      const run = this.state();
      if (this.failed(run)) throw new Error('远程 Release 未通过；本机已完成阶段保留，请重跑失败任务。');
      if (run.status === 'completed' && run.conclusion === 'success') return;
      if (!attempt) console.log('⟳ 本机 macOS 已完成，等待远程其余产物及草稿');
      await this.sleep(30000);
    }
    throw new Error('等待远程 Release 完成超时；可重新运行 ./publish。');
  }
}
