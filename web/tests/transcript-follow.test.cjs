const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function fixture() {
  class Viewport extends EventTarget {
    clientHeight = 400;
    scrollHeight = 1200;
    #top = 800;
    get scrollTop() { return this.#top; }
    set scrollTop(top) { this.#top = Math.max(0, Math.min(top, this.scrollHeight - this.clientHeight)); }
  }
  const viewport = new Viewport();
  const frames = new Map();
  let nextFrame = 0;
  const scope = {
    requestAnimationFrame(callback) { frames.set(++nextFrame, callback); return nextFrame; },
    cancelAnimationFrame(id) { frames.delete(id); },
    getComputedStyle(node) { return node.style; },
  };
  vm.createContext(scope);
  vm.runInContext(fs.readFileSync('web/navigation-help.js', 'utf8'), scope);
  const follow = scope.createTranscriptFollow(viewport, () => {});
  return {
    viewport, follow,
    frame() { for (const [id, callback] of frames) { frames.delete(id); callback(); } },
    scroll(top) { viewport.scrollTop = top; viewport.dispatchEvent(new Event('scroll')); },
    wheel(deltaY, target = viewport) {
      const event = Object.assign(new Event('wheel'), { deltaY, deltaX: 0 });
      Object.defineProperty(event, 'target', { value: target });
      viewport.dispatchEvent(event);
    },
  };
}

test('a small upward wheel gesture releases follow before queued output can scroll', () => {
  const { viewport, follow, frame, scroll, wheel } = fixture();
  follow.schedule();
  wheel(-4);
  viewport.scrollHeight += 12;
  frame(); // The browser has yet to apply the wheel movement.
  assert.equal(viewport.scrollTop, 800);
  scroll(796);
  follow.schedule();
  frame();
  assert.equal(viewport.scrollTop, 796);

  scroll(809);
  assert.equal(follow.isPinned(), false);
  scroll(812);
  assert.equal(follow.isPinned(), true);
  viewport.scrollHeight += 10;
  follow.schedule();
  frame();
  assert.equal(viewport.scrollTop, 822);
});

test('a position change before the scroll event releases follow, while layout clamping preserves it', () => {
  const { viewport, follow, frame, scroll } = fixture();
  viewport.scrollTop = 798;
  viewport.scrollHeight += 15;
  follow.schedule();
  frame();
  assert.equal(viewport.scrollTop, 798);
  assert.equal(follow.isPinned(), false);

  follow.resume();
  viewport.scrollHeight = 1100;
  scroll(815); // Simulate the browser clamping after a round collapses.
  assert.equal(follow.isPinned(), true);
  viewport.scrollHeight += 20;
  follow.schedule();
  frame();
  assert.equal(viewport.scrollTop, 720);
});

test('scrolling inside tool output leaves the transcript following until the gesture reaches it', () => {
  const { viewport, follow, wheel } = fixture();
  const tool = { parentElement: viewport, scrollTop: 10, style: { overflowY: 'auto' } };
  wheel(-4, tool);
  assert.equal(follow.isPinned(), true);
  tool.scrollTop = 0;
  wheel(-4, tool);
  assert.equal(follow.isPinned(), false);
});
