import {test} from 'node:test'
import assert from 'node:assert/strict'
import {randomUUID} from 'node:crypto'
const names=['clatNetDns','clatNetRequest','clatNetAddresses','clatNetDns64','clatNetHeaders','clatNetInfo','clatNetDispose']
async function fixture(callback) {
 const original=names.map(name=>globalThis[name]),calls=[]
 Object.assign(globalThis,{
  clatNetDns:async origin=>{calls.push(['dns',origin]);return {origin}},
  clatNetAddresses:()=>[['8.8.8.8','ipv4']],clatNetDns64:()=>[],
  clatNetRequest:async(grant,url,_verb,_headers,_body,_timeout,_max,signal)=>{calls.push(['http',grant,url,signal]);return {}},
  clatNetHeaders:()=>[],clatNetInfo:()=>200,clatNetDispose:()=>{},
 })
 try {await callback(await import('./pinned-adapter.mjs?test='+randomUUID()),calls)}
 finally {names.forEach((name,i)=>original[i]===undefined?delete globalThis[name]:globalThis[name]=original[i])}
}
const agent=(api,addresses)=>new api.Agent({autoSelectFamily:true,connect:{lookup:(_name,_options,callback)=>callback(null,addresses)}})
test('private host resolution is consumed once without a second DNS lookup',()=>fixture(async(api,calls)=>{
 api.configureOrigins(['https://allowed.example.com'])
 const addresses=await api.lookup('allowed.example.com',{all:true,order:'verbatim'}),dispatcher=agent(api,addresses)
 const response=await api.fetch('https://allowed.example.com/path',{dispatcher,redirect:'manual'})
 assert.equal(response.status,200);assert.equal(calls.filter(v=>v[0]==='dns').length,1);assert.equal(calls.filter(v=>v[0]==='http').length,1)
 await assert.rejects(api.fetch('https://allowed.example.com/again',{dispatcher,redirect:'manual'}),/invalid-resolution/)
 await dispatcher.close()
}))
test('unknown origin, ambiguous origin and forged pins never enter HTTP',()=>fixture(async(api,calls)=>{
 api.configureOrigins(['https://allowed.example.com'])
 await assert.rejects(api.lookup('other.example.com',{all:true,order:'verbatim'}),/capability-denied/)
 await api.lookup('allowed.example.com',{all:true,order:'verbatim'})
 const dispatcher=agent(api,[{address:'127.0.0.1',family:4}])
 await assert.rejects(api.fetch('https://allowed.example.com/',{dispatcher,redirect:'manual'}),/invalid-resolution/)
 assert.equal(calls.filter(v=>v[0]==='http').length,0);await dispatcher.close()
 await assert.rejects(api.fetch('https://allowed.example.com/',{dispatcher:{}}),/foreign dispatcher/)
 assert.throws(()=>new api.ProxyAgent(),/proxy/)
}))
test('closing a dispatcher aborts its in-flight typed HTTP task',()=>fixture(async(api,calls)=>{
 api.configureOrigins(['https://allowed.example.com'])
 const addresses=await api.lookup('allowed.example.com',{all:true,order:'verbatim'}),dispatcher=agent(api,addresses)
 let started;const entered=new Promise(resolve=>started=resolve)
 globalThis.clatNetRequest=(_grant,_url,_verb,_headers,_body,_timeout,_max,signal)=>new Promise((resolve,reject)=>{signal.addEventListener('abort',()=>reject(new DOMException('aborted','AbortError')),{once:true});started()})
 const pending=api.fetch('https://allowed.example.com/',{dispatcher,redirect:'manual'})
 await entered;await dispatcher.close();await assert.rejects(pending,{name:'AbortError'})
}))
test('original provider DNS resolver receives the original deadline signal',()=>fixture(async(api,calls)=>{
 api.configureOrigins(['https://allowed.example.com'])
 const controller=new AbortController();let seen
 globalThis.clatNetDns=(_origin,_timeout,signal)=>{seen=signal;return Promise.reject(new Error('capability-denied'))}
 const provider={id:'http',resolveAddresses:async(host,signal,resolver)=>resolver(host,{all:true,order:'verbatim'}),async fetch(_request,signal){return this.resolveAddresses('allowed.example.com',signal)}}
 await assert.rejects(api.bindFetchProvider(provider).fetch({},controller.signal),/capability-denied/)
 assert.equal(seen,controller.signal,'the typed task must observe the provider deadline signal')
}))
