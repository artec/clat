import { copyFileSync, existsSync, lstatSync, mkdirSync, renameSync } from 'node:fs';
import { join } from 'node:path';
import { checksum, digest, json, withTemp, writeJson } from './common';
import { DesktopRelease, type AssetRecord } from './release';

interface Receipt { commit: string; target: string; stage: string; hashes: Record<string, string> }

function duration(milliseconds: number): string {
  const seconds = Math.floor(milliseconds / 1000);
  const minutes = Math.floor(seconds / 60);
  const tail = `${String(seconds % 60).padStart(2, '0')}秒`;
  return minutes < 60 ? `${minutes}分${tail}` : `${Math.floor(minutes / 60)}小时${String(minutes % 60).padStart(2, '0')}分${tail}`;
}

// Rebuildable packaging cache only. No credentials or business data live here.
export class LocalMacos {
  constructor(readonly release: DesktopRelease, readonly directory: string,
    readonly backend: (target: string) => Promise<{ folder: string; runId: string }> = async () => { throw new Error('verified CLI backend provider required'); },
    readonly clock: () => number = () => performance.now()) {}
  source(): string {
    const git = (...args: string[]) => this.release.execute(['git', ...args], { cwd: this.release.root, quiet: true }).trim();
    if (git('status', '--porcelain', '--untracked-files=all')) throw new Error('正式本机构建要求干净工作树，请先由负责人提交改动。');
    const commit = git('rev-parse', 'HEAD');
    if (!/^[a-f0-9]{40}$/.test(commit) || git('rev-parse', `v${this.release.version()}^{commit}`) !== commit) throw new Error('本机 HEAD 必须与当前版本标签对应。');
    return commit;
  }
  names(record: AssetRecord, signed: boolean): string[] {
    const stem = `clat-desktop-v${this.release.version()}-${record.label}`;
    return [stem + '.app.tar.gz', ...(signed ? [stem + '.dmg'] : [])];
  }
  cached(folder: string, commit: string, record: AssetRecord, stage: string): boolean {
    if (!existsSync(folder)) return false;
    const receipt = json<Receipt>(join(folder, 'receipt.json'));
    const names = this.names(record, stage === 'signed');
    if (receipt.commit !== commit || receipt.target !== record.target || receipt.stage !== stage
      || JSON.stringify(Object.keys(receipt.hashes).sort()) !== JSON.stringify(names.sort())) throw new Error('本机阶段产物身份不匹配，请检查缓存：' + folder);
    for (const name of names) if (!lstatSync(join(folder, name)).isFile() || receipt.hashes[name] !== digest(join(folder, name))) throw new Error('本机阶段产物已改变，拒绝复用：' + folder);
    return true;
  }
  async stage(parent: string, commit: string, record: AssetRecord, stage: string, action: (folder: string) => Promise<void>): Promise<string> {
    const destination = join(parent, stage);
    if (this.cached(destination, commit, record, stage)) { console.log(`✓ 复用 ${record.label} ${stage} 阶段产物`); return destination; }
    const started = this.clock();
    const label = `${record.label} ${stage === 'unsigned' ? '准备与编译打包' : '签名、公证与安装包制作'}`;
    try {
      await withTemp('preparing-', async folder => {
        await action(folder);
        if (this.source() !== commit) throw new Error('构建期间源码发生变化，未保存阶段产物。');
        const hashes = Object.fromEntries(this.names(record, stage === 'signed').map(name => [name, digest(join(folder, name))]));
        writeJson(join(folder, 'receipt.json'), { commit, target: record.target, stage, hashes });
        renameSync(folder, destination);
      }, parent);
    } catch (error) {
      console.log(`✗ ${label}失败，已用时 ${duration(this.clock() - started)}`);
      throw error;
    }
    console.log(`✓ ${label}完成，耗时 ${duration(this.clock() - started)}`);
    return destination;
  }
  async prepare(output: string): Promise<void> {
    const commit = this.source();
    const totals: string[] = [];
    for (const record of this.release.records('desktop').filter(r => r.platform!.startsWith('darwin'))) {
      const started = this.clock();
      const parent = join(this.directory, 'macos', commit, record.target);
      mkdirSync(parent, { recursive: true });
      const unsigned = await this.stage(parent, commit, record, 'unsigned', async folder => {
        const backend = await this.backend(record.target);
        console.log(`⟳ 本机编译 ${record.label} 桌面程序（复用 CLI 后端）`);
        this.release.execute(['rustup', 'target', 'add', record.target], { cwd: this.release.root });
        await this.release.build(record.target, { release: true, official: true, appOnly: true, backend: backend.folder, runId: backend.runId });
        this.release.package(record.target, folder, true);
      });
      const signed = await this.stage(parent, commit, record, 'signed', async folder => {
        for (const name of this.names(record, false)) copyFileSync(join(unsigned, name), join(folder, name));
        console.log(`⟳ 签名、公证并制作 ${record.label} 安装包`);
        await this.release.signMacos(folder, record, `v${this.release.version()}`);
      });
      for (const name of this.names(record, true)) { copyFileSync(join(signed, name), join(output, name)); checksum(join(output, name)); }
      const total = `${record.label} 本次准备总耗时 ${duration(this.clock() - started)}`;
      console.log(`✓ ${total}`); totals.push(total);
    }
    if (this.source() !== commit) throw new Error('本机构建结束时源码身份已改变。');
    console.log('macOS 桌面打包耗时汇总（含后端等待/下载、编译、签名、公证；缓存复用只计本次检查时间）：\n' + totals.join('\n'));
  }
}
