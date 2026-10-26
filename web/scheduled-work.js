/* Schedules authorize work in the core. Editing and reading have separate owners. */
function scheduleTriggerText(trigger) {
  const msg = (key, args) => CLATI18n.msg('schedule.' + key, args);
  const date = ms => new Intl.DateTimeFormat(CLATI18n.locale, {dateStyle:'medium',timeStyle:'short'}).format(new Date(ms));
  if (trigger.kind === 'job') return msg('time_job', {id:trigger.job_id});
  const r = trigger.rule;
  if (r.kind === 'at') return msg('time_at', {time:date(r.at_ms)});
  if (r.kind === 'interval') return msg('time_interval', {seconds:String(r.every_ms / 1000),time:date(r.anchor_ms)});
  const clock = `${String(r.hour).padStart(2,'0')}:${String(r.minute).padStart(2,'0')}`;
  if (r.kind === 'daily') return msg('time_daily', {clock,timezone:r.timezone});
  const day = new Intl.DateTimeFormat(CLATI18n.locale, {weekday:'long',timeZone:'UTC'}).format(new Date(Date.UTC(2024,0,r.weekday)));
  return msg('time_weekly', {day,clock,timezone:r.timezone});
}
function appendHostInteraction(card, session) {
  if (!session) return appendTranscript(card);
  const container = document.querySelector('.schedule-interactions');
  card.prepend(el('small', '', () => `${CLATI18n.msg('schedule.title')} · ${session}`)); container.append(card);
  document.getElementById('nav-tasks')?.setAttribute('data-pending','true');
}
function createScheduleEditor() {
  const dialog = el('dialog', 'task-editor'); dialog.id = 'schedule-editor';
  const head = el('header', 'page-header'); head.append(el('h2', '', () => pageText('edit_task')), pageAction('cancel', () => dialog.close()));
  const form = el('form', 'schedule-form'); const advanced = el('details', 'settings-advanced');
  advanced.append(el('summary', '', () => pageText('advanced')));
  const fields = {}, msg = key => CLATI18n.msg('schedule.' + key);
  function field(key, type = 'text', value = '', parent = form) {
    const label = el('label', ''); const input = el(key === 'instruction' ? 'textarea' : type === 'select' ? 'select' : 'input');
    if (input.tagName === 'INPUT') input.type = type;
    input.name = key; input.value = value; label.append(el('span', '', () => ['title','session','notification'].includes(key) ? pageText(key) : msg(key)), input);
    if (type === 'number') {input.min = '0'; input.step = '1';}
    fields[key] = input; parent.append(label); return input;
  }
  field('title'); field('instruction'); field('session','select'); field('trigger','select');
  for (const key of ['delay','at','interval','daily','weekly']) { const o = el('option', '', () => msg(key)); o.value = key; fields.trigger.append(o); }
  for (const [key,type,value] of [['time','datetime-local',''],['delay','number','10'],['interval','number','60'],['clock','time','09:00'],['timezone','text',Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'],['weekday','number','1']]) field(key,type,value);
  for (const [key,value] of Object.entries({max_runs:'1',max_tokens:'100000',max_seconds:'300',run_tokens:'100000',run_seconds:'300',max_paid_web_calls:'0',run_paid_web_calls:'0'})) field(key,'number',value,advanced);
  field('model_profile','text','',advanced); field('notification','select','',advanced);
  for (const key of ['attention','completion','off']) {const o = el('option','',()=>pageText('notify_'+key));o.value=key;fields.notification.append(o);}
  form.append(advanced); const status = el('p','settings-note'); status.setAttribute('role','status');
  dialog.append(head,form,status); document.body.append(dialog);
  return {dialog,form,fields,status};
}
function scheduleDraftFrom(fields, editing, job) {
  const kind = fields.trigger.value, now = Date.now(); let rule;
  if (kind === 'delay') rule = {kind:'at',at_ms:now + Number(fields.delay.value)*60000};
  else if (kind === 'at') rule = {kind,at_ms:new Date(fields.time.value).getTime()};
  else if (kind === 'interval') {const every_ms=Number(fields.interval.value)*60000;rule={kind,anchor_ms:editing?.spec.trigger.rule?.every_ms===every_ms?editing.spec.trigger.rule.anchor_ms:now+every_ms,every_ms};}
  else {const [hour,minute]=fields.clock.value.split(':').map(Number);rule={kind,hour,minute,timezone:fields.timezone.value};if(kind==='weekly')rule.weekday=Number(fields.weekday.value);}
  const budget = Object.fromEntries(['max_runs','max_tokens','max_seconds','run_tokens','run_seconds','max_paid_web_calls','run_paid_web_calls'].map(key=>{
    const value=Number(fields[key].value);if(!Number.isSafeInteger(value)||value<0)throw CLATI18n.error(CLATI18n.msg('schedule.invalid_budget'));return [key,value];
  }));
  return {title:fields.title.value.trim() || null, notification:fields.notification.value, session:editing?.scope.session || job?.session || fields.session.value,
    instruction:fields.instruction.value,trigger:job?{kind:'job',job_id:job.id}:{kind:'time',rule},budget,model_profile:fields.model_profile.value.trim()||null};
}
function installScheduledWork() {
  const {dialog,form,fields,status} = createScheduleEditor();
  let editing=null, job=null, epoch=0, pending=false, draftKey='new'; const drafts=new Map();
  const fail = error => CLATI18n.bind(status,'textContent',()=>error.message);
  const defaults=Object.fromEntries(Object.entries(fields).map(([key,input])=>[key,input.value]));
  function visibility() {
    const kind=fields.trigger.value;
    for(const key of ['trigger','time','delay','interval','clock','timezone','weekday']) fields[key].closest('label').hidden=Boolean(job)||!(key==='trigger'||key===kind||kind==='at'&&key==='time'||['daily','weekly'].includes(kind)&&['clock','timezone'].includes(key)||kind==='weekly'&&key==='weekday');
  }
  async function open(rule=null, source=null) {
    save.disabled=true;const request=++epoch;for(const [key,value]of Object.entries(defaults))fields[key].value=value;editing=rule;job=source||(rule?.spec.trigger.kind==='job'?{id:rule.spec.trigger.job_id,session:rule.scope.session}:null);draftKey=rule?.id||source?.id||'new';status.textContent='';
    fields.title.value=rule?.spec.title||'';fields.instruction.value=rule?.spec.instruction||'';
    fields.model_profile.value=rule?.spec.model.profile||'';fields.notification.value=rule?.spec.notification||'attention';
    if(rule){for(const [key,value]of Object.entries(rule.spec.budget))fields[key].value=value;const r=rule.spec.trigger.rule;
      if(r){fields.trigger.value=r.kind;if(r.kind==='at'){const date=new Date(r.at_ms);date.setMinutes(date.getMinutes()-date.getTimezoneOffset());fields.time.value=date.toISOString().slice(0,16);}if(r.kind==='interval')fields.interval.value=String(r.every_ms/60000);if(r.timezone){fields.timezone.value=r.timezone;fields.clock.value=`${String(r.hour).padStart(2,'0')}:${String(r.minute).padStart(2,'0')}`;}if(r.weekday!==undefined)fields.weekday.value=r.weekday;}}
    if(drafts.has(draftKey))for(const [key,value]of Object.entries(drafts.get(draftKey)))fields[key].value=value;
    dialog.dataset.unsaved=String(drafts.has(draftKey));
    visibility();if(!dialog.open)dialog.showModal();
    try {const data=await rpc('session.list',{});if(request!==epoch)return;fields.session.replaceChildren();for(const session of data.sessions.filter(s=>!s.archived)){const o=el('option','',session.title||session.id);o.value=session.id;fields.session.append(o);}if(rule&&!data.sessions.some(s=>!s.archived&&s.id===rule.scope.session)){const prior=el('option','',rule.scope.session);prior.value=rule.scope.session;fields.session.append(prior);}
      fields.session.value=rule?.scope.session||source?.session||drafts.get(draftKey)?.session||state.sessionId||fields.session.options[0]?.value||'';fields.session.disabled=Boolean(rule||source);save.disabled=!fields.session.value;if(!fields.session.value){CLATI18n.bind(status,'textContent',()=>pageText('task_needs_session'));status.append(pageAction('start_conversation',()=>{dialog.close();workbenchShell.chat();}));}}
    catch(error){if(request===epoch)fail(error);}
  }
  const save=pageAction('save',async()=>{
    if(pending)return;pending=true;save.disabled=true;dialog.dataset.saving='true';const request=epoch, token=settingsSaveToken(form);
    try{const draft=scheduleDraftFrom(fields,editing,job);const value=await rpc(editing?'schedule.update':'schedule.create',editing?{id:editing.id,revision:editing.revision,draft}:draft);
      await taskCenter.refresh();if(request!==epoch)return;if(token!==settingsSaveToken(form)){editing=value.rule;return;}drafts.delete(draftKey);delete dialog.dataset.unsaved;const closed=new Promise(resolve=>dialog.addEventListener('close',resolve,{once:true}));dialog.close();await closed;await review(value.rule);}
    catch(error){if(request===epoch)fail(error);}finally{pending=false;save.disabled=false;delete dialog.dataset.saving;}
  },true);form.append(save);form.onsubmit=e=>{e.preventDefault();save.click();};fields.trigger.onchange=visibility;
  async function review(rule) {
    const owner=++epoch;
    try{const value=await rpc('schedule.review',{id:rule.id,revision:rule.revision});if(owner!==epoch)return;showScheduleReview(value.review,()=>taskCenter.refresh());}
    catch(error){taskCenter.fail(error);}
  }
  async function change(rule,operation){try{await rpc('schedule.'+operation,{id:rule.id,revision:rule.revision});await taskCenter.refresh();}catch(error){taskCenter.fail(error);}}
  const requests=new Map();
  async function run(rule){const key=rule.id+':'+rule.revision;if(!requests.has(key))requests.set(key,crypto.randomUUID());try{await rpc('schedule.run_now',{id:rule.id,revision:rule.revision,request_id:requests.get(key)});requests.delete(key);await taskCenter.refresh();}catch(error){taskCenter.fail(error);}}
  taskCenter.register({duplicate:rule=>{void open().then(()=>{fields.title.value=rule.spec.title||'';fields.instruction.value=rule.spec.instruction;for(const [key,value]of Object.entries(rule.spec.budget))fields[key].value=value;dialog.dataset.unsaved='true';});},create:(example)=>{void open().then(()=>{if(typeof example==='string'){fields.title.value=pageText(example);fields.instruction.value=pageText(example+'_prompt');dialog.dataset.unsaved='true';}});},edit:open,review,change,run});
  const trigger=sidebarToolButton('schedules-open','schedule',()=>CLATI18n.msg('schedule.open'));trigger.hidden=true;trigger.onclick=()=>taskCenter.openSchedules();document.querySelector('.sidebar-footer').append(trigger);
  const interactions=el('section','schedule-interactions');taskCenter.page.prepend(interactions);
  window.addEventListener('clat:job-authorize',event=>{taskCenter.openSchedules();void open(null,event.detail);});
  form.addEventListener('input',()=>{dialog.dataset.unsaved='true';});
  form.append(pageAction('discard',()=>{drafts.delete(draftKey);delete dialog.dataset.unsaved;dialog.close();}));
  dialog.addEventListener('close',()=>{epoch++;if(dialog.dataset.unsaved==='true')drafts.set(draftKey,Object.fromEntries(Object.entries(fields).map(([key,input])=>[key,input.value])));});
}
function showScheduleReview(review, saved) {
  const dialog=el('dialog','task-editor schedule-review'); const r=review.rule;
  dialog.append(el('h2','',()=>pageText('review')),el('p','',r.spec.title||r.spec.instruction),el('pre','task-instruction',r.spec.instruction),
    el('p','page-scope',()=>pageText('session_scope',{project:r.scope.project,session:r.scope.session})),el('p','',()=>scheduleTriggerText(r.spec.trigger)),el('p','',r.spec.model.model));
  for(const [key,value]of Object.entries(r.spec.budget))dialog.append(el('p','settings-note',()=>CLATI18n.msg('schedule.'+key)+': '+value));
  const status=el('p','error');dialog.append(status,pageAction('confirm',async()=>{
    try{await rpc('schedule.confirm',{id:r.id,revision:r.revision,nonce:review.nonce,digest:review.digest});dialog.close();await saved();}catch(error){status.textContent=error.message;}
  },true),pageAction('cancel',()=>dialog.close()));
  document.body.append(dialog);dialog.addEventListener('close',()=>dialog.remove(),{once:true});dialog.showModal();
}
