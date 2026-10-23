// Host-owned, process-local queue. Recall prepends without overwriting drafts.
let nextTurnBusy = false;
let nextTurnRetry = null;
let nextTurnRecallRetry = null;
function renderNextTurnQueue(items) {
  dom['next-turn-queue'].replaceChildren();
  for (const [index, item] of items.entries()) {
    const row = el('div', 'queue-item', () => (`${index + 1}. ${item.text}`));
    dom['next-turn-queue'].appendChild(row);
  }
  dom['next-turn-queue'].hidden = !items.length;
  dom['queue-recall'].hidden = !items.length && !nextTurnRecallRetry;
}
async function enqueueNextTurn() {
  if (nextTurnBusy || state.switching || state.compactionActive) return;
  const text = dom.prompt.value;
  if (!state.runActive || !text.trim() || state.draft.images.length) {
    updateRunState(CLATI18n.msg("web.queue_requires_text_during_an_active_run_images_remain_in_the_composer", {}, "Queue requires text during an active run; images remain in the composer"));
    return;
  }
  const owner = composerScope, generation = state.composerGeneration;
  if (!nextTurnRetry || nextTurnRetry.text !== text || nextTurnRetry.owner !== owner) {
    nextTurnRetry = { text, owner, clientMessageId: newOpaqueClientId('queue') };
  }
  nextTurnBusy = true;
  try {
    await rpc('queue.enqueue', { text, clientMessageId: nextTurnRetry.clientMessageId, expected_selection_generation: state.selectionGeneration });
    clearOwnedSubmittedText(owner, generation);
    nextTurnRetry = null;
    await refreshWorkbench();
  } catch (error) { updateRunState(CLATI18n.msg("web.line_queue_failed_value",{value1:(error.message)},"queue failed: {value1}")); }
  finally { nextTurnBusy = false; }
}
async function recallNextTurn() {
  if (nextTurnBusy || state.switching) return;
  nextTurnRecallRetry ||= { id: newOpaqueClientId('recall'), owner: composerScope, selection: state.selectionGeneration };
  const retry = nextTurnRecallRetry;
  nextTurnBusy = true;
  try {
    const value = await rpc('queue.recall', { clientMessageId: retry.id, expected_selection_generation: retry.selection });
    nextTurnRecallRetry = null;
    if (value.item) restoreTextSteering('', { owner: retry.owner, text: value.item.text });
    await refreshWorkbench();
  } catch (error) { dom['queue-recall'].hidden = false; updateRunState(CLATI18n.msg("web.line_recall_failed_retry_to_recover_value",{value1:(error.message)},"recall failed; retry to recover: {value1}")); }
  finally { nextTurnBusy = false; }
}
function installNextTurn() {
dom['queue-enqueue'].addEventListener('click', enqueueNextTurn);
dom['queue-recall'].addEventListener('click', recallNextTurn);
dom.prompt.addEventListener('keydown', (event) => {
  if (event.key.toLowerCase() !== 'q') return;
  if (event.ctrlKey && !event.altKey) { event.preventDefault(); enqueueNextTurn(); }
  else if (event.altKey && !event.ctrlKey) { event.preventDefault(); recallNextTurn(); }
});

}
