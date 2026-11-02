const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const settle=()=>new Promise(setImmediate);

function fixture(){
  const window=new EventTarget(),dataset={},requests=[],timers=new Map();let next=0;
  const scope={window,document:{documentElement:{dataset}},
    invoke:action=>{assert.equal(action,'info');return new Promise((resolve,reject)=>requests.push({resolve,reject}));},
    setTimeout:callback=>{timers.set(++next,callback);return next;},clearTimeout:id=>timers.delete(id)};
  const source=fs.readFileSync('desktop/ui/window-bar.js','utf8').replace(/\r\n/g,'\n');
  vm.createContext(scope);
  vm.runInContext(source.slice(source.indexOf('  function trackFullscreen()'),source.indexOf('  function createControls(')),scope);
  scope.trackFullscreen();
  const flush=()=>{for(const [id,callback]of timers){timers.delete(id);callback();}};
  const nativeFullscreen=detail=>{
    const event=new Event('clat-desktop-fullscreen');event.detail=detail;window.dispatchEvent(event);
  };
  return {window,dataset,requests,flush,nativeFullscreen};
}

test('native fullscreen frame state follows entry, exit and reload without DOM fullscreen',async()=>{
  const view=fixture();
  view.requests.shift().resolve({fullscreen:true});await settle();
  assert.equal(view.dataset.nativeFullscreen,'true');
  view.nativeFullscreen(false);
  assert.equal(view.dataset.nativeFullscreen,'false');
  view.window.dispatchEvent(new Event('pageshow'));view.flush();
  view.requests.shift().resolve({fullscreen:true});await settle();
  assert.equal(view.dataset.nativeFullscreen,'true');
});

test('native fullscreen completion overrides early state queries without a click or focus event',async()=>{
  const view=fixture();
  view.requests.shift().resolve({fullscreen:true});await settle();
  // The early DOM resize still sees fullscreen; no further DOM event follows.
  view.window.dispatchEvent(new Event('resize'));
  view.nativeFullscreen(false);
  assert.equal(view.dataset.nativeFullscreen,'false');
  view.nativeFullscreen(true);
  assert.equal(view.dataset.nativeFullscreen,'true');
  for(const invalid of [null,'false',0])view.nativeFullscreen(invalid);
  assert.equal(view.dataset.nativeFullscreen,'true');
});

test('native frame events supersede stale replies and pending focus refreshes',async()=>{
  const view=fixture(),initial=view.requests.shift();
  view.window.dispatchEvent(new Event('focus'));
  view.nativeFullscreen(true);view.flush();
  assert.equal(view.requests.length,0);
  initial.resolve({fullscreen:false});await settle();
  assert.equal(view.dataset.nativeFullscreen,'true');
  view.window.dispatchEvent(new Event('focus'));view.flush();
  view.requests.shift().reject(new Error('unavailable'));await settle();
  assert.equal(view.dataset.nativeFullscreen,'true');
  view.window.dispatchEvent(new Event('focus'));view.flush();
  view.window.dispatchEvent(new Event('pagehide'));
  view.requests.shift().resolve({fullscreen:false});await settle();
  assert.equal(view.dataset.nativeFullscreen,'true');
});
