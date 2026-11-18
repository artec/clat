const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const scope={};vm.runInNewContext(fs.readFileSync('web/rail-allowance.js','utf8').replace(/\r\n/g,'\n'),scope);
const project=scope.projectRailAllowance,compact=scope.compactRailMoney;
function columns(text){let count=0;for(const char of String(text))count+='万萬亿億'.includes(char)?2:1;return count;}
test('ring uses remaining percentages from the supplied current allowance only',()=>{
 const a={status:'available',items:[{kind:'quota',unit:'%',value:'62',reset_at:123},{kind:'quota',unit:'%',value:'19'}]};
 assert.equal(project(a).kind,'quota');assert.equal(project(a).remaining,19);assert.equal(project(a).items[0].reset_at,123);
 assert.equal(project({status:'available',items:[{kind:'quota',unit:'%',value:'87'}]}).remaining,87);
});
test('zero remaining is a real quota while invalid readings are absent',()=>{
 assert.equal(project({status:'available',items:[{kind:'quota',unit:'%',value:'0'}]}).remaining,0);
 for(const value of ['','NaN','-1','101'])assert.equal(project({status:'available',items:[{kind:'quota',unit:'%',value}]}),null);
 assert.equal(project({status:'available',items:[{kind:'money',unit:'CNY',value:'-1'}]}),null);
 assert.equal(project({status:'available',items:[{kind:'money',unit:'CNY',value:''}]}),null);
});
test('unsupported loading and failed routes never retain another model percentage',()=>{
 for(const status of ['loading','unsupported','failed','unavailable'])assert.equal(project({status,items:[{kind:'quota',unit:'%',value:'87'}]}),null);
 assert.equal(project(null),null);
 assert.equal(project({status:'failed',items:[{kind:'money',unit:'CNY',value:'110'}]}),null);
});
test('a wallet balance is listed without becoming a percentage ring',()=>{
 const value=project({status:'available',updated_at:9,items:[{kind:'money',unit:'CNY',value:'110.00'},{kind:'money',unit:'USD',value:'1'},{kind:'money',unit:'EUR',value:'nope'}]});
 assert.equal(value.kind,'money');assert.equal(value.remaining,undefined);assert.equal(value.updated,9);
 assert.deepEqual(value.items.map(item=>item.unit),['CNY','USD']);
 const mixed=project({status:'available',items:[{kind:'money',unit:'CNY',value:'10'},{kind:'quota',unit:'%',value:'40'}]});
 assert.equal(mixed.kind,'quota');assert.equal(mixed.remaining,40);assert.equal(mixed.items.length,1);
 assert.equal(project({status:'available',items:[{kind:'money',unit:'CNY',value:'0.00'}]}).items[0].value,'0.00');
});
test('compact balances stay within three characters and omit the currency sign',()=>{
 const cases=[
  ['0.00','CNY','zh-Hans','0'],
  ['12.50','CNY','zh-Hans','13'],
  ['19','CNY','zh-Hans','19'],
  ['110.00','CNY','en','110'],
  ['1234','CNY','zh-Hans','1k'],
  ['50','EUR','en','50'],
  ['0','USD','en','0'],
  ['9.5','CNY','en','9.5'],
  ['10000','CNY','zh-Hans','10k'],
  ['12345','CNY','zh-Hans','12k'],
  ['12345','CNY','zh-Hant','12k'],
  ['12345','USD','en','12k'],
  ['1000000','USD','en','1M'],
  ['1e20','CNY','zh-Hans','99B'],
  ['1e20','USD','en','99B'],
 ];
 for(const [value,unit,locale,expected] of cases){
  const text=compact(value,unit,locale);
  assert.equal(text,expected);
  assert.ok(columns(text)<=3,text);
  assert.equal(/[¥$￥]/.test(text),false);
 }
 for(const locale of ['en','zh-Hans','zh-Hant']){
  for(const value of ['0','0.04','12.5','19','110','999','1000','12345','99999','1000000','9999999','1e8','1e11','1e20']){
   const text=compact(value,'CNY',locale);
   assert.ok(text&&columns(text)<=3&&!/[¥$￥]/.test(text),`${locale} ${value} -> ${text}`);
  }
 }
});
test('quota windows present 720 hours as a monthly window and keep weeks and hours',()=>{
 const t=(key,args)=>key+(args&&args.count!==undefined?`(${args.count})`:'');
 const label=hours=>scope.railWindowLabel(hours,t);
 assert.equal(label(720),'window_month');
 assert.equal(label(1440),'window_months(2)');
 assert.equal(label(3600),'window_months(5)');
 assert.equal(label(168),'window_week');
 assert.equal(label(336),'window_weeks(2)');
 assert.equal(label(5),'window_hours(5)');
 assert.equal(label(100),'window_hours(100)');
 assert.equal(label(0),label(undefined),label(NaN)?undefined:'remaining');
 assert.equal(label(0),'remaining');
});
test('reset countdowns compose days, hours and minutes instead of raw minutes',()=>{
 const t=(key,args)=>args&&args.count!==undefined?`${args.count} ${key}`:key;
 const countdown=minutes=>scope.railResetCountdown(minutes,t);
 // 6371 分钟（OC-Go 周窗实测值）→ 4 天 10 小时 11 分钟。
 assert.equal(countdown(6371),'4 days 10 hours 11 minutes');
 // 42965 分钟（月窗实测值）→ 29 天 20 小时 5 分钟。
 assert.equal(countdown(42965),'29 days 20 hours 5 minutes');
 // 零单位跳过：整天/整小时/整组合。
 assert.equal(countdown(1440),'1 days');
 assert.equal(countdown(180),'3 hours');
 assert.equal(countdown(70),'1 hours 10 minutes');
 // 不足一小时保留纯分钟；到期转 reset_due。
 assert.equal(countdown(42),'42 minutes');
 assert.equal(countdown(0),'reset_due');
 assert.equal(countdown(-5),'reset_due');
});
