const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
// terminal.js only declares functions at top level, so a bare sandbox can host
// the pure input-policy decision without any DOM stubs.
const ctx = vm.createContext({});
vm.runInContext(fs.readFileSync('web/development/terminal.js', 'utf8'), ctx);
const policy = ctx.terminalInputPolicy;

test('terminalInputPolicy retries transient failures with exponential backoff', () => {
  // Field-wise assertions: the result object lives in the vm realm, so
  // deepStrictEqual would compare prototypes across realms.
  assert.equal(policy({ code: 'busy' }, 1).action, 'retry');
  assert.equal(policy({ code: 'busy' }, 1).delay, 200);
  assert.equal(policy({ code: 'busy' }, 2).delay, 400);
  assert.equal(policy({ code: 'busy' }, 3).delay, 800);
});

test('terminalInputPolicy treats timeouts and network failures as transient', () => {
  assert.equal(policy({ name: 'AbortError' }, 1).action, 'retry');
  assert.equal(policy(new TypeError('fetch failed'), 1).action, 'retry');
  assert.equal(policy({}, 1).action, 'retry');
});

test('terminalInputPolicy demotes on definitive terminal errors', () => {
  const definitive = ['terminal_observer', 'terminal_sequence', 'terminal_ended',
    'terminal_missing', 'terminal_input_large', 'host_closed'];
  for (const code of definitive) {
    assert.equal(policy({ code }, 1).action, 'demote', code);
  }
  assert.equal(policy({ code: 'busy', authentication: 'invalid_credentials' }, 1).action, 'demote');
});

test('terminalInputPolicy demotes after the retry budget is exhausted', () => {
  assert.equal(policy({ code: 'busy' }, 4).action, 'demote');
  assert.equal(policy({ name: 'AbortError' }, 5).action, 'demote');
});

const terminalView=fs.readFileSync('web/development/terminal-view.js','utf8');

test('terminal directory renders and preserves the project menu with the shared organization API',()=>{
  const node=()=>({
    children:[],dataset:{},classList:{add(){},toggle(){}},setAttribute(){},
    append(...items){this.children.push(...items);},
    replaceChildren(){this.children=[];},
  });
  const bound=[];
  const menu=node();
  const scope=vm.createContext({
    el:node,svgIcon:node,navigationIcon:node,
    CLATI18n:{attribute(){}},
    context:{organization:{
      menuButton(project,items){bound.push({project,items});return menu;},
      bind(_header,project,button){assert.equal(button,menu);assert.equal(project,bound.at(-1).project);},
    }},
  });
  vm.runInContext(terminalView,scope);
  const project={root:'/project',title:'Project',pinned:true,terminals:[{id:'pty',name:'Shell'}]};
  const ui={list:node(),query:{value:''}};
  const actions={};
  scope.renderTerminalDirectory(ui,[project],null,actions,new Map());
  assert.equal(ui.list.children.length,1);
  assert.equal(bound[0].project,project);
  let cleaned;
  actions.cleanup=value=>{cleaned=value;};
  bound[0].items[0][2]();
  assert.equal(cleaned,project);
});

test('terminal notices keep raw errors in collapsed details and clear only the recovered error',()=>{
  const scope=vm.createContext({
    devText:key=>'localized:'+key,
    devStatus:(element,key)=>{element.textContent='localized:'+key;},
    CLATI18n:{
      bind:(element,property,value)=>{element[property]=value();},
      present:(presentation,fallback)=>presentation?'localized:'+presentation.key:fallback,
    },
  });
  vm.runInContext(terminalView,scope);
  const status={classList:new Set()};
  status.classList.remove=status.classList.delete;
  const diagnostic={hidden:true,open:false},body={textContent:''};
  const notice=scope.createTerminalStatus(status,diagnostic,body);
  const clientError=new TypeError('Internal diagnostic text');
  notice.report(clientError);
  assert.equal(status.textContent,'localized:failed');
  assert.equal(diagnostic.open,false);
  assert.equal(diagnostic.hidden,false);
  assert.equal(body.textContent,clientError.message);
  const hostError={presentation:{key:'dev.observer'},technical:'terminal_observer'};
  notice.report(hostError);
  notice.clear(clientError);
  assert.equal(status.textContent,'localized:dev.observer');
  assert.equal(body.textContent,'terminal_observer');
  notice.clear(hostError);
  assert.equal(status.textContent,'');
  assert.equal(diagnostic.hidden,true);
  assert.equal(body.textContent,'');
});
