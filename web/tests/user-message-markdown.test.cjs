const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function element(tag, _className, text) {
  return {
    tag, children: text === undefined ? [] : [typeof text === 'function' ? text() : text],
    classList: { add() {} },
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
  CLATI18n: { msg: key => key },
});
const app = fs.readFileSync('web/app.js', 'utf8');
vm.runInContext(app.slice(app.indexOf('const INLINE_MARKDOWN'), app.indexOf('const ICON_PATHS')), scope);
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
