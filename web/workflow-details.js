function workflowSections(detail, value) {
  detail.append(el('h3', '', 'Plan Mode (execution policy)'), el('p', '', value.plan_mode ? 'Enabled · read-only planning tools' : 'Disabled'));
  detail.append(el('h3', '', 'Approved plan (durable text)'));
  if (value.approved_plan) detail.append(el('p', 'picker-hint', `Approved event ${value.approved_plan.event_seq} · ${value.approved_plan.digest}`), el('pre', 'review-diff', value.approved_plan.text));
  else detail.append(el('p', '', 'No approved plan.'));
  detail.append(el('h3', '', 'Goal (bounded durable execution)'));
  const goal = value.goal.goal;
  if (!goal) detail.append(el('p', '', 'No current goal.'));
  else {
    detail.append(el('p', '', goal.objective), el('p', 'picker-hint', `${goal.phase} · revision ${goal.revision} · ${value.goal.armed ? 'armed' : 'disarmed'}`));
    detail.append(el('p', '', `Rounds ${goal.roundsStarted}/${goal.limits.maxRounds} · tokens ${goal.tokensUsed}/${goal.limits.maxTokens} · seconds ${(goal.elapsedMs / 1000).toFixed(1)}/${goal.limits.maxTimeSecs} · failures ${goal.failures}/${goal.limits.maxFailures}`));
    detail.append(el('p', '', goal.blockedReason ? `Stop: ${goal.blockedReason.code} · ${goal.blockedReason.message}` : 'No recorded block reason. Budget/phase facts shown above.'));
    if (goal.lastResult) detail.append(el('pre', 'review-diff', goal.lastResult));
    detail.append(el('p', 'picker-hint', 'Acceptance: ' + JSON.stringify(goal.acceptance)));
  }
  detail.append(el('h3', '', 'Todo (model-maintained list; not Goal completion)'));
  if (!value.todos.length) detail.append(el('p', '', 'No todo entries.'));
  for (const todo of value.todos) detail.append(el('p', '', `${todo.status} · ${todo.content}`));
  if (value.todos_truncated) detail.append(el('p', 'picker-hint', 'First 100 todo entries only; remaining entries omitted.'));
}

function installWorkflowDetails() {
  const trigger = el('button', 'ghost', 'Plan / Goal'); trigger.type = 'button'; trigger.id = 'workflow-open';
  document.getElementById('review-open').after(trigger);
  const dialog = el('dialog', 'workflow-details'); dialog.id = 'workflow-details'; dialog.setAttribute('aria-label', 'Plan and Goal details');
  const header = el('div', 'review-header'); const close = el('button', 'icon-button', '×'); close.setAttribute('aria-label', 'Close Plan and Goal details');
  const refresh = el('button', 'ghost', 'Refresh'); header.append(el('h2', '', 'Plan / Goal'), refresh, close);
  const detail = el('section', ''); const status = el('p', 'picker-hint'); status.setAttribute('role', 'status');
  const actions = el('div', 'file-range'); dialog.append(header, status, detail, actions); document.body.append(dialog);
  let generation = 0;
  const owner = () => [state.stream, state.selectionGeneration, workspacePrefix];
  async function load() {
    const request = ++generation; const expected = owner();
    const current = () => dialog.open && request === generation && expected.every((v, i) => v === owner()[i]);
    detail.replaceChildren(); actions.replaceChildren(); status.textContent = 'Reading host workflow facts…';
    try {
      const result = await rpc('workflow.details', {}); if (!current()) return;
      status.textContent = result.note + (result.busy ? ' · Actions unavailable while busy.' : '');
      workflowSections(detail, result);
      const goal = result.goal.goal;
      const options = [ ...(result.plan_mode ? [['Exit Plan Mode', '/plan off']] : []),
        ...(goal ? [['Start bounded continuation', '/goal run'], ['Resume goal state (does not run)', '/goal resume'], ['Clear goal', '/goal clear']] : []) ];
      for (const [label, command] of options) {
        const button = el('button', 'ghost', label); button.type = 'button'; button.disabled = result.busy;
        button.addEventListener('click', async () => {
          if (!current() || !confirm(`${label}? This invokes ${command} on the selected session.${command === '/goal run' ? ' It can execute tools within the existing Goal budget and permissions.' : ''}`)) return;
          button.disabled = true;
          try {
            const value = await rpc('command.run', { command, ...(command.startsWith('/goal') ? { expected_goal_revision: goal.revision, expected_goal_id: goal.id } : {}) });
            if (!current()) return;
            status.textContent = value.message || value.kind; await refreshWorkbench(); await load();
          } catch (error) { if (current()) { status.textContent = error.message; button.disabled = false; } }
        }); actions.append(button);
      }
    } catch (error) { if (current()) status.textContent = error.message; }
  }
  const open = () => { dialog.showModal(); void load(); };
  trigger.addEventListener('click', open); document.getElementById('plan-mode-badge').addEventListener('click', open); document.getElementById('goal-badge').addEventListener('click', open);
  close.addEventListener('click', () => dialog.close()); refresh.addEventListener('click', () => { void load(); });
  dialog.addEventListener('close', () => { generation += 1; focusWorkbenchTool(trigger); });
}
