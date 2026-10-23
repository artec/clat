import { closeSync, existsSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { cleanEnv, digest, readPassword, withTemp } from './common';
import { DesktopRelease } from './release';
import { LocalMacos } from './local-macos';
import { ReleaseBuild } from './release-build';
import { assembleIndex, PAYLOAD, publicFiles, verifyIndex } from './release-index';

export const REPO = 'artec/clat';
export interface RemoteRelease { tagName: string; isDraft: boolean; assets: { name: string }[] }
export type Interactive = (args: string[]) => number;
export const interactive: Interactive = args => Bun.spawnSync(args, { env: cleanEnv(), stdin: 'inherit', stdout: 'inherit', stderr: 'inherit' }).exitCode;

export class OwnerRelease {
  constructor(readonly release = new DesktopRelease(), readonly prompt = readPassword, readonly interact: Interactive = interactive) {}
  tag(): string { return 'v' + this.release.version(); }
  directory(): string { return join(this.release.root, 'dist-release', this.tag()); }
  gh(...args: string[]): string { return this.release.execute(['gh', ...args], { cwd: this.release.root, quiet: true }); }
  draft(): RemoteRelease {
    const value = JSON.parse(this.gh('release', 'view', this.tag(), '--repo', REPO, '--json', 'tagName,isDraft,assets')) as RemoteRelease;
    if (value.tagName !== this.tag() || !value.isDraft) throw new Error('当前 Cargo.toml 版本必须已有 CI 创建的草稿 Release。');
    return value;
  }
  archives(): string[] { return [...this.release.names('cli', this.tag()), ...this.release.names('desktop', this.tag())]; }
  remoteArchives(): string[] {
    return [...this.release.names('cli', this.tag()), ...this.release.names('desktop', this.tag()).filter(n => n.includes('-windows-'))];
  }
  payloads(): string[] { return [...this.archives(), 'latest.json']; }
  files(): string[] { return publicFiles(this.release, this.tag()); }
  notes(): string {
    const base = `https://github.com/${REPO}/releases/download/${this.tag()}/`;
    const cli = this.release.names('cli', this.tag()).map(name => `- [${name}](${base}${name})`);
    const desktop = this.release.records('desktop').map(record => {
      const name = `clat-desktop-${this.tag()}-${record.label}${record.platform!.startsWith('darwin') ? '.dmg' : '-setup.exe'}`;
      return `- [${record.label}](${base}${name})`;
    });
    return ['CLAT ' + this.tag(), '', '**v2.1.1 及更早版本请手动重装一次；Do not use the old updater: reinstall once.**',
      '', '## CLI / Host', '', ...cli, '', '## Desktop', '', ...desktop,
      '', 'Signatures and checksums are authenticated by [latest.json](' + base + 'latest.json).', ''].join('\n');
  }
  verifyChecksum(folder: string, name: string): void {
    const path = join(folder, name), manifest = path + '.sha256';
    for (const file of [path, manifest]) if (!existsSync(file) || !lstatSync(file).isFile()) throw new Error('缺少普通发布文件：' + name);
    const fields = readFileSync(manifest, 'utf8').match(/^([\da-fA-F]{64})[ \t]+\*?([^\r\n]+)\r?\n?$/);
    if (!fields || fields[2] !== name || fields[1]!.toLowerCase() !== digest(path)) throw new Error('发布文件校验失败：' + name);
  }
  async verifyDesktop(folder: string): Promise<void> { await verifyIndex(this.release, folder, true); }
  async verify(folder: string): Promise<void> { await verifyIndex(this.release, folder); }
  async localLock<T>(action: () => Promise<T>): Promise<T> {
    if (process.platform === 'win32') throw new Error('./publish 请在负责人的 macOS 签名机器运行。');
    const { dlopen } = await import('bun:ffi');
    const library = dlopen(process.platform === 'darwin' ? '/usr/lib/libSystem.B.dylib' : 'libc.so.6', {
      flock: { args: ['int', 'int'], returns: 'int' },
    });
    let fd: number | undefined;
    try {
      mkdirSync(this.directory(), { recursive: true });
      fd = openSync(join(this.directory(), '.owner.lock'), 'a', 0o600);
      if (library.symbols.flock(fd, 6) !== 0) throw new Error('已有本机发布操作正在运行。');
      return await action();
    } finally {
      try { if (fd !== undefined) closeSync(fd); } finally { library.close(); }
    }
  }
  async prepare(): Promise<void> {
    if (process.platform !== 'darwin') throw new Error('./publish 必须在负责人的 macOS 签名机器运行。');
    await this.localLock(async () => {
      const required = this.remoteArchives().flatMap(n => [n, n + '.sha256']);
      new LocalMacos(this.release, this.directory()).source();
      const previous = process.env.CLAT_DESKTOP_KEY_PASSWORD;
      try {
        process.env.CLAT_DESKTOP_KEY_PASSWORD = previous ?? await this.prompt('桌面更新私钥密码：');
        await this.release.preflightSigning(this.tag());
        await this.buildPrepared(required);
      } finally {
        if (previous === undefined) delete process.env.CLAT_DESKTOP_KEY_PASSWORD;
        else process.env.CLAT_DESKTOP_KEY_PASSWORD = previous;
      }
    });
    console.log('本机签名及公证完成。即将核验、上传并发布。');
  }
  async buildPrepared(required: string[]): Promise<void> {
    const destination = join(this.directory(), 'prepared');
    if (existsSync(destination)) throw new Error('本版本已有 prepared 产物；请先移走该目录再重新准备，避免覆盖。');
    const commit = new LocalMacos(this.release, this.directory()).source();
    const build = new ReleaseBuild(this.gh.bind(this), commit, this.release.version(), this.directory());
    await build.locate();
    const local = new LocalMacos(this.release, this.directory(), build.backend.bind(build));
    await withTemp('preparing-', async folder => {
      await local.prepare(folder);
      await build.ready();
      const remote = this.draft();
      if (!required.every(n => remote.assets.some(a => a.name === n))) throw new Error('CI 草稿缺少正式发布产物或校验文件。');
      this.gh('release', 'download', this.tag(), '--repo', REPO, '--dir', folder, ...required.flatMap(n => ['--pattern', n]));
      for (const name of this.remoteArchives()) this.verifyChecksum(folder, name);
      await this.release.finalize(folder, this.tag(), true);
      const args = ['minisign', '-S', '-s', join(this.release.root, '.release-secrets/clat-minisign.key'), '-t', `CLAT ${this.tag()} release index`,
        '-m', join(folder, PAYLOAD)];
      if (this.interact(args) !== 0) throw new Error('CLI 索引签名失败；未上传。');
      assembleIndex(folder);
      await this.verify(folder);
      if (local.source() !== commit) throw new Error('签名期间源码身份已改变，未保存发布产物。');
      renameSync(folder, destination);
    }, this.directory());
  }
  async compareRemote(remote: RemoteRelease): Promise<void> {
    if (JSON.stringify(remote.assets.map(a => a.name).sort()) !== JSON.stringify(this.files())) throw new Error('远程缺少完整发布资产；请重新运行 ./publish。');
    await withTemp('clat-remote-verify-', folder => {
      this.gh('release', 'download', this.tag(), '--repo', REPO, '--dir', folder, ...this.files().flatMap(n => ['--pattern', n]));
      for (const name of this.files()) if (digest(join(folder, name)) !== digest(join(this.directory(), 'prepared', name))) throw new Error('远程资产与本机已验证文件不一致：' + name);
    });
  }
  async upload(): Promise<void> {
    await this.localLock(async () => {
      this.draft(); const folder = join(this.directory(), 'prepared'); await this.verify(folder);
      this.gh('release', 'upload', this.tag(), '--repo', REPO, ...this.files().map(n => join(folder, n)), '--clobber');
      await this.compareRemote(this.draft());
    });
    console.log('已上传并核对草稿，准备公开发布。');
  }
  async publish(): Promise<void> {
    await this.localLock(async () => {
      await this.verify(join(this.directory(), 'prepared')); await this.compareRemote(this.draft()); this.draft();
      await withTemp('clat-release-notes-', folder => {
        const notes = join(folder, 'notes.md'); writeFileSync(notes, this.notes());
        this.gh('release', 'edit', this.tag(), '--repo', REPO, '--notes-file', notes, '--draft=false');
      });
    });
    console.log('已公开发布 ' + this.tag());
  }
  authenticate(): void {
    try { this.gh('auth', 'status'); }
    catch {
      if (this.interact(['gh', 'auth', 'login', '--web', '--clipboard', '--git-protocol', 'https']) !== 0) throw new Error('GitHub 登录未完成；未发布。');
      this.gh('auth', 'status');
    }
  }
  async complete(): Promise<void> {
    this.authenticate();
    if (existsSync(join(this.directory(), 'prepared'))) {
      await this.verify(join(this.directory(), 'prepared')); console.log('复用已验证的本机签名产物，继续发布。');
    } else await this.prepare();
    await this.upload(); await this.publish();
  }
}
