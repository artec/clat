import test from 'node:test'
import assert from 'node:assert/strict'
import {releaseDefinitions,requireReleaseTool} from './release-tools.mjs'
test('search-only release has the same discovery and call boundary',()=>{
 const tools=[{name:'web_search'},{name:'web_fetch'}]
 assert.deepEqual(releaseDefinitions(tools,true),[tools[0]])
 assert.throws(()=>requireReleaseTool('web_fetch',true))
 assert.throws(()=>requireReleaseTool('invented',true))
 requireReleaseTool('web_search',true)
 assert.equal(releaseDefinitions(tools,false),tools)
})
