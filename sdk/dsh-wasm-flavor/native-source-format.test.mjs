import assert from 'node:assert/strict'
import {test} from 'node:test'
import {normalizeNativeSource} from './native-source-format.mjs'
test('native body whitespace canonicalizes without changing constructor identity',()=>{
 assert.equal(normalizeNativeSource('function Object() {\n    [native code]\n}'),'function Object() { [native code] }')
 assert.equal(normalizeNativeSource('function Object() { [native code] }'),'function Object() { [native code] }')
})
test('user functions, strings and comments cannot become native constructor witnesses',()=>{
 for(const source of ["function Object(){return '[native code]'}",'function Object(){/* [native code] */}',"function Object(){throw '[native code]'}",'()=>Object'])assert.equal(normalizeNativeSource(source),source)
})
