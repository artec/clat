/* Pending input presentation; the Core owns scheduling. */
const pendingInputStore = { items: [], paused: false, interrupting: false };
const pendingInputEditors = new Map();
let pendingInputPopup = null, pendingInputTrigger = null, pendingInputPopover = null;
function inputText(key, params = {}) { return CLATI18n.msg('input.' + key, params); }
function supportsPendingInputs() { return state.workbench?.methods?.includes('input.submit'); }
function inputGlyph(name) {
  const glyph = el('span', 'input-glyph'); glyph.setAttribute('aria-hidden', 'true');
  glyph.style.setProperty('--input-icon', `url('/icons/input-${name}.svg')`); return glyph;
}
function pendingButton(key, action, icon) {
  const button = el('button', 'text-action input-action'); button.type = 'button';
  CLATI18n.attribute(button, 'title', () => inputText(key));
  CLATI18n.attribute(button, 'aria-label', () => inputText(key));
  if (icon) button.append(inputGlyph(icon)); else button.append(svgIcon('tool'));
  button.onclick = action; return button;
}
function installPendingInputs() {
  const seat = el('div', 'usage-seat pending-input-seat');
  seat.hidden = true;
  const trigger = pendingInputTrigger = el('button', 'icon-button pending-input-trigger'); trigger.type = 'button';
  trigger.id = 'pending-input-trigger'; trigger.setAttribute('aria-haspopup', 'dialog'); trigger.setAttribute('aria-expanded', 'false');
  CLATI18n.attribute(trigger, 'aria-label', () => inputText('title'));
  const badge = el('span', 'pending-input-count'); badge.hidden = true; trigger.append(svgIcon('pending'), badge);
  const popup = pendingInputPopup = el('section', 'usage-popover input-popover hidden'); popup.setAttribute('role', 'dialog');
  CLATI18n.attribute(popup, 'aria-label', () => inputText('title')); seat.append(trigger, popup);
  dom['queue-enqueue'].before(seat);
  pendingInputPopover = attachComposerPopup(seat, trigger, popup, renderPendingInputPopup, 'right');
  trigger.onclick = pendingInputPopover.toggle;
  document.addEventListener('keydown', event => {
    if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
    if (activeComposerPopup) { event.preventDefault(); activeComposerPopup.close(); return; }
    if (!supportsPendingInputs()) return;
    if ((dom.app.dataset.page && !borrowedConversation) || commandPanelOpen() || pageIsOpen(dom['settings-dialog']) || pageIsOpen(dom['market-dialog'])) return;
    if (!popup.classList.contains('hidden')) { event.preventDefault(); pendingInputPopover.close(); return; }
    if (state.runActive || pendingInputStore.interrupting) { event.preventDefault(); dom.cancel.click(); }
  });
  CLATI18n.onChange(() => { renderPendingInputs(pendingInputStore); });
}
function renderPendingInputs(value) {
  if (!pendingInputTrigger) return;
  Object.assign(pendingInputStore, value || { items: [], paused: false, interrupting: false });
  pendingInputTrigger.parentElement.hidden = !supportsPendingInputs();
  if (!supportsPendingInputs()) return;
  dom['queue-enqueue'].hidden = true; dom['queue-recall'].hidden = true;
  const badge = pendingInputTrigger.querySelector('.pending-input-count');
  badge.hidden = !pendingInputStore.items.length; badge.textContent = String(pendingInputStore.items.length);
  pendingInputTrigger.setAttribute('aria-busy', String(pendingInputStore.interrupting || optimisticInputInterrupt !== null));
  for (const item of pendingInputStore.items) rememberInputReferences(item);
  for (const [key, preview] of inputPreviews) {
    if (!pendingInputStore.items.some(item => JSON.parse(key)[0] === item.id && JSON.parse(key)[1] === item.version)) {
      preview.then(value => URL.revokeObjectURL(value.url)).catch(() => {}); inputPreviews.delete(key);
    }
  }
  if (!pendingInputPopup.classList.contains('hidden')) renderPendingInputPopup();
  syncInputSubmitButtons();
}
function syncInputSubmitButtons() {
  const busy = state.runActive || (supportsPendingInputs() && pendingInputStore.interrupting);
  const hasDraft = Boolean(dom.prompt.value.trim() || state.draft.images.length);
  const sending = supportsPendingInputs() ? (!busy || hasDraft) : !state.runActive;
  dom.send.classList.toggle('hidden', !sending); dom.cancel.classList.toggle('hidden', sending);
}
function renderPendingInputPopup() {
  if (!pendingInputPopup) return;
  const active = document.activeElement, editing = active?.dataset.inputEditor;
  const selection = editing ? [active.selectionStart, active.selectionEnd] : null;
  const header = el('div', 'usage-row input-popup-heading'); header.append(el('span', '', () => inputText('title')), el('span', '', String(pendingInputStore.items.length)));
  pendingInputPopup.replaceChildren(header, el('small', '', () => inputText(pendingInputStore.paused ? 'paused' : 'step_hint')));
  if (!pendingInputStore.items.length) pendingInputPopup.append(el('p', 'input-empty', () => inputText('empty')));
  pendingInputStore.items.forEach((item, index) => pendingInputPopup.append(renderPendingInputRow(item, index)));
  for (const editor of pendingInputEditors.values()) {
    if (composerOwns(editor.owner) && !pendingInputStore.items.some(item => item.id === editor.id)) pendingInputPopup.append(renderInputEditor({ id:editor.id, version: editor.version, phase: 'received' }));
  }
  for (const recovery of inputRecallRecoveries.values()) {
    if (!composerOwns(recovery.owner)) continue;
    const retry = el('button', 'text-action', () => inputText('retry_recall'));
    retry.type = 'button'; retry.onclick = () => recallPendingInput(recovery.item, recovery); pendingInputPopup.append(retry);
  }
  if (state.runActive || pendingInputStore.interrupting) {
    const stop = el('button', 'input-stop text-action', () => inputText('stop'));
    stop.type = 'button'; stop.prepend(dom.cancel.querySelector('svg').cloneNode(true));
    stop.onclick = () => { pendingInputPopover.close(); dom.cancel.click(); }; pendingInputPopup.append(stop);
  }
  if (editing) {
    const editor = [...pendingInputPopup.querySelectorAll('textarea')].find(node => node.dataset.inputEditor === editing);
    if (editor) { editor.focus(); editor.setSelectionRange(...selection); }
  }
}
function renderPendingInputRow(item, index) {
  if(optimisticInputInterrupt===item.id)item={...item,phase:'interrupting'};
  const row = el('div', 'pending-input-row'); row.dataset.inputId = item.id;
  const preview = el('div', 'pending-input-preview');
  preview.append(el('span', 'pending-input-number', String(index + 1)), el('span', 'pending-input-text', item.text || (() => inputText('images_only')))); row.append(preview);
  const attachments = el('div', 'pending-input-attachments');
  item.draft.uploads.forEach((_, i) => { const image = el('img', 'pending-input-image'); image.alt = String(inputText('image')); inputPreview(item, i).then(value => { image.src = value.url; }).catch(() => image.remove()); attachments.append(image); });
  for (const reference of item.draft.references) attachments.append(el('span', 'pending-input-reference', reference.path || reference.quote || ''));
  if (attachments.children.length) row.append(attachments);
  if (pendingInputEditor(item.id)) { row.append(renderInputEditor(item)); return row; }
  const actions = el('div', 'pending-input-actions');
  const interrupt = pendingButton('interrupt', () => performInputAction('interrupt', item), 'zap'); interrupt.append(el('span', '', () => inputText('interrupt'))); interrupt.classList.add('input-interrupt');
  const edit = pendingButton('edit', () => beginPendingInputEdit(item));
  const recall = pendingButton('recall', () => recallPendingInput(item), 'corner-up-left');
  const remove = pendingButton('delete', () => performInputAction('delete', item), 'trash-2');
  for (const button of [interrupt, edit, recall, remove]) { button.disabled = item.phase !== 'queued' || inputOperations.has(item.id); actions.append(button); }
  interrupt.disabled ||= pendingInputStore.interrupting || optimisticInputInterrupt !== null;
  if (item.phase !== 'queued') actions.append(el('small', '', () => inputText(item.phase === 'interrupting' ? 'interrupting' : 'receiving')));
  row.append(actions); return row;
}
