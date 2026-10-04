// Author-side syntax lowering only. Never edit upstream source or emulate I/O.
import { tokenizer } from 'acorn'
import rewritePattern from 'regexpu-core'
import unicodeVersion from 'regenerate-unicode-properties/unicode-version.js'

export const loweredUnicodeVersion = unicodeVersion

export function lowerUnicodeProperties(source) {
  const tokens = tokenizer(source, { ecmaVersion: 'latest', sourceType: 'module' })
  const edits = []
  for (const token of tokens) {
    if (token.type.label !== 'regexp') continue
    const { pattern, flags } = token.value
    if (!/\\[pP]\{/.test(pattern)) continue
    if (!flags.includes('u') || flags.includes('v')) {
      throw new Error('Spike lowering only supports Unicode u-mode property literals')
    }
    const lowered = rewritePattern(pattern, flags, { unicodePropertyEscapes: 'transform' })
    edits.push({ start: token.start, end: token.end, replacement: `/${lowered}/${flags}` })
  }
  let code = source
  for (const edit of edits.toReversed()) {
    code = code.slice(0, edit.start) + edit.replacement + code.slice(edit.end)
  }
  return { code, literals: edits.length, unicodeVersion }
}
