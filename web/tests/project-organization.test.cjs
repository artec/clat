const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

test('directory merge clears cached background activity when a settled row omits the list', () => {
  const source = fs.readFileSync(path.join(__dirname, '..', 'project-organization.js'), 'utf8');
  const scope = {
    window: { addEventListener() {} },
    CLATI18n: { bind() {}, msg: key => key },
  };
  vm.createContext(scope);
  vm.runInContext(source, scope);
  const organization = vm.runInContext(
    "CLATProjectOrganization.create({request:async()=>{},refresh:async()=>{},icon:()=>({})})",
    scope
  );
  organization.accept([
    { root: '/project', title: 'Project', background_activities: [{ session_id: 's', kind: 'terminal', count: 1 }] },
  ]);
  assert.equal(organization.rows()[0].background_activities.length, 1);
  // Settled rows omit the empty list entirely (serde skip_serializing_if and
  // older backends); the cached merge must treat a missing list as no live
  // activity instead of keeping the stale badge from the running phase.
  organization.accept([{ root: '/project', title: 'Project' }]);
  const merged = organization.rows()[0].background_activities;
  assert.ok(Array.isArray(merged), 'merged row must still carry the activity list');
  assert.equal(merged.length, 0);
});
