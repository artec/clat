const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

const source = fs.readFileSync('web/conversation-experience.js', 'utf8');
const start = source.indexOf('function positionUsagePopover(');
const end = source.indexOf('function installUsagePopover(', start);
assert.ok(start >= 0 && end > start, 'popover positioning function is available');
const positionSource = source.slice(start, end);

function position({ viewport, shell, popupWidth, popupId = 'model-picker' }) {
  const popup = { id: popupId, offsetWidth: popupWidth, style: {},
    classList: { contains: () => false } };
  const trigger = { getBoundingClientRect: () =>
    ({ left: shell.right - 120, width: 100, top: 700 }) };
  const context = {
    popup, trigger, innerWidth: viewport, innerHeight: 800,
    dom: { 'composer-shell': { getBoundingClientRect: () => shell } },
  };
  vm.runInNewContext(positionSource + "\npositionUsagePopover(popup, trigger, 'center');", context);
  return { left: Number.parseFloat(popup.style.left), right: Number.parseFloat(popup.style.left) + popupWidth,
    maxWidth: Number.parseFloat(popup.style.maxWidth) };
}

test('model chooser right edge matches desktop composer', () => {
  const shell = { left: 140, right: 1215, width: 1075 };
  const actual = position({ viewport: 1280, shell, popupWidth: 500 });
  assert.equal(actual.right, shell.right);
  assert.ok(actual.maxWidth <= shell.width);
});

test('model chooser stays within narrow mobile composer', () => {
  const shell = { left: 8, right: 367, width: 359 };
  const actual = position({ viewport: 375, shell, popupWidth: 359 });
  assert.equal(actual.right, shell.right);
  assert.ok(actual.left >= shell.left);
});
