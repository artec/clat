const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');

const source=fs.readFileSync('web/app.js','utf8').replace(/\r\n/g,'\n');
// 真实管线是 markdownReference → localMarkdownTarget（锚点剥落/编码/
// scheme 拒绝）→ localMarkdownPath（项目内解析）——两段一起提取，
// 表格输入先过 target 再进 path，镜像产品路径。
const start=source.indexOf('function localMarkdownTarget('), end=source.indexOf('\nfunction localMarkdownLink(',start);
assert.ok(start>=0 && end>start);
const scope={URL};vm.createContext(scope);vm.runInContext(source.slice(start,end),scope);
const resolve=(value,root)=>{const target=scope.localMarkdownTarget(value,true);return target?scope.localMarkdownPath(target.path,root):null;};

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
  ]) assert.equal(resolve(value,root)?.relative??null,expected,value);
  assert.equal(resolve('/repo-other/a','/repo').absolute,'/repo-other/a');
  assert.equal(resolve('file:///outside/file.md','/repo').relative,null);
});

test('line hints stay separate from the fenced file path',()=>{
  for (const [value, expected, line] of [
    ['./docs/a.md:12', './docs/a.md', 12],
    ['file:///repo/a.md#L2', '/repo/a.md', 2],
    ['C:\\repo\\a.md:8:3', 'C:/repo/a.md', 8],
    ['./a%23b.md#L17-L20', './a#b.md', 17],
    ['./README.md', './README.md', null],
  ]) {
    const target = scope.localMarkdownTarget(value, true);
    assert.equal(target.path, expected);
    assert.equal(target.line, line);
  }
});
