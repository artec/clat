import {test} from 'node:test'
import assert from 'node:assert/strict'
import {installAbortAny} from './abort-any.mjs'
const native=AbortSignal.any
installAbortAny()
test('any consumes an iterable, keeps live sources live, and propagates the first reason',()=>{
 const a=new AbortController(),b=new AbortController(),reason={why:'b'}
 const signal=AbortSignal.any(new Set([a.signal,b.signal]))
 assert.equal(signal.aborted,false);b.abort(reason);assert.equal(signal.reason,reason)
 a.abort('later');assert.equal(signal.reason,reason)
 assert.equal(AbortSignal.any([]).aborted,false)
})
test('pre-aborted ordering, duplicate sources and invalid complete iterables',()=>{
 const a=new AbortController(),b=new AbortController();a.abort('a');b.abort('b')
 assert.equal(AbortSignal.any([b.signal,a.signal,b.signal]).reason,'b')
 assert.throws(()=>AbortSignal.any([a.signal,{}]),TypeError)
 assert.throws(()=>AbortSignal.any(undefined),TypeError)
 const c=new AbortController(),signal=AbortSignal.any([c.signal,c.signal]);let events=0
 signal.addEventListener('abort',()=>events++);c.abort();assert.equal(events,1)
})
test('native reference retains the same observable standard cases',()=>{
 const a=new AbortController(),b=new AbortController(),signals=[a.signal,b.signal]
 const expected=native(signals),actual=AbortSignal.any(signals);assert.equal(actual.aborted,expected.aborted)
 b.abort('reason');assert.equal(actual.reason,expected.reason)
})
test('sequence input must be iterable and reads the iterator getter once',()=>{
 assert.throws(()=>AbortSignal.any({length:0}),TypeError)
 let reads=0;const a=new AbortController()
 const input={get [Symbol.iterator](){reads++;return function*(){yield a.signal}}}
 assert.equal(AbortSignal.any(input).aborted,false);assert.equal(reads,1)
})
