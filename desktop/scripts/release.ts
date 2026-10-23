import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { createArchive, safeExtract } from './archive';
import { sourceCommit, verifyBackend } from './backend';
import { checksum, DESKTOP, digest, json, ROOT, run, type Runner, withTemp, writeJson, decodeWrapper, privateWrite } from './common';

import { PAYLOAD, writePayload } from './release-index';

export interface AssetRecord { os: string; target: string; platform?: string; label?: string }
export type AssetKind = 'cli' | 'desktop';
const MATRIX = json<Record<AssetKind, AssetRecord[]>>(join(DESKTOP, 'assets.json'));
export interface BuildOptions { release?: boolean; official?: boolean; appOnly?: boolean; backend?: string; runId?: string }

export class DesktopRelease {
  constructor(readonly root = ROOT, readonly execute: Runner = run) {}
  get desktop(): string { return join(this.root, 'desktop'); }
  version(): string {
    const section = readFileSync(join(this.root, 'Cargo.toml'), 'utf8').split('[workspace.package]')[1]?.split(/\n\[/)[0];
    const value = section?.match(/^version\s*=\s*"(\d+\.\d+\.\d+)"/m)?.[1];
    if (!value) throw new Error('workspace release version must be x.y.z');
    return value;
  }
  records(kind: AssetKind): AssetRecord[] {
    if (!(kind in MATRIX)) throw new Error('unknown asset family');
    return MATRIX[kind];
  }
  names(kind: AssetKind, tag: string): string[] {
    if (!/^v\d+\.\d+\.\d+$/.test(tag)) throw new Error('unsafe release tag');
    return this.records(kind).flatMap(record => {
      if (kind === 'cli') return [`clat-${tag}-${record.target}${record.target.includes('windows') ? '.zip' : '.tar.gz'}`];
      const stem = `clat-desktop-${tag}-${record.label}`;
      return record.platform!.startsWith('darwin') ? [stem + '.dmg', stem + '.app.tar.gz'] : [stem + '-setup.exe'];
    });
  }
  targetDirectory(): string {
    return JSON.parse(this.execute(['cargo', 'metadata', '--no-deps', '--format-version', '1', '--offline'],
      { cwd: this.root, quiet: true, timeout: 30000 })).target_directory;
  }
  buildEnv(target: string, official = false): Record<string, string> {
    return { ...(official ? { CLAT_OFFICIAL_RELEASE: '1' } : {}),
      ...(target.includes('apple-darwin') ? { MACOSX_DEPLOYMENT_TARGET: '13.0' } : {}) };
  }
  stageBackend(target: string, options: BuildOptions = {}): void {
    this.records('desktop').find(r => r.target === target) ?? (() => { throw new Error('unsupported desktop target'); })();
    let source: string;
    if (options.backend) {
      if (!options.release || !options.official) throw new Error('prebuilt backend requires an official release build');
      source = verifyBackend(options.backend, { commit: sourceCommit(this.root, this.execute), version: this.version(), target, runId: options.runId ?? process.env.GITHUB_RUN_ID ?? '' });
      console.log('✓ 复用已验证的 CLI 后端：' + target);
    } else {
      const args = ['cargo', 'build', '-p', 'clat', '--locked', '--target', target];
      if (options.release) args.push('--release');
      this.execute(args, { cwd: this.root, env: this.buildEnv(target, options.official) });
      source = join(this.targetDirectory(), target, options.release ? 'release' : 'debug', target.includes('windows') ? 'clat.exe' : 'clat');
    }
    const resources = join(this.desktop, 'resources');
    mkdirSync(resources, { recursive: true });
    for (const name of readdirSync(resources).filter(n => n.startsWith('clat-backend'))) unlinkSync(join(resources, name));
    const staged = join(resources, target.includes('windows') ? 'clat-backend.exe' : 'clat-backend');
    copyFileSync(source, staged);
    if (process.platform !== 'win32') chmodSync(staged, 0o755);
    this.backendManifest(resources, target, !!options.official);
  }
  backendManifest(directory: string, target: string, official: boolean): void {
    const binary = join(directory, target.includes('windows') ? 'clat-backend.exe' : 'clat-backend');
    writeJson(join(directory, 'backend.json'), { version: this.version(), target, sha256: digest(binary), size: statSync(binary).size, official });
  }
  async build(target: string, options: BuildOptions = {}): Promise<void> {
    this.stageBackend(target, options);
    const args = [process.execPath, join(this.desktop, 'node_modules/@tauri-apps/cli/tauri.js'), 'build', '--target', target,
      '--bundles', target.includes('windows') ? 'nsis' : 'app', '--config', JSON.stringify({ version: this.version() })];
    if (!options.release) args.push('--debug');
    args.push('--', '--locked');
    this.execute(args, { cwd: this.desktop, env: this.buildEnv(target, options.official) });
    if (target.includes('apple-darwin') && !options.appOnly) {
      const bundle = join(this.targetDirectory(), target, options.release ? 'release' : 'debug', 'bundle');
      const app = this.single(join(bundle, 'macos'), '.app');
      const arch = target.startsWith('aarch64-') ? 'aarch64' : 'x64';
      await this.makeDmg(app, join(bundle, 'dmg', `CLAT Desktop_${this.version()}_${arch}.dmg`));
    }
  }
  single(directory: string, suffix: string): string {
    const matches = readdirSync(directory).filter(n => n.endsWith(suffix));
    if (matches.length !== 1) throw new Error('expected exactly one ' + suffix);
    return join(directory, matches[0]!);
  }
  async makeDmg(app: string, destination: string): Promise<void> {
    mkdirSync(dirname(destination), { recursive: true });
    await withTemp('clat-desktop-dmg-', folder => {
      const image = join(folder, 'image-root'); mkdirSync(image);
      this.execute(['ditto', app, join(image, basename(app))]);
      symlinkSync('/Applications', join(image, 'Applications'));
      this.execute(['hdiutil', 'create', '-volname', 'CLAT Desktop', '-srcfolder', image, '-ov', '-format', 'UDZO', destination]);
    });
  }
  package(target: string, output: string, appOnly = false): void {
    const record = this.records('desktop').find(r => r.target === target);
    if (!record) throw new Error('unsupported desktop target');
    const bundle = join(this.targetDirectory(), target, 'release/bundle');
    mkdirSync(output, { recursive: true });
    const stem = `clat-desktop-v${this.version()}-${record.label}`;
    const files: string[] = [];
    if (target.includes('windows')) {
      files.push(join(output, stem + '-setup.exe'));
      copyFileSync(this.single(join(bundle, 'nsis'), '.exe'), files[0]!);
    } else {
      if (!appOnly) {
        files.push(join(output, stem + '.dmg'));
        copyFileSync(this.single(join(bundle, 'dmg'), '.dmg'), files[0]!);
      }
      files.push(join(output, stem + '.app.tar.gz'));
      createArchive(this.single(join(bundle, 'macos'), '.app'), files.at(-1)!);
    }
    files.forEach(checksum);
  }
  notarize(path: string): void {
    const args = ['xcrun', 'notarytool', 'submit', path, '--wait'];
    if (process.env.CLAT_NOTARY_PROFILE) args.push('--keychain-profile', process.env.CLAT_NOTARY_PROFILE);
    else {
      const password = process.env.APPLE_APP_SPECIFIC_PASSWORD ?? process.env.APPLE_PASSWORD;
      if (!process.env.APPLE_ID || !process.env.APPLE_TEAM_ID || !password) throw new Error('configure local Apple notarization credentials');
      args.push('--apple-id', process.env.APPLE_ID, '--password', password, '--team-id', process.env.APPLE_TEAM_ID);
    }
    this.execute(args);
  }
  verifyMacosDraft(app: string, record: AssetRecord): void {
    const resources = join(app, 'Contents/Resources/backend');
    const binary = join(resources, 'clat-backend');
    const manifest = json<{ version: string; target: string; official: boolean; sha256: string; size: number }>(join(resources, 'backend.json'));
    if (manifest.version !== this.version() || manifest.target !== record.target || !manifest.official
      || manifest.sha256 !== digest(binary) || manifest.size !== statSync(binary).size) throw new Error('backend draft identity does not match the release');
    const info = JSON.parse(this.execute(['plutil', '-convert', 'json', '-o', '-', join(app, 'Contents/Info.plist')], { quiet: true }));
    if (info.CFBundleIdentifier !== 'com.artec.clat.desktop' || info.CFBundleShortVersionString !== this.version()) throw new Error('app draft identity does not match the release');
    const arch = record.target.startsWith('aarch64-') ? 'arm64' : 'x86_64';
    this.execute(['lipo', binary, '-verify_arch', arch]);
    this.execute(['lipo', join(app, 'Contents/MacOS/clat-desktop'), '-verify_arch', arch]);
  }
  async signMacos(directory: string, record: AssetRecord, tag: string): Promise<void> {
    const stem = `clat-desktop-${tag}-${record.label}`;
    const archive = join(directory, stem + '.app.tar.gz');
    const identity = process.env.APPLE_SIGNING_IDENTITY ?? 'Developer ID Application: Artec Inc. (VJUBRKV7K9)';
    await withTemp('clat-desktop-sign-', async folder => {
      safeExtract(archive, folder);
      const app = this.single(folder, '.app');
      const resources = join(app, 'Contents/Resources/backend');
      this.verifyMacosDraft(app, record);
      this.execute(['codesign', '--force', '--options', 'runtime', '--timestamp', '--entitlements', join(this.desktop, 'BackendEntitlements.plist'), '--sign', identity, join(resources, 'clat-backend')]);
      this.backendManifest(resources, record.target, true);
      this.execute(['codesign', '--force', '--options', 'runtime', '--timestamp', '--entitlements', join(this.desktop, 'Entitlements.plist'), '--sign', identity, app]);
      this.execute(['codesign', '--verify', '--deep', '--strict', app]);
      const submission = join(folder, 'notarization.zip');
      this.execute(['ditto', '-c', '-k', '--keepParent', app, submission]);
      this.notarize(submission);
      this.execute(['xcrun', 'stapler', 'staple', app]); this.execute(['xcrun', 'stapler', 'validate', app]);
      createArchive(app, archive);
      const dmg = join(directory, stem + '.dmg');
      if (existsSync(dmg)) unlinkSync(dmg);
      unlinkSync(submission);
      await this.makeDmg(app, dmg);
      this.execute(['codesign', '--force', '--timestamp', '--sign', identity, dmg]); this.notarize(dmg);
      this.execute(['xcrun', 'stapler', 'staple', dmg]); this.execute(['xcrun', 'stapler', 'validate', dmg]);
    });
  }
  async signFile(path: string, tag: string): Promise<void> {
    const key = join(this.root, '.release-secrets/clat-desktop.key');
    if (!existsSync(key)) throw new Error('desktop updater private key missing');
    await withTemp('clat-desktop-sign-', folder => {
      // Tauri's file reader requires unbroken outer base64. Keep the encrypted
      // Minisign bytes intact and never rewrite the owner's original key.
      const signingKey = join(folder, 'key');
      privateWrite(signingKey, Buffer.from(decodeWrapper(readFileSync(key)).toString('base64')));
      this.execute([process.execPath, join(this.desktop, 'node_modules/@tauri-apps/cli/tauri.js'), 'signer', 'sign', '--private-key-path', signingKey,
        '--app-version', tag.slice(1), path], { env: { TAURI_SIGNING_PRIVATE_KEY_PASSWORD: process.env.CLAT_DESKTOP_KEY_PASSWORD ?? '' } });
      const publicKey = join(folder, 'public'), signature = join(folder, 'signature');
      writeFileSync(publicKey, decodeWrapper(readFileSync(join(this.desktop, 'updater.pub'))));
      writeFileSync(signature, decodeWrapper(readFileSync(path + '.sig')));
      this.execute(['minisign', '-Vm', path, '-p', publicKey, '-x', signature, '-q']);
    });
  }
  async preflightSigning(tag: string): Promise<void> {
    if (process.platform !== 'darwin') throw new Error('finalize the complete release on the owner’s macOS machine');
    for (const tool of ['codesign', 'security', 'xcrun', 'lipo', 'plutil', 'hdiutil', 'ditto', 'minisign']) if (!Bun.which(tool)) throw new Error('required local release tool missing: ' + tool);
    if (!existsSync(join(this.desktop, 'node_modules/@tauri-apps/cli/tauri.js'))) throw new Error('run bun install --cwd desktop --frozen-lockfile --ignore-scripts');
    if (!process.env.CLAT_NOTARY_PROFILE && !(process.env.APPLE_ID && process.env.APPLE_TEAM_ID && (process.env.APPLE_APP_SPECIFIC_PASSWORD || process.env.APPLE_PASSWORD))) throw new Error('configure local Apple notarization credentials before signing');
    const identity = process.env.APPLE_SIGNING_IDENTITY ?? 'Developer ID Application: Artec Inc. (VJUBRKV7K9)';
    if (!this.execute(['security', 'find-identity', '-v', '-p', 'codesigning']).includes(identity)) throw new Error('configured Developer ID identity is not available');
    await withTemp('clat-desktop-key-check-', async folder => { const probe = join(folder, 'probe'); writeFileSync(probe, 'CLAT Desktop local signing preflight\n'); await this.signFile(probe, tag); });
  }
  async finalize(directory: string, tag: string, signedMacos = false): Promise<void> {
    if (tag !== 'v' + this.version()) throw new Error('tag does not match workspace version');
    for (const name of [...this.names('cli', tag), ...this.names('desktop', tag)]) {
      const path = join(directory, name), manifest = path + '.sha256';
      if (!existsSync(path) || !existsSync(manifest) || readFileSync(manifest, 'utf8').split(/\s/)[0] !== digest(path)) throw new Error('missing or mismatched draft asset: ' + name);
    }
    await this.preflightSigning(tag);
    if (!signedMacos) for (const record of this.records('desktop')) if (record.platform!.startsWith('darwin')) await this.signMacos(directory, record, tag);
    const platforms: Record<string, { url: string; signature: string }> = {};
    for (const record of this.records('desktop')) {
      const suffix = record.platform!.startsWith('darwin') ? '.app.tar.gz' : '-setup.exe';
      const name = `clat-desktop-${tag}-${record.label}${suffix}`;
      await this.signFile(join(directory, name), tag);
      platforms[record.platform!] = { url: `https://github.com/artec/clat/releases/download/${tag}/${name}`, signature: readFileSync(join(directory, name + '.sig'), 'utf8').trim() };
    }
    writePayload(this, directory, tag, platforms);
    await this.signFile(join(directory, PAYLOAD), tag);
  }
}

export async function releaseMain(args = process.argv.slice(2)): Promise<void> {
  const { positionals, values } = parseArgs({ args, allowPositionals: true, options: {
    kind: { type: 'string', default: 'desktop' }, target: { type: 'string' }, tag: { type: 'string' }, output: { type: 'string', default: join(ROOT, 'dist-desktop') },
    release: { type: 'boolean' }, official: { type: 'boolean' }, ci: { type: 'boolean' }, help: { type: 'boolean' }, backend: { type: 'string' },
  } });
  const release = new DesktopRelease(), command = positionals[0];
  if (values.help) { console.log('release.ts matrix|names|stage|build|package|finalize [--kind cli|desktop] [--target TARGET] [--release --official] [matrix: --ci]'); return; }
  if (['stage', 'build', 'package'].includes(command ?? '') && !values.target) throw new Error('--target is required');
  const kind = values.kind as AssetKind, tag = values.tag ?? 'v' + release.version();
  switch (command) {
    case 'matrix': {
      const include = release.records(kind).filter(r => (!values.target || values.target === 'all' || r.target === values.target)
        && (!values.ci || kind !== 'desktop' || r.platform!.startsWith('windows')));
      if (!include.length) throw new Error('no supported targets selected');
      console.log(JSON.stringify({ include })); break;
    }
    case 'names': console.log(release.names(kind, tag).join('\n')); break;
    case 'stage': release.stageBackend(values.target!, values); break;
    case 'build': await release.build(values.target!, values); break;
    case 'package': release.package(values.target!, resolve(values.output!)); break;
    case 'finalize': await release.finalize(resolve(values.output!), tag); break;
    default: throw new Error('unknown release command');
  }
}
if (import.meta.main) releaseMain().catch(error => { console.error('CLAT Desktop: ' + error.message); process.exitCode = 1; });
