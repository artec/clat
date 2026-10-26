function installFileBrowser() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("web.files", {}, "Files"))); trigger.type = 'button'; trigger.id = 'files-open';
  document.getElementById('review-open').after(trigger);
  const browser = createProjectFileBrowser({ id: 'file-browser', onClose: quoted => {
    if (quoted) { if(!borrowedConversation)workbenchShell?.chat(); dom.prompt.focus(); } else focusWorkbenchTool(trigger);
  } });
  document.body.append(browser.element);
  trigger.addEventListener('click', browser.open);
}

function createProjectFileBrowser({ id, inline = false, onClose = () => {} }) {
  const dialog = el(inline ? 'section' : 'dialog', inline ? 'project-files' : 'workspace-review'); dialog.id = id;
  CLATI18n.attribute(dialog,'aria-label',()=>CLATI18n.msg("web.project_files", {}, "Project files"));
  const header = el('div', 'review-header'); const query = el('input', ''); query.type = 'search'; CLATI18n.bind(query, "placeholder", () => CLATI18n.msg("web.find_project_file", {}, "Find project file")); CLATI18n.attribute(query,'aria-label',()=>CLATI18n.msg("web.find_project_file", {}, "Find project file"));
  const close = el('button', 'icon-button', () => ('×')); CLATI18n.attribute(close,'aria-label',()=>CLATI18n.msg("web.close_project_files", {}, "Close project files"));
  header.append(el('h2', '', () => (CLATI18n.msg("web.files", {}, "Files"))), query, close);
  const status = el('p', 'picker-hint'); status.setAttribute('role', 'status');
  const list = el('nav', 'review-files'); const detail = el('section', 'review-patch'); const body = el('div', 'review-body');
  body.append(list, detail); dialog.append(header, status, body);
  if (inline) close.remove();
  let generation = 0, fileGeneration = 0, quoted = false, active = false, directory = '', selectedPath = null;
  const isOpen = () => inline ? active : dialog.open;
  const finish = () => { if(inline){active=false;generation++;fileGeneration++;onClose(quoted);quoted=false;}else dialog.close(); };
  const owner = () => [state.stream, state.selectionGeneration, workspacePrefix, composerScope];
  const owns = (captured) => captured.every((v, i) => v === owner()[i]);
  async function search() {
    const request = ++generation; const captured = owner(); fileGeneration += 1;
    list.replaceChildren(); detail.replaceChildren(); CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.searching_bounded_project_scope", {}, "Searching bounded project scope…"));
    try {
      const searching = Boolean(query.value.trim());
      const result = await rpc(searching ? 'files.search' : 'files.list', searching ? {query:query.value} : directory?{directory}:{});
      if (!isOpen() || request !== generation || !owns(captured)) return;
      CLATI18n.bind(status, "textContent", () => result.truncated ? CLATI18n.msg("web.partial_results_scan_time_file_limit_reached_narrow_your_query", {}, "Partial results · scan/time/file limit reached. Narrow your query.") : CLATI18n.msg("web.current_project_sensitive_private_paths_excluded", {}, "Current project · sensitive/private paths excluded"));
      const entries = result.entries || result.paths.map(path=>({path,name:path,directory:false}));
      if (directory && !searching) list.append(pageAction('parent_folder',()=>{directory=directory.split('/').slice(0,-1).join('/');void search();}));
      if (!entries.length) list.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.no_files_in_searched_scope", {}, "No files in searched scope."))));
      for (const entry of entries) {
        const path = entry.path;
        const button = el('button', 'review-file', () => entry.name);
        if(entry.directory)button.prepend(svgIcon('folder')); button.type = 'button';
        button.addEventListener('click', () => { if(entry.directory){directory=path;query.value='';void search();}else void preview(path, captured); }); list.append(button);
      }
    } catch (error) { if (isOpen() && request === generation && owns(captured)) CLATI18n.bind(status, "textContent", () => error.message); }
  }
  async function preview(path, captured, start = 1, end = 200) {
    selectedPath={path,start,end};
    const request = ++fileGeneration; const current = () => isOpen() && request === fileGeneration && owns(captured);
    CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.reading_selected_file_snapshot", {}, "Reading selected file snapshot…"));
    detail.replaceChildren(el('h3', '', () => (path)), el('p', 'picker-hint', () => (CLATI18n.msg("web.reading", {}, "Reading…"))));
    try {
      const result = await rpc('files.preview', { path, start_line: start, end_line: end }); if (!current()) return;
      CLATI18n.bind(status, "textContent", () => CLATI18n.present(result.presentation,result.note));
      const first = el('input', ''); first.type = 'number'; first.min = '1'; first.value = String(result.start_line); CLATI18n.attribute(first,'aria-label',()=>CLATI18n.msg("web.first_line", {}, "First line"));
      const last = el('input', ''); last.type = 'number'; last.min = '1'; last.value = String(result.end_line); CLATI18n.attribute(last,'aria-label',()=>CLATI18n.msg("web.last_line", {}, "Last line"));
      const refresh = el('button', 'ghost', () => (CLATI18n.msg("web.read_recheck", {}, "Read / recheck"))); const quote = el('button', 'ghost', () => (CLATI18n.msg("web.quote_snapshot_to_draft", {}, "Quote snapshot to draft")));
      refresh.type = quote.type = 'button';
      refresh.dataset.action = 'refresh-file-preview'; quote.dataset.action = 'quote-file-snapshot';
      const controls = el('div', 'file-range'); controls.append(first, last, refresh, quote);
      const modified = () => result.modified_at_ms == null ? CLATI18n.msg("web.modified_time_unavailable", {}, "Modified time unavailable") : CLATI18n.msg("web.dynamic_modified_value", {value1:(new Date(Number(result.modified_at_ms)).toLocaleString(CLATI18n.locale))}, "Modified {value1}");
      detail.replaceChildren(el('h3', '', () => (result.path)), el('p', 'picker-hint', () => (CLATI18n.msg("web.dynamic_read_value_value_valuevalue", {value1:(new Date(Number(result.read_at_ms)).toLocaleString(CLATI18n.locale)),value2:(modified()),value3:(result.version.slice(0, 12)),value4:(result.truncated ? CLATI18n.msg("web.file_prefix",{}," · bounded prefix only") : '')}, "Read {value1} · {value2} · {value3}{value4}"))), controls, el('pre', 'review-diff', () => (result.content)));
      detail.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.preview_may_be_stale_quoting_rechecks_the_selected_version_once_added_it_is_editab", {}, "Preview may be stale. Quoting rechecks the selected version; once added, it is editable fixed text, never a live file reference."))));
      refresh.addEventListener('click', () => { void preview(path, captured, Number(first.value), Number(last.value)); });
      quote.addEventListener('click', async () => {
        quote.disabled = true;
        try {
          const reference = await rpc('files.reference', { path, start_line: Number(first.value), end_line: Number(last.value), version: result.version });
          if (!current() || state.switching || state.compactionActive || dom.prompt.disabled) return;
          dom.prompt.value += (dom.prompt.value ? '\n\n' : '') + reference.reference + '\n';
          state.composerGeneration += 1; saveComposerText(); invalidateSuggestion(); resizePrompt(); quoted = true; queueFileReference(reference); finish(); dom.prompt.focus();
        } catch (error) { if (current()) CLATI18n.bind(status, "textContent", () => error.message); }
        finally { if (current()) quote.disabled = false; }
      });
    } catch (error) { if (current()) detail.replaceChildren(el('h3', '', () => (path)), el('p', 'error', () => (error.message))); }
  }
  query.addEventListener('input', () => { void search(); }); close.addEventListener('click', finish);
  dialog.addEventListener('close', () => { generation += 1; fileGeneration += 1; const submitted = quoted; quoted = false; onClose(submitted); });
  return {
    element: dialog,
    open: () => { if (inline) active = true; else if (!isOpen()) dialog.showModal(); if (!inline) query.focus(); const prior=selectedPath;void search().then(()=>{if(prior&&isOpen())void preview(prior.path,owner(),prior.start,prior.end);}); },
    suspend: () => { if (inline) active = false; generation += 1; fileGeneration += 1; },
  };
}
