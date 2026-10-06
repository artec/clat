// Page-local UX only. Session receipts and attachment leases remain host-owned.
const composerDrafts = new Map();
const composerOwnerAliases = new Map();
let composerSession = undefined;
let composerSelection = undefined;
let composerScope = '';
let freshComposer = 0;

function saveComposerText() {
  if (composerScope) composerDrafts.set(composerScope, {
    text: dom.prompt.value, generation: state.composerGeneration,
  });
}

function observeComposerSession(id, selection) {
  const next = id || null;
  if (composerSession === next && composerSelection === selection) return;
  const materializing = composerSession === null && next !== null && composerSelection === selection;
  saveComposerText();
  const oldScope = composerScope;
  const scope = workspacePrefix + ':' + (next || 'new-' + (++freshComposer));
  if (materializing) {
    composerOwnerAliases.set(oldScope, scope);
    composerDrafts.set(scope, { text: dom.prompt.value, generation: state.composerGeneration });
    composerDrafts.delete(oldScope);
  } else if (composerSession !== undefined) {
    dom.prompt.value = composerDrafts.get(scope)?.text || '';
    state.composerGeneration += 1;
    invalidateSuggestion();
    const hadImages = state.draft.images.length > 0;
    clearDraft();
    if (hadImages) {
      updateRunState('Session changed; attachments released. Reattach images in this session.');
    }
  }
  composerScope = scope;
  composerSession = next;
  composerSelection = selection;
  resizePrompt();
}

function clearOwnedSubmittedText(owner, generation) {
  const canonical = composerOwnerAliases.get(owner) || owner;
  const draft = composerDrafts.get(canonical);
  if (draft && draft.generation === generation) composerDrafts.delete(canonical);
  clearSubmittedPrompt(generation);
}

function composerOwns(owner) {
  return (composerOwnerAliases.get(owner) || owner) === composerScope;
}

function composerSubmissionParams(text, images) {
  return images.length === 0 ? { text } : {
    text, draftScopeId: state.draft.scope && state.draft.scope.draftScopeId,
    attachments: images.map((image) => image.uploadId), clientMessageId: state.draft.clientMessageId,
  };
}

async function submitComposerSteering({ text, images, owner, draftEpoch, composerGeneration }) {
  const clientMessageId = state.draft.clientMessageId;
  const value = await rpc('steer.send', composerSubmissionParams(text, images));
  if (value && value.outcome === 'queued') clearOwnedSubmittedText(owner, composerGeneration);
  if (!composerOwns(owner) || draftEpoch !== state.draft.epoch) return;
  if (!value || value.outcome !== 'queued') {
    updateRunState('run ended before steering was accepted; draft retained');
    return;
  }
  addNoticeLine('steering queued');
  if (images.length > 0 && state.draft.clientMessageId === clientMessageId) {
    state.draft.queuedClientMessageId = clientMessageId;
    for (const image of images) image.status = 'queued';
    state.draft.notice = 'steering accepted; holding the local draft until durable claim';
  }
}

async function submitComposerCommand({ text, owner }) {
  if (text === '/new' || text === '/clear') {
    if (!confirmAttachmentSelection()) return false;
    await rpc('session.new', {});
    if (!composerOwns(owner)) return true;
    addNoticeLine('new conversation');
    await loadSessions();
    resubscribe();
    return true;
  }
  const value = await rpc('command.run', { command: text });
  if (!composerOwns(owner)) return true;
  if (value?.kind === 'plugin_manager') {
    dom['market-open'].click();
    return true;
  }
  if (value && ['status', 'help', 'memory', 'goal', 'subagent_status', 'goal_run'].includes(value.kind)) {
    const notice = addNoticeLine(value.message || 'command completed');
    if (['memory', 'goal', 'subagent_status'].includes(value.kind)) {
      notice.classList.add('content-notice');
      notice.setAttribute('aria-label', value.kind.replaceAll('_', ' '));
    }
  } else if (value && value.kind === 'context') {
    addContextSnapshot(value.context);
  } else if (value && value.kind === 'session_reset') {
    addNoticeLine('new conversation');
    await loadSessions();
    resubscribe();
  }
  await refreshWorkbench();
  return true;
}

async function recoverComposerSubmission(error, { owner, text, images, composerGeneration }) {
  if (!composerOwns(owner)) return;
  if (error.code !== 'busy' || images.length !== 0) {
    updateRunState('send failed: ' + error.message); return;
  }
  updateRunState('run active · sending as steering');
  try {
    const value = await rpc('steer.send', { text });
    if (value && value.outcome === 'queued') clearOwnedSubmittedText(owner, composerGeneration);
    if (!composerOwns(owner)) return;
    addNoticeLine('steering ' + (value && value.outcome === 'queued' ? 'queued' : 'not running'));
  } catch (steerError) {
    if (composerOwns(owner)) updateRunState('steering failed: ' + steerError.message);
  }
}

function confirmAttachmentSelection() {
  if (state.draft.sending || state.draft.queuedClientMessageId !== null) {
    updateRunState('Wait for the submitted attachment receipt before changing sessions.');
    return false;
  }
  return state.draft.images.length === 0 || window.confirm(
    'Changing sessions releases these draft images. Text stays with its session. Continue?'
  );
}

async function copyMessageText(button, text) {
  const label = button.textContent;
  try {
    if (!navigator.clipboard) throw new Error('Clipboard unavailable');
    await navigator.clipboard.writeText(text);
    button.textContent = 'Copied';
  } catch (_) {
    button.textContent = 'Copy failed';
  }
  setTimeout(() => { button.textContent = label; }, 1600);
}

function copyTextButton(label, source) {
  const button = el('button', 'text-action', label);
  button.type = 'button';
  button.setAttribute('aria-label', label);
  button.addEventListener('click', () => { void copyMessageText(button, source()); });
  return button;
}

function addReplyActions(node, source) {
  if (node.querySelector(':scope > .message-actions')) return;
  const actions = el('div', 'message-actions');
  actions.appendChild(copyTextButton('Copy reply', source));
  node.appendChild(actions);
}

function quoteSelection(text, role, seq) {
  if (state.switching || state.compactionActive || dom.prompt.disabled) return;
  const header = `[Quote from ${role === 'user' ? 'your message' : 'agent reply'}${seq ? ' · message ' + seq : ''}]`;
  const quote = header + '\n' + text.split(/\r?\n/).map((line) => '> ' + line).join('\n');
  dom.prompt.value += (dom.prompt.value ? '\n\n' : '') + quote + '\n\n';
  state.composerGeneration += 1;
  saveComposerText();
  invalidateSuggestion();
  resizePrompt();
  dom.prompt.focus();
}

function installSelectionQuote() {
  const button = el('button', 'selection-quote hidden', 'Quote selection');
  button.type = 'button';
  let chosen = null;
  button.addEventListener('pointerdown', (event) => event.preventDefault());
  button.addEventListener('click', () => {
    if (chosen && chosen.owner === composerScope) quoteSelection(chosen.text, chosen.role, chosen.seq);
    hide(button);
  });
  document.body.appendChild(button);
  document.addEventListener('selectionchange', () => {
    hide(button); chosen = null;
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount !== 1 || state.switching) return;
    const range = selection.getRangeAt(0);
    const node = range.commonAncestorContainer;
    const element = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
    const body = element?.closest('.msg > .body');
    if (!body || !dom.transcript.contains(body) || !body.contains(range.startContainer)
      || !body.contains(range.endContainer)) return;
    const text = selection.toString().slice(0, 16000);
    if (!text.trim()) return;
    const message = body.parentElement;
    chosen = { text, role: message.classList.contains('user') ? 'user' : 'assistant',
      seq: message.dataset.seq, owner: composerScope };
    const rect = range.getBoundingClientRect();
    show(button);
    button.style.left = Math.max(8, Math.min(innerWidth - button.offsetWidth - 8, rect.left)) + 'px';
    button.style.top = Math.max(8, Math.min(innerHeight - button.offsetHeight - 8, rect.bottom + 6)) + 'px';
  });
}
