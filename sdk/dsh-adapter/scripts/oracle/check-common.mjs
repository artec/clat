import { readFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'

const pinned = 'b150a551b8'

export async function checkOracle(generator, upstream, symbols) {
  const root = resolve(process.env.DSH_ROOT ?? join(import.meta.dirname, '..', '..', '..', '..', '..', 'deepseek-harness'))
  const source = await readFile(join(root, upstream), 'utf8')
  for (const symbol of symbols) {
    if (!source.includes(symbol)) throw new Error(`${generator}: pinned ${pinned} source lacks ${symbol}`)
  }
  const generatorSource = await readFile(join(import.meta.dirname, generator), 'utf8')
  if (generatorSource.includes("from '../../src/") || generatorSource.includes("from '../../../src/")) {
    throw new Error(`${generator}: oracle generator must not import CLAT adapter source`)
  }
  process.stdout.write(`${generator}: pinned-source wiring OK\n`)
}
