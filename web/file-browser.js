function installFileBrowser() {
  const trigger = el('button', 'ghost', 'Files'); trigger.type = 'button'; trigger.id = 'files-open';
  document.getElementById('review-open').after(trigger);
  const dialog = el('dialog', 'workspace-review'); dialog.id = 'file-browser'; dialog.setAttribute('aria-label', 'Project files');
  const header = el('div', 'review-header'); const query = el('input', ''); query.type = 'search'; query.placeholder = 'Find project file'; query.setAttribute('aria-label', 'Find project file');
  const close = el('button', 'icon-button', '×'); close.setAttribute('aria-label', 'Close project files');
  header.append(el('h2', '', 'Files'), query, close);
  const status = el('p', 'picker-hint'); status.setAttribute('role', 'status');
  const list = el('nav', 'review-files'); const detail = el('section', 'review-patch'); const body = el('div', 'review-body');
  body.append(list, detail); dialog.append(header, status, body); document.body.append(dialog);
  let generation = 0, fileGeneration = 0, quoted = false;
  const owner = () => [state.stream, state.selectionGeneration, workspacePrefix, composerScope];
  const owns = (captured) => captured.every((v, i) => v === owner()[i]);
  async function search() {
    const request = ++generation; const captured = owner(); fileGeneration += 1;
    list.replaceChildren(); detail.replaceChildren(); status.textContent = 'Searching bounded project scope…';
    try {
      const result = await rpc('files.search', { query: query.value });
      if (!dialog.open || request !== generation || !owns(captured)) return;
      status.textContent = result.truncated ? 'Partial results · scan/time/file limit reached. Narrow your query.' : 'Current project · sensitive/private paths excluded';
      if (!result.paths.length) list.append(el('p', 'picker-hint', 'No files in searched scope.'));
      for (const path of result.paths) {
        const button = el('button', 'review-file', path); button.type = 'button';
        button.addEventListener('click', () => { void preview(path, captured); }); list.append(button);
      }
    } catch (error) { if (dialog.open && request === generation && owns(captured)) status.textContent = error.message; }
  }
  async function preview(path, captured, start = 1, end = 200) {
    const request = ++fileGeneration; const current = () => dialog.open && request === fileGeneration && owns(captured);
    status.textContent = 'Reading selected file snapshot…';
    detail.replaceChildren(el('h3', '', path), el('p', 'picker-hint', 'Reading…'));
    try {
      const result = await rpc('files.preview', { path, start_line: start, end_line: end }); if (!current()) return;
      status.textContent = result.note;
      const first = el('input', ''); first.type = 'number'; first.min = '1'; first.value = String(result.start_line); first.setAttribute('aria-label', 'First line');
      const last = el('input', ''); last.type = 'number'; last.min = '1'; last.value = String(result.end_line); last.setAttribute('aria-label', 'Last line');
      const refresh = el('button', 'ghost', 'Read / recheck'); const quote = el('button', 'ghost', 'Quote snapshot to draft');
      const controls = el('div', 'file-range'); controls.append(first, last, refresh, quote);
      const modified = result.modified_at_ms == null ? 'Modified time unavailable' : `Modified ${new Date(Number(result.modified_at_ms)).toLocaleString()}`;
      detail.replaceChildren(el('h3', '', result.path), el('p', 'picker-hint', `Read ${new Date(Number(result.read_at_ms)).toLocaleString()} · ${modified} · ${result.version.slice(0, 12)}${result.truncated ? ' · bounded prefix only' : ''}`), controls, el('pre', 'review-diff', result.content));
      detail.append(el('p', 'picker-hint', 'Preview may be stale. Quoting rechecks the selected version; once added, it is editable fixed text, never a live file reference.'));
      refresh.addEventListener('click', () => { void preview(path, captured, Number(first.value), Number(last.value)); });
      quote.addEventListener('click', async () => {
        quote.disabled = true;
        try {
          const reference = await rpc('files.reference', { path, start_line: Number(first.value), end_line: Number(last.value), version: result.version });
          if (!current() || state.switching || state.compactionActive || dom.prompt.disabled) return;
          dom.prompt.value += (dom.prompt.value ? '\n\n' : '') + reference.reference + '\n';
          state.composerGeneration += 1; saveComposerText(); invalidateSuggestion(); resizePrompt(); quoted = true; dialog.close(); dom.prompt.focus();
        } catch (error) { if (current()) status.textContent = error.message; }
        finally { if (current()) quote.disabled = false; }
      });
    } catch (error) { if (current()) detail.replaceChildren(el('h3', '', path), el('p', 'error', error.message)); }
  }
  query.addEventListener('input', () => { void search(); }); close.addEventListener('click', () => dialog.close());
  trigger.addEventListener('click', () => { dialog.showModal(); query.focus(); void search(); });
  dialog.addEventListener('close', () => { generation += 1; fileGeneration += 1; if (quoted) { quoted = false; dom.prompt.focus(); } else focusWorkbenchTool(trigger); });
}
