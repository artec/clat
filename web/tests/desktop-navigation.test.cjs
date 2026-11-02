const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

function fixture(platform='MacIntel',native='macos'){
  const window=new EventTarget(),document=new EventTarget(),steps=[];
  class CustomEvent extends Event {constructor(type,options={}){super(type,options);this.detail=options.detail;}}
  let modal=false;
  // Desktop embeds an open, non-modal About dialog in its hidden settings page.
  // Both selectors see a true modal; only [open] also sees the embedded panel.
  document.querySelector=selector=>modal||selector==='dialog[open]:not(.shell-surface)'?{}:null;
  window.__CLAT_NATIVE_FRAME__=native;
  const scope={window,document,navigator:{platform},CustomEvent};
  vm.createContext(scope);
  const source=fs.readFileSync('web/project-host.js','utf8').replace(/\r\n/g,'\n');
  vm.runInContext(source.slice(0,source.indexOf('/* One application document')),scope);
  scope.installHistoryShortcuts({go:delta=>steps.push(delta)});
  const rust=fs.readFileSync(process.env.CLAT_NAVIGATION_FRAME_SOURCE||'desktop/src/window_frame.rs','utf8').replace(/\r\n/g,'\n');
  const body=rust.match(/fn navigation_script\([^]*?\n\}/)?.[0]||'';
  function menu(id){
    const match=body.match(new RegExp('"'+id+'"\\s*=>\\s*(?:\\{\\s*)?Some\\(\\s*"([^"\\n]+)"'));
    assert.ok(match,'Native navigation must dispatch directly to the renderer without an event subscription');
    vm.runInContext(JSON.parse('"'+match[1]+'"'),scope);
  }
  function key(code,modifiers={}){
    const event=new Event('keydown',{cancelable:true});
    Object.assign(event,{code,...modifiers});document.dispatchEvent(event);return event;
  }
  return {window,scope,steps,menu,key,setModal:value=>{modal=value;}};
}

test('native Back and Forward work with the embedded non-modal Desktop settings panel',()=>{
  const view=fixture();
  assert.equal(view.window.__TAURI__,undefined);
  view.menu('desktop-back');view.menu('desktop-forward');
  assert.deepEqual(view.steps,[-1,1]);
  view.key('BracketLeft',{metaKey:true});
  assert.deepEqual(view.steps,[-1,1],'native macOS accelerator must not also step through the DOM key handler');
});

test('native menu navigation retains modal blocking and rejects invalid directions',()=>{
  const view=fixture();view.setModal(true);
  view.menu('desktop-back');view.menu('desktop-forward');
  assert.deepEqual(view.steps,[]);
  view.setModal(false);
  for(const detail of [0,2,'-1',null])view.window.dispatchEvent(new view.scope.CustomEvent('clat:navigate',{detail}));
  assert.deepEqual(view.steps,[]);
  view.menu('desktop-back');assert.deepEqual(view.steps,[-1]);
});

test('browser macOS and Windows keyboard shortcuts still navigate exactly once',()=>{
  for(const [platform,native,modifier]of [['MacIntel',null,'metaKey'],['Win32','windows','ctrlKey']]){
    const view=fixture(platform,native);
    assert.equal(view.key('BracketLeft',{[modifier]:true}).defaultPrevented,true);
    assert.equal(view.key('BracketRight',{[modifier]:true}).defaultPrevented,true);
    assert.deepEqual(view.steps,[-1,1]);
    view.key('BracketLeft',{[modifier]:true,shiftKey:true});
    assert.deepEqual(view.steps,[-1,1]);
  }
});
