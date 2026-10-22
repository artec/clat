import { expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { decodeWrapper, privateWrite, withTemp } from './common';
import { minisign, passwords, protect } from './protect-key';
import { generateKey } from './test-support';

async function keyFixture(action: (key: string, publicKey: string) => Promise<void>): Promise<void> {
  await withTemp('key-test-', async folder => {
    const raw = join(folder, 'raw'), publicKey = join(folder, 'public'), key = join(folder, 'key');
    generateKey(raw, publicKey); writeFileSync(publicKey, readFileSync(publicKey).toString('base64'));
    privateWrite(key, Buffer.from(readFileSync(raw).toString('base64'))); await action(key, publicKey);
  });
}
function protectWith(key: string, publicKey: string, old: string, next: string, repeat = next): Promise<void> {
  const answers = [old, next, repeat]; return protect(key, publicKey, async () => answers.shift()!);
}
const cryptoTest = test.skipIf(!Bun.which('minisign'));
cryptoTest('password change preserves public identity and rejects wrong password', async () => {
  await keyFixture(async (key, publicKey) => {
    await protectWith(key, publicKey, '', 'fixture-first-password'); const first = readFileSync(key);
    if (process.platform !== 'win32') expect(statSync(key).mode & 0o777).toBe(0o600);
    await expect(protectWith(key, publicKey, 'wrong-fixture-password', 'fixture-next-password')).rejects.toThrow(); expect(readFileSync(key)).toEqual(first);
    await protectWith(key, publicKey, 'fixture-first-password', 'fixture-next-password'); expect(readFileSync(key)).not.toEqual(first);
  });
}, 60000);
cryptoTest('existing empty-password Tauri key can be protected', async () => {
  await keyFixture(async (key, publicKey) => {
    const raw = key + '.raw'; privateWrite(raw, decodeWrapper(readFileSync(key))); minisign(['-C', '-s', raw], ['', '']);
    writeFileSync(key, readFileSync(raw).toString('base64')); await protectWith(key, publicKey, '', 'fixture-new-password');
    if (process.platform !== 'win32') expect(statSync(key).mode & 0o777).toBe(0o600);
  });
}, 60000);
cryptoTest('empty or mismatched password never changes original', async () => {
  await keyFixture(async (key, publicKey) => {
    const original = readFileSync(key);
    for (const [next, repeat] of [['', ''], ['fixture-password', 'different']]) {
      await expect(protectWith(key, publicKey, '', next!, repeat!)).rejects.toThrow(); expect(readFileSync(key)).toEqual(original);
    }
  });
});
cryptoTest('wrong public key never replaces original or leaves temporary secret', async () => {
  await keyFixture(async (key, publicKey) => {
    const original = readFileSync(key); writeFileSync(publicKey, Buffer.from('untrusted comment: fixture\nwrong-key\n').toString('base64'));
    await expect(protectWith(key, publicKey, '', 'fixture-password')).rejects.toThrow('公钥不匹配'); expect(readFileSync(key)).toEqual(original);
    expect(readdirSync(join(key, '..')).filter(n => n.startsWith('.desktop-key-'))).toEqual([]);
  });
}, 60000);
test('password input rejects newline, null and empty new passwords', () => {
  for (const [old, next, repeat] of [['', '', ''], ['', 'fixture\n', 'fixture\n'], ['\0', 'fixture', 'fixture']]) expect(() => passwords(old!, next!, repeat!)).toThrow();
});
