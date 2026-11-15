const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
function harness({staged=false,autoStage=false,confirm=true,phase='completed',afterStage=()=>{},commitOutcome,onStart=()=>{}}={}){
  const owner={id:'project',autoStage,snapshot:{files:[{path:'selected.txt',staged,unstaged:!staged}]},draft:'',edit:0,pending:null,operation:null};
  let current=owner;
  const calls=[],questions=[],errors=[];
  const ctx=vm.createContext({
    CLATDialog:{confirm:async message=>{questions.push(message);return confirm;},prompt:async()=>null},
    devText:key=>key,devId:()=>String(calls.length+1),navigator:{language:'en'},
    localStorage:{setItem(){},removeItem(){}},CLATI18n:{present:(_,fallback)=>fallback},
    gitConflicted:f=>Boolean(f.conflict),
    async devOperation(request,_progress,resume){
      onStart(owner);calls.push(request);
      if(request.action==='commit'&&commitOutcome)return commitOutcome(resume);
      if(request.action==='stage'){
        if(phase==='completed')owner.snapshot={files:[{path:'selected.txt',staged:true}]};
        await afterStage(owner,()=>{current={...owner,id:'other'};});
        return {phase,localUnknown:phase==='unknown'};
      }
      return {phase:'completed',message:request.action==='generate'?'generated subject':undefined};
    }
  });
  vm.runInContext(fs.readFileSync(process.env.CLAT_GIT_ACTIONS_SOURCE||'web/development/git-actions.js','utf8'),ctx);
  const api=ctx.createGitActions({seat:()=>current,call:()=>{},refresh:async()=>{},render(){},report:e=>errors.push(e),result(){}});
  return {api,owner,calls,questions,errors};
}
test('empty input stages only after consent then continues requested action',async()=>{
  for(const action of ['commit','generate']){
    const h=harness();await h.api.start(action);
    assert.deepEqual(h.calls.map(r=>r.action),['stage',action]);
    assert.equal(h.calls[1].expected,h.owner.snapshot);
    assert.equal(h.calls[1].message,'');assert.equal(h.questions.length,1);
    assert.deepEqual(Array.from(h.calls[0].paths),['selected.txt']);
  }
});
test('auto-stage preference submits one commit without opening the confirmation',async()=>{
  const h=harness({autoStage:true,confirm:false});
  await h.api.start('commit');
  assert.deepEqual(h.calls.map(request=>request.action),['commit']);
  assert.equal(h.questions.length,0);
  await h.api.start('generate');
  assert.equal(h.calls.length,1);
  assert.equal(h.questions.length,1);
});
test('declining stage permission has no mutation or generation',async()=>{
  const h=harness({confirm:false});await h.api.start('commit');assert.equal(h.calls.length,0);
});
test('partial staging preserves selection without staging other files',async()=>{
  const h=harness({staged:true});h.owner.snapshot.files.push({path:'excluded.txt',unstaged:true});
  await h.api.start('generate');assert.deepEqual(h.calls.map(r=>r.action),['generate']);assert.equal(h.questions.length,0);
});
test('failed cancelled or uncertain staging never continues',async()=>{
  for(const phase of ['failed','cancelled','unknown']){
    const h=harness({phase});await h.api.start('commit');assert.deepEqual(h.calls.map(r=>r.action),['stage']);
  }
});
test('repository switches or edits during staging cancel continuation',async()=>{
  for(const afterStage of [(owner,switchRepo)=>switchRepo(),owner=>{owner.edit++;owner.draft='new draft';}]){
    const h=harness({afterStage});await h.api.start('commit');assert.deepEqual(h.calls.map(r=>r.action),['stage']);
  }
});
test('unresolved conflicts do not start staging',async()=>{
  const h=harness();h.owner.snapshot.files[0].conflict=true;
  await h.api.start('commit');assert.equal(h.calls.length,0);assert.equal(h.questions.length,0);
});
test('repeated submission during stage preparation does not double submit',async()=>{
  let proceed;const blocked=new Promise(resolve=>{proceed=resolve;});
  const h=harness({afterStage:()=>blocked});const first=h.api.start('commit');
  await h.api.start('generate');proceed();await first;
  assert.deepEqual(h.calls.map(r=>r.action),['stage','commit']);
});

test('simultaneous submissions of an already staged selection execute once',async()=>{
  const h=harness({staged:true});
  await Promise.all([h.api.start('commit'),h.api.start('commit')]);
  assert.equal(h.calls.length,1);
});

test('returning to Git during commit and push cannot start a second monitor',async()=>{
  let complete;
  const outcome={phase:'completed',message:'Commit from the host',result:{push:{status:'completed'}}};
  const inFlight=new Promise(resolve=>{complete=resolve;});
  const h=harness({autoStage:true,commitOutcome:()=>inFlight});
  const submitted=h.api.start('commit');
  await Promise.resolve();
  assert.equal(h.calls.length,1);
  const reopened=h.api.reconcile();
  complete(outcome);
  await Promise.all([reopened,submitted]);
  assert.equal(h.calls.length,1);
  assert.deepEqual(h.errors,[]);
  assert.equal(h.owner.result,outcome);
  assert.equal(h.owner.draft,'');
  assert.equal(h.owner.pending,null);
});

test('reopening an uncertain submission monitors the original operation',async()=>{
  const h=harness({staged:true,commitOutcome:resume=>resume
    ? {phase:'completed',message:'Confirmed commit',result:{push:{status:'completed'}}}
    : {phase:'unknown',localUnknown:true}});
  await h.api.start('commit');
  assert.ok(h.owner.pending);
  await h.api.reconcile();
  assert.equal(h.calls[0].operationId,h.calls[1].operationId);
  assert.equal(h.owner.result.phase,'completed');
  assert.equal(h.owner.result.result.push.status,'completed');
  assert.equal(h.owner.pending,null);
});

test('main action follows repository facts after commit and push',()=>{
  const ctx=vm.createContext({});vm.runInContext(fs.readFileSync('web/development/git-actions.js','utf8'),ctx);
  const project=snapshot=>ctx.gitPrimaryAction(snapshot);
  assert.equal(project({files:[{unstaged:true}],tracking:{ahead:1,behind:0}}).action,'commit');
  const outgoing=project({files:[],tracking:{ahead:2,behind:0}});
  assert.equal(outgoing.action,'push');assert.equal(outgoing.count,2);assert.equal(outgoing.enabled,true);
  const diverged=project({files:[],tracking:{ahead:2,behind:3}});
  assert.equal(diverged.action,'pull');assert.equal(diverged.count,3);
  assert.equal(project({files:[],tracking:{ahead:0,behind:0}}).enabled,false);
  assert.equal(project({files:[]}).enabled,false);
  assert.equal(project(null).enabled,false);
});

// An operation's phase/result belong to that operation, including after reconciliation.
test('new operations and reconciled operations cannot expose a previous result or phase',async()=>{
  for(const reconcile of [false,true]){
    const seen=[],h=harness({staged:true,onStart:owner=>seen.push({result:owner.result,notice:owner.notice,phase:owner.phase})});
    h.owner.result={phase:'completed',message:'previous operation'};h.owner.notice={message:'old error'};h.owner.phase='completed';
    if(reconcile){h.owner.pending={operationId:'existing',action:'generate'};await h.api.reconcile();}
    else await h.api.start('generate');
    assert.deepEqual(seen,[{result:null,notice:null,phase:'pending'}]);
    assert.equal(h.owner.result.phase,'completed');
  }
});
