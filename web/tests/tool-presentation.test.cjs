const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
function presentation() {
  const source = fs.readFileSync('web/app.js', 'utf8');
  const scope = {CLATI18n: {msg: (key, params) => JSON.stringify({locale:scope.locale,key,params})}, locale:'en'};
  vm.createContext(scope);
  vm.runInContext(source.slice(source.indexOf('const TOOL_PRESENTATION_KEYS'), source.indexOf('function appendWorkRecord')), scope);
  return scope;
}
test('capability presentation preserves raw model text and distinguishes native and external actions', () => {
  const scope = presentation();
  scope.original = 'Use web_search then mcp_glm_search_web_search_prime; keep user_input unchanged.';
  const original = scope.original;
  const preview = vm.runInContext('toolPreview(original)', scope);
  assert.equal(scope.original, original);
  assert.notEqual(preview, original);
  assert.ok(preview.includes('user_input'));
  const native = JSON.parse(vm.runInContext("toolDisplayName('web_search')", scope));
  const external = JSON.parse(vm.runInContext("toolDisplayName('mcp_glm_search_web_search_prime')", scope));
  assert.notEqual(native.key, external.key);
  assert.equal(JSON.parse(external.params.action).key, native.key);
  scope.locale = 'zh-Hans';
  assert.notEqual(vm.runInContext("toolDisplayName('web_search')", scope), JSON.stringify(native));
});
test('unknown operations do not invent an action and presentation keys exist in every catalog', () => {
  const scope = presentation();
  for (const name of ['unknown_operation', '__proto__', 'constructor', 'mcp_unknown_operation']) {
    scope.name = name;
    let value = JSON.parse(vm.runInContext('toolDisplayName(name)', scope));
    if (value.params) value = JSON.parse(value.params.action);
    const generic = JSON.parse(vm.runInContext("toolDisplayName('')", scope));
    assert.equal(value.key, generic.key);
  }
  for (const locale of ['en','zh-Hans','zh-Hant']) {
    const catalog = JSON.parse(fs.readFileSync(`locales/${locale}.json`, 'utf8'));
    const keys = vm.runInContext("[...Object.values(TOOL_PRESENTATION_KEYS), 'other','external','identifier','allowed','denied','approval']", scope);
    for (const key of keys) assert.ok(catalog['web.tool_action.' + key]);
  }
});
test('permission presentation keeps the original decision for inspection and preserves grouping semantics', () => {
  const scope = presentation();
  scope.el = (tag, cls, text) => ({tag,cls,text,children:[],dataset:{},append(...items){this.children.push(...items);}});
  scope.appendTranscript = node => {node.grouped=false;return node;};
  scope.appendWorkRecord = node => {node.grouped=true;return node;};
  scope.jsonText = JSON.stringify;
  scope.tool = 'mcp_glm_search_web_search_prime';
  scope.decision = {deny:{reason:'original internal_diagnostic'}};
  const before = JSON.stringify(scope.decision);
  const denied = vm.runInContext("addPermissionRecord(tool,decision,'permission_checked')",scope);
  assert.equal(JSON.stringify(scope.decision),before);
  assert.ok(denied.children[1].text().includes(before));
  assert.ok(denied.children[1].text().includes(scope.tool));
  assert.equal(denied.grouped,false);
  assert.equal(vm.runInContext("addPermissionRecord(tool,'allow','permission_checked')",scope).grouped,true);
  assert.equal(JSON.parse(vm.runInContext("permissionDisplay({unavailable:{reason:'offline'}})",scope)).key,
    JSON.parse(vm.runInContext("permissionDisplay({deny:{reason:'denied'}})",scope)).key);
});
