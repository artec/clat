const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm'),path=require('node:path');
function loader(){const context=vm.createContext({window:new EventTarget()});vm.runInContext(fs.readFileSync(path.join(__dirname,'../workflow-details.js'),'utf8')+'\n'+fs.readFileSync(path.join(__dirname,'../task-center.js'),'utf8'),context);return context.loadTaskCenterContent;}
test('task center never calls an absent capability and keeps the other section',async()=>{
 for(const jobs of [false,true])for(const scheduler of [false,true]){
  const calls=[],load=loader(),availability={builtin_availability:{'builtin.jobs':{enabled:jobs},'builtin.scheduler':{enabled:scheduler}}};
  const content=await load(availability,async method=>{calls.push(method);return {jobs:[{id:'job'}],rules:[{id:'rule'}],deliveries:[]};});
  assert.equal(calls.includes('jobs.list'),jobs);assert.equal(calls.includes('schedule.list'),scheduler);assert.equal(calls.includes('schedule.deliveries'),scheduler);
  assert.equal(content.jobs.length,Number(jobs));assert.equal(content.rules.length,Number(scheduler));assert.equal(content.errors.length,0);
 }
});
test('a real section read failure stays visible while independent content survives',async()=>{
 const failure=Error('storage read failed'),load=loader();
 const content=await load({},async method=>{if(method==='jobs.list')throw failure;return {rules:[{id:'still-visible'}],deliveries:[{id:'delivery'}]};});
 assert.equal(content.jobs.length,0);assert.equal(content.rules[0].id,'still-visible');assert.equal(content.deliveries.length,1);assert.equal(content.errors[0],failure);
});
test('the work-mode goal action follows Goal availability independently of Plan',()=>{
 const nodes=[];const element=()=>{const children=[];return {dataset:{},disabled:false,setAttribute(){},addEventListener(){},append:(...added)=>children.push(...added),replaceChildren(){children.length=0;},querySelectorAll:()=>children};};
 const scope={window:new EventTarget(),state:{runActive:false},workbenchShell:{workflow:{plan_mode:false,builtin_availability:{'builtin.goal':{enabled:false,reason:{key:'disabled'}},'builtin.plan_mode':{enabled:true}}}},workflowText:k=>k,workflowGoalStatus:()=> 'paused',el:element,svgIcon:()=>({classList:{add(){}}}),CLATI18n:{attribute:(node,key,read)=>node[key]=read(),present:v=>v?.key||'',msg:k=>k},shellButton:()=>{const node=element();nodes.push(node);return node;},attachComposerPopup:(_,__,___,render)=>{render();return {toggle(){},close(){}};}};
 vm.createContext(scope);vm.runInContext(fs.readFileSync(path.join(__dirname,'../workflow-details.js'),'utf8')+'\n'+fs.readFileSync(path.join(__dirname,'../work-mode.js'),'utf8'),scope);
 scope.installWorkModeMenu(element());
 assert.equal(nodes.find(n=>n.dataset.workMode==='goal').disabled,true);
 assert.equal(nodes.find(n=>n.dataset.workMode==='plan').disabled,false);
 assert.equal(nodes.find(n=>n.dataset.workMode==='normal').disabled,false);
 assert.ok(nodes.find(n=>n.dataset.workMode==='goal').title);
});
