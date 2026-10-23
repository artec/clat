import { afterEach, expect, mock, spyOn, test } from 'bun:test';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, readlinkSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import * as tar from 'tar';
import { createArchive, safeExtract } from './archive';
import { checksum, cleanEnv, DESKTOP, digest, json, redact, run, withTemp } from './common';
import { DesktopRelease } from './release';
import { fixture } from './test-support';

afterEach(() => mock.restore());
test('desktop display name is CLAT while executable and application identity stay separate from CLI', () => {
  const config = json<any>(join(DESKTOP, 'tauri.conf.json'));
  expect(config.productName).toBe('CLAT');
  expect(config.mainBinaryName).toBe('clat-desktop');
  expect(config.identifier).toBe('com.artec.clat.desktop');
});
test('packaging selects the current app without deleting earlier build output', async () => {
  await fixture(release => {
    for (const name of ['CLAT.app', 'CLAT Desktop.app']) mkdirSync(join(release.root, name));
    expect(release.single(release.root, '.app')).toBe(join(release.root, 'CLAT.app'));
    expect(existsSync(join(release.root, 'CLAT Desktop.app'))).toBe(true);
    for (const name of ['CLAT_2.0.0_x64.dmg', 'CLAT Desktop_2.0.0_x64.dmg']) writeFileSync(join(release.root, name), 'fixture');
    expect(release.single(release.root, '.dmg')).toBe(join(release.root, 'CLAT_2.0.0_x64.dmg'));
    expect(existsSync(join(release.root, 'CLAT Desktop_2.0.0_x64.dmg'))).toBe(true);
  });
});
test('four desktop and six CLI targets are isolated', () => {
  const release = new DesktopRelease(), cli = release.names('cli', 'v2.2.0'), desktop = release.names('desktop', 'v2.2.0');
  expect(release.records('desktop')).toHaveLength(4); expect(release.records('cli')).toHaveLength(6);
  expect(cli).toHaveLength(6); expect(desktop).toHaveLength(6);
  for (const name of desktop) for (const record of release.records('cli')) expect(name).not.toContain(record.target);
  expect(() => release.names('cli', '../../escape')).toThrow('unsafe');
});
test('build environment never inherits another product’s signing key', () => {
  const env = cleanEnv({ CLAT_OFFICIAL_RELEASE: '1' }, { PATH: '/bin', TAURI_SIGNING_PRIVATE_KEY: 'fixture', APPLE_PASSWORD: 'fixture', CLAT_DESKTOP_KEY_PASSWORD: 'fixture', GH_TOKEN: 'fixture' });
  expect(env).toEqual({ PATH: '/bin', CLAT_OFFICIAL_RELEASE: '1' });
});
test('signing password is not a process argument', async () => {
  await fixture(async release => {
    const calls: { args: string[]; password?: string }[] = [];
    const signing = new DesktopRelease(release.root, (args, options) => { calls.push({ args, password: options?.env?.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }); return ''; });
    writeFileSync(join(release.root, '.release-secrets/clat-desktop.key'), Buffer.from('fixture-only-key').toString('base64'));
    writeFileSync(join(release.desktop, 'updater.pub'), Buffer.from('fixture-public').toString('base64'));
    const path = join(release.root, 'package'); writeFileSync(path + '.sig', Buffer.from('fixture-signature').toString('base64'));
    const previous = process.env.CLAT_DESKTOP_KEY_PASSWORD;
    try { process.env.CLAT_DESKTOP_KEY_PASSWORD = 'fixture-password'; await signing.signFile(path, 'v2.0.0'); }
    finally { if (previous === undefined) delete process.env.CLAT_DESKTOP_KEY_PASSWORD; else process.env.CLAT_DESKTOP_KEY_PASSWORD = previous; }
    expect(calls[0]!.args).not.toContain('fixture-password'); expect(calls[0]!.password).toBe('fixture-password');
  });
});
function archiveEntry(path: string, type = '0', link = '', size = 0): Buffer {
  const header = Buffer.alloc(512); header.write(path); header.write('0000644\0', 100); header.write('0000000\0', 108); header.write('0000000\0', 116);
  header.write(size.toString(8).padStart(11, '0') + '\0', 124); header.write('00000000000\0', 136); header.fill(32, 148, 156); header.write(type, 156); header.write(link, 157);
  header.write('ustar\0', 257); header.write('00', 263); const sum = header.reduce((a, b) => a + b, 0); header.write(sum.toString(8).padStart(6, '0') + '\0 ', 148);
  return Buffer.concat([header, Buffer.alloc(1024)]);
}
test('archive path fence and links reject before writes', async () => {
  await withTemp('archive-contract-', folder => {
    for (const [name, type, link] of [['../outside', '0', ''], ['/outside', '0', ''], ['app/link', '2', '../../outside'], ['C:/outside', '0', ''], ['app/hardlink', '1', 'other']]) {
      const path = join(folder, 'bad.tar'); writeFileSync(path, archiveEntry(name!, type, link));
      const destination = join(folder, 'unpack'); expect(() => safeExtract(path, destination)).toThrow(); expect(existsSync(destination)).toBe(false);
    }
  });
});
test('version and public key have one authority', () => {
  const config = json<any>(join(DESKTOP, 'tauri.conf.json'));
  expect(config.version).toBeUndefined(); expect(config.plugins.updater.pubkey).toBe(readFileSync(join(DESKTOP, 'updater.pub'), 'utf8').trim());
  expect(config.plugins.updater.requireSignedVersion).toBe(true); expect(config.plugins.updater.allowDowngrades).toBe(false);
  expect(new DesktopRelease().version()).toMatch(/^\d+\.\d+\.\d+$/);
});
test('finalization refuses partial release without signing', async () => {
  await fixture(async release => {
    const check = spyOn(release, 'preflightSigning');
    await expect(release.finalize(release.root, 'v2.0.0')).rejects.toThrow('missing or mismatched');
    for (const name of release.names('desktop', 'v2.0.0')) { writeFileSync(join(release.root, name), 'fixture'); checksum(join(release.root, name)); }
    await expect(release.finalize(release.root, 'v2.0.0')).rejects.toThrow('missing or mismatched');
    expect(check).not.toHaveBeenCalled();
  });
});
test.skipIf(process.platform === 'win32')('DMG stores a large left-to-right Finder layout before compression', async () => {
  await fixture(async release => {
    const app = join(release.root, 'CLAT.app'); mkdirSync(app); const seen: string[][] = [];
    const packaging = new DesktopRelease(release.root, args => {
      seen.push(args);
      if (args[0] === 'ditto') cpSync(args[1]!, args[2]!, { recursive: true });
      if (args[0] === 'hdiutil' && args[1] === 'create') {
        const image = args[args.indexOf('-srcfolder') + 1]!;
        expect(readdirSync(image).sort()).toEqual(['Applications', 'CLAT.app']);
        expect(readlinkSync(join(image, 'Applications'))).toBe('/Applications');
        expect(args[args.indexOf('-volname') + 1]).toBe('CLAT');
      }
      if (args[0] === 'osascript') writeFileSync(join(args.at(-2)!, '.DS_Store'), 'fixture layout');
      return '';
    });
    await packaging.makeDmg(app, join(release.root, 'output.dmg'));
    expect(seen.filter(a => a[0] === 'hdiutil').map(a => a[1])).toEqual(['create', 'attach', 'detach', 'convert']);
    const script = seen.find(a => a[0] === 'osascript')!;
    expect(script).toBeDefined();
    expect(script[2]).toContain('set icon size of viewOptions to 128');
    expect(script[2]).toContain('{180, 170}'); expect(script[2]).toContain('{480, 170}');
    expect(script[2]).toContain('{400, 200, 1060, 600}');
    expect(script.at(-1)).toBe('CLAT.app');
    expect(existsSync(app)).toBe(true);
  });
});
test.skipIf(process.platform === 'win32')('DMG layout failure detaches and prevents publishing an unstyled image', async () => {
  await fixture(async release => {
    const app = join(release.root, 'CLAT.app'); mkdirSync(app); const seen: string[][] = [];
    const packaging = new DesktopRelease(release.root, args => {
      seen.push(args);
      if (args[0] === 'osascript') throw new Error('Finder layout denied');
      return '';
    });
    await expect(packaging.makeDmg(app, join(release.root, 'output.dmg'))).rejects.toThrow('Finder layout denied');
    expect(seen.some(a => a[0] === 'hdiutil' && a[1] === 'detach')).toBe(true);
    expect(seen.some(a => a[0] === 'hdiutil' && a[1] === 'convert')).toBe(false);
  });
});
test.skipIf(process.platform === 'win32')('DMG requires a saved layout and detaches when it is missing', async () => {
  await fixture(async release => {
    const app = join(release.root, 'CLAT.app'); mkdirSync(app); const seen: string[][] = [];
    const packaging = new DesktopRelease(release.root, args => { seen.push(args); return ''; });
    await expect(packaging.makeDmg(app, join(release.root, 'output.dmg'))).rejects.toThrow('Finder did not save');
    expect(seen.some(a => a[1] === 'detach')).toBe(true);
    expect(seen.some(a => a[1] === 'convert')).toBe(false);
  });
});
test.skipIf(process.platform === 'win32')('DMG detach failure retains mounted contents and never compresses', async () => {
  await fixture(async release => {
    const app = join(release.root, 'CLAT.app'); mkdirSync(app); let mount = ''; let converted = false;
    const packaging = new DesktopRelease(release.root, args => {
      if (args[0] === 'osascript') { mount = args.at(-2)!; writeFileSync(join(mount, '.DS_Store'), 'fixture layout'); }
      if (args[1] === 'detach') throw new Error('fixture busy volume');
      if (args[1] === 'convert') converted = true;
      return '';
    });
    try {
      await expect(packaging.makeDmg(app, join(release.root, 'output.dmg'))).rejects.toThrow('DMG detach failed');
      expect(readFileSync(join(mount, '.DS_Store'), 'utf8')).toBe('fixture layout');
      expect(converted).toBe(false);
    } finally { if (mount) rmSync(dirname(mount), { recursive: true, force: true }); }
  });
});
test('archive round trip preserves nested executable bytes and permissions', async () => {
  await withTemp('archive-roundtrip-', folder => {
    const app = join(folder, 'Example.app'); mkdirSync(join(app, 'Contents/MacOS'), { recursive: true });
    const binary = join(app, 'Contents/MacOS/runner'); writeFileSync(binary, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
    const archive = join(folder, 'app.tar.gz'); createArchive(app, archive);
    const output = join(folder, 'output'); safeExtract(archive, output); const result = join(output, basename(app), 'Contents/MacOS/runner');
    expect(digest(result)).toBe(digest(binary)); if (process.platform !== 'win32') expect(statSync(result).mode & 0o777).toBe(0o755);
  });
});
test('archive supports existing PAX app packages and rejects oversized inventory', async () => {
  await withTemp('pax-roundtrip-', folder => {
    const name = 'long/'.repeat(35) + 'file'; mkdirSync(join(folder, 'input', name, '..'), { recursive: true }); writeFileSync(join(folder, 'input', name), 'PAX bytes');
    tar.c({ file: join(folder, 'pax.tar'), cwd: join(folder, 'input'), sync: true }, [name]);
    safeExtract(join(folder, 'pax.tar'), join(folder, 'out')); expect(readFileSync(join(folder, 'out', name), 'utf8')).toBe('PAX bytes');
    const huge = join(folder, 'huge.tar'); writeFileSync(huge, archiveEntry('huge', '0', '', 2 * 1024 ** 3 + 1));
    expect(() => safeExtract(huge, join(folder, 'huge-output'))).toThrow('too large');
  });
});
test('captured diagnostics redact inherited and explicitly supplied credentials', () => {
  expect(redact('fixture-secret', [{ PRIVATE_KEY_PASSWORD: 'fixture-secret' }])).toBe('[redacted]');
  const previous = process.env.CLAT_DESKTOP_KEY_PASSWORD;
  try { process.env.CLAT_DESKTOP_KEY_PASSWORD = 'fixture-hidden'; expect(run([process.execPath, '-e', 'console.log(process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD)'], { quiet: true, env: { TAURI_SIGNING_PRIVATE_KEY_PASSWORD: 'fixture-hidden' } })).toBe('[redacted]\n'); }
  finally { if (previous === undefined) delete process.env.CLAT_DESKTOP_KEY_PASSWORD; else process.env.CLAT_DESKTOP_KEY_PASSWORD = previous; }
});
test('Bun directly invokes installed Tauri CLI', () => {
  expect(run([process.execPath, join(DESKTOP, 'node_modules/@tauri-apps/cli/tauri.js'), '--version'], { quiet: true })).toContain('tauri-cli 2.12.4');
});
test.skipIf(!Bun.which('minisign'))('Bun Tauri signer accepts protected keys with LF and wrapped CRLF without rewriting originals', async () => {
  const { generateKey } = await import('./test-support'); const { minisign } = await import('./protect-key');
  await fixture(async release => {
    const raw = join(release.root, 'raw.key'), publicKey = join(release.desktop, 'raw.pub'); generateKey(raw, publicKey);
    minisign(['-C', '-s', raw], ['fixture-protected', 'fixture-protected']);
    const key = join(release.root, '.release-secrets/clat-desktop.key'), encoded = readFileSync(raw).toString('base64');
    writeFileSync(join(release.desktop, 'updater.pub'), readFileSync(publicKey).toString('base64'));
    const staged: string[] = [];
    const signing = new DesktopRelease(release.root, (args, options) => {
      if (args.includes('--private-key-path')) staged.push(args[args.indexOf('--private-key-path') + 1]!);
      return run(args[1]?.endsWith('tauri.js') ? [args[0]!, join(DESKTOP, 'node_modules/@tauri-apps/cli/tauri.js'), ...args.slice(2)] : args, { ...options, quiet: true });
    });
    const path = join(release.root, 'package.bin'); writeFileSync(path, 'test-only package'); const previous = process.env.CLAT_DESKTOP_KEY_PASSWORD;
    try {
      process.env.CLAT_DESKTOP_KEY_PASSWORD = 'fixture-protected';
      for (const wrapper of [encoded, encoded + '\n', encoded.match(/.{1,64}/g)!.join('\r\n') + '\r\n']) {
        writeFileSync(key, wrapper, { mode: 0o600 });
        await signing.signFile(path, 'v2.0.0');
        expect(readFileSync(key, 'utf8')).toBe(wrapper);
        expect(staged.at(-1)).not.toBe(key); expect(existsSync(staged.at(-1)!)).toBe(false);
      }
    }
    finally { if (previous === undefined) delete process.env.CLAT_DESKTOP_KEY_PASSWORD; else process.env.CLAT_DESKTOP_KEY_PASSWORD = previous; }
    expect(existsSync(path + '.sig')).toBe(true);
  });
}, 60000);

test('signing uses a private normalized temporary key and removes it on failure', async () => {
  await fixture(async release => {
    const key = join(release.root, '.release-secrets/clat-desktop.key');
    const encoded = Buffer.from('fixture-only Minisign secret\n').toString('base64'), original = encoded + '\r\n';
    writeFileSync(key, original, { mode: 0o600 });
    let staged = '';
    const signing = new DesktopRelease(release.root, args => {
      staged = args[args.indexOf('--private-key-path') + 1]!;
      expect(staged).not.toBe(key);
      expect(readFileSync(staged, 'utf8')).toBe(encoded);
      if (process.platform !== 'win32') expect(statSync(staged).mode & 0o777).toBe(0o600);
      throw new Error('fixture signing failure');
    });
    await expect(signing.signFile(join(release.root, 'package'), 'v2.0.0')).rejects.toThrow('fixture signing failure');
    expect(staged).not.toBe(''); expect(existsSync(staged)).toBe(false);
    expect(readFileSync(key, 'utf8')).toBe(original);
  });
});
