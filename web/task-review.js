function installTaskReview() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("web.tasks", {}, "Tasks"))); trigger.type = 'button'; trigger.id = 'tasks-open';
  document.getElementById('review-open').after(trigger);
  const dialog = el('dialog', 'workspace-review'); dialog.id = 'task-review'; CLATI18n.attribute(dialog,'aria-label',()=>CLATI18n.msg("web.current_run_processes", {}, "Current run processes"));
  const header = el('div', 'review-header'); const refresh = el('button', 'ghost', () => (CLATI18n.msg("web.refresh", {}, "Refresh")));
  const close = el('button', 'icon-button', () => ('×')); CLATI18n.attribute(close,'aria-label',()=>CLATI18n.msg("web.close_tasks", {}, "Close tasks"));
  header.append(el('h2', '', () => (CLATI18n.msg("web.current_run_processes", {}, "Current run processes"))), refresh, close);
  const status = el('p', 'picker-hint'); status.setAttribute('role', 'status');
  const body = el('div', 'review-body'); const list = el('nav', 'review-files'); const detail = el('section', 'review-patch');
  body.append(list, detail); dialog.append(header, status, body); document.body.append(dialog);
  let generation = 0, logGeneration = 0;
  const owner = () => [state.stream, state.selectionGeneration, workspacePrefix];
  async function load() {
    const request = ++generation; const captured = owner(); logGeneration += 1;
    const current = () => dialog.open && request === generation && captured.every((v, i) => v === owner()[i]);
    list.replaceChildren(); detail.replaceChildren(); CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.reading_host_process_facts", {}, "Reading host process facts…"));
    try {
      const result = await rpc('tasks.list', {}); if (!current()) return;
      CLATI18n.bind(status, "textContent", () => CLATI18n.present(result.presentation,result.note));
      if (!result.tasks.length) list.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.no_retained_processes_for_the_selected_current_run_not_a_history_or_background_job", {}, "No retained processes for the selected current run. Not a history or background job list."))));
      for (const task of result.tasks) {
        const button = el('button', 'review-file'); button.type = 'button';
        button.append(el('span', '', () => (CLATI18n.msg("web.dynamic_value_value_exit_value", {value1:(task.id),value2:(task.state),value3:(task.exit_code ?? '—')}, "#{value1} · {value2} · exit {value3}"))), el('small', '', () => (task.command)));
        button.addEventListener('click', async () => {
          const logRequest = ++logGeneration;
          try {
            const logs = await rpc('tasks.logs', { generation: result.generation, id: task.id });
            if (!current() || logRequest !== logGeneration) return;
            detail.replaceChildren(el('h3', '', () => (logs.command)), el('p', 'picker-hint', () => (CLATI18n.msg("web.dynamic_session_value_generation_value_value_exit_value", {value1:(logs.session),value2:(logs.generation),value3:(logs.state),value4:(logs.exit_code ?? '—')}, "Session {value1} · generation {value2} · {value3} · exit {value4}"))));
            for (const name of ['stdout', 'stderr', 'pty']) {
              detail.append(el('h4', '', () => (name + (logs[name].truncated ? CLATI18n.msg("web.logs_tail",{}," · bounded tail / earlier omitted") : ''))), el('pre', 'review-diff', () => (logs[name].text || CLATI18n.msg("web.empty_log"))));
            }
          } catch (error) { if (current() && logRequest === logGeneration) detail.replaceChildren(el('p', 'error', () => (error.message))); }
        }); list.append(button);
      }
    } catch (error) { if (current()) CLATI18n.bind(status, "textContent", () => error.message); }
  }
  trigger.addEventListener('click', () => { dialog.showModal(); void load(); }); refresh.addEventListener('click', () => { void load(); });
  close.addEventListener('click', () => dialog.close()); dialog.addEventListener('close', () => { generation += 1; logGeneration += 1; focusWorkbenchTool(trigger); });
}

function focusWorkbenchTool(trigger) {
  const menu = trigger.closest('#workbench-tools');
  if (menu?.classList.contains('hidden')) document.getElementById('tools-open').focus();
  else trigger.focus();
}

function installWorkbenchTools() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("tui.tools", {}, "Tools"))); trigger.type = 'button'; trigger.id = 'tools-open'; trigger.setAttribute('aria-expanded', 'false'); trigger.setAttribute('aria-controls', 'workbench-tools');
  const menu = el('div', 'workbench-tools hidden'); menu.id = 'workbench-tools'; CLATI18n.attribute(menu,'aria-label',()=>CLATI18n.msg("web.workbench_tools", {}, "Workbench tools"));
  document.getElementById('review-open').before(trigger, menu);
  for (const id of ['review-open', 'turn-review-open', 'files-open', 'workflow-open', 'tasks-open', 'help-open']) {
    const button = document.getElementById(id);
    button.className = 'ghost'; if (id === 'review-open') CLATI18n.bind(button, "textContent", () => CLATI18n.msg("web.workspace_changes", {}, "Workspace changes")); menu.append(button);
  }
  const close = () => { menu.classList.add('hidden'); trigger.setAttribute('aria-expanded', 'false'); };
  trigger.addEventListener('click', () => { const opened = menu.classList.toggle('hidden'); trigger.setAttribute('aria-expanded', String(!opened)); });
  menu.addEventListener('click', (event) => { if (event.target.closest('button')) close(); });
  menu.addEventListener('keydown', (event) => { if (event.key === 'Escape') { event.preventDefault(); close(); trigger.focus(); } });
  document.addEventListener('click', (event) => { if (!document.querySelector('dialog[open]') && !menu.contains(event.target) && event.target !== trigger) close(); });
}
