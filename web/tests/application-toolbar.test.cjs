const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');

function makeNode(tag='div',cls=''){
  return {
    tag,className:cls,children:[],dataset:{},attrs:{},hidden:false,disabled:false,
    append(...items){this.children.push(...items);},
    replaceChildren(...items){this.children=[...items];},
    setAttribute(name,value){this.attrs[name]=String(value);},
    getAttribute(name){return this.attrs[name];},
    querySelector(sel){return sel==='svg'?this.children.find(c=>c.tag==='svg')||null:null;},
    replaceWith(next){this.replacedBy=next;},
  };
}

function toolbarFixture(){
  const svgIcon=name=>({tag:'svg',icon:name,replaceWith(next){this.icon=next.icon;}});
  const scope={
    WORKSPACE_TOOLS:[
      {key:'files',icon:'folder',id:'file-browser'},
      {key:'changes',icon:'branch',id:'workspace-review'},
      {key:'turn_review',icon:'trace',id:'turn-review'},
    ],
    state:{sessionId:'sess-1',switching:false,sessions:[{id:'sess-1'}]},
    dom:{'inspector-toggle':{click(){}}},
    document:{getElementById:()=>({classList:{contains:()=>true}})},
    CLATI18n:{msg:k=>k,attribute:(node,key,read)=>{node.attrs[key]=read();}},
    builtinReady:()=>true,
    builtinUnavailableReason:()=>'',
    helpText:k=>'help.'+k,
    experienceText:k=>'exp.'+k,
    openHelp(){},
    setConversationFindOpen(){},
    toggleSessionMenu(){},
    activeViewContext:()=>({id:''}),
    updateRunState(){},
    svgIcon,
    el:(tag,cls)=>makeNode(tag,cls),
    navigationIcon:(label,icon)=>{const n=makeNode('button','icon-button');n.icon=icon;n.label=label;n.append(svgIcon(icon));return n;},
    shellButton:(_cls,icon,label)=>{const n=makeNode('button','shell-button');n.icon=icon;n.label=label;return n;},
  };
  vm.createContext(scope);
  // 从真实源码加载 viewAction、conversationViewActions 与 renderApplicationActions
  const viewSrc=fs.readFileSync('web/view-context.js','utf8').replace(/\r\n/g,'\n');
  for(const sig of ['const viewText=','function viewAction(','function conversationViewActions(']){
    const start=viewSrc.indexOf(sig);
    const end=sig.startsWith('const ')?viewSrc.indexOf('\n',start):viewSrc.indexOf('\n}',start)+2;
    vm.runInContext(viewSrc.slice(start,end),scope);
  }
  const barSrc=fs.readFileSync('web/application-toolbar.js','utf8').replace(/\r\n/g,'\n');
  const start=barSrc.indexOf('function renderApplicationActions(');
  vm.runInContext(barSrc.slice(start,barSrc.indexOf('\n}',start)+2),scope);
  const more=makeNode('button','icon-button');
  more.append(svgIcon('tools'));
  const chrome={actions:makeNode('nav'),utility:makeNode('div'),menu:makeNode('div'),more,signature:'',closeCalled:0,close(){this.closeCalled++;}};
  const shell={tools:{current:()=>null,open(){}}};
  return {scope,chrome,shell};
}

test('对话页按四段秩序渲染：查找 → 工具菜单 → 此页帮助 → 工作区右栏开关',()=>{
  const {scope,chrome,shell}=toolbarFixture();
  const view={id:'conversation',workspace:true,actions:()=>scope.conversationViewActions(shell)};
  scope.renderApplicationActions(chrome,view,shell);

  assert.deepEqual(chrome.actions.children.map(n=>n.dataset.action),['find']);
  assert.equal(chrome.more.hidden,false);
  assert.equal(chrome.more.dataset.kind,'tools');
  assert.equal(chrome.more.attrs['aria-label'],'tui.tools');
  // 菜单内先放会话操作与分隔线，再放工作区工具组
  assert.equal(chrome.menu.children[0].dataset.action,'session');
  assert.equal(chrome.menu.children[1].tag,'hr');
  assert.deepEqual(chrome.menu.children[2].children.map(n=>n.dataset.action),['tool-files','tool-changes','tool-turn_review']);
  // 尾槽中「此页帮助」在前，右栏开关固定在最右侧且使用右栏镜像图标
  assert.deepEqual(chrome.utility.children.map(n=>n.dataset.action),['page-help','inspector']);
  assert.equal(chrome.utility.children[1].icon,'panel-right');
});

test('设置页无页面操作且非工作区时，自动隐藏空菜单并仅在尾槽保留此页帮助',()=>{
  const {scope,chrome,shell}=toolbarFixture();
  const view={id:'settings-dialog',workspace:false,actions:()=>[]};
  scope.renderApplicationActions(chrome,view,shell);

  assert.deepEqual(chrome.actions.children,[]);
  assert.equal(chrome.menu.children.length,0);
  assert.equal(chrome.more.hidden,true);
  assert.equal(chrome.more.dataset.kind,'more');
  assert.deepEqual(chrome.utility.children.map(n=>n.dataset.action),['page-help']);
});

test('非工作区页存在次操作时显示三点更多菜单，且内联主操作最多保留 3 枚',()=>{
  const {scope,chrome,shell}=toolbarFixture();
  const view={
    id:'terminal-page',
    workspace:false,
    actions:()=>[
      scope.viewAction('create','plus',()=>'new',()=>{},true),
      scope.viewAction('refresh','refresh',()=>'refresh',()=>{},true),
      scope.viewAction('settings','settings',()=>'settings',()=>{},true),
      scope.viewAction('overflow','info',()=>'overflow',()=>{},true),
      scope.viewAction('find','search',()=>'find',()=>{},false),
    ],
  };
  scope.renderApplicationActions(chrome,view,shell);

  assert.deepEqual(chrome.actions.children.map(n=>n.dataset.action),['create','refresh','settings']);
  assert.equal(chrome.more.hidden,false);
  assert.equal(chrome.more.dataset.kind,'more');
  assert.equal(chrome.more.attrs['aria-label'],'pages.actions');
  assert.deepEqual(chrome.menu.children.map(n=>n.dataset.action),['overflow','find']);
  assert.deepEqual(chrome.utility.children.map(n=>n.dataset.action),['page-help']);
});

test('帮助页不重复注入此页帮助按钮，且空菜单保持隐藏',()=>{
  const {scope,chrome,shell}=toolbarFixture();
  const view={
    id:'help-dialog',
    workspace:false,
    actions:()=>[scope.viewAction('help-source','return',()=>'back',()=>{},true)],
  };
  scope.renderApplicationActions(chrome,view,shell);

  assert.deepEqual(chrome.actions.children.map(n=>n.dataset.action),['help-source']);
  assert.equal(chrome.more.hidden,true);
  assert.deepEqual(chrome.utility.children,[]);
});

test('各页主次操作顺序、语义图标及三语标签键保持一致',()=>{
  const appSrc=fs.readFileSync('web/app.js','utf8');
  const iconNames=new Set([...appSrc.matchAll(/^\s*(?:'([^']+)'|"([^"]+)"|([a-z0-9_-]+)):\s*\[/gm)].map(m=>m[1]||m[2]||m[3]));
  for(const required of ['panel-right','cloud-download','import','export','external-link','return','telegram']){
    assert.ok(iconNames.has(required),`web/app.js 缺少图标定义 ${required}`);
  }
  const gitSrc=fs.readFileSync('web/development/git.js','utf8');
  const termSrc=fs.readFileSync('web/development/terminal-view.js','utf8');
  const histSrc=fs.readFileSync('web/history-query.js','utf8');
  const extSrc=fs.readFileSync('web/extension-view.js','utf8');
  const helpSrc=fs.readFileSync('web/help-center.js','utf8');
  // Git 主操作：branches → refresh → settings；次操作 fetch/pull/push 区分 cloud-download/download/upload
  assert.match(gitSrc,/viewAction\('branches','branch'[\s\S]*?viewAction\('refresh','refresh'[\s\S]*?viewAction\('settings','settings',\(\)=>devText\('git_settings'\),\(\)=>openSettings\('git'\),true\)/);
  assert.match(gitSrc,/\['fetch','cloud-download'\],\['pull','download'\],\['push','upload'\]/);
  // 终端主操作：create → refresh → settings；find 归入次操作菜单
  assert.match(termSrc,/viewAction\('create','plus'[\s\S]*?viewAction\('refresh','refresh'[\s\S]*?viewAction\('settings','settings'[\s\S]*?viewAction\('find','search',\(\)=>devText\('find'\),\(\)=>actions\.find\(\),false,actions\.hasSelection\)/);
  // 历史归档导入/导出使用 import/export，扩展官网使用 external-link，帮助返回来源页使用 return
  assert.match(histSrc,/viewAction\('archive-import','import'/);
  assert.match(histSrc,/viewAction\('archive-export','export'/);
  assert.match(extSrc,/viewAction\('extension-website','external-link'/);
  assert.match(helpSrc,/viewAction\('help-source','return'/);
  // 核对新增三语键 dev.git_settings 齐全
  for(const file of ['locales/en.json','locales/zh-Hans.json','locales/zh-Hant.json']){
    const catalog=JSON.parse(fs.readFileSync(file,'utf8'));
    assert.ok(catalog['dev.git_settings']&&catalog['dev.git_settings'].trim().length>0,`${file} 缺少 dev.git_settings`);
  }
});
