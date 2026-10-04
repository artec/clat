import test from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { codePointDigest } from './regex-oracle.mjs'
import { lowerUnicodeProperties, loweredUnicodeVersion } from './regex-lowering.mjs'

function lowerRegex(regex) {
  const result = lowerUnicodeProperties(String(regex))
  assert.doesNotMatch(result.code, /\\[pP]\{/, 'guest output must no longer require property escape support')
  return new Function(`return ${result.code}`)()
}

test('Unicode tables are pinned; native oracle membership detects runtime drift', () => {
  assert.equal(loweredUnicodeVersion, '17.0.0')
  const oracle = JSON.parse(execFileSync('bun', [fileURLToPath(new URL('./regex-oracle.mjs', import.meta.url))], { encoding: 'utf8' }))
  // Bun reports ICU 15.1 here, but its regex engine's XID membership matches 17.0.
  // Metadata is insufficient: only the exhaustive native digest below is acceptance.
  assert.equal(oracle.digests.length, 2)
})

test('XID lowering preserves every Unicode code point, including surrogates', () => {
  const oracle = JSON.parse(execFileSync('bun', [fileURLToPath(new URL('./regex-oracle.mjs', import.meta.url))], { encoding: 'utf8' }))
  const digests = [/^\p{XID_Start}$/u, /^\p{XID_Continue}$/u].map(regex => codePointDigest(lowerRegex(regex)))
  assert.deepEqual(digests, oracle.digests)
})

test('original identifier and split patterns retain composition and flags', () => {
  const regex = /^[\p{XID_Start}_]\p{XID_Continue}*$/u
  const split = /[^\p{XID_Continue}]+|_+/u
  const lowered = lowerRegex(regex)
  const loweredSplit = lowerRegex(split)
  for (const input of ['', '_', '工具', '工具_2', '𐐀abc', '2bad', 'e\u0301', '\ud800', 'bad-name', 'a\n', 'a b___工具']) {
    assert.equal(lowered.test(input), regex.test(input), JSON.stringify(input))
    assert.deepEqual(input.split(loweredSplit), input.split(split))
  }
  const flagged = /\p{XID_Start}+/giu
  assert.deepEqual('Ab 工具 𐐀'.match(lowerRegex(flagged)), 'Ab 工具 𐐀'.match(flagged))
})

test('lexer changes only regex literals, including template interpolations', () => {
  const source = 'const text = "\\\\p{XID_Start}"; // /\\p{XID_Start}/u\n' +
    'const template = `raw /\\\\p{XID_Start}/u ${/\\p{XID_Start}/u.test("a")}`;'
  const result = lowerUnicodeProperties(source)
  assert.equal(result.literals, 1)
  assert.ok(result.code.startsWith('const text = "\\\\p{XID_Start}"; // /\\p{XID_Start}/u\n'))
  assert.equal(new Function(`${source}; return template`)(), new Function(`${result.code}; return template`)())
})
