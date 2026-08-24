#!/usr/bin/env node
import { scanDshCompatibility, writeCompatibilityMatrix } from './scanner.js'

const args = process.argv.slice(2)
const outputIndex = args.indexOf('--output')
const output = outputIndex >= 0 ? args[outputIndex + 1] : undefined
if (outputIndex >= 0) args.splice(outputIndex, 2)
const root = args[0]
if (root === undefined) {
  process.stderr.write('usage: clat-dsh-scan <dsh-checkout> [--output matrix.json]\n')
  process.exitCode = 2
} else {
  const matrix = await scanDshCompatibility(root)
  if (output === undefined) process.stdout.write(`${JSON.stringify(matrix, null, 2)}\n`)
  else await writeCompatibilityMatrix(matrix, output)
}
