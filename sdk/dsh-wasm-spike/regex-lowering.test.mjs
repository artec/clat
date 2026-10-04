import test from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { codePointDigest } from './regex-oracle.mjs'
import { lowerUnicodeProperties, loweredUnicodeVersion } from './regex-lowering.mjs'

function lowerRegex(regex) {
  const result = lowerUnicodeProperties(String(regex))
  assert.doesNotMatch(result.code, /\\[pP]\{/, 'guest output must no longer require property escape support')
  return new Function(`return ${result.code}`)()
}

// The native Bun oracle is pinned to regex-oracle.expected.json (CI runners
// have no Bun). Every assertion below always runs against the pinned digests;
// when a local Bun IS present it must reproduce them exactly, so drift in the
// pinned tables or in Bun itself still fails the suite on developer machines.
function pinnedOracle() {
  const pinned = JSON.parse(readFileSync(new URL('./regex-oracle.expected.json', import.meta.url), 'utf8'))
  assert.equal(pinned.digests.length, 2)
  try {
    execFileSync('bun', ['--version'], { stdio: 'ignore' })
  } catch {
    console.error('bun oracle absent (CI runner); comparing against pinned digests only — cross-check skipped')
    return pinned
  }
  const live = JSON.parse(execFileSync('bun', [fileURLToPath(new URL('./regex-oracle.mjs', import.meta.url))], { encoding: 'utf8' }))
  assert.deepEqual(live.digests, pinned.digests, 'live Bun oracle drifted from the pinned fixture; regenerate with: bun regex-oracle.mjs > regex-oracle.expected.json')
  return live
}

test('Unicode tables are pinned; native oracle membership detects runtime drift', () => {
  assert.equal(loweredUnicodeVersion, '17.0.0')
  pinnedOracle()
  // Bun reports ICU 15.1 here, but its regex engine's XID membership matches 17.0.
  // Metadata is insufficient: only the exhaustive native digest below is acceptance.
})

test('XID lowering preserves every Unicode code point, including surrogates', () => {
  const oracle = pinnedOracle()
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
