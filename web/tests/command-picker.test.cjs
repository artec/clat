const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

test('the skill picker offers skills with unrelated names and filters their arguments',()=>{
  const prompt={value:''};
  const scope=vm.createContext({dom:{prompt}});
  vm.runInContext(fs.readFileSync(process.env.CLAT_COMMAND_PICKER_SOURCE||'web/command-picker.js','utf8'),scope);
  const catalog={
    commands:[{name:'skill',aliases:['skills']},{name:'mcp'}],
    skills:{entries:[{name:'clat-upgrade-check',description:'检查升级'},{name:'audit'}]},
  };
  const choices=text=>{
    prompt.value=text;
    return Array.from(scope.catalogCandidates(catalog,scope.commandQuery()),row=>row.value);
  };
  assert.deepEqual(choices('/skill'),['/skill ','/skill clat-upgrade-check ','/skill audit ']);
  assert.deepEqual(choices('/skills'),choices('/skill'));
  assert.deepEqual(choices('/skill '),['/skill clat-upgrade-check ','/skill audit ']);
  for(const text of ['/skill upgrade','/skills 检查'])assert.deepEqual(choices(text),['/skill clat-upgrade-check ']);
  prompt.value='/skill manage list';
  assert.equal(scope.commandQuery(),null);
  assert.deepEqual(choices('/mcp'),['/mcp ']);
});
