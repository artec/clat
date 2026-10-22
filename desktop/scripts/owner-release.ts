import { closeSync, existsSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { cleanEnv, decodeWrapper, digest, json, readPassword, withTemp } from './common';
import { DesktopRelease } from './release';

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
  payloads(): string[] {
    const updates = this.release.names('desktop', this.tag()).filter(n => !n.endsWith('.dmg')).map(n => n + '.sig');
    return [...this.archives(), ...updates, 'desktop-latest.json', 'desktop-latest.json.sig'];
  }
  files(): string[] { return this.payloads().flatMap(n => [n, n + '.sha256', n + '.sha256.minisig']).sort(); }
  verifyChecksum(folder: string, name: string): void {
    const path = join(folder, name), manifest = path + '.sha256';
    for (const file of [path, manifest]) if (!existsSync(file) || !lstatSync(file).isFile()) throw new Error('缺少普通发布文件：' + name);
    const fields = readFileSync(manifest, 'utf8').match(/^([\da-fA-F]{64})[ \t]+\*?([^\r\n]+)\r?\n?$/);
    if (!fields || fields[2] !== name || fields[1]!.toLowerCase() !== digest(path)) throw new Error('发布文件校验失败：' + name);
  }
  async verifyDesktop(folder: string): Promise<void> {
    await withTemp('clat-release-verify-', temporary => {
      const publicKey = join(temporary, 'public'), signature = join(temporary, 'signature');
      writeFileSync(publicKey, decodeWrapper(readFileSync(join(this.release.desktop, 'updater.pub'))));
      const signed = ['desktop-latest.json', ...this.release.names('desktop', this.tag()).filter(n => !n.endsWith('.dmg'))];
      for (const name of signed) {
        const raw = decodeWrapper(readFileSync(join(folder, name + '.sig')));
        const lines = raw.toString().trimEnd().split(/\r?\n/);
        if (lines.length !== 4 || !lines[2]!.split('\t').includes('version:' + this.release.version())) throw new Error('桌面更新签名未绑定当前版本。');
        writeFileSync(signature, raw);
        this.release.execute(['minisign', '-Vm', join(folder, name), '-p', publicKey, '-x', signature, '-q']);
      }
    });
    const index = json<{ version: string; platforms: Record<string, { url: string; signature: string }> }>(join(folder, 'desktop-latest.json'));
    const expectedPlatforms = this.release.records('desktop').map(r => r.platform).sort();
    if (index.version !== this.release.version() || JSON.stringify(Object.keys(index.platforms).sort()) !== JSON.stringify(expectedPlatforms)) throw new Error('桌面更新索引的版本或四平台清单不匹配。');
    for (const record of this.release.records('desktop')) {
      const suffix = record.platform!.startsWith('darwin') ? '.app.tar.gz' : '-setup.exe';
      const name = `clat-desktop-${this.tag()}-${record.label}${suffix}`;
      const item = index.platforms[record.platform!]!;
      if (item.url !== `https://github.com/${REPO}/releases/download/${this.tag()}/${name}`
        || item.signature !== readFileSync(join(folder, name + '.sig'), 'utf8').trim()) throw new Error('更新索引的下载目标或包签名不匹配。');
    }
  }
  async verify(folder: string): Promise<void> {
    for (const name of this.payloads()) {
      this.verifyChecksum(folder, name);
      const manifest = join(folder, name + '.sha256'), signature = manifest + '.minisig';
      if (!lstatSync(signature).isFile()) throw new Error('签名必须是普通文件。');
      const lines = readFileSync(signature, 'utf8').trimEnd().split(/\r?\n/);
      if (lines.length !== 4 || lines[2] !== `trusted comment: CLAT ${this.tag()} release checksum manifests`) throw new Error('校验清单签名未绑定当前版本。');
      this.release.execute(['minisign', '-Vm', manifest, '-p', join(this.release.root, 'release/minisign.pub'), '-x', signature, '-q']);
    }
    await this.verifyDesktop(folder);
  }
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
      const remote = this.draft();
      const required = this.archives().flatMap(n => [n, n + '.sha256']);
      if (!required.every(n => remote.assets.some(a => a.name === n))) throw new Error('CI 草稿尚未包含全部六种 CLI 和四平台桌面产物及校验文件。');
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
    await withTemp('preparing-', async folder => {
      this.gh('release', 'download', this.tag(), '--repo', REPO, '--dir', folder, ...required.flatMap(n => ['--pattern', n]));
      for (const name of this.archives()) this.verifyChecksum(folder, name);
      await this.release.finalize(folder, this.tag());
      const args = ['minisign', '-S', '-s', join(this.release.root, '.release-secrets/clat-minisign.key'), '-t', `CLAT ${this.tag()} release checksum manifests`,
        '-m', ...this.payloads().map(n => join(folder, n + '.sha256'))];
      if (this.interact(args) !== 0) throw new Error('CLI 校验清单签名失败；未上传。');
      await this.verify(folder);
      renameSync(folder, destination);
    }, this.directory());
  }
  async compareRemote(remote: RemoteRelease): Promise<void> {
    if (!this.files().every(n => remote.assets.some(a => a.name === n))) throw new Error('远程缺少完整发布资产；请重新运行 ./publish。');
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
      this.gh('release', 'edit', this.tag(), '--repo', REPO, '--draft=false');
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
    this.authenticate(); this.draft();
    if (existsSync(join(this.directory(), 'prepared'))) {
      await this.verify(join(this.directory(), 'prepared')); console.log('复用已验证的本机签名产物，继续发布。');
    } else await this.prepare();
    await this.upload(); await this.publish();
  }
}
