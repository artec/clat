import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import {createRequire} from 'node:module'
import {pathToFileURL} from 'node:url'
import http from 'node:http'
const require=createRequire(new URL('../dsh-adapter/examples/official-web/plugin.mjs',import.meta.url))
const {HttpFetchProvider}=await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-web-fetch-http')))
const cases=JSON.parse(await readFile(new URL('./encoding-cases.json',import.meta.url),'utf8'))
test('unchanged original HTTP provider consumes the shared encoding wire vectors',async()=>{
 const server=http.createServer((request,response)=>{
  const index=Number(request.url.slice(1)),item=cases[index],wire=Buffer.from(item.wireHex,'hex')
  response.writeHead(200,{'Content-Type':'text/plain; charset=utf-8','Content-Encoding':item.encoding,'Content-Length':wire.length})
  response.end(wire)
 })
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve))
 const port=server.address().port
 // Trusted numeric test route; the ordinary provider resolver is unchanged.
 const provider=new HttpFetchProvider({timeoutMs:5000,maxResponseBytes:8*1024*1024,maxOutputChars:100000,maxRedirects:5},async()=>[{address:'127.0.0.1',family:4}])
 try {
  for(const [index,item] of cases.entries()){
   const result=await provider.fetch({url:`http://encoding.invalid:${port}/${index}`})
   assert.deepEqual(result.body,{kind:'text',content:Buffer.from(item.plainHex,'hex').toString('utf8')},item.encoding)
   assert.equal(result.truncated,false)
  }
 }finally{await new Promise(resolve=>server.close(resolve))}
})
