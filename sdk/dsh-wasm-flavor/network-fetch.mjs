// Semantic HTTP adapter only. Every network operation uses the typed host authority.
const MAX_BODY=8*1024*1024, TIMEOUT=30000
const encoder=new TextEncoder()
export async function fetch(input,options={}) {
  if(options.dispatcher!==undefined)throw new Error('unsupported: dispatcher requires the pinned adapter')
  return request(input,options)
}
export async function request(input,options={},resolution) {
  const url=new URL(String(input)),signal=options.signal??new AbortController().signal
  if(options.redirect!==undefined&&!['manual','error'].includes(options.redirect))throw new Error('unsupported: implicit redirects')
  if(signal.aborted)throw new DOMException('The operation was aborted','AbortError')
  const verb=String(options.method??'GET').toUpperCase(),headers=requestHeaders(options.headers)
  const body=options.body===undefined?new Uint8Array():typeof options.body==='string'?encoder.encode(options.body):options.body
  if(!(body instanceof Uint8Array))throw new Error('unsupported: request body must be text or bytes')
  resolution??=await clatNetDns(url.origin,TIMEOUT,signal)
  try {
    const response=await clatNetRequest(resolution,url.href,verb,headers,body,TIMEOUT,MAX_BODY,signal)
    const wrapped=wrapResponse(response,url.href,signal)
    if(options.redirect==='error'&&wrapped.status>=300&&wrapped.status<400){await wrapped.body.cancel();throw new Error('redirect rejected')}
    return wrapped
  } finally {clatNetDispose(resolution)}
}
function requestHeaders(headers) {
  if(headers===undefined)return []
  if(Array.isArray(headers))return headers.map(([name,value])=>[String(name),String(value)])
  if(typeof headers.entries==='function')return Array.from(headers.entries(),([name,value])=>[String(name),String(value)])
  return Object.entries(headers).map(([name,value])=>[name,String(value)])
}
function wrapResponse(handle,url,signal) {
  const controller=new AbortController()
  const abort=()=>controller.abort(signal.reason)
  signal.addEventListener('abort',abort,{once:true})
  if(signal.aborted)abort()
  const entries=clatNetHeaders(handle),status=clatNetInfo(handle)
  let used=false,reading=false,finished=false
  const close=()=>{if(!finished){finished=true;controller.abort();signal.removeEventListener('abort',abort);clatNetDispose(handle)}}
  const read=async()=>{
    if(reading)throw new Error('invalid-request: concurrent body read')
    if(finished)return {done:true,value:undefined}
    reading=true
    try {
      const chunk=await clatNetRead(handle,65536,controller.signal)
      handle=chunk.response
      const bytes=chunk.bytes
      if(finished){clatNetDispose(handle);throw new DOMException('The operation was aborted','AbortError')}
      if(bytes.length===0){close();return {done:true,value:undefined}}
      return {done:false,value:new Uint8Array(bytes)}
    }catch(error){close();throw error}finally{reading=false}
  }
  const reader=()=>{
    if(used)throw new Error('invalid-request: body already consumed')
    used=true
    return {read,cancel:async()=>close(),releaseLock(){}}
  }
  const text=async()=>{
    const stream=reader(),decoder=new TextDecoder(),parts=[]
    for(;;){const chunk=await stream.read();if(chunk.done)break;parts.push(decoder.decode(chunk.value,{stream:true}))}
    parts.push(decoder.decode());return parts.join('')
  }
  return {
    status,ok:status>=200&&status<300,url,
    headers:{get(name){const values=entries.filter(([key])=>key.toLowerCase()===String(name).toLowerCase()).map(([,value])=>value);return values.length?values.join(', '):null},entries(){return entries.map(pair=>[...pair])[Symbol.iterator]()}},
    body:{getReader:reader,cancel:async()=>close()},text,json:async()=>JSON.parse(await text()),
  }
}
