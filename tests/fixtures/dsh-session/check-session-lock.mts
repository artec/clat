// Optional DV-2 live interop probe. Rust's test runner invokes this only when
// a sibling/passed DSH checkout and Bun are available; normal CLAT users and
// CI do not need either runtime.
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

const [mode, directory] = process.argv.slice(2)
const root = process.env.DSH_CHECKOUT
if (!mode || !directory || !root) throw new Error('mode, directory, and DSH_CHECKOUT are required')
const { SessionWriteLease } = await import(pathToFileURL(join(
  root,
  'packages/session/session-persistence-jsonl/src/lease.ts',
)).href)

if (mode === 'hold') {
  const lease = await SessionWriteLease.acquire(directory, 'clat-dv2-live')
  process.stdout.write('DSH_SESSION_LEASE_READY\n')
  await new Promise<void>((resolve) => process.stdin.once('data', () => resolve()))
  await lease.release()
} else if (mode === 'try') {
  try {
    const lease = await SessionWriteLease.acquire(directory, 'clat-dv2-live')
    await lease.release()
    process.stdout.write('FREE\n')
  } catch (error) {
    if ((error as Error).name !== 'SessionAlreadyOwnedError') throw error
    process.stdout.write('BUSY\n')
  }
} else {
  throw new Error(`unknown mode: ${mode}`)
}
