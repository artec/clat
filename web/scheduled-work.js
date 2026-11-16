/* Schedules authorize work in the core. Editing and reading have separate owners. */
function scheduleTriggerText(trigger) {
  const msg = (key, args) => CLATI18n.msg('schedule.' + key, args);
  const date = ms => new Intl.DateTimeFormat(CLATI18n.locale, {dateStyle:'medium',timeStyle:'short'}).format(new Date(ms));
  if (trigger.kind === 'job') return pageText('task_job_trigger');
  const r = trigger.rule;
  if (r.kind === 'at') return msg('time_at', {time:date(r.at_ms)});
  if (r.kind === 'interval') return msg('time_interval', {seconds:String(r.every_ms / 1000),time:date(r.anchor_ms)});
  const clock = `${String(r.hour).padStart(2,'0')}:${String(r.minute).padStart(2,'0')}`;
  if (r.kind === 'daily') return msg('time_daily', {clock,timezone:r.timezone});
  const day = scheduleWeekday(r.weekday);
  return msg('time_weekly', {day,clock,timezone:r.timezone});
}
function appendHostInteraction(card, session) {
  if (!session) return appendTranscript(card);
  const container = document.querySelector('.schedule-interactions');
  card.prepend(el('small', '', () => `${CLATI18n.msg('schedule.title')} · ${session}`)); container.append(card);
  document.getElementById('nav-tasks')?.setAttribute('data-pending','true');
}
function createScheduleEditor() {
  const dialog = el('dialog', 'workbench-dialog task-editor'); dialog.id = 'schedule-editor';
  const head = el('header', 'workbench-dialog-header');
  const title = el('h2'); title.id = 'schedule-editor-title';
  dialog.setAttribute('aria-labelledby', title.id);
  const close = navigationIcon(() => CLATI18n.msg('common.close'), 'close', () => dialog.close());
  close.classList.add('workbench-dialog-close'); head.append(title, close);
  const form = el('form', 'schedule-form'); form.id = 'schedule-form';
  const timing = el('fieldset', 'task-timing'); timing.append(el('legend', '', () => pageText('task_timing')));
  const fields = {};
  function field(key, label, parent, type = 'text', value = '') {
    const row = el('label', parent === timing ? 'task-setting-row' : '');
    const input = el(type === 'textarea' || type === 'select' ? type : 'input');
    if (input.tagName === 'INPUT') input.type = type;
    input.name = key; input.value = value;
    row.append(el('span', '', () => CLATI18n.msg(label)), input);
    if (type === 'number') { input.min = '1'; input.step = '1'; }
    fields[key] = input; parent.append(row); return input;
  }
  const name = field('title', 'pages.title', form);
  name.maxLength = 256; name.autofocus = true;
  const instructions = field('instruction', 'pages.task_instructions', form, 'textarea');
  instructions.required = true; instructions.maxLength = 16384;
  CLATI18n.attribute(instructions, 'placeholder', () => pageText('task_prompt_hint'));
  const trigger = field('trigger', 'pages.task_frequency', timing, 'select');
  for (const kind of ['delay', 'at', 'interval', 'daily', 'weekly']) {
    const option = el('option', '', () => CLATI18n.msg((kind === 'delay' || kind === 'interval' ? 'pages.task_' : 'schedule.') + kind));
    option.value = kind; trigger.append(option);
  }
  field('delay', 'pages.task_delay_minutes', timing, 'number', '10').min = '0';
  field('time', 'schedule.time', timing, 'datetime-local');
  const interval = field('interval', 'pages.task_interval_minutes', timing, 'number', '60');
  interval.min = String(1 / 60); interval.step = 'any';
  const weekday = field('weekday', 'pages.task_weekday', timing, 'select');
  for (let day = 1; day <= 7; day++) {
    const option = el('option', '', CLATI18n.view(() => scheduleWeekday(day))); option.value = day; weekday.append(option);
  }
  field('clock', 'pages.task_clock', timing, 'time', '09:00');
  field('timezone', 'pages.task_timezone', timing, 'text', Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC');
  const jobLabel = el('p', 'task-job-trigger'); jobLabel.hidden = true; timing.append(jobLabel);
  for (const field of Object.values(fields).slice(2)) field.required = true;
  field('session', 'pages.session', timing, 'select').required = true;
  form.append(timing);
  const status = el('p', 'settings-note task-editor-status'); status.setAttribute('role', 'status'); form.append(status);
  const actions = el('footer', 'workbench-dialog-actions');
  dialog.append(head, form, actions); document.body.append(dialog);
  return {dialog, title, form, fields, jobLabel, status, actions};
}
function scheduleWeekday(day) {
  return new Intl.DateTimeFormat(CLATI18n.locale, {weekday:'long', timeZone:'UTC'}).format(new Date(Date.UTC(2024, 0, day)));
}
function scheduleTimeFrom(fields, previous, now) {
  const kind = fields.trigger.value;
  if (kind === 'delay') return {kind:'at', at_ms:now + Number(fields.delay.value) * 60000};
  if (kind === 'at') {
    const time = fields.time.value;
    return {kind, at_ms:previous?.kind === 'at' && scheduleLocalTime(previous.at_ms) === time ? previous.at_ms : new Date(time).getTime()};
  }
  if (kind === 'interval') {
    const every_ms = Math.round(Number(fields.interval.value) * 60000);
    return {kind, every_ms, anchor_ms:previous?.kind === 'interval' && previous.every_ms === every_ms ? previous.anchor_ms : now + every_ms};
  }
  const [hour, minute] = fields.clock.value.split(':').map(Number);
  const rule = {kind, hour, minute, timezone:fields.timezone.value.trim()};
  if (kind === 'weekly') rule.weekday = Number(fields.weekday.value);
  return rule;
}
function scheduleLocalTime(ms) {
  const date = new Date(ms); date.setMinutes(date.getMinutes() - date.getTimezoneOffset());
  return date.toISOString().slice(0,16);
}
function defaultScheduleBudget(trigger) {
  const max_runs = trigger.kind === 'time' && trigger.rule.kind !== 'at' ? 30 : 1;
  const run_tokens = 100000, run_seconds = 300;
  return {max_runs, max_tokens:max_runs * run_tokens, max_seconds:max_runs * run_seconds,
    run_tokens, run_seconds, max_paid_web_calls:0, run_paid_web_calls:0};
}
function scheduleDraftFrom(fields, editing, job) {
  const spec = editing?.spec;
  const trigger = job ? {kind:'job', job_id:job.id} : {kind:'time', rule:scheduleTimeFrom(fields, spec?.trigger.rule, Date.now())};
  return {title:fields.title.value.trim() || null, notification:spec ? spec.notification ?? null : 'attention',
    session:editing?.scope.session || job?.session || fields.session.value, instruction:fields.instruction.value, trigger,
    budget:spec?.budget ?? defaultScheduleBudget(trigger), model_profile:spec?.model.profile ?? null};
}
function fillScheduleTime(fields, rule) {
  if (!rule) return;
  fields.trigger.value = rule.kind;
  if (rule.kind === 'at') fields.time.value = scheduleLocalTime(rule.at_ms);
  if (rule.kind === 'interval') fields.interval.value = String(rule.every_ms / 60000);
  if (rule.timezone) {
    fields.timezone.value = rule.timezone;
    fields.clock.value = `${String(rule.hour).padStart(2,'0')}:${String(rule.minute).padStart(2,'0')}`;
  }
  if (rule.weekday !== undefined) fields.weekday.value = rule.weekday;
}
function scheduleEditorVisibility(fields, job) {
  const kind = fields.trigger.value;
  const visible = {
    trigger:true, delay:kind === 'delay', time:kind === 'at', interval:kind === 'interval',
    clock:kind === 'daily' || kind === 'weekly', timezone:kind === 'daily' || kind === 'weekly',
    weekday:kind === 'weekly',
  };
  for (const [key, show] of Object.entries(visible)) {
    fields[key].disabled = fields[key].closest('label').hidden = Boolean(job) || !show;
  }
}
function loadScheduleEditorSessions(field, sessions, bound, preferred) {
  const available = sessions.filter(session => !session.archived);
  for (const session of available) {
    const option = el('option', '', session.title || session.id);
    option.value = session.id; field.append(option);
  }
  if (bound && !available.some(session => session.id === bound)) {
    const option = el('option', '', bound);
    option.value = bound; field.append(option);
  }
  field.value = [bound, preferred, state.sessionId].find(id => available.some(session => session.id === id))
    || bound || field.options[0]?.value || '';
  field.disabled = Boolean(bound) || !field.value;
}
function createScheduleEditorController(review) {
  const {dialog, title, form, fields, jobLabel, status, actions} = createScheduleEditor();
  let editing = null, job = null, epoch = 0, pending = false, draftKey = 'new';
  const drafts = new Map();
  const fail = error => CLATI18n.bind(status, 'textContent', () => CLATI18n.present(error.presentation, error.message));
  const values = () => Object.fromEntries(Object.entries(fields).map(([key,input]) => [key,input.value]));
  const defaults = values();
  const visibility = () => scheduleEditorVisibility(fields, job);
  async function open(rule = null, source = null, seed = null) {
    if (pending) return;
    const request = ++epoch;
    editing = rule;
    job = source || (rule?.spec.trigger.kind === 'job' ? {id:rule.spec.trigger.job_id, session:rule.scope.session} : null);
    draftKey = rule?.id || (source ? 'job:' + source.id : seed?.key || 'new');
    const staged = drafts.get(draftKey);
    for (const [key, value] of Object.entries(defaults)) fields[key].value = value;
    fields.time.value = scheduleLocalTime(Date.now() + 10 * 60000);
    fields.title.value = rule?.spec.title || seed?.title || '';
    fields.instruction.value = rule?.spec.instruction || seed?.instruction || '';
    fillScheduleTime(fields, rule?.spec.trigger.rule || seed?.trigger);
    if (staged) for (const [key, value] of Object.entries(staged)) fields[key].value = value;
    dialog.dataset.unsaved = String(Boolean(staged || seed));
    status.replaceChildren();
    fields.session.replaceChildren();
    fields.session.disabled = true; save.disabled = true;
    CLATI18n.bind(title, 'textContent', () => pageText(rule ? 'edit_task' : 'create_task'));
    jobLabel.hidden = !job;
    if (job) CLATI18n.bind(jobLabel, 'textContent', () => pageText('task_job_trigger'));
    visibility();
    if (!dialog.open) dialog.showModal();
    try {
      const {sessions} = await rpc('session.list', {});
      if (request !== epoch || !dialog.open) return;
      loadScheduleEditorSessions(fields.session, sessions, rule?.scope.session || source?.session, staged?.session);
      save.disabled = !fields.session.value;
      if (!fields.session.value) {
        status.append(el('span', '', () => pageText('task_needs_session')),
          pageAction('start_conversation', () => { dialog.close(); workbenchShell.chat(); }));
      }
    } catch (error) { if (request === epoch && dialog.open) fail(error); }
  }
  async function persist(event) {
    event.preventDefault();
    if (pending || save.disabled) return;
    if (!fields.instruction.value.trim()) {
      CLATI18n.bind(status, 'textContent', () => pageText('task_instruction_required'));
      fields.instruction.focus(); return;
    }
    pending = true; save.disabled = true; dialog.dataset.saving = 'true';
    const request = epoch, token = settingsSaveToken(form);
    try {
      const draft = scheduleDraftFrom(fields, editing, job);
      const {rule} = await rpc(editing ? 'schedule.update' : 'schedule.create',
        editing ? {id:editing.id, revision:editing.revision, draft} : draft);
      await taskCenter.refresh();
      if (request !== epoch || !dialog.open) return;
      if (token !== settingsSaveToken(form)) { editing = rule; return; }
      drafts.delete(draftKey); delete dialog.dataset.unsaved;
      const closed = new Promise(resolve => dialog.addEventListener('close', resolve, {once:true}));
      dialog.close(); await closed;
      if (context.isActive() && pageIsOpen(taskCenter.page)) {
        taskCenter.showSchedule(rule);
        await review(rule);
      }
    } catch (error) { if (request === epoch && dialog.open) fail(error); }
    finally { pending = false; save.disabled = !fields.session.value; delete dialog.dataset.saving; }
  }
  const save = pageAction('save', null, true);
  save.type = 'submit'; save.setAttribute('form', form.id);
  form.onsubmit = persist; fields.trigger.onchange = visibility;
  form.addEventListener('input', () => { dialog.dataset.unsaved = 'true'; });
  actions.append(pageAction('cancel', () => {
    drafts.delete(draftKey); delete dialog.dataset.unsaved; dialog.close();
  }), save);
  dialog.addEventListener('close', () => {
    epoch++;
    if (dialog.dataset.unsaved === 'true') drafts.set(draftKey, values());
  });
  return {open, epoch:() => epoch};
}
function installScheduledWork() {
  let reviewGeneration = 0;
  async function review(rule, editorEpoch = editor.epoch) {
    const generation = ++reviewGeneration, owner = editorEpoch();
    const current = () => generation === reviewGeneration && owner === editorEpoch()
      && context.isActive() && pageIsOpen(taskCenter.page);
    try {
      const result = await rpc('schedule.review', {id:rule.id, revision:rule.revision});
      if (current()) showScheduleReview(result.review, () => taskCenter.refresh());
    } catch (error) { if (current()) taskCenter.fail(error); }
  }
  const editor = createScheduleEditorController(review);
  const {open} = editor;
  taskCenter.register({
    create:example => void open(null, null, typeof example === 'string' ? {key:example, title:pageText(example), instruction:pageText(example + '_prompt')} : null),
    duplicate:rule => {
      const trigger = rule.spec.trigger.rule;
      void open(null, null, {key:'copy:' + rule.id, title:rule.spec.title || '', instruction:rule.spec.instruction,
        trigger:trigger?.kind !== 'at' ? trigger : null});
    },
    edit:open, review, ...scheduleRunActions(),
  });
  const trigger = sidebarToolButton('schedules-open', 'schedule', () => CLATI18n.msg('schedule.open')); trigger.hidden = true;
  trigger.onclick = () => taskCenter.openSchedules(); document.querySelector('.sidebar-footer').append(trigger);
  taskCenter.interactionHost.prepend(el('section', 'schedule-interactions'));
  window.addEventListener('clat:job-authorize', event => { taskCenter.openSchedules(); void open(null,event.detail); });
}
function scheduleRunActions() {
  const requests = new Map();
  return {
    async change(rule, operation) {
      try { await rpc('schedule.' + operation, {id:rule.id, revision:rule.revision}); await taskCenter.refresh(); }
      catch (error) { taskCenter.fail(error); }
    },
    async run(rule) {
      const key = rule.id + ':' + rule.revision;
      if (!requests.has(key)) requests.set(key, crypto.randomUUID());
      try {
        await rpc('schedule.run_now', {id:rule.id, revision:rule.revision, request_id:requests.get(key)});
        requests.delete(key); await taskCenter.refresh();
      } catch (error) { taskCenter.fail(error); }
    },
  };
}
function showScheduleReview(review, saved) {
  const dialog = el('dialog', 'workbench-dialog schedule-review'), rule = review.rule;
  CLATI18n.attribute(dialog, 'aria-label', () => pageText('review'));
  const head = el('header', 'workbench-dialog-header'); head.append(el('h2', '', () => pageText('review')));
  const sessionName = state.sessions.find(session => session.id === rule.scope.session)?.title || rule.scope.session;
  const when = el('p', 'task-review-when'); when.append(svgIcon('schedule'), el('span', '', () => scheduleTriggerText(rule.spec.trigger)));
  const project = projectDisplayName(rule.scope.project, state.workbench?.project?.name);
  const scope = el('p', 'page-scope', () => pageText('session_scope', {project, session:sessionName}));
  scope.title = rule.scope.project;
  dialog.append(head, el('h3', 'task-review-title', rule.spec.title || rule.spec.instruction.split('\n')[0]), when,
    el('pre', 'task-instruction', rule.spec.instruction), scope,
    el('p', 'settings-note', () => pageText('task_review_execution')));
  if (rule.spec.budget.run_paid_web_calls > 0) {
    dialog.append(el('p', 'settings-note', () => pageText('task_review_paid', {count:rule.spec.budget.run_paid_web_calls})));
  }
  const status = el('p', 'error'); status.setAttribute('role', 'status');
  const actions = el('footer', 'workbench-dialog-actions');
  const confirm = pageAction('confirm', async () => {
    confirm.disabled = true;
    try { await rpc('schedule.confirm', {id:rule.id, revision:rule.revision, nonce:review.nonce, digest:review.digest}); dialog.close(); await saved(); }
    catch (error) { status.textContent = error.message; confirm.disabled = false; }
  }, true);
  actions.append(pageAction('cancel', () => dialog.close()), confirm); dialog.append(status, actions);
  document.body.append(dialog); dialog.addEventListener('close', () => dialog.remove(), {once:true}); dialog.showModal();
}
