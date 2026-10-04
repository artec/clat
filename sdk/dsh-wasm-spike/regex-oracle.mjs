// Native Bun oracle, separate process from the Node author compiler.
import { createHash } from 'node:crypto'

export function codePointDigest(regex) {
  const membership = new Uint8Array(0x110000)
  for (let code = 0; code < membership.length; code++) {
    membership[code] = Number(regex.test(String.fromCodePoint(code)))
  }
  return createHash('sha256').update(membership).digest('hex')
}

if (import.meta.main) {
  console.log(JSON.stringify({ unicode: process.versions.unicode,
    digests: [/^\p{XID_Start}$/u, /^\p{XID_Continue}$/u].map(codePointDigest) }))
}
