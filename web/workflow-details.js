function workflowSections(detail, value) {
  detail.append(el('h3', '', () => (CLATI18n.msg("web.plan_mode_execution_policy", {}, "Plan Mode (execution policy)"))), el('p', '', () => (value.plan_mode ? CLATI18n.msg("web.enabled_read_only_planning_tools", {}, "Enabled · read-only planning tools") : CLATI18n.msg("web.disabled", {}, "Disabled"))));
  detail.append(el('h3', '', () => (CLATI18n.msg("web.approved_plan_durable_text", {}, "Approved plan (durable text)"))));
  if (value.approved_plan) detail.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.dynamic_approved_event_value_value", {value1:(value.approved_plan.event_seq),value2:(value.approved_plan.digest)}, "Approved event {value1} · {value2}"))), el('pre', 'review-diff', () => (value.approved_plan.text)));
  else detail.append(el('p', '', () => (CLATI18n.msg("web.no_approved_plan", {}, "No approved plan."))));
  detail.append(el('h3', '', () => (CLATI18n.msg("web.goal_bounded_durable_execution", {}, "Goal (bounded durable execution)"))));
  const goal = value.goal.goal;
  if (!goal) detail.append(el('p', '', () => (CLATI18n.msg("web.no_current_goal", {}, "No current goal."))));
  else {
    detail.append(el('p', '', () => (goal.objective)), el('p', 'picker-hint', () => (CLATI18n.msg("web.dynamic_value_revision_value_value", {value1:(goal.phase),value2:(goal.revision),value3:(value.goal.armed ? CLATI18n.msg("web.armed_short",{},"armed") : CLATI18n.msg("web.disarmed_short",{},"disarmed"))}, "{value1} · revision {value2} · {value3}"))));
    detail.append(el('p', '', () => (CLATI18n.msg("web.dynamic_rounds_value_value_tokens_value_value_seconds_value_value_failures_value_value", {value1:(goal.roundsStarted),value2:(goal.limits.maxRounds),value3:(goal.tokensUsed),value4:(goal.limits.maxTokens),value5:((goal.elapsedMs / 1000).toFixed(1)),value6:(goal.limits.maxTimeSecs),value7:(goal.failures),value8:(goal.limits.maxFailures)}, "Rounds {value1}/{value2} · tokens {value3}/{value4} · seconds {value5}/{value6} · failures {value7}/{value8}"))));
    detail.append(el('p', '', () => (goal.blockedReason ? CLATI18n.msg("web.dynamic_stop_value_value", {value1:(goal.blockedReason.code),value2:(goal.blockedReason.message)}, "Stop: {value1} · {value2}") : CLATI18n.msg("web.no_recorded_block_reason_budget_phase_facts_shown_above", {}, "No recorded block reason. Budget/phase facts shown above."))));
    if (goal.lastResult) detail.append(el('pre', 'review-diff', () => (goal.lastResult)));
    detail.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.line_acceptance_value",{value1:(JSON.stringify(goal.acceptance))},"Acceptance: {value1}"))));
  }
  detail.append(el('h3', '', () => (CLATI18n.msg("web.todo_model_maintained_list_not_goal_completion", {}, "Todo (model-maintained list; not Goal completion)"))));
  if (!value.todos.length) detail.append(el('p', '', () => (CLATI18n.msg("web.no_todo_entries", {}, "No todo entries."))));
  for (const todo of value.todos) detail.append(el('p', '', () => (`${todo.status} · ${todo.content}`)));
  if (value.todos_truncated) detail.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.first_100_todo_entries_only_remaining_entries_omitted", {}, "First 100 todo entries only; remaining entries omitted."))));
}

function installWorkflowDetails() {
  const trigger = el('button', 'ghost', () => (CLATI18n.msg("web.plan_goal", {}, "Plan / Goal"))); trigger.type = 'button'; trigger.id = 'workflow-open';
  document.getElementById('review-open').after(trigger);
  const dialog = el('dialog', 'workflow-details'); dialog.id = 'workflow-details'; CLATI18n.attribute(dialog,'aria-label',()=>CLATI18n.msg("web.plan_and_goal_details", {}, "Plan and Goal details"));
  const header = el('div', 'review-header'); const close = el('button', 'icon-button', () => ('×')); CLATI18n.attribute(close,'aria-label',()=>CLATI18n.msg("web.close_plan_and_goal_details", {}, "Close Plan and Goal details"));
  const refresh = el('button', 'ghost', () => (CLATI18n.msg("web.refresh", {}, "Refresh"))); header.append(el('h2', '', () => (CLATI18n.msg("web.plan_goal", {}, "Plan / Goal"))), refresh, close);
  const detail = el('section', ''); const status = el('p', 'picker-hint'); status.setAttribute('role', 'status');
  const actions = el('div', 'file-range'); dialog.append(header, status, detail, actions); document.body.append(dialog);
  let generation = 0;
  const owner = () => [state.stream, state.selectionGeneration, workspacePrefix];
  async function load() {
    const request = ++generation; const expected = owner();
    const current = () => dialog.open && request === generation && expected.every((v, i) => v === owner()[i]);
    detail.replaceChildren(); actions.replaceChildren(); CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.reading_host_workflow_facts", {}, "Reading host workflow facts…"));
    try {
      const result = await rpc('workflow.details', {}); if (!current()) return;
      CLATI18n.bind(status, "textContent", () => CLATI18n.present(result.presentation,result.note) + (result.busy ? CLATI18n.msg("web.busy_actions",{}," · Actions unavailable while busy.") : ''));
      workflowSections(detail, result);
      const goal = result.goal.goal;
      const options = [ ...(result.plan_mode ? [[CLATI18n.msg("web.exit_plan_mode", {}, "Exit Plan Mode"), '/plan off']] : []),
        ...(goal ? [[CLATI18n.msg("web.start_bounded_continuation", {}, "Start bounded continuation"), '/goal run'], [CLATI18n.msg("web.resume_goal_state_does_not_run", {}, "Resume goal state (does not run)"), '/goal resume'], [CLATI18n.msg("web.clear_goal", {}, "Clear goal"), '/goal clear']] : []) ];
      for (const [label, command] of options) {
        const button = el('button', 'ghost', () => (label)); button.type = 'button'; button.disabled = result.busy;
        button.addEventListener('click', async () => {
          if (!current() || !confirm(CLATI18n.msg("web.dynamic_value_this_invokes_value_on_the_selected_session_value", {value1:(label),value2:(command),value3:(command === '/goal run' ? CLATI18n.msg("web.goal_action_warning",{}," It can execute tools within the existing Goal budget and permissions.") : '')}, "{value1}? This invokes {value2} on the selected session.{value3}"))) return;
          button.disabled = true;
          try {
            const value = await rpc('command.run', { command, ...(command.startsWith('/goal') ? { expected_goal_revision: goal.revision, expected_goal_id: goal.id } : {}) });
            if (!current()) return;
            CLATI18n.bind(status, "textContent", () => CLATI18n.present(value.presentation,value.message || value.kind)); await refreshWorkbench(); await load();
          } catch (error) { if (current()) { CLATI18n.bind(status, "textContent", () => error.message); button.disabled = false; } }
        }); actions.append(button);
      }
    } catch (error) { if (current()) CLATI18n.bind(status, "textContent", () => error.message); }
  }
  const open = () => { dialog.showModal(); void load(); };
  trigger.addEventListener('click', open); document.getElementById('plan-mode-badge').addEventListener('click', open); document.getElementById('goal-badge').addEventListener('click', open);
  close.addEventListener('click', () => dialog.close()); refresh.addEventListener('click', () => { void load(); });
  dialog.addEventListener('close', () => { generation += 1; focusWorkbenchTool(trigger); });
}
