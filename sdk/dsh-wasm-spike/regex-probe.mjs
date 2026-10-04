// Exact three patterns from dsh-tools@0.2.0-rc.2, lowered only in generated output.
const identifier = /^[\p{XID_Start}_]\p{XID_Continue}*$/u
const split = /[^\p{XID_Continue}]+|_+/u
const starts = /^\p{XID_Start}/u

export function regexProbe() {
  const inputs = ['', '_', '工具', '工具_2', '𐐀abc', '2bad', 'e\u0301', '\ud800', 'bad-name', 'a\n', 'a b___工具']
  return inputs.map(input => ({ identifier: identifier.test(input), starts: starts.test(input), parts: input.split(split) }))
}
