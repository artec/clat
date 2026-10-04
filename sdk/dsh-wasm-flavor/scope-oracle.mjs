// Invariants run against the real Bun shim first, then the explicit WASM flavor.
import assert from 'node:assert/strict'
const tool = name => ({name,description:name,parameters:{type:'object'},output:{render:()=>[]},execute:async()=>({})})
const defer = () => { let resolve; const promise=new Promise(r=>resolve=r); return {promise,resolve} }
export async function scopeOracle(Shim) {
  const trace=[], logs=[]
  const shim=new Shim({sampling:async()=>{},elicitation:async()=>{},capabilities:{sampling:false,elicitation:false,hostServices:false},log:(...v)=>logs.push(v)},'scope-oracle')
  const root=shim.buildContext(), gates=[defer(),defer()]
  const setup=(name,gate)=>root.inject('tools',async ctx=>{
    await gate.promise
    ctx.tools.register(tool(name))
    ctx.on('late',()=>trace.push('event-'+name))
    ctx.systemPrompt.section({name:'section-'+name,order:1,text:name})
    await ctx.effect(async function*(){
      await Promise.resolve();yield ()=>trace.push(name+'-first')
      await Promise.resolve();yield ()=>trace.push(name+'-second')
    })
    ctx.effect(()=>()=>{trace.push(name+'-failure');throw new Error(name+' cleanup')})
    ctx.effect(()=>()=>trace.push(name+'-last'))
  })
  const a=setup('a',gates[0]),b=setup('b',gates[1])
  gates[1].resolve();await b;gates[0].resolve();await a
  assert.deepEqual(shim.listTools().map(t=>t.name).filter(n=>['a','b'].includes(n)).sort(),['a','b'])
  await assert.rejects(a.dispose(),/a cleanup/)
  assert.deepEqual(shim.listTools().map(t=>t.name).filter(n=>['a','b'].includes(n)),['b'],'disposing A must retain B and remove only A')
  root.emit('late')
  assert.deepEqual(trace,['a-last','a-failure','a-second','a-first','event-b'],'LIFO, failure isolation and late listener ownership')
  await a.dispose()
  await assert.rejects(b.dispose(),/b cleanup/)
  assert.deepEqual(trace.slice(5),['b-last','b-failure','b-second','b-first'])
  assert.equal(shim.listTools().filter(t=>['a','b'].includes(t.name)).length,0)
  root.emit('late');assert.equal(trace.length,9,'no late disposed event may survive')
  assert.equal(shim.listPrompts().length,0,'prompt registration must follow the same scope')
  await shim.disposeAll();await shim.disposeAll()
  return {trace,failures:logs.filter(v=>v[0]==='effect cleanup failed (1 step(s)):').length}
}
