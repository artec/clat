const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
const context=vm.createContext({});
vm.runInContext(fs.readFileSync(path.join(__dirname,'../settings-pages.js'),'utf8'),context,{filename:'settings-pages.js'});
const clone=value=>JSON.parse(JSON.stringify(value));

test('MCP Settings accepts friendly newline arguments and exact JSON arrays',()=>{
  assert.deepEqual(clone(context.parseMcpArgs('first\nsecond with spaces')),['first','second with spaces']);
  assert.deepEqual(clone(context.parseMcpArgs('["--stdio","with spaces"]')),['--stdio','with spaces']);
  assert.deepEqual(clone(context.parseMcpArgs('{"valid":"literal-argument"}')),['{"valid":"literal-argument"}']);
});
test('MCP Settings parses environment and header pairs without changing values',()=>{
  assert.deepEqual(clone(context.parseMcpPairs('MODE=test\nHEADER=Bearer value with spaces')),{
    MODE:'test',HEADER:'Bearer value with spaces'
  });
  assert.deepEqual(clone(context.parseMcpPairs('{"A":"x=y","B":"z"}')),{A:'x=y',B:'z'});
  assert.throws(()=>context.parseMcpPairs('INVALID'));
  assert.throws(()=>context.parseMcpPairs('__proto__=mutation'));
});
