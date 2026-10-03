// Native operation evidence is distinct from the Git working-tree review.
function installTurnReview() {
  const trigger = el('button', 'ghost', 'Captured operations'); trigger.type = 'button'; trigger.id = 'turn-review-open';
  document.getElementById('review-open').after(trigger);
  const dialog = el('dialog', 'workspace-review'); dialog.id = 'turn-review'; dialog.setAttribute('aria-label', 'Captured native file operations');
  const header = el('div', 'review-header'); const refresh = el('button', 'ghost', 'Refresh');
  const close = el('button', 'icon-button', '×'); close.setAttribute('aria-label', 'Close captured operations');
  header.append(el('h2', '', 'Captured operations'), refresh, close);
  const status = el('p', 'picker-hint'); status.setAttribute('role', 'status');
  const body = el('div', 'review-body'); const list = el('nav', 'review-files'); const detail = el('section', 'review-patch');
  const restore = el('button', 'danger-button', 'Restore captured files…'); restore.type = 'button'; restore.disabled = true;
  body.append(list, detail); dialog.append(header, status, body, restore); document.body.append(dialog);
  let request = 0, fileRequest = 0, snapshot = null;
  const owner = () => [state.stream, state.selectionGeneration, workspacePrefix];
  function showFiles(result) {
    list.replaceChildren();
    if (!result.files.length) list.append(el('p', 'picker-hint', 'No captured native operations. This does not prove no workspace changes.'));
    for (const file of result.files) {
      const button = el('button', 'review-file'); button.type = 'button';
      button.append(el('span', '', file.path), el('small', '', `${file.kind} · ${file.status} · ${file.disk_state}`));
      button.addEventListener('click', () => { void load(file.path); }); list.append(button);
    }
  }
  async function load(path = null) {
    const version = path ? request : ++request; const fileVersion = ++fileRequest; const expected = owner();
    const current = () => dialog.open && version === request && fileVersion === fileRequest && expected.every((v, i) => v === owner()[i]);
    if (!path) { list.replaceChildren(); detail.replaceChildren(); restore.disabled = true; snapshot = null; }
    status.textContent = 'Reading native operation evidence…';
    try {
      const result = await rpc('turn.changes', { ...(path ? { turn: snapshot.turn, path } : {}), expected_selection_generation: expected[1] });
      if (!current()) return;
      status.textContent = `Turn ${result.turn} · ${result.note}`;
      if (!path) {
        snapshot = result;
        restore.disabled = state.runActive || state.compactionActive || !result.files.some(f => f.disk_state === 'safe');
        showFiles(result);
      } else {
        detail.replaceChildren(el('h3', '', path));
        const afterLabel = result.preview.status === 'prepared' ? 'Expected native result (commit unconfirmed)' : 'Captured native result (not current disk)';
        const previous = result.preview.status === 'prepared' && result.preview.captured_after != null
          ? [['Last confirmed native result (before unfinished operation)', result.preview.captured_after]] : [];
        for (const [label, text] of [['Pre-operation baseline (includes existing edits)', result.preview.before], ...previous, [afterLabel, result.preview.after]]) {
          detail.append(el('h4', '', label), el('pre', 'review-diff', text === null ? '(did not exist)' : text));
        }
        if (result.preview.truncated) detail.append(el('p', 'picker-hint', 'Preview capped at 128 KiB per side.'));
      }
    } catch (error) { if (current()) status.textContent = error.message; }
  }
  restore.addEventListener('click', async () => {
    if (!snapshot || state.runActive || state.compactionActive) return;
    if (!confirm('Restore only captured native files whose current identity and contents still match? Conflicts stop the batch; results may be partial. Retained recovery material is not automatically deleted. Shell/MCP/external effects are NOT undone.')) return;
    const expected = owner(); const version = request; restore.disabled = true;
    try {
      const result = await rpc('turn.restore', { turn: snapshot.turn, revision: snapshot.revision, confirmed: true, expected_selection_generation: expected[1] });
      if (!dialog.open || version !== request || !expected.every((v, i) => v === owner()[i])) return;
      detail.replaceChildren(el('h3', '', 'Recovery results'));
      for (const file of result.files) detail.append(el('p', '', `${file.path}: ${file.status} ${file.note || ''}${file.recovery_path ? ' · retained: ' + file.recovery_path : ''}`));
      snapshot = result;
      showFiles(result);
      status.textContent = `Turn ${result.turn} · Recovery results; inspect individual statuses and retained material.`;
    } catch (error) { if (dialog.open && version === request && expected.every((v, i) => v === owner()[i])) status.textContent = error.message; }
  });
  trigger.addEventListener('click', () => { dialog.showModal(); void load(); });
  refresh.addEventListener('click', () => { void load(); }); close.addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', () => { request += 1; fileRequest += 1; focusWorkbenchTool(trigger); });
}
