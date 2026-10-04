// Verify captured author evidence only; this never runs components or grants DNS authority.
import assert from 'node:assert/strict'
import {readFile,writeFile,mkdir} from 'node:fs/promises'
import {createHash} from 'node:crypto'
import path from 'node:path'
assert.equal(process.argv.length,8,'usage: node verify-native-typed-evidence.mjs DNS_GREEN HTTP_GREEN CANCEL_RED PREABORT_RED PROTOTYPE_RED /absolute/new-evidence')
const [dnsGreen,httpGreen,cancel,preabort,prototype,out]=process.argv.slice(2)
assert(path.isAbsolute(out))
await mkdir(out)
const paths=[dnsGreen,httpGreen,cancel,preabort,prototype]
const logs=await Promise.all(paths.map(p=>readFile(p,'utf8')))
for (const log of logs.slice(0,2)) assert.match(log,/test result: ok\. 1 passed; 0 failed/)
const dns=logs[0].split('\n').filter(l=>l.startsWith('TYPED_NATIVE ')).map(l=>JSON.parse(l.slice('TYPED_NATIVE '.length)))
const http=logs[1].split('\n').filter(l=>l.startsWith('TYPED_NATIVE_TRANSPORT ')).map(l=>JSON.parse(l.slice('TYPED_NATIVE_TRANSPORT '.length)))
assert.equal(dns.length,12)
assert.equal(http.length,14)
for (const [cases,phase] of [[dns,'dns'],[http.filter(v=>v.phase==='headers'),'headers'],[http.filter(v=>v.phase==='body'),'body']]) {
  for (const order of ['before','microtask','during','none','after']) {
    const value=cases.find(v=>v.order===order)
    assert(value,`${phase}/${order} missing`)
    assert.equal(value.kind,['before','microtask','during'].includes(order)?'error':'success')
    if(value.kind==='error') assert.equal(value.name,'AbortError')
    const trace=order==='before'?['abort','enter','error']:['microtask','during'].includes(order)?['enter','abort','error']:order==='after'?['enter','return','abort']:['enter','return']
    assert.deepEqual(value.trace,trace)
  }
}
for(const log of logs.slice(2)) {
  assert.match(log,/panicked at/)
  assert.match(log,/test result: FAILED\./)
  assert.doesNotMatch(log,/error\[E\d+\]|could not compile/)
}
assert.match(logs[2],/physical cleanup before Store Drop/)
assert.match(logs[2],/left: 2\s+right: 0/)
assert.match(logs[3],/pre-abort must not submit/)
assert.match(logs[3],/left: 1\s+right: 0/)
assert.match(logs[4],/ambient then getter reached credential/)
const sha=bytes=>createHash('sha256').update(bytes).digest('hex')
await writeFile(path.join(out,'report.json'),JSON.stringify({nativeDnsCalls:dns.length,privateTransportFixtureCalls:http.length,logs:paths.map((p,i)=>({path:path.resolve(p),sha256:sha(logs[i])})),dnsSystemTested:false,productionOwnerWired:false,fullD1AttackMatrixComplete:false},null,2)+'\n')
console.log(JSON.stringify({nativeDnsCalls:dns.length,privateTransportFixtureCalls:http.length,scope:'captured author evidence only'}))
