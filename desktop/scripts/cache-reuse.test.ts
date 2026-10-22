import { expect, test } from 'bun:test';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { ROOT, run, withTemp, type Environment } from './common';

async function cargoFixture(action: (fixture: {
  execute: (args: string[]) => string;
  artifact: (output: string) => { fresh: boolean; profile: { debuginfo: number; debug_assertions: boolean; overflow_checks: boolean } };
  version: (version: string) => void; root: string;
}) => void, extra: Environment = {}): Promise<void> {
  const toolchain = readFileSync(join(ROOT, 'rust-toolchain.toml'), 'utf8').match(/^channel\s*=\s*"([^"]+)"/m)![1]!;
  const env = { RUSTUP_TOOLCHAIN: toolchain, ...extra };
  const host = run(['rustc', '-vV'], { quiet: true, env }).match(/^host: (.+)$/m)![1]!;
  await withTemp('clat-cargo-cache-contract-', root => {
    mkdirSync(join(root, 'vendor/dep/src'), { recursive: true }); mkdirSync(join(root, 'src'));
    writeFileSync(join(root, 'vendor/dep/Cargo.toml'), '[package]\nname="cache-dep"\nversion="1.0.0"\nedition="2024"\n');
    writeFileSync(join(root, 'vendor/dep/src/lib.rs'), 'pub fn value() -> u32 { 42 }\n');
    const execute = (args: string[]) => run(['cargo', ...args], { cwd: root, quiet: true, env: { ...env, CARGO_TARGET_DIR: join(root, 'target') } });
    const version = (value: string) => {
      writeFileSync(join(root, 'Cargo.toml'), `[package]\nname="cache-app"\nversion="${value}"\nedition="2024"\n[workspace]\nexclude=["vendor/dep"]\n[dependencies]\ncache-dep={path="vendor/dep"}\n[profile.release]\nlto="thin"\ncodegen-units=1\n`);
      execute(['generate-lockfile', '--offline']);
    };
    writeFileSync(join(root, 'src/lib.rs'), 'pub fn value() -> u32 { cache_dep::value() }\n');
    writeFileSync(join(root, 'src/main.rs'), 'fn main() { println!("{}", cache_app::value()); }\n');
    version('1.0.0');
    const artifact = (output: string) => {
      const artifact = output.split('\n').filter(line => line.startsWith('{')).map(line => JSON.parse(line))
        .find(item => item.reason === 'compiler-artifact' && item.target?.name === 'cache_dep');
      expect(artifact).toBeDefined(); return artifact;
    };
    action({ execute: args => execute([args[0]!, '--offline', '--locked', '--target', host, '--message-format=json', ...args.slice(1)]), artifact, version, root });
  });
}

test('release library warming survives a product version change; debug artifacts remain distinct', async () => {
  await cargoFixture(({ execute, artifact, version }) => {
    expect(artifact(execute(['build', '--release', '--lib'])).fresh).toBe(false);
    version('1.0.1');
    expect(artifact(execute(['build', '--release', '--bin', 'cache-app'])).fresh).toBe(true);
    expect(artifact(execute(['build', '--lib'])).fresh).toBe(false);
  });
}, 60000);

test('compact CI profiles reuse test dependencies while assertions and overflow checks remain active', async () => {
  const ci = Bun.YAML.parse(readFileSync(join(ROOT, '.github/workflows/ci.yml'), 'utf8')) as any;
  const configured = ci.jobs['plugin-performance'].env;
  await cargoFixture(({ execute, artifact, root }) => {
    const checked = artifact(execute(['check', '--all-targets']));
    expect(checked.profile.debuginfo).toBe(0);
    writeFileSync(join(root, 'src/lib.rs'), `pub fn value() -> u32 { cache_dep::value() }
#[test] fn native_safety() {
    assert!(cfg!(debug_assertions));
    let value = std::hint::black_box(255u8);
    assert!(std::panic::catch_unwind(|| value + 1).is_err());
    assert_eq!(self::value(), 42);
}
`);
    const first = execute(['test']);
    expect(first).toContain('1 passed');
    expect(artifact(first).fresh).toBe(false); // check metadata cannot replace native code/linking.
    writeFileSync(join(root, 'src/main.rs'), 'fn main() { println!("updated: {}", cache_app::value()); }\n');
    const reused = execute(['test']); expect(reused).toContain('1 passed');
    const dependency = artifact(reused); expect(dependency.fresh).toBe(true);
    expect(dependency.profile.debuginfo).toBe(0);
    expect(dependency.profile.debug_assertions).toBe(true); expect(dependency.profile.overflow_checks).toBe(true);
  }, { CARGO_PROFILE_DEV_DEBUG: configured.CARGO_PROFILE_DEV_DEBUG, CARGO_PROFILE_TEST_DEBUG: configured.CARGO_PROFILE_TEST_DEBUG });
}, 60000);
