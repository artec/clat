/* Plan text comes from the existing exit_plan_mode tool contract. */
function decoratePlanToolCard(card, name, argsText) {
  if (name !== 'exit_plan_mode' || card.classList.contains('plan-artifact')) return false;
  let args;
  try { args = JSON.parse(argsText); } catch (_) { return false; }
  if (typeof args?.plan !== 'string' || !args.plan.trim()) return false;
  card.classList.add('plan-artifact'); card.open = true;
  const body = card.querySelector('.tool-body');
  const text = el('div', 'rich-text plan-text'); renderMarkdown(text, args.plan);
  const parameters = el('details', 'plan-parameters');
  parameters.append(el('summary', '', () => shellText('tool_parameters')));
  const raw = body.querySelector('.tool-args'); if (raw) parameters.append(raw);
  body.prepend(text, copyTextButton(CLATI18n.msg('workbench.copy_plan'), () => args.plan), parameters);
  const label = card.querySelector('summary span');
  CLATI18n.bind(label, 'textContent', () => shellText('proposed_plan'));
  return true;
}
