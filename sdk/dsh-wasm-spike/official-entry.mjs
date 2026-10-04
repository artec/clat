// Strict attempt: the official packages and existing Shim stay byte-for-byte intact.
import { Shim } from '../dsh-adapter/src/shim.ts'
import { officialWeb } from '../dsh-adapter/examples/official-web/plugin.mjs'
import { get } from 'clat:plugin/config@0.1.0'

let shim
function initialize() {
  if (shim) return shim
  const unavailable = async () => { throw new Error('not provided by this experiment') }
  shim = new Shim({
    sampling: unavailable, elicitation: unavailable,
    capabilities: { sampling: false, elicitation: false, hostServices: false },
    log() {},
  }, 'dsh-official-web-wasm')
  officialWeb.apply(shim.buildContext(), JSON.parse(get()))
  return shim
}
export const tools = {
  listTools() {
    return initialize().listTools().map(tool => ({
      name: tool.name, description: tool.description,
      inputSchema: JSON.stringify(tool.parameters), effect: 'network',
    }))
  },
  async call(name, argumentsJson) {
    const result = await initialize().callTool(name, JSON.parse(argumentsJson), 'wit-call')
    return JSON.stringify(result.structuredContent)
  },
}
