// Generation fence shared by author staging and the owner-run v1 publisher.
const legacyCapabilities = new Set(['tools', 'prompts', 'sampling', 'elicitation', 'hostContext', 'hostTools'])
const assert = (condition, message) => { if (!condition) throw new Error(`index generation: ${message}`) }
export function validateIndexGeneration(index, expected = 1) {
  assert(expected === 1 || expected === 2, 'unsupported generation')
  assert(index?.schemaVersion === expected, `expected schemaVersion ${expected}`)
  assert(Array.isArray(index.packages), 'packages must be an array')
  for (const item of index.packages) {
    assert(Array.isArray(item.versions), 'versions must be an array')
    for (const version of item.versions) {
      const caps = version.capabilities ?? {}
      assert(caps && typeof caps === 'object' && !Array.isArray(caps), 'capabilities must be an object')
      if (expected === 1) {
        assert(Object.keys(caps).every(key => legacyCapabilities.has(key)), 'v2 or unknown capabilities cannot enter v1')
        assert(version.manifestVersion === undefined || version.manifestVersion === 1, 'v2 manifest cannot enter v1')
        assert(version.runtime === 'wasm-component' || version.runtime === 'mcp-stdio', 'unknown v1 runtime')
      } else {
        assert(Object.keys(caps).every(key => ['network','clock','sampling','hostTools','preopens'].includes(key)), 'unknown v2 capability')
        assert(Object.keys(caps.network ?? {}).every(key => ['protocol','origins'].includes(key)), 'unknown network capability')
        assert(version.manifestVersion === 2, 'v2 catalog requires v2 manifest')
        assert(version.runtime === 'wasm-component', 'v2 network flavor requires wasm-component')
        assert(caps.network?.protocol === 'clat:net-task@0.1.0', 'v2 network protocol must be pinned')
        assert(Array.isArray(caps.network.origins), 'v2 origins must be explicit')
        assert(Array.isArray(caps.hostTools ?? []) && Array.isArray(caps.preopens ?? []) && (caps.hostTools ?? []).length === 0 && (caps.preopens ?? []).length === 0, 'network cannot carry tools/preopens')
        assert(item.id.endsWith('-wasm'), 'v2 flavor requires an independent wasm id')
        for (const artifact of version.artifacts ?? []) {
          assert(typeof artifact.url === 'string' && /^packages\/[a-zA-Z0-9._-]+\.clatpkg$/.test(artifact.url), 'v2 artifact must stay in its own relative namespace')
        }
      }
    }
  }
  return index
}
