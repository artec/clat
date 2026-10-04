// Checks captured behavior only; does not arm or execute components.
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {readFile, mkdir, writeFile} from 'node:fs/promises'
import path from 'node:path'
assert.equal(process.argv.length,7,'usage: node verify-native-mapping-evidence.mjs DNS_GREEN HTTP_LOG PRE_RED TRANSFER_RED /absolute/new-directory')
const [dnsPath, httpPath, prePath, transferPath, out] = process.argv.slice(2)
assert(path.isAbsolute(out))
const logs = await Promise.all([dnsPath, httpPath, prePath, transferPath].map(p=>readFile(p,'utf8')))
const [dnsGreen, httpLog, pre, transfer] = logs
assert.match(dnsGreen,/test result: ok\. 1 passed; 0 failed/)
assert.match(httpLog,/typed_native_http_read_abort_orders_physically_close_before_store_drop \.\.\. ok/)
assert.doesNotMatch(httpLog,/typed_native_http_read_abort_orders_physically_close_before_store_drop \.\.\. FAILED/)
const rows = (source, prefix) => source.split('\n').filter(l=>l.startsWith(prefix)).map(l=>JSON.parse(l.slice(prefix.length)))
const dns = rows(dnsGreen,'TYPED_NATIVE '), http = rows(httpLog,'TYPED_NATIVE_TRANSPORT ')
assert.equal(dns.length,13)
assert.equal(http.length,26)
assert.equal(dns.find(r=>r.order==='dns64')?.kind,'success')
for (const phase of ['headers','body']) {
  for (const order of ['before','microtask','during','none','after','prototype']) {
    const row=http.find(r=>r.phase===phase && r.order===order)
    assert(row)
    const aborted=['before','microtask','during'].includes(order)
    assert.equal(row.kind,aborted?'error':'success')
    if(aborted) assert.equal(row.name,'AbortError')
    const expected = order==='before'?['abort','enter','error'] : aborted?['enter','abort','error'] : order==='after'?['enter','return','abort']:['enter','return']
    assert.deepEqual(row.trace,expected)
  }
}
for (const order of ['GET','HEAD','POST','PUT','PATCH','DELETE','OPTIONS','invalid','reentry','abort','denied','forbidden']) {
  assert.equal(http.find(r=>r.phase==='mapping' && r.order===order)?.kind,'success')
}
assert.match(pre,/clatNetDns64 is not defined/)
assert.match(pre,/clatNetRequest is not defined/)
assert.match(pre,/test result: FAILED\. 0 passed; 2 failed/)
assert.match(transfer,/test result: FAILED\. 0 passed; 1 failed/)
assert.match(transfer,/panicked/)
assert.match(transfer,/error while executing at wasm backtrace/)
assert.match(transfer,/TYPED_NATIVE_TRANSPORT .*"order":"invalid","kind":"success"/)
assert.doesNotMatch(transfer,/"phase":"mapping","order":"reentry","kind":"success"/)
for (const source of logs) assert.doesNotMatch(source,/error\[E\d+\]|could not compile/)
await mkdir(out)
const sha=value=>createHash('sha256').update(value).digest('hex')
await writeFile(path.join(out,'report.json'),JSON.stringify({nativeDnsCalls:dns.length,privateTransportFixtureCalls:http.length,requestMethods:7,requestConversionReentryGuardDeletionRed:true,logs:[dnsPath,httpPath,prePath,transferPath].map((p,i)=>({path:path.resolve(p),sha256:sha(logs[i])})),systemDnsTested:false,productionOwnerWired:false,fullD1AttackMatrixComplete:false,scope:'author mapping: DNS suite and HTTP suite separately verified; HTTP log also retains earlier rejected DNS fixture; captured logs only'},null,2)+'\n')
console.log(JSON.stringify({nativeCalls:dns.length+http.length,report:path.join(out,'report.json')}))
