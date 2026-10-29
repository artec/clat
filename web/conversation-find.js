// Search raw message data, not a DOM scrape; node identity survives prefix pages.
const findMessages = new Map();
let findNextId = 0;
let findMatches = [];
let findActive = null;
let findRequest = 0;

function foldedFindSource(source) {
  const offsets = [];
  let originalOffset = 0;
  for (const character of source) {
    for (let i = 0; i < character.toLocaleLowerCase().length; i += 1) offsets.push(originalOffset);
    originalOffset += character.length;
  }
  return { text: source.toLocaleLowerCase(), offsets };
}

function rememberFindMessage(node, role, source) {
  const id = ++findNextId;
  findMessages.set(id, { id, node, role, source });
  updateConversationFind();
}

function resetConversationFind() {
  findMessages.clear(); findMatches = []; findActive = null; findRequest += 1;
  setConversationFindOpen(false);
}

function conversationFindAnchor(){return document.querySelector('.application-actions [data-action=find]')||document.getElementById('find-open');}
function positionConversationFind(bar) {
  const trigger = conversationFindAnchor(), toolbar = trigger.closest('.application-toolbar');
  positionSessionMenu(bar, 0, 0, toolbar || trigger);
  const inset = parseFloat(getComputedStyle(bar).getPropertyValue('--toolbar-menu-inset')) || 12;
  const right = Math.min((toolbar || dom.app).getBoundingClientRect().right, innerWidth) - inset;
  bar.style.left = Math.max(inset, right - bar.getBoundingClientRect().width) + 'px';
}

function setConversationFindOpen(open, keyboard = false) {
  const bar = document.getElementById('find-bar');
  const trigger = conversationFindAnchor();
  bar.classList.toggle('hidden', !open);
  trigger.setAttribute('aria-expanded', String(open));
  if (open) {
    updateConversationFind();
    positionConversationFind(bar);
    if (keyboard) {
      const input = document.getElementById('find-query');
      input.focus(); input.select();
    }
  } else {
    findRequest += 1; findActive = null;
    for (const message of findMessages.values()) message.node.classList.remove('find-current');
    CLATI18n.bind(document.getElementById('find-preview'), 'textContent', () => '');
    if (keyboard) trigger.focus({ preventScroll: true });
  }
}

function updateConversationFind() {
  const bar = document.getElementById('find-bar');
  if (!bar || bar.classList.contains('hidden')) return;
  const query = document.getElementById('find-query').value.toLocaleLowerCase();
  findMatches = [];
  const messages = [...findMessages.values()].sort((a, b) =>
    Number(a.node.dataset.seq || Number.MAX_SAFE_INTEGER) - Number(b.node.dataset.seq || Number.MAX_SAFE_INTEGER) || a.id - b.id);
  for (const message of messages) {
    if (!query) break;
    const { text, offsets } = foldedFindSource(message.source());
    for (let index = text.indexOf(query); index >= 0 && findMatches.length < 2000;
      index = text.indexOf(query, index + query.length)) {
      findMatches.push({ ...message, offset: offsets[index] });
    }
  }
  let index = findMatches.findIndex((m) => m.id === findActive?.id && m.offset === findActive?.offset);
  if (index < 0) { index = 0; findActive = findMatches[0] || null; }
  for (const message of findMessages.values()) message.node.classList.toggle('find-current', message.id === findActive?.id);
  CLATI18n.bind(document.getElementById('find-status'), "textContent", () => CLATI18n.msg("web.line_value_user_agent_original_body_value",{value1:((findMatches.length ? `${index + 1}/${findMatches.length}${findMatches.length === 2000 ? '+' : ''}` : CLATI18n.t('web.no_matches'))),value2:((state.history.hasMore ? CLATI18n.msg("web.loaded_window_only_earlier_available", {}, "loaded window only; earlier available") : CLATI18n.msg("web.all_messages_loaded", {}, "all messages loaded")))},"{value1} · user/agent original body · {value2}"));
  document.getElementById('find-earlier').disabled = !state.history.hasMore || state.history.loading;
  for (const id of ['find-prev', 'find-next']) document.getElementById(id).disabled = !findMatches.length;
}

function moveConversationFind(direction) {
  updateConversationFind();
  if (!findMatches.length) return;
  const current = findMatches.findIndex((m) => m.id === findActive?.id && m.offset === findActive?.offset);
  findActive = findMatches[(current + direction + findMatches.length) % findMatches.length];
  updateConversationFind();
  findActive.node.scrollIntoView({ block: 'center', behavior: 'auto' });
  const match = findActive;
  CLATI18n.bind(document.getElementById('find-preview'), "textContent", () => match.role + ': '
    + match.source().slice(Math.max(0, match.offset - 40), match.offset + 160));
}

async function findEarlierMessages() {
  const request = ++findRequest;
  CLATI18n.bind(document.getElementById('find-status'), "textContent", () => CLATI18n.msg("web.loading_one_earlier_page_close_to_cancel_search", {}, "Loading one earlier page… · Close to cancel search"));
  await loadOlderHistory();
  if (request === findRequest) updateConversationFind();
}

function installConversationFind() {
  const bar = document.getElementById('find-bar');
  // Keep viewport-positioned search outside the clipped conversation/tool canvas.
  document.body.appendChild(bar);
  const input = document.getElementById('find-query');
  const trigger = document.getElementById('find-open');
  window.addEventListener('resize', () => {
    if (!bar.classList.contains('hidden')) positionConversationFind(bar);
  });
  new ResizeObserver(() => {
    if (!bar.classList.contains('hidden')) positionConversationFind(bar);
  }).observe(bar);
  trigger.addEventListener('click', event => setConversationFindOpen(bar.classList.contains('hidden'), event.detail === 0));
  document.addEventListener('pointerdown', event => {
    if (!bar.classList.contains('hidden') && !bar.contains(event.target) && !conversationFindAnchor().contains(event.target)) setConversationFindOpen(false);
  });
  document.getElementById('find-next').addEventListener('click', () => moveConversationFind(1));
  document.getElementById('find-prev').addEventListener('click', () => moveConversationFind(-1));
  document.getElementById('find-earlier').addEventListener('click', () => { void findEarlierMessages(); });
  input.addEventListener('input', () => {
    findActive = null; CLATI18n.bind(document.getElementById('find-preview'), 'textContent', () => '');
    updateConversationFind(); moveConversationFind(0);
  });
  input.addEventListener('keydown', (event) => {
    if (event.isComposing) return;
    if (event.key === 'Enter') { event.preventDefault(); moveConversationFind(event.shiftKey ? -1 : 1); }
  });
  document.addEventListener('keydown', (event) => {
    if (event.defaultPrevented || event.isComposing || dom.app.dataset.page || dom.app.dataset.tool || document.querySelector('dialog[open]')) return;
    if (event.key === 'Escape' && !bar.classList.contains('hidden')) {
      event.preventDefault(); setConversationFindOpen(false, true);
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f' && !trigger.hidden) {
      event.preventDefault(); setConversationFindOpen(true, true);
    }
  });
}
