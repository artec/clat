/* Only the Host owns schedules, authorization and execution. UI holds drafts. */
function scheduleTriggerText(trigger) {
  const msg = (key, args) => CLATI18n.msg(`schedule.${key}`, args);
  const date = ms => new Intl.DateTimeFormat(CLATI18n.locale, {dateStyle:'medium',timeStyle:'long'}).format(new Date(ms));
  if (trigger.kind === 'job') return msg('time_job', {id:trigger.job_id});
  const r = trigger.rule;
  if (r.kind === 'at') return msg('time_at', {time:date(r.at_ms)});
  if (r.kind === 'interval') return msg('time_interval', {seconds:new Intl.NumberFormat(CLATI18n.locale).format(r.every_ms / 1000),time:date(r.anchor_ms)});
  const clock = `${String(r.hour).padStart(2,'0')}:${String(r.minute).padStart(2,'0')}`;
  if (r.kind === 'daily') return msg('time_daily', {clock,timezone:r.timezone});
  const day = new Intl.DateTimeFormat(CLATI18n.locale, {weekday:'long',timeZone:'UTC'}).format(new Date(Date.UTC(2024,0,r.weekday)));
  return msg('time_weekly', {day,clock,timezone:r.timezone});
}
function appendHostInteraction(card, session) {
  if (!session) return appendTranscript(card);
  const container = document.querySelector('.schedule-interactions');
  card.prepend(el('small', '', () => `${CLATI18n.msg('schedule.title')} · ${session}`));
  container.append(card);
  const trigger = document.querySelector('#schedules-open'); trigger.dataset.pending = 'true';
}
function installScheduledWork() {
  const msg = (key) => CLATI18n.msg(`schedule.${key}`);
  const button = (key, action) => { const b = el('button', '', () => msg(key)); b.type = 'button'; b.onclick = action; return b; };
  const trigger = sidebarToolButton('schedules-open', 'schedule', () => msg('open'));
  trigger.onclick = () => { previousFocus = document.activeElement; dialog.showModal(); void load(); };
  document.querySelector('.sidebar-footer').prepend(trigger);
  const dialog = el('dialog', 'history-dialog'); dialog.id = 'schedules-dialog';
  const head = el('div', 'history-dialog-head'); head.append(el('h2', '', () => msg('title')), button('refresh', () => void load()), button('close', () => dialog.close()));
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const list = el('div', 'jobs-list'); const form = el('form', 'schedule-form');
  const editor = el('details', ''); editor.append(el('summary', '', () => msg('create')), form);
  const review = el('section', 'schedule-review'); review.hidden = true;
  const deliveries = el('details', ''); const records = el('pre', 'history-json'); deliveries.append(el('summary', '', () => msg('deliveries')), records);
  const interactions = el('section', 'schedule-interactions');
  dialog.append(head, status, interactions, editor, review, list, deliveries); document.body.append(dialog);
  let generation = 0, previousFocus = null, editing = null, job = null;
  const fields = {};
  const fail = (e) => CLATI18n.bind(status, 'textContent', () => e.presentation ? CLATI18n.present(e.presentation) : e.message);
  function field(key, type = 'text', value = '') {
    const label = el('label', ''); label.append(el('span', '', () => msg(key)));
    const input = document.createElement(key === 'instruction' ? 'textarea' : 'input');
    if (input.tagName === 'INPUT') input.type = type; input.value = value; input.name = key;
    if (type === 'number') { input.min = '0'; input.step = '1'; }
    label.append(input); form.append(label); fields[key] = input; return input;
  }
  field('instruction');
  const select = document.createElement('select'); select.name = 'trigger'; fields.trigger = select;
  for (const key of ['delay', 'at', 'interval', 'daily', 'weekly']) { const option = el('option', '', () => msg(key)); option.value = key; select.append(option); }
  const label = el('label', ''); label.append(el('span', '', () => msg('trigger')), select); form.append(label);
  field('time', 'datetime-local'); field('delay', 'number', '10'); field('interval', 'number', '60'); field('clock', 'time', '09:00');
  field('timezone', 'text', Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'); field('weekday', 'number', '1');
  for (const [key, value] of Object.entries({ max_runs: '1', max_tokens: '100000', max_seconds: '300', run_tokens: '100000', run_seconds: '300', max_paid_web_calls: '0', run_paid_web_calls: '0' })) field(key, 'number', value);
  field('model_profile');
  const save = button('create', () => void saveDraft()); const reset = button('cancel', () => { editing = null; job = null; editor.open = false; }); form.append(save, reset);
  form.onsubmit = (event) => { event.preventDefault(); void saveDraft(); };
  function visibility() {
    const kind = select.value;
    for (const key of ['trigger', 'time', 'delay', 'interval', 'clock', 'timezone', 'weekday']) {
      const visible = !job && (key === 'trigger' || key === kind || ['daily', 'weekly'].includes(kind) && ['clock', 'timezone'].includes(key) || kind === 'at' && key === 'time' || kind === 'weekly' && key === 'weekday');
      fields[key].closest('label').hidden = !visible;
    }
  }
  select.onchange = visibility; visibility();
  function budget() {
    return Object.fromEntries(['max_runs', 'max_tokens', 'max_seconds', 'run_tokens', 'run_seconds', 'max_paid_web_calls', 'run_paid_web_calls'].map(key => {
      const value = Number(fields[key].value); if (!Number.isSafeInteger(value) || value < 0) throw CLATI18n.error(msg('invalid_budget')); return [key, value];
    }));
  }
  function timeRule() {
    const now = Date.now(); const kind = select.value;
    if (kind === 'delay') return { kind: 'at', at_ms: now + Number(fields.delay.value) * 60000 };
    if (kind === 'at') return { kind, at_ms: new Date(fields.time.value).getTime() };
    if (kind === 'interval') return { kind, anchor_ms: now + Number(fields.interval.value) * 60000, every_ms: Number(fields.interval.value) * 60000 };
    const [hour, minute] = fields.clock.value.split(':').map(Number);
    const rule = { kind, hour, minute, timezone: fields.timezone.value };
    if (kind === 'weekly') rule.weekday = Number(fields.weekday.value); return rule;
  }
  async function saveDraft() {
    try {
      const draft = { session: job?.session || editing?.scope.session || state.sessionId, instruction: fields.instruction.value,
        trigger: job ? { kind: 'job', job_id: job.id } : { kind: 'time', rule: timeRule() }, budget: budget(), model_profile: fields.model_profile.value.trim() || null };
      const result = editing ? await rpc('schedule.update', { id: editing.id, revision: editing.revision, draft }) : await rpc('schedule.create', draft);
      editing = null; job = null; editor.open = false; visibility(); await load(); await showReview(result.rule);
    } catch (e) { fail(e); }
  }
  function edit(rule) {
    editing = rule; job = rule.spec.trigger.kind === 'job' ? { id: rule.spec.trigger.job_id, session: rule.scope.session } : null;
    fields.instruction.value = rule.spec.instruction; fields.model_profile.value = rule.spec.model.profile || '';
    for (const [key, value] of Object.entries(rule.spec.budget)) fields[key].value = value;
    const time = rule.spec.trigger.rule;
    if (time) {
      select.value = time.kind;
      if (time.kind === 'at') { const d = new Date(time.at_ms); d.setMinutes(d.getMinutes() - d.getTimezoneOffset()); fields.time.value = d.toISOString().slice(0, 16); }
      if (time.kind === 'interval') fields.interval.value = time.every_ms / 60000;
      if (time.timezone) { fields.timezone.value = time.timezone; fields.clock.value = `${String(time.hour).padStart(2, '0')}:${String(time.minute).padStart(2, '0')}`; }
      if (time.weekday) fields.weekday.value = time.weekday;
    }
    editor.open = true; visibility(); fields.instruction.focus();
  }
  async function showReview(rule) {
    try {
      const result = await rpc('schedule.review', { id: rule.id, revision: rule.revision }); const r = result.review;
      review.replaceChildren(el('p', '', () => msg('review_note')));
      review.append(el('p', '', () => scheduleTriggerText(r.rule.spec.trigger)));
      // Protocol values stay verbatim; headings and actions follow live locale.
      for (const [key, value] of [['instruction', r.rule.spec.instruction], ['trigger', JSON.stringify({ session: r.rule.scope.session, project: r.rule.scope.project, trigger: r.rule.spec.trigger }, null, 2)], ['model_profile', JSON.stringify(r.rule.spec.model, null, 2)]]) {
        review.append(el('h3', '', () => msg(key)), el('pre', 'history-json', value));
      }
      for (const [key, value] of Object.entries(r.rule.spec.budget)) review.append(el('p', '', () => `${msg(key)}: ${new Intl.NumberFormat(CLATI18n.locale).format(value)}`));
      const confirm = button('confirm', async () => {
        confirm.disabled = true;
        try { await rpc('schedule.confirm', { id: r.rule.id, revision: r.rule.revision, nonce: r.nonce, digest: r.digest }); review.hidden = true; await load(); }
        catch (e) { fail(e); confirm.disabled = false; }
      });
      review.append(confirm, button('cancel', () => { review.hidden = true; })); review.hidden = false;
    } catch (e) { fail(e); }
  }
  function card(rule) {
    const row = el('article', 'history-hit'); row.append(el('strong', '', rule.spec.instruction), el('span', '', () => msg(rule.phase)), el('small', '', `${rule.scope.session} · ${rule.id}`));
    row.append(el('p','',()=>scheduleTriggerText(rule.spec.trigger)));
    if (rule.blocked) row.append(el('p', 'settings-note', () => CLATI18n.msg(rule.blocked.startsWith('schedule.') ? rule.blocked : `schedule.${rule.blocked}`)));
    const data = el('details', ''); data.append(el('summary', '', () => msg('trigger')), el('pre', 'history-json', JSON.stringify({ trigger: rule.spec.trigger, next_at: rule.next_at, budget: rule.spec.budget, used: rule.used }, null, 2))); row.append(data);
    const actions = [['edit', () => edit(rule)], [rule.phase === 'enabled' ? 'pause' : 'resume', () => void change(rule, rule.phase === 'enabled' ? 'pause' : 'resume')], ['delete', () => { if (window.confirm(msg('delete_confirm'))) void change(rule, 'delete'); }]];
    if (rule.phase !== 'enabled') actions.unshift(['review', () => void showReview(rule)]);
    for (const [key, action] of actions) row.append(button(key, action)); return row;
  }
  async function change(rule, operation) {
    try { await rpc(`schedule.${operation}`, { id: rule.id, revision: rule.revision }); await load(); } catch (e) { fail(e); }
  }
  async function load() {
    const request = ++generation;
    try {
      const [rules, log] = await Promise.all([rpc('schedule.list', {}), rpc('schedule.deliveries', {})]);
      if (request !== generation || !dialog.open) return;
      list.replaceChildren(...rules.rules.map(card)); if (!rules.rules.length) list.append(el('p', '', () => msg('empty')));
      records.textContent = JSON.stringify(log.deliveries, null, 2);
    } catch (e) { if (request === generation) fail(e); }
  }
  window.addEventListener('clat:job-authorize', event => {
    previousFocus = document.activeElement; if (!dialog.open) dialog.showModal(); job = event.detail; editing = null;
    fields.max_runs.value = '1'; fields.instruction.value = ''; editor.open = true; visibility(); fields.instruction.focus(); void load();
  });
  window.addEventListener('clat:background-run', event => { if (dialog.open && event.detail.phase !== 'event') void load(); });
  dialog.addEventListener('close', () => { generation++; previousFocus?.focus(); });
}
