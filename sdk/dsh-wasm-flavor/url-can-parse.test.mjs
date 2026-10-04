import {test} from 'node:test'
import assert from 'node:assert/strict'
import {installUrlCanParse} from './url-can-parse.mjs'
const original=URL.canParse;delete URL.canParse;installUrlCanParse()
test('URL predicate agrees with native parsing for absolute, relative and invalid forms',()=>{
 for(const [url,base] of [['https://example.com'],['/path','https://example.com'],['/path'],['http://[bad]'],['mailto:a@b'],['http://127.1'],['https://例子.测试'],['x','invalid']])assert.equal(URL.canParse(url,base),original(url,base))
})
test('Web IDL coercion errors stay errors and valid coercion occurs once',()=>{
 let reads=0;assert.equal(URL.canParse({toString(){reads++;return 'https://example.com'}}),true);assert.equal(reads,1)
 assert.throws(()=>URL.canParse(),TypeError);assert.throws(()=>URL.canParse(Symbol()),TypeError)
 const error=new Error('conversion');assert.throws(()=>URL.canParse({toString(){throw error}}),value=>value===error)
})
