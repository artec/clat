const inputOperations = new Set(), inputPreviews = new Map();
const inputSubmissions = new Map();
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
  const shares = [...sessionSharesFor(owner)];
  if (shares.some(share => share.needsRefresh)) throw CLATI18n.error(inputText('share_refresh_required'));
  if (shares.length) params.sessionShares = shares.map(({reference, recent_turns, version}) => ({reference, recent_turns, version}));
  params.clientMessageId ||= newOpaqueClientId('input');
  const signature = JSON.stringify([text, images.map(i => i.uploadId), params.sessionShares]);
  const canonical = composerOwnerAliases.get(owner) || owner;
  let submission = inputSubmissions.get(canonical);
  if (!submission || submission.signature !== signature) {
    submission = { signature, operationId: newOpaqueClientId('input-op'), clientMessageId: params.clientMessageId, selection: state.selectionGeneration };
    inputSubmissions.set(canonical, submission);
  }
  Object.assign(params, { operationId: submission.operationId, clientMessageId: submission.clientMessageId });
  bindFileReferenceSubmission(owner, text, params);
  params.references = (pendingFileReferences.get(owner) || []).filter(ref => text.includes(ref.quote)).map(({path,version,quote}) => ({path,version,quote}));
  params.expected_selection_generation = submission.selection;
  const deferredIntoRun = state.runActive;
  try {
    await rpc('input.submit', params);
  } catch (error) {
    if (error.code === 'busy' && submission.selection !== state.selectionGeneration) {
      forgetInputSubmission(owner, submission);
    }
    throw error;
  }
  forgetInputSubmission(owner, submission);
  const retained = sessionSharesFor(owner);
  for (const share of shares) { const index = retained.indexOf(share); if (index >= 0) retained.splice(index, 1); }
  if (composerOwns(owner)) renderSessionShareDraft();
  // INV-OE1：run 进行中的提交是 deferred 插话——立即乐观回显，等下一
  // 模型请求边界 claim（steering_applied）原地转正。等待期间 run 已终结
  // 则不回显：条目转为空闲队列，由下一 run_started 正常入转录。
  if (deferredIntoRun && state.runActive) echoOptimisticSteering(params.clientMessageId, text);
  if (composerOwns(owner)) {
    clearOwnedSubmittedText(owner, composerGeneration);
    if (images.length && draftEpoch === state.draft.epoch) clearDraft();
    showAcceptedInputFeedback();
    void refreshPendingInputs();
  }
}

function forgetInputSubmission(owner, submission) {
  const canonical = composerOwnerAliases.get(owner) || owner;
  if (inputSubmissions.get(canonical) === submission) inputSubmissions.delete(canonical);
}

/// 事故修复（2026-11-08 插话冻结事故）：82d7b3df 引入了对本函数的调用
/// 但未随附定义，steering_applied 处理链在第一行即 ReferenceError；异常
/// 沿 openStream 泵循环 reject 整个事件流 → connect 重连 → 全量重放，
/// 形成「重连→重放→再抛」风暴（会话运行中↔停止横跳、UI 与输入队列冻
/// 结、host serve 层被重放锤死需强杀）。
/// 语义按 owner 红测试 "queued input feedback clears on receipt or
/// expiry without hiding newer status"：accepted 提示在 steering_applied
///（任意 receipt——队列在移动即提示已过时）时清除，或 5s 自动过期；
/// 清除与过期都只作用于仍处 input.accepted 的提示，较新状态（如
/// input.failed / interrupting / 错误文本）永不被掩盖。
let acceptedFeedbackExpiry = null;
const ACCEPTED_FEEDBACK_TTL_MS = 5000;

function acceptedPresentationShowing() {
  return (dom['run-state'].dataset.presentation || '') === 'input.accepted';
}

function showAcceptedInputFeedback() {
  updateRunState(inputText('accepted'));
  if (acceptedFeedbackExpiry) clearTimeout(acceptedFeedbackExpiry);
  acceptedFeedbackExpiry = setTimeout(() => {
    acceptedFeedbackExpiry = null;
    if (acceptedPresentationShowing()) updateRunState('');
  }, ACCEPTED_FEEDBACK_TTL_MS);
}

function dismissAcceptedInputFeedback(clientMessageId) {
  void clientMessageId; // 保留调用方签名：per-input 反馈语义的未来扩展点。
  if (acceptedFeedbackExpiry) {
    clearTimeout(acceptedFeedbackExpiry);
    acceptedFeedbackExpiry = null;
  }
  if (!acceptedPresentationShowing()) return;
  updateRunState('');
}
async function performInputAction(action, item, extra = {}) {
  if (inputOperations.has(item.id)) return;
  if (action === 'interrupt' && !state.runActive) { await refreshPendingInputs(); return; }
  const owner = composerScope, editor = pendingInputEditor(item.id);
  if(action==='interrupt')optimisticInputInterrupt=item.id;
  const retryKey = JSON.stringify([owner, action, item.id, item.version, extra]);
  if (!inputRetryOperations.has(retryKey)) inputRetryOperations.set(retryKey, newOpaqueClientId('input-op'));
  inputOperations.add(item.id); renderPendingInputPopup();
  try {
    const result = await rpc('input.' + action, { id: item.id, version: item.version, operationId: inputRetryOperations.get(retryKey), expected_selection_generation: state.selectionGeneration, ...extra });
    inputRetryOperations.delete(retryKey);
    if (action === 'update' && editor) pendingInputEditors.delete(editor.key);
    if (action === 'interrupt' && composerOwns(owner) && state.runActive) updateRunState(inputText('interrupting'));
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
    restoreSessionShares(recalled, owner);
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
