import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import {validateIndexGeneration} from './index-generation.mjs'
const source = JSON.parse(await readFile(new URL('../index.source.json', import.meta.url)))
test('existing signed-v1 source remains publishable', () => assert.equal(validateIndexGeneration(source, 1), source))
test('v1 publication refuses new network, clock and preopen faces before writing or signing', () => {
  for (const [field, value] of [['network', {protocol:'clat:net-task@0.1.0',origins:[]}], ['clock',{}], ['preopens',[]]]) {
    const index=structuredClone(source)
    index.packages[0].versions[0].capabilities[field]=value
    assert.throws(() => validateIndexGeneration(index,1),/cannot enter v1/)
  }
})
test('v2 records cannot enter legacy generation, including capability-free records', () => {
  const index=structuredClone(source)
  index.packages[0].versions[0].manifestVersion=2
  assert.throws(() => validateIndexGeneration(index,1),/v2 manifest/)
  index.schemaVersion=2
  assert.throws(() => validateIndexGeneration(index,1),/schemaVersion/)
})
test('new namespace requires explicit network policy and separate identity', () => {
  const index={schemaVersion:2,packages:[{id:'io.artec.dsh-official-web-wasm',versions:[{
    manifestVersion:2,runtime:'wasm-component',capabilities:{network:{protocol:'clat:net-task@0.1.0',origins:[]}},artifacts:[{url:'packages/net.clatpkg'}],
  }]}]}
  assert.equal(validateIndexGeneration(index,2),index)
  for(const change of [v=>v.runtime='mcp-stdio',v=>v.manifestVersion=1,v=>v.capabilities.hostTools=['run_command'],v=>v.artifacts[0].url='../packages/net.clatpkg']) {
    const copy=structuredClone(index);change(copy.packages[0].versions[0]);assert.throws(()=>validateIndexGeneration(copy,2))
  }
})
test('v2 generation cannot smuggle malformed permission combinations or unknown capabilities',()=>{
 const index={schemaVersion:2,packages:[{id:'io.artec.dsh-official-web-wasm',versions:[{manifestVersion:2,runtime:'wasm-component',capabilities:{network:{protocol:'clat:net-task@0.1.0',origins:[]}}}]}]}
 for(const change of [caps=>caps.hostTools=12,caps=>caps.preopens={},caps=>caps.workerThreads=true,caps=>caps.network.proxy='http://proxy.invalid']) {
  const copy=structuredClone(index);change(copy.packages[0].versions[0].capabilities)
  assert.throws(()=>validateIndexGeneration(copy,2))
 }
})
