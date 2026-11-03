const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

function fixture(saved={}){
  let repoHeight=100,total=800,persisted,resize;
  const element=rect=>({style:{removeProperty(name){delete this[name];}},getBoundingClientRect:rect,
    setAttribute(name,value){this[name]=value;},setPointerCapture(){}});
  const summary=element(()=>({height:32})),repositories=element(()=>({height:repositories.open?repoHeight:32}));
  const graph=element(()=>({}));graph.open=true;graph.querySelector=()=>summary;
  const split=element(()=>({height:split.hidden?0:5})),bottom=element(()=>({bottom:total}));
  const sidebar=element(()=>({bottom:total})),changes=element(()=>({top:0}));
  changes.parentElement=sidebar;sidebar.children=[changes,split,bottom];
  const node={dataset:{},style:{setProperty(){}},querySelector:()=>bottom};
  const scope={localStorage:{getItem:()=>JSON.stringify(saved),setItem:(_,value)=>{persisted=JSON.parse(value);}},
    ResizeObserver:class{constructor(callback){resize=callback;}observe(){}}};
  vm.createContext(scope);
  vm.runInContext(fs.readFileSync('web/development/git-layout.js','utf8').replace(/\r\n/g,'\n'),scope);
  scope.gitLayoutPreference(node,changes,split,repositories,graph);
  const toggle=(target,open)=>{target.open=open;target.ontoggle();};
  return {repositories,graph,split,toggle,resize:()=>resize(),saved:()=>persisted,
    setRepoHeight:value=>{repoHeight=value;resize();},setAvailable:value=>{total=value;resize();}};
}

test('repository expansion never enables resizing a collapsed graph; reopening restores its preference',()=>{
  const ui=fixture({graph:false,repositories:false,ratio:55});
  assert.equal(ui.split.hidden,true);
  ui.toggle(ui.repositories,true);
  assert.equal(ui.split.hidden,true);
  assert.equal(ui.graph.style.height,undefined);
  ui.toggle(ui.graph,true);
  const height=ui.graph.style.height;
  assert.equal(ui.split.hidden,false);
  ui.toggle(ui.repositories,false);
  assert.equal(ui.graph.style.height,height);
  ui.toggle(ui.graph,false);ui.toggle(ui.graph,true);
  assert.equal(ui.graph.style.height,height);
  assert.equal(ui.saved().ratio,55);
});

test('repository content changes preserve graph size unless the remaining space requires a smaller graph',()=>{
  const ui=fixture({graph:true,repositories:true});
  const height=parseFloat(ui.graph.style.height);
  ui.setRepoHeight(200);assert.equal(parseFloat(ui.graph.style.height),height);
  ui.setRepoHeight(600);assert.ok(parseFloat(ui.graph.style.height)<height);
  ui.setRepoHeight(100);assert.equal(parseFloat(ui.graph.style.height),height);
  ui.setAvailable(1000);assert.ok(parseFloat(ui.graph.style.height)>height);
  assert.equal(ui.saved().ratio,60,'automatic fitting must not replace the user preference');
});

test('dragging the group boundary resizes only the graph and preserves the chosen size across repository toggles',()=>{
  const ui=fixture({graph:true,repositories:true});
  const before=parseFloat(ui.graph.style.height);
  ui.split.onpointerdown({pointerId:1});
  ui.split.onpointermove({clientY:300});ui.split.onpointerup();
  const after=parseFloat(ui.graph.style.height);
  assert.ok(after>before);
  ui.toggle(ui.repositories,false);assert.equal(parseFloat(ui.graph.style.height),after);
  ui.toggle(ui.repositories,true);assert.equal(parseFloat(ui.graph.style.height),after);
  let prevented=false;ui.split.onkeydown({key:'ArrowDown',preventDefault(){prevented=true;}});
  assert.equal(prevented,true);assert.ok(parseFloat(ui.graph.style.height)<after);
});
