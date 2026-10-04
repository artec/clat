import assert from 'node:assert/strict'
import {test} from 'node:test'
import {fetch} from './network-fetch.mjs'
const slots=['clatNetDns','clatNetRequest','clatNetHeaders','clatNetInfo','clatNetRead','clatNetDispose']
function host(callback) {
 const old=slots.map(key=>globalThis[key]),trace=[]
 Object.assign(globalThis,{
  clatNetDns:async(origin,timeout,signal)=>{trace.push(['dns',origin,timeout,signal]);return {kind:'resolution'}},
  clatNetRequest:async(resolution,url,verb,headers,body)=>{trace.push(['http',url,verb,headers,[...body]]);return {kind:'response',index:0}},
  clatNetHeaders:()=>[['content-type','application/json'],['x-test','one'],['x-test','two']],clatNetInfo:()=>201,
  clatNetRead:async(response,max,signal)=>{trace.push(['read',response.index,max,signal]);return {response:{kind:'response',index:response.index+1},bytes:response.index===0?new TextEncoder().encode('{"ok":true}'):new Uint8Array()}},
  clatNetDispose:resource=>trace.push(['dispose',resource]),
 })
 return Promise.resolve().then(()=>callback(trace)).finally(()=>slots.forEach((key,i)=>old[i]===undefined?delete globalThis[key]:globalThis[key]=old[i]))
}
test('fetch preserves POST bytes, status, repeated headers and sequential owned body',()=>host(async trace=>{
 const response=await fetch('https://api.example.com/search',{method:'POST',headers:{'content-type':'application/json'},body:'{"q":"hi"}'})
 assert.equal(response.status,201);assert.equal(response.ok,true);assert.equal(response.headers.get('X-Test'),'one, two')
 assert.deepEqual(await response.json(),{ok:true});assert.deepEqual(trace.find(v=>v[0]==='http').slice(1),['https://api.example.com/search','POST',[['content-type','application/json']],[...new TextEncoder().encode('{"q":"hi"}')]])
 assert.deepEqual(trace.filter(v=>v[0]==='read').map(v=>v[1]),[0,1]);assert.throws(()=>response.body.getReader(),/already consumed/)
 assert.equal(trace.at(-1)[1].index,2,'dispose the final transferred handle')
}))
test('cancel during pending read aborts the physical task signal',()=>host(async trace=>{
 globalThis.clatNetRead=(_resource,_max,signal)=>new Promise((resolve,reject)=>signal.addEventListener('abort',()=>reject(new DOMException('aborted','AbortError')),{once:true}))
 const response=await fetch('https://api.example.com'),reader=response.body.getReader(),pending=reader.read()
 await assert.rejects(reader.read(),/concurrent/)
 await response.body.cancel();await assert.rejects(pending,{name:'AbortError'})
 assert.equal(trace.filter(v=>v[0]==='dispose'&&v[1].kind==='response').length,1)
}))
test('pre-abort, implicit redirect and dispatcher cannot enter DNS',()=>host(async trace=>{
 const controller=new AbortController();controller.abort()
 await assert.rejects(fetch('https://api.example.com',{signal:controller.signal}),{name:'AbortError'})
 await assert.rejects(fetch('https://api.example.com',{redirect:'follow'}),/implicit redirects/)
 await assert.rejects(fetch('https://api.example.com',{dispatcher:{}}),/dispatcher/)
 assert.equal(trace.length,0)
}))
test('redirect error semantics close a 3xx response rather than following it',()=>host(async trace=>{
 globalThis.clatNetInfo=()=>302
 await assert.rejects(fetch('https://api.example.com',{redirect:'error'}),/redirect rejected/)
 assert.equal(trace.filter(v=>v[0]==='http').length,1)
 assert.equal(trace.filter(v=>v[0]==='dispose'&&v[1].kind==='response').length,1)
}))
