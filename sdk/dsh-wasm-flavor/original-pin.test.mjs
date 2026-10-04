// Same immutable address vectors as the Rust host; original provider classifier.
import {test} from 'node:test'
import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import {HttpFetchProvider} from '../dsh-adapter/examples/official-web/node_modules/@deepseek-ai/dsh-web-fetch-http/lib/index.js'
test('original Bun HTTP classifier and WASM host use the same public/mixed/NAT64 pin vectors',async()=>{
 const cases=JSON.parse(await readFile(new URL('./pin-cases.json',import.meta.url),'utf8'))
 const provider=new HttpFetchProvider({})
 for(const row of cases) {
  const names=[]
  const resolver=async name=>{names.push(name);assert(['pin.invalid','ipv4only.arpa'].includes(name));return (name==='pin.invalid'?row.addresses:row.discovery).map(address=>({address,family:address.includes(':')?6:4}))}
  const operation=provider.resolveAddresses('pin.invalid',new AbortController().signal,resolver)
  if(row.allow)assert.deepEqual((await operation).map(item=>item.address),row.addresses,row.id)
  else await assert.rejects(operation,error=>error.code==='WEB_BLOCKED_URL',row.id)
  assert.deepEqual(names,row.addresses.some(ip=>ip.includes(':'))?['pin.invalid','ipv4only.arpa']:['pin.invalid'],row.id)
 }
})
