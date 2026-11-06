const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');

const source=fs.readFileSync('web/app.js','utf8').replace(/\r\n/g,'\n');
const start=source.indexOf('function localMarkdownPath('), end=source.indexOf('\nfunction localMarkdownLink(',start);
assert.ok(start>=0 && end>start);
const scope={URL};vm.createContext(scope);vm.runInContext(source.slice(start,end),scope);

test('local links resolve within the current project and reject traversal or remote URLs',()=>{
  for (const [value,root,expected] of [
    ['docs/readme.md','/repo','docs/readme.md'],
    ['readme.md:12','/repo','readme.md'],
    ['/repo/docs/a.md:12','/repo','docs/a.md'],
    ['file:///repo/a.md#L2','/repo','a.md'],
    ['C:\\repo\\a.md','C:\\repo','a.md'],
    ['../secret','/repo',null], ['%2e%2e/secret','/repo',null],
    ['/repo-other/a','/repo',null], ['file://remote/repo/a','/repo',null],
    ['https://example.com','/repo',null],
  ]) assert.equal(scope.localMarkdownPath(value,root)?.relative??null,expected,value);
  assert.equal(scope.localMarkdownPath('/repo-other/a','/repo').absolute,'/repo-other/a');
  assert.equal(scope.localMarkdownPath('file:///outside/file.md','/repo').relative,null);
});
