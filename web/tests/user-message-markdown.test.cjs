const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { execFileSync } = require('node:child_process');

function element(tag, _className, text) {
  return {
    tag, children: text === undefined ? [] : [typeof text === 'function' ? text() : text],
    classList: { add() {}, toggle() {} },
    setAttribute() {},
    closest() { return null; },
    replaceWith(next) { this.replacement = next; },
    isConnected: true,
    append(...children) { this.children.push(...children); },
    replaceChildren(fragment) { this.children = fragment.children; },
  };
}

function visibleText(node) {
  if (typeof node === 'string') return node;
  if (node.tag === 'br') return '\n';
  return node.children.map(visibleText).join('');
}

const scope = vm.createContext({
  el: element,
  document: { createDocumentFragment: () => element('fragment'), createTextNode: text => text },
  ResizeObserver: class {},
  queueMicrotask() {},
  URL,
  state: { workbench: { project: { root: '/repo' } } },
  CLATI18n: { msg: key => key, attribute() {} },
  svgIcon: () => element('svg'),
});
const app = fs.readFileSync('web/app.js', 'utf8').replace(/\r\n/g, '\n');
const markdownSource = app.slice(app.indexOf('const INLINE_MARKDOWN'), app.indexOf('const ICON_PATHS'));
vm.runInContext(markdownSource, scope);
vm.runInContext(fs.readFileSync('web/user-message-reader.js', 'utf8'), scope);

test('user Markdown preserves entered line breaks and inline formatting', () => {
  const preview = element('div');
  scope.renderUserMessageMarkdown(preview, 'First line\r\n**Second line**\nThird line');
  assert.equal(visibleText(preview), 'First line\nSecond line\nThird line');
  assert.ok(preview.children[0].children.some(node => node.tag === 'strong' && visibleText(node) === 'Second line'));

  scope.renderUserMessageMarkdown(preview, '> First line\n> Second line');
  assert.equal(visibleText(preview), 'First line\nSecond line');
});

test('regular Markdown still folds soft line breaks into spaces', () => {
  const body = element('div');
  scope.renderMarkdown(body, 'First line\nSecond line');
  assert.equal(visibleText(body), 'First line Second line');
});

test('assistant references preserve delimiters and only turn safe destinations into actions', () => {
  const body = element('div');
  const options = {replyLinks:true, projectRoot:'/repo'};
  const source = 'summary.md. `report.md` `builtin.development` 和 docs/结果.csv；`C:\\repo\\report.md`；(http://[::1]:3000/a_b_c). ' +
    'localhost:3000?mode=clean [空格](</repo/a b.md>) [bad](%6Aavascript:alert) ' +
    'https://someone@localhost:3000 private@example.md [network](//remote/share.md)';
  scope.renderMarkdown(body, source, options);
  const parts = body.children[0].children;
  assert.deepEqual(parts.filter(node => node.tag === 'button').map(visibleText), [
    'http://[::1]:3000/a_b_c', 'localhost:3000?mode=clean',
  ]);
  assert.ok(parts.some(node => node.tag === 'code' && visibleText(node) === 'builtin.development'));
  assert.ok(parts.some(node => node.tag === 'code' && visibleText(node) === 'report.md'));
  assert.equal(scope.markdownWebAddress('localhost:3000?mode=clean'), 'http://localhost:3000?mode=clean');
  assert.match(visibleText(body), /\(http:\/\/\[::1\]:3000\/a_b_c\)\./);
  assert.match(visibleText(body), /\[bad\]\(%6Aavascript:alert\)/);
  assert.equal(scope.localMarkdownPath('c:/repo/report.md', 'C:\\Repo').absolute, 'C:\\Repo\\report.md');
  assert.equal(scope.localMarkdownPath('/repo/../private.md', '/repo'), null);
});

test('named reply links display their label, open directly, and put copy actions on the context menu', async () => {
  const opens = [], menus = [], copied = [];
  scope.CLATDesktop = { openInBrowser: url => opens.push(url) };
  scope.editorMenu = (items, source, x, y) => menus.push({items, source, x, y});
  scope.editorPopup = (items, source) => menus.push({items, source});
  scope.navigator = {clipboard:{writeText:text=>copied.push(text)}};
  scope.devAvailable = () => true;
  scope.document.getElementById = id => ({click:()=>opens.push(id)});
  scope.developmentEditor = {openFile:(path, options) => opens.push([path, options.line])};
  scope.rpc = async (method, params) => {
    assert.equal(method, 'files.resolve');
    assert.ok(params.paths.includes('docs/my report.md'));
    assert.ok(!params.paths.includes('/outside/report.md'));
    return {paths:['docs/my report.md']};
  };
  const body = element('div');
  scope.renderMarkdown(body, '[项目报告](<./docs/my report.md:32>) · [本地预览](http://localhost:3000/view_(v2)) · [项目外](/outside/report.md)', {replyLinks:true, projectRoot:'/repo'});
  await scope.verifyMarkdownFiles();
  const [file, url, external] = body.children[0].children.map(node => node?.replacement ?? node)
    .filter(node => node.tag === 'button');
  assert.equal(visibleText(file), '项目报告');
  assert.equal(file.title, '/repo/docs/my report.md:32');
  assert.equal(visibleText(url), '本地预览');
  assert.equal(url.title, 'http://localhost:3000/view_(v2)');

  await file.onclick();
  await url.onclick();
  assert.deepEqual(opens, ['files-open', ['docs/my report.md', 32], 'http://localhost:3000/view_(v2)']);
  assert.equal(menus.length, 0);
  let prevented = 0;
  const rightClick = {button:2,clientX:20,clientY:30,preventDefault:()=>prevented++};
  url.oncontextmenu(rightClick);
  assert.equal(prevented, 1);
  assert.equal(menus[0].x, 20);
  await menus[0].items.find(item=>item.key === 'copy_url').run();
  file.onkeydown({key:'F10',shiftKey:true,preventDefault:()=>prevented++});
  assert.equal(prevented, 2);
  await menus[1].items.find(item=>item.key === 'copy_absolute_path').run();
  await menus[1].items.find(item=>item.key === 'copy_path').run();
  assert.deepEqual(copied, ['http://localhost:3000/view_(v2)', '/repo/docs/my report.md', 'docs/my report.md']);
  external.oncontextmenu(rightClick);
  assert.equal(menus[2].items.find(item=>item.key === 'open_file').disabled, true);
  assert.equal(opens.length, 3);
});

test('bare reply URLs retain their click actions menu', async () => {
  const menus = [];
  scope.editorPopup = items => menus.push(items);
  const body = element('div');
  scope.renderMarkdown(body, 'http://localhost:3000/', {replyLinks:true, projectRoot:null});
  const link = body.children[0].children.find(node=>node.tag === 'button');
  link.onclick();
  assert.ok(menus[0].some(item=>item.key === 'open_in_browser'));
  assert.equal(link.oncontextmenu, undefined);
});

test('macOS system engine preserves titled links alongside implicit references', {
  skip: process.platform !== 'darwin',
}, () => {
  // WKWebView uses the OS engine; Playwright WebKit can be newer and miss its bugs.
  const paths = ['src/native_tools/ui/mod.rs:305', 'src/native_tools/ui/plugin.rs:118', 'docs/permissions.md:420'];
  const labels = paths.map(path => path.replace(/:\d+$/, ''));
  const source = '**核实结果**：' + paths.map((path, index) => `[${labels[index]}](${path})`).join('，')
    + '；`ui_apps`；http://localhost:3000/a_b_c。';
  const script = `${element.toString()}\n${visibleText.toString()}
    const el = element;
    const document = {createDocumentFragment:()=>element('fragment'), createTextNode:text=>text};
    const state = {workbench:{project:{root:'/repo'}}};
    const CLATI18n = {msg:key=>key, attribute(){}};
    const queueMicrotask = () => {};
    const svgIcon = () => element('svg');
    ${markdownSource}
    const body = element('div');
    renderMarkdown(body, ${JSON.stringify(source)}, {replyLinks:true, projectRoot:'/repo'});
    JSON.stringify({text:visibleText(body), files:[...pendingMarkdownFiles.values()].map(item=>item.label)});`;
  const result = JSON.parse(execFileSync('/usr/bin/osascript', ['-l', 'JavaScript', '-'], {
    input: script, encoding: 'utf8', timeout: 10000,
  }));
  assert.equal(result.text, '核实结果：' + labels.join('，') + '；ui_apps；http://localhost:3000/a_b_c。');
  assert.deepEqual(result.files, labels);
});
