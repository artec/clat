const inputOperations = new Set(), inputPreviews = new Map();
let inputSubmission = null;
let optimisticInputInterrupt = null;
const inputRetryOperations = new Map(), inputRecallRecoveries = new Map(), recalledInputFiles = new Map();
function rememberInputReferences(item, owner = composerScope) {
  if (!owner || !item.draft.references.length) return;
  const refs = pendingFileReferences.get(owner) || [];
  for (const ref of item.draft.references) {
    let existing = refs.find(r => r.path === ref.path && r.version === ref.version && r.quote === ref.quote);
    if (!existing) { existing = { ...ref }; refs.push(existing); }
    existing.clientMessageIds = [...new Set([...(existing.clientMessageIds || []), item.id])];
  }
  pendingFileReferences.set(owner, refs);
}
async function inputPreview(item, index) {
  const key = JSON.stringify([item.id,item.version,index]);
  if (!inputPreviews.has(key)) inputPreviews.set(key, rpc('input.image', { id: item.id, version: item.version, index }).then(value => {
    const bytes = Uint8Array.from(atob(value.base64), c => c.charCodeAt(0));
    const type = bytes[0] === 255 && bytes[1] === 216 ? 'image/jpeg' : 'image/png';
    const blob = new Blob([bytes], { type });
    return { ...value, blob, url: URL.createObjectURL(blob) };
  }).catch(error => { inputPreviews.delete(key); throw error; }));
  return inputPreviews.get(key);
}
async function refreshPendingInputs() {
  if (!supportsPendingInputs()) return;
  const owner = composerScope;
  try { const value = await rpc('input.pending', {}); if (composerOwns(owner)) renderPendingInputs(value); }
  catch (_) {}
}
async function submitPendingInput({ text, images, owner, draftEpoch, composerGeneration }) {
  const params = { ...composerSubmissionParams(text, images) };
  params.clientMessageId ||= newOpaqueClientId('input');
  const signature = JSON.stringify([owner, text, images.map(i => i.uploadId)]);
  if (!inputSubmission || inputSubmission.signature !== signature) inputSubmission = { signature, operationId: newOpaqueClientId('input-op'), clientMessageId: params.clientMessageId };
  Object.assign(params, { operationId: inputSubmission.operationId, clientMessageId: inputSubmission.clientMessageId });
  bindFileReferenceSubmission(owner, text, params);
  params.references = (pendingFileReferences.get(owner) || []).filter(ref => text.includes(ref.quote)).map(({path,version,quote}) => ({path,version,quote}));
  params.expected_selection_generation = state.selectionGeneration;
  await rpc('input.submit', params);
  if (composerOwns(owner)) {
    clearOwnedSubmittedText(owner, composerGeneration);
    if (images.length && draftEpoch === state.draft.epoch) clearDraft();
    updateRunState(inputText('accepted')); void refreshPendingInputs();
  }
  inputSubmission = null;
}
async function performInputAction(action, item, extra = {}) {
  if (inputOperations.has(item.id)) return;
  const owner = composerScope, editor = pendingInputEditor(item.id);
  if(action==='interrupt')optimisticInputInterrupt=item.id;
  const retryKey = JSON.stringify([owner, action, item.id, item.version, extra]);
  if (!inputRetryOperations.has(retryKey)) inputRetryOperations.set(retryKey, newOpaqueClientId('input-op'));
  inputOperations.add(item.id); renderPendingInputPopup();
  try {
    const result = await rpc('input.' + action, { id: item.id, version: item.version, operationId: inputRetryOperations.get(retryKey), expected_selection_generation: state.selectionGeneration, ...extra });
    inputRetryOperations.delete(retryKey);
    if (action === 'update' && editor) pendingInputEditors.delete(editor.key);
    if (action === 'interrupt' && composerOwns(owner)) updateRunState(inputText('interrupting'));
    return result;
  } catch (error) { if (composerOwns(owner)) updateRunState(error.message); }
  finally { inputOperations.delete(item.id);if(action==='interrupt')optimisticInputInterrupt=null;if (composerOwns(owner)) await refreshPendingInputs(); }
}
function renderInputEditor(item) { return buildInputEditor(item, pendingInputEditor(item.id)); }
async function recallPendingInput(item, recovery = null) {
  if (inputOperations.has(item.id) || state.draft.sending || state.draft.images.some(i => i.status !== 'uploaded')) return;
  const owner = recovery?.owner || composerScope;
  const draftEpoch = state.draft.epoch;
  state.draft.sending = true; renderDraft();
  try {
    if (state.draft.images.length + item.draft.uploads.length > MAX_DRAFT_IMAGES) { updateRunState(inputText('merge_limit')); return; }
    const previews = recovery?.previews || await Promise.all(item.draft.uploads.map((_, index) => inputPreview(item, index)));
    const scope = recovery?.scope || await ensureDraftScope();
    recovery ||= {item, owner, previews, scope, selection:state.selectionGeneration, operationId:newOpaqueClientId('recall-input'), draftUploads:state.draft.images.map(image=>image.uploadId)};
    inputRecallRecoveries.set(item.id, recovery);
    const result = await performInputAction('recall', item, { draftScopeId: scope.draftScopeId, draftUploads:recovery.draftUploads, operationId:recovery.operationId, expected_selection_generation:recovery.selection });
    if (!result?.item) return;
    const recalled = result.item;
    restoreTextSteering('', { owner, text: recalled.text });
    rememberInputReferences(recalled, owner);
    const files = previews.map(preview => new File([preview.blob], preview.name || String(inputText('image')), {type:preview.blob.type}));
    inputRecallRecoveries.delete(item.id);
    if (!composerOwns(owner)) { recalledInputFiles.set(owner, [...(recalledInputFiles.get(owner) || []), ...files]); return; }
    recalled.draft.uploads.forEach((uploadId, index) => {
      const preview = previews[index];
      state.draft.images.push({ id: newOpaqueClientId('image'), uploadId, url: URL.createObjectURL(preview.blob), name: files[index].name, bytes: preview.bytes, type: files[index].type, width:0, height:0, status: 'uploaded', file:files[index] });
      URL.revokeObjectURL(preview.url);
      inputPreviews.delete(JSON.stringify([item.id,item.version,index]));
    });
    state.draft.scope = scope; state.draft.clientMessageId = newOpaqueClientId('message'); renderDraft(); dom.prompt.focus();
  } catch (error) { if (composerOwns(owner)) updateRunState(error.message); }
  finally { if (draftEpoch === state.draft.epoch) { state.draft.sending = false; renderDraft(); if (!pendingInputPopup.classList.contains('hidden')) renderPendingInputPopup(); } }
}
function restoreRecalledInputFiles() {
  for (const [owner, files] of recalledInputFiles) {
    if (!composerOwns(owner)) continue;
    if (state.draft.images.length + files.length > MAX_DRAFT_IMAGES) { updateRunState(inputText('merge_limit')); continue; }
    recalledInputFiles.delete(owner); void addDraftFiles(files);
  }
}
