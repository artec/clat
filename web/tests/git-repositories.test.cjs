const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

function fixture(root){
  const rows=[],scope={state:{workbench:{project:{root}}},
    directory:[{id:'project',path:'',branch:'main',available:true,dirty:true},
      {id:'pages',path:'pages',branch:'pages',available:true,dirty:false}],
    repository:'project',devText:key=>key,choose(){},navigationRow:value=>value,
    ui:{repoName:{textContent:'clat'},repos:{replaceChildren(){rows.length=0;},append(row){rows.push(row);}}}};
  const source=fs.readFileSync('web/development/git.js','utf8').replace(/\r\n/g,'\n');
  vm.createContext(scope);vm.runInContext(source+'\n'+source.match(/^  function renderRepositories\(\).*$/m)[0],scope);
  return {scope,render(){scope.renderRepositories();return rows.map(row=>({name:row.title,meta:row.meta(),selected:row.selected}));}};
}

test('selecting a sub-repository never replaces the root repository name or its branch',()=>{
  const view=fixture('/work/clat');
  assert.deepEqual(view.render(),[
    {name:'clat',meta:'main · git_modified',selected:true},
    {name:'pages',meta:'pages · clean',selected:false},
  ]);
  view.scope.repository='pages';view.scope.ui.repoName.textContent='pages';
  assert.deepEqual(view.render(),[
    {name:'clat',meta:'main · git_modified',selected:false},
    {name:'pages',meta:'pages · clean',selected:true},
  ]);
});

test('root names follow the owning project path, including Windows paths and trailing separators',()=>{
  for(const root of ['/work/clat/','C:\\work\\clat\\']){
    const view=fixture(root);view.scope.ui.repoName.textContent='other-repository';
    assert.equal(view.render()[0].name,'clat');
    view.scope.state.workbench.project.root='/work/another-project';
    assert.equal(view.render()[0].name,'another-project');
    assert.equal(view.render()[1].name,'pages');
  }
});
