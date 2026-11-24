function contextUsageReport(snapshot, format) {
  const unit = snapshot.unit || CLATI18n.msg('context.units');
  const amount = key => Number.isFinite(snapshot[key]) ? format(snapshot[key]) : '—';
  const estimate = key => `${amount(key)} ${unit}`;
  const injected = key => CLATI18n.msg(snapshot[key] > 0 ? 'web.injected' : 'web.not_injected');
  const metric = (key, value = () => estimate(key)) => ({ key, value });
  return {
    unit,
    totals: ['input', 'output_reserve', 'total'].map(key => metric(key, () => amount(key))),
    input: [
      metric('base_prompt'),
      metric('project_instructions'),
      ...(Number.isFinite(snapshot.runtime_context) ? [metric('runtime_context')] : []),
      metric('plan_policy', () => `${estimate('plan_policy')} · ${injected('plan_policy')}`),
      metric('skill_catalog'),
      metric('goal_policy', () => `${estimate('goal_policy')} · ${injected('goal_policy')}`),
      metric('memory', () => `${CLATI18n.msg('tui.bytes_ratio', {
        value: amount('memory'), budget: amount('memory_budget_bytes'),
      })} · ${injected('memory')}`),
      metric('tool_schemas'),
      metric('history'),
    ],
    images: [
      metric('image_count', () => amount('image_count')),
      metric('image_original_count', () => amount('image_original_count')),
      metric('image_offloaded_count', () => amount('image_offloaded_count')),
      metric('image_bytes', () => CLATI18n.msg('context.bytes', { value: amount('image_bytes') })),
      metric('image_tokens'),
      metric('image_safety_factor', () => `${amount('image_safety_factor')}×`),
    ],
  };
}

function contextTechnicalLines(snapshot) {
  const names = list => Array.isArray(list) && list.length ? list.join(', ') : CLATI18n.msg('context.none');
  const diagnostics = Array.isArray(snapshot.skill_diagnostics) ? snapshot.skill_diagnostics : [];
  return [
    CLATI18n.msg('web.dynamic_tools_value', { value1: names(snapshot.tools) }),
    CLATI18n.msg('web.dynamic_skills_value', { value1: names(snapshot.skills) }),
    ...(diagnostics.length ? [CLATI18n.msg('web.skill_diagnostics'), ...diagnostics.map(item =>
      `! ${item?.source || '-'} / ${item?.name || '-'} / ${item?.kind || '-'}: ${item?.message || '-'}`,
    )] : [CLATI18n.msg('web.skill_diagnostics_none')]),
    ...(snapshot.estimator ? [CLATI18n.msg('web.dynamic_estimator_value', { value1: snapshot.estimator })] : []),
  ];
}

function formatContextSnapshot(snapshot) {
  snapshot = snapshot || {};
  const { unit, input, images, totals } = contextUsageReport(snapshot, String);
  const line = ({ key, value }, suffix = '') => `${CLATI18n.msg(`context.${key}`)}: ${value()}${suffix}`;
  return [
    CLATI18n.msg('web.dynamic_context_estimate_value', { value1: unit }),
    ...input.map(row => line(row)),
    ...images.map(row => line(row)),
    ...[totals[1], totals[0], totals[2]].map(row => line(row, ` ${unit}`)),
    ...contextTechnicalLines(snapshot),
  ].join('\n');
}

function contextMetricList(metrics, className, unit) {
  const list = el('dl', className);
  for (const { key, value } of metrics) {
    const row = el('div', 'context-metric');
    row.dataset.contextMetric = key;
    const description = el('dd');
    description.append(el('span', 'context-metric-value', () => CLATI18n.view(value)));
    if (unit) description.append(el('small', 'context-metric-unit', () => unit));
    row.append(el('dt', '', () => CLATI18n.msg(`context.${key}`)), description);
    list.append(row);
  }
  return list;
}

function renderContextUsageReport(content, snapshot) {
  const report = contextUsageReport(snapshot, value => value.toLocaleString(CLATI18n.locale));
  const groups = el('div', 'context-usage-groups');
  for (const [heading, metrics] of [['input_breakdown', report.input], ['image_breakdown', report.images]]) {
    const section = el('section');
    section.append(el('h3', '', () => CLATI18n.msg(`context.${heading}`)), contextMetricList(metrics, 'context-usage-metrics'));
    groups.append(section);
  }
  const technical = el('details', 'context-usage-technical');
  technical.append(el('summary', '', () => CLATI18n.msg('view.technical')),
    el('pre', '', () => contextTechnicalLines(snapshot).join('\n')));
  const hint = snapshot.input_calibrated === false ? 'experience.estimate_uncalibrated' : 'experience.estimate';
  content.append(el('p', 'context-usage-hint', () => CLATI18n.msg(hint)),
    contextMetricList(report.totals, 'context-usage-totals', report.unit), groups, technical);
}

let contextUsageView = null;
function syncContextUsageReader() {
  if (contextUsageView && !contextUsageView.current()) contextUsageView.dialog.close();
}

function showContextUsageReader(snapshot, source, current) {
  const dialog = createReaderDialog({
    id: 'context-usage-reader',
    title: () => CLATI18n.msg('context.usage'),
    copySource: () => formatContextSnapshot(snapshot),
    onClose: () => {
      dialog.remove();
      if (contextUsageView?.dialog !== dialog) return;
      contextUsageView = null;
      if (current()) source.focus({ preventScroll: true });
    },
  });
  renderContextUsageReport(dialog.querySelector('.user-reader-content'), snapshot);
  contextUsageView = { dialog, current };
  dialog.showModal();
}
