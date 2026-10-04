// Isolate the exact built-in import used by the original adapter Shim.
import { AsyncLocalStorage } from 'node:async_hooks'
export const tools = {
  listTools() { return [] },
  async call() { return new AsyncLocalStorage().run('scope', () => 'scope') },
}
