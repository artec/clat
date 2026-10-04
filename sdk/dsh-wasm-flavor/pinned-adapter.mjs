// Original lookup/Undici calls consume private host grants, never guest-chosen IPs.
import {request} from './network-fetch.mjs'
let origins=[],pending=[]
export function configureOrigins(values) {
  if(pending.length)throw new Error('invalid-request: cannot reconfigure live resolutions')
  origins=[...new Set(values.map(value=>new URL(value).origin))]
}
export async function lookup(hostname,options,signal=new AbortController().signal) {
  if(options?.all!==true||options.order!=='verbatim')throw new Error('unsupported: DNS lookup shape')
  if(hostname==='ipv4only.arpa') {
    const discovery=pending.map(item=>JSON.stringify(item.discovery))
    if(!discovery.length||discovery.some(item=>item!==discovery[0]))throw new Error('unsupported: ambiguous DNS64 metadata')
    return JSON.parse(discovery[0])
  }
  const allowed=origins.filter(origin=>new URL(origin).hostname===hostname)
  if(allowed.length!==1)throw new Error('capability-denied: hostname must identify one declared origin')
  if(pending.length>=16)throw new Error('limit-exceeded')
  const resolution=await clatNetDns(allowed[0],30000,signal)
  try {
    const addresses=clatNetAddresses(resolution).map(([address,family])=>({address,family:family==='ipv4'?4:6}))
    const discovery=clatNetDns64(resolution).map(([address,family])=>({address,family:family==='ipv4'?4:6}))
    pending.push({origin:allowed[0],resolution,addresses,discovery})
    return addresses.map(item=>({...item}))
  }catch(error){clatNetDispose(resolution);throw error}
}
export class Agent {
  #lookup; #responses=[]; #closed=false; #calls=new Set(); #listeners=[]
  constructor(options) {
    if(options?.autoSelectFamily!==true||typeof options.connect?.lookup!=='function'||Object.keys(options).some(key=>!['autoSelectFamily','connect'].includes(key))||Object.keys(options.connect).some(key=>key!=='lookup'))throw new Error('unsupported: only pinned semantic HTTP dispatch is available')
    this.#lookup=options.connect.lookup
  }
  async request(url,options) {
    if(this.#closed)throw new Error('consumed: dispatcher closed')
    const addresses=await new Promise((resolve,reject)=>this.#lookup(new URL(url).hostname,{all:true,family:0},(error,answers)=>error?reject(error):resolve(answers)))
    const signature=items=>JSON.stringify(items.map(item=>[item.address,item.family]).sort())
    const origin=new URL(url).origin,candidates=pending.filter(item=>item.origin===origin&&signature(item.addresses)===signature(addresses))
    if(candidates.length!==1)throw new Error('invalid-resolution: no unique private grant')
    const grant=candidates[0];pending.splice(pending.indexOf(grant),1)
    const controller=new AbortController(),signal=options.signal
    const abort=()=>controller.abort(signal?.reason)
    this.#calls.add(controller)
    signal?.addEventListener('abort',abort,{once:true})
    this.#listeners.push(()=>signal?.removeEventListener('abort',abort))
    if(signal?.aborted)abort()
    try {
      const response=await request(url,{...options,signal:controller.signal},grant.resolution)
      if(this.#closed){await response.body.cancel();throw new Error('cancelled')}
      this.#responses.push(response);return response
    }finally{clatNetDispose(grant.resolution)}
  }
  async close() {
    this.#closed=true
    for(const controller of this.#calls)controller.abort()
    this.#calls.clear();for(const remove of this.#listeners.splice(0))remove()
    await Promise.all(this.#responses.splice(0).map(response=>response.body.cancel()))
  }
}
export async function fetch(url,options) {
  if(!(options?.dispatcher instanceof Agent))throw new Error('unsupported: proxy or foreign dispatcher')
  const {dispatcher,...requestOptions}=options
  return dispatcher.request(url,requestOptions)
}
export class ProxyAgent {constructor(){throw new Error('unsupported: proxy')}}
export class Pool {constructor(){throw new Error('unsupported: raw pool')}}

// The original HTTP provider accepts its DNS resolver as an explicit third argument.
// Preserve its classifier while carrying its actual deadline signal into the host task.
export function bindFetchProvider(provider) {
  if(provider?.id!=='http'||typeof provider.resolveAddresses!=='function')return provider
  return new Proxy(provider,{
    get(target,key,receiver) {
      if(key==='resolveAddresses')return (hostname,signal)=>Reflect.apply(target.resolveAddresses,target,[
        hostname,signal,(name,options)=>lookup(name,options,signal),
      ])
      return Reflect.get(target,key,receiver)
    },
  })
}
