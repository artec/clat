/* Public history is rendered independently of active replay/admission state. */
let borrowedConversation = null;
function returnConversationPane() {
  if (!borrowedConversation) return;
  document.getElementById('conversation-page').append(borrowedConversation.pane);
  borrowedConversation = null;
}
function borrowConversationPane(parent, session) {
  if (state.sessionId !== session || state.switching || !context.isActive()) return false;
  returnConversationPane();
  const pane = document.querySelector('.conversation'); parent.append(pane);
  borrowedConversation = {pane, session}; parent.classList.add('has-live-conversation');
  return true;
}
function waitForConversation(session, current) {
  return new Promise((resolve, reject) => {
    const finish = value => { clearTimeout(timer); clearInterval(checker); resolve(value); };
    const check = () => { if (!current()) finish(false); else if (!state.switching && state.connected && state.sessionId === session) finish(true); };
    const checker = setInterval(check, 50);
    const timer = setTimeout(() => { clearInterval(checker); reject(CLATI18n.error(pageText('conversation_unavailable'))); }, 15000);
    check();
  });
}
async function selectConversationTarget(reference, current) {
  if (reference.project !== state.workbench?.project?.root) throw CLATI18n.error(pageText('other_project'));
  if (state.sessionId !== reference.session) {
    if (state.runActive || state.compactionActive || state.switching) throw CLATI18n.error(pageText('conversation_busy'));
    if(!confirmAttachmentSelection())return false;
    const generation=state.selectionGeneration;setSwitching(true);
    try{await rpc('session.switch',{id:reference.session,expected_selection_generation:generation});resubscribe();}
    catch(error){setSwitching(false);throw error;}
  }
  return waitForConversation(reference.session, current);
}
function createConversationReader(parent, reference, {trusted = false, editable = false, current = () => true} = {}) {
  let epoch = 0, after = null, version = null;
  const list = el('div', 'reader-messages'); const status = el('p', 'settings-note');
  const more = pageAction('more', () => void load(true)); more.hidden = true;
  const controls = el('div', 'page-toolbar'); controls.append(more);
  const scope = el('p', 'page-scope', () => pageText('session_scope', {project: reference.project || '', session: reference.session}));
  parent.append(scope, status, list, controls);
  function messageNode(message) {
    const node = el('article', 'reader-message ' + message.role); node.dataset.seq = String(message.reference.seq);
    node.append(el('small', '', () => pageText(message.role)));
    const body = el('div', 'body rich-text'); renderMarkdown(body, message.text); node.append(body);
    const actions = el('div', 'reader-actions');
    actions.append(pageAction('copy', () => void navigator.clipboard.writeText(message.text)), pageAction('bookmark', async () => {
      try{await rpc('library.bookmark.add', {reference: message.reference});
      CLATI18n.bind(status, 'textContent', () => pageText('bookmarked'));}catch(error){status.textContent=error.message;}
    }));
    if(message.reference.project!==state.workbench?.project?.root){actions.lastChild.remove();actions.append(el('small','settings-note',()=>pageText('other_project')));}
    for(const attachment of message.attachments || []) actions.append(pageAction('view_attachment',()=>showHistoricalAttachment(message.reference,attachment,trusted)));
    node.append(actions); messageTime(node,message.time);
    if (message.truncated) node.append(el('p', 'settings-note', () => pageText('bounded_message')));
    return node;
  }
  async function load(append = false) {
    const request = ++epoch;
    try {
      const value = await rpc('history.conversation', {reference, trusted_projects: trusted, after_seq: append ? after : null, version: append ? version : null});
      if (request !== epoch || !current()) return;
      if (!append) list.replaceChildren();
      list.append(...value.messages.map(messageNode)); version = value.version; after = value.next_seq; more.hidden = after === null;
      if (!list.children.length) list.append(pageEmpty('empty_conversation'));
      const target = reference.seq == null ? null : list.querySelector(`[data-seq="${reference.seq}"]`);
      if (target) { target.classList.add('search-target'); target.scrollIntoView({block: 'nearest'}); }
      else if (reference.seq != null && after !== null && after < reference.seq) await load(true);
    } catch (error) {
      if (request !== epoch || !current()) return;
      CLATI18n.bind(status, 'textContent', () => error.message);
      if (error.presentation?.key === 'history.source_changed') { version = null; after = null; more.hidden = false; more.onclick = () => void load(false); }
    }
  }
  if (editable) installReaderDraft(parent, reference, status, current);
  const ready=load();
  return {ready,dispose: () => { epoch++; }, refresh: () => load()};
}
function installReaderDraft(parent, reference, status, current) {
  const owner = workspacePrefix + ':' + reference.session;
  const input = el('textarea', 'reader-draft'); input.value = state.sessionId === reference.session ? dom.prompt.value : composerDrafts.get(owner)?.text || '';
  CLATI18n.attribute(input, 'aria-label', () => pageText('draft'));
  input.oninput = () => composerDrafts.set(owner, {text: input.value, generation: (composerDrafts.get(owner)?.generation || 0) + 1});
  const activate = pageAction('continue_conversation', async () => {
    activate.disabled = true;
    try {
      if (!await selectConversationTarget(reference, current)) return;
      if (current()) { parent.replaceChildren(); borrowConversationPane(parent, reference.session); }
    } catch (error) { if (current()) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { activate.disabled = false; }
  }, true);
  parent.append(el('p', 'settings-note', () => pageText('reader_draft_note')), input, activate);
}
