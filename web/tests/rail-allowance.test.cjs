const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const scope={};vm.runInNewContext(fs.readFileSync('web/rail-allowance.js','utf8').replace(/\r\n/g,'\n'),scope);
const project=scope.projectRailAllowance;
test('ring uses remaining percentages from the supplied current allowance only',()=>{
 const a={status:'available',items:[{kind:'quota',unit:'%',value:'62',reset_at:123},{kind:'quota',unit:'%',value:'19'}]};
 assert.equal(project(a).remaining,19);assert.equal(project(a).items[0].reset_at,123);
 assert.equal(project({status:'available',items:[{kind:'quota',unit:'%',value:'87'}]}).remaining,87);
});
test('zero remaining is a real value while money and invalid readings are not quota',()=>{
 assert.equal(project({status:'available',items:[{kind:'quota',unit:'%',value:'0'}]}).remaining,0);
 for(const item of [{kind:'money',unit:'CNY',value:'50'},...['','NaN','-1','101'].map(value=>({kind:'quota',unit:'%',value}))])assert.equal(project({status:'available',items:[item]}),null);
});
test('unsupported loading and failed routes never retain another model percentage',()=>{
 for(const status of ['loading','unsupported','failed','unavailable'])assert.equal(project({status,items:[{kind:'quota',unit:'%',value:'87'}]}),null);
 assert.equal(project(null),null);
});
