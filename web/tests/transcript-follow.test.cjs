const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const appSource = fs.readFileSync('web/app.js', 'utf8').replace(/\r\n/g, '\n');

function loadAppFunctions(scope, start, end) {
  vm.runInContext(appSource.slice(appSource.indexOf(start), appSource.indexOf(end)), scope);
}

function fixture() {
  class Viewport extends EventTarget {
    dataset = {};
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
    viewport, follow, scope,
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

test('rendering that momentarily clamps the viewport does not stop following live output', () => {
  const { viewport, follow, frame, scroll } = fixture();
  viewport.scrollHeight = 1140;
  viewport.scrollTop = 800; // The browser clamps the old bottom as content shrinks.
  viewport.scrollHeight = 1220; // New output arrives before the scroll event is delivered.
  scroll(viewport.scrollTop);
  follow.schedule();
  frame();
  assert.equal(viewport.scrollTop, 820);
});

test('a layout change that puts the reader at the bottom does not resume paused follow', () => {
  const { viewport, follow, frame, scroll } = fixture();
  scroll(600);
  viewport.scrollHeight = 1010;
  scroll(610); // Layout shifts the reading anchor onto the new bottom.
  viewport.scrollHeight += 20;
  follow.schedule();
  frame();
  assert.equal(viewport.scrollTop, 610);
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

function composerFixture(modern, running = false) {
  const view = fixture();
  const { scope, viewport, follow } = view;
  Object.assign(scope, {
    transcriptFollow: follow, syncHistoryNavigation() {},
    context: { isActive: () => true },
    state: { selectionGeneration: 1, runActive: running, draft: { images: [], queuedClientMessageId: null } },
    dom: { prompt: { value: 'A new question' }, 'transcript-scroll': viewport },
    document: { getElementById: () => ({ classList: { contains: () => true } }) },
    composerScope: 'session', composerOwns: owner => owner === scope.composerScope,
    composerOwnerAliases: new Map(), pendingFileReferences: new Map(),
    supportsPendingInputs: () => modern,
    borrowedConversation: null, sessionSharesFor: () => [],
    newOpaqueClientId: purpose => purpose + '-id',
    composerSubmissionParams: text => ({ text }),
    bindFileReferenceSubmission() {}, renderSessionShareDraft() {}, renderDraft() {},
    rememberSubmittedInput() {}, clearOwnedSubmittedText() {}, showAcceptedInputFeedback() {},
    echoOptimisticSteering() {}, recoverComposerSubmission() {},
    rpc: async () => ({ outcome: 'queued' }),
  });
  loadAppFunctions(scope, 'function isFollowingLatest()', 'function syncHistoryStatus()');
  loadAppFunctions(scope, 'async function submitConfiguredPrompt()', 'function settleQueuedDraft(');
  vm.runInContext(fs.readFileSync('web/pending-input-actions.js', 'utf8'), scope);
  return { ...view, send: () => scope.submitConfiguredPrompt() };
}

test('sending a new question resumes following even when reading earlier messages', async () => {
  for (const modern of [true, false]) {
    const { viewport, follow, scroll, frame, send } = composerFixture(modern);
    scroll(600);
    viewport.scrollHeight += 20;
    await send();
    assert.equal(viewport.scrollTop, 820);
    viewport.scrollHeight += 20;
    follow.schedule();
    frame();
    assert.equal(viewport.scrollTop, 840);
  }
});

test('an interjection or a later manual scroll keeps the chosen reading position', async () => {
  const interjection = composerFixture(true, true);
  interjection.scroll(600);
  await interjection.send();
  assert.equal(interjection.viewport.scrollTop, 600);

  const { scope, viewport, scroll, send } = composerFixture(true);
  scroll(600);
  let accept;
  scope.rpc = () => new Promise(resolve => { accept = resolve; });
  const sending = send();
  scroll(500);
  accept({ outcome: 'queued' });
  await sending;
  assert.equal(viewport.scrollTop, 500);
});
