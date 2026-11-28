const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
const context=vm.createContext({
  el(tag, cls, text) {
    return {tag, cls, text, children: [], append(...nodes) { this.children.push(...nodes); }};
  },
  pageText: key => key,
  svgIcon: name => ({tag: 'svg', name, children: []}),
  pageAction(key, action) {
    const button = context.el('button', 'secondary', () => context.pageText(key));
    button.onclick = action;
    button.action = key;
    return button;
  },
});
vm.runInContext(fs.readFileSync(path.join(__dirname, '../settings-pages.js'), 'utf8'), context, {filename: 'settings-pages.js'});

function visible(node) {
  const parts = [];
  const walk = item => {
    if (!item || typeof item !== 'object') return;
    if (typeof item.text === 'function') parts.push(String(item.text()));
    else if (item.text !== undefined) parts.push(String(item.text));
    for (const child of item.children || []) walk(child);
  };
  walk(node);
  return parts.join('\n');
}

test('an authorized remote user row shows the person and the grant, not the mapped conversation id', () => {
  const list = {rows: [], replaceChildren() { this.rows = []; }, append(row) { this.rows.push(row); }};
  let revoked = null;
  context.renderRemoteAuthorizedUsers(list, [
    {user_id: 'ou_8134bb25358076f5efd5a6f1e137fe9b', paired: true, manual: false, sessions: ['628dbc59-771b-4a9d-a40c-5dc260a97dff']},
    {user_id: 'manual-only', paired: false, manual: true, sessions: []},
  ], userId => { revoked = userId; });
  const shown = list.rows.map(visible);
  assert.deepEqual(shown, [
    'ou_8134bb25358076f5efd5a6f1e137fe9b\npaired_grant\nrevoke',
    'manual-only\nmanual_grant\nrevoke',
  ]);
  assert.equal(shown.join('\n').includes('628dbc59-771b-4a9d-a40c-5dc260a97dff'), false);
  list.rows[0].children.find(node => node.tag === 'button').onclick();
  assert.equal(revoked, 'ou_8134bb25358076f5efd5a6f1e137fe9b');
});
