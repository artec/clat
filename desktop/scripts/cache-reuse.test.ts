import { expect, test } from 'bun:test';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { ROOT, run, withTemp } from './common';

test('release library warming survives a product version change; debug artifacts remain distinct', async () => {
  const toolchain = readFileSync(join(ROOT, 'rust-toolchain.toml'), 'utf8').match(/^channel\s*=\s*"([^"]+)"/m)![1]!;
  const env = { RUSTUP_TOOLCHAIN: toolchain };
  const host = run(['rustc', '-vV'], { quiet: true, env }).match(/^host: (.+)$/m)![1]!;
  await withTemp('clat-release-cache-contract-', folder => {
    mkdirSync(join(folder, 'vendor/dep/src'), { recursive: true }); mkdirSync(join(folder, 'src'));
    writeFileSync(join(folder, 'vendor/dep/Cargo.toml'), '[package]\nname="cache-dep"\nversion="1.0.0"\nedition="2024"\n');
    writeFileSync(join(folder, 'vendor/dep/src/lib.rs'), 'pub fn value() -> u32 { 42 }\n');
    const manifest = (version: string) => `[package]\nname="cache-app"\nversion="${version}"\nedition="2024"\n[workspace]\nexclude=["vendor/dep"]\n[dependencies]\ncache-dep={path="vendor/dep"}\n[profile.release]\nlto="thin"\ncodegen-units=1\n`;
    writeFileSync(join(folder, 'Cargo.toml'), manifest('1.0.0'));
    writeFileSync(join(folder, 'src/lib.rs'), 'pub fn value() -> u32 { cache_dep::value() }\n');
    writeFileSync(join(folder, 'src/main.rs'), 'fn main() { println!("{}", cache_app::value()); }\n');
    const execute = (args: string[]) => run(['cargo', ...args], { cwd: folder, quiet: true, env: { ...env, CARGO_TARGET_DIR: join(folder, 'target') } });
    const build = (...args: string[]) => {
      const output = execute(['build', '--offline', '--locked', '--target', host, '--message-format=json', ...args]);
      const artifact = output.split('\n').filter(line => line.startsWith('{')).map(line => JSON.parse(line))
        .find(item => item.reason === 'compiler-artifact' && item.target?.name === 'cache_dep');
      expect(artifact).toBeDefined(); return artifact.fresh as boolean;
    };
    execute(['generate-lockfile', '--offline']);
    expect(build('--release', '--lib')).toBe(false);
    writeFileSync(join(folder, 'Cargo.toml'), manifest('1.0.1')); execute(['generate-lockfile', '--offline']);
    expect(build('--release', '--bin', 'cache-app')).toBe(true);
    expect(build('--lib')).toBe(false);
  });
}, 60000);
