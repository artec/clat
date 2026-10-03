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
  document.getElementById('find-bar')?.classList.add('hidden');
  document.querySelector('.conversation')?.classList.remove('is-finding');
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
  document.getElementById('find-status').textContent =
    (findMatches.length ? `${index + 1}/${findMatches.length}${findMatches.length === 2000 ? '+' : ''}` : '0 matches')
    + ' · user/agent original body · ' + (state.history.hasMore ? 'loaded window only; earlier available' : 'all messages loaded');
  document.getElementById('find-earlier').disabled = !state.history.hasMore || state.history.loading;
}

function moveConversationFind(direction) {
  updateConversationFind();
  if (!findMatches.length) return;
  const current = findMatches.findIndex((m) => m.id === findActive?.id && m.offset === findActive?.offset);
  findActive = findMatches[(current + direction + findMatches.length) % findMatches.length];
  updateConversationFind();
  findActive.node.scrollIntoView({ block: 'center', behavior: 'auto' });
  document.getElementById('find-preview').textContent = findActive.role + ': '
    + findActive.source().slice(Math.max(0, findActive.offset - 40), findActive.offset + 160);
}

async function findEarlierMessages() {
  const request = ++findRequest;
  document.getElementById('find-status').textContent = 'Loading one earlier page… · Close to cancel search';
  await loadOlderHistory();
  if (request === findRequest) updateConversationFind();
}

function installConversationFind() {
  const bar = document.getElementById('find-bar');
  const input = document.getElementById('find-query');
  const open = () => { show(bar); document.querySelector('.conversation').classList.add('is-finding'); updateConversationFind(); input.focus(); input.select(); };
  const close = () => {
    findRequest += 1; hide(bar); findActive = null;
    document.querySelector('.conversation').classList.remove('is-finding');
    for (const message of findMessages.values()) message.node.classList.remove('find-current');
    dom.prompt.focus();
  };
  document.getElementById('find-open').addEventListener('click', open);
  document.getElementById('find-close').addEventListener('click', close);
  document.getElementById('find-next').addEventListener('click', () => moveConversationFind(1));
  document.getElementById('find-prev').addEventListener('click', () => moveConversationFind(-1));
  document.getElementById('find-earlier').addEventListener('click', () => { void findEarlierMessages(); });
  input.addEventListener('input', () => { findActive = null; updateConversationFind(); moveConversationFind(0); });
  input.addEventListener('keydown', (event) => {
    if (event.isComposing) return;
    if (event.key === 'Escape') { event.preventDefault(); close(); }
    if (event.key === 'Enter') { event.preventDefault(); moveConversationFind(event.shiftKey ? -1 : 1); }
  });
  document.addEventListener('keydown', (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f' && !document.querySelector('dialog[open]')) {
      event.preventDefault(); open();
    }
  });
}
