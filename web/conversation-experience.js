let liveMessageBase=Infinity, liveMessageNodes=[];
/* Conversation-only presentation. Core owns timestamps, usage and followup admission. */
function experienceText(key, params = {}) { return CLATI18n.msg(`experience.${key}`, params); }
function messageTime(node, time) {
  if (Number.isFinite(time) && time > 0) node.dataset.time = String(time);
  let actions=node.querySelector('.message-actions,.reader-actions');
  if(!actions) {actions=el('div','message-actions');node.append(actions);}
  let stamp = actions.querySelector('.message-time');
  if (!stamp) { stamp = el('time', 'message-time'); actions.append(stamp); }
  CLATI18n.bind(stamp, 'textContent', () => Number(node.dataset.time) > 0
    ? new Date(Number(node.dataset.time)).toLocaleString(CLATI18n.locale) : experienceText('time_unknown'));
  if(!actions.querySelector('.message-time-toggle')) {
    const toggle=el('button','text-action message-time-toggle');toggle.type='button';toggle.append(svgIcon('schedule'));
    CLATI18n.attribute(toggle,'aria-label',()=>experienceText('time'));
    toggle.onclick=()=>{const shown=node.classList.toggle('time-open');toggle.setAttribute('aria-expanded',String(shown));};actions.append(toggle);
  }
  if (Number(node.dataset.time) > 0) stamp.dateTime = new Date(Number(node.dataset.time)).toISOString();
}
function syncMessageTimes() {
  const outline = new Map(state.history.outline.map(item => [String(item.seq), item]));
  for(const node of dom.transcript.querySelectorAll('.msg'))messageTime(node,outline.get(node.dataset.seq)?.time_ms);
}
function installConversationExperience() {
  const shell = dom['composer-shell'];
  const row = shell.querySelector('.composer-row');
  row.before(dom.prompt); // The editor and bottom controls are the whole permanent composer.
  const status = el('div', 'composer-feedback'); status.hidden = true;
  status.append(dom['run-state']); shell.before(status, dom['next-turn-queue']);
  dom['goal-badge'].classList.add('context-pill'); row.insertBefore(dom['goal-badge'], row.querySelector('.composer-actions'));
  shell.querySelector('.composer-context').remove();
  shell.before(dom['suggestion-panel']);
  dom['suggestion-panel'].classList.add('round-suggestions');
  const observer = new MutationObserver(() => {
    const text = dom['run-state'].textContent;
    status.hidden = !text || ['web.running','web.task_completed_send_a_message_to_continue','web.completed_send_a_message_to_continue','web.idle','web.run_completed_send_a_message_to_continue'].includes(dom['run-state'].dataset.presentation);
  });
  observer.observe(dom['run-state'], {subtree:true,childList:true,attributes:true});
  installUsagePopover(row.querySelector('.composer-actions'));
  installComposerPermissions();
  installModelPickerPopup();
}
let roundFollowupPanel = null;
function renderRoundFollowups(value) {
  const panel = roundFollowupPanel || (roundFollowupPanel=el('section','round-suggestions')); panel.replaceChildren();
  if (!value) { hide(panel); return; }
  panel.append(el('span', 'visually-hidden', () => experienceText('followups')));
  if (value.status === 'pending') panel.append(el('small', '', () => experienceText('generating')));
  for (const text of value.items || []) {
    const button = el('button', 'followup-choice'); button.type = 'button';
    button.append(svgIcon('sparkles'),el('span','',text));
    button.onclick = () => {
      if (state.desktopUpdateLocked || state.switching || !composerOwns(value.owner)) return;
      dom.prompt.value += (dom.prompt.value ? '\n\n' : '') + text;
      state.composerGeneration++; saveComposerText(); resizePrompt(); dom.prompt.focus();
      clearRoundFollowups();
    };
    panel.append(button);
  }
  if (value.status === 'failed') {
    panel.append(el('small', '', () => experienceText('suggest_failed')));
    const retry = el('button', 'text-action', () => experienceText('retry'));
    retry.onclick = () => void loadRoundFollowups(value.run, 'retry'); panel.append(retry);
  }
  if (value.items?.length || value.status === 'pending' || value.status === 'failed') {
    dom.transcript.append(panel); show(panel);
  } else hide(panel);
}
let followupEpoch = 0;
function clearRoundFollowups() { followupEpoch++; renderRoundFollowups(null); }
async function loadRoundFollowups(run, action = 'generate') {
  const epoch = ++followupEpoch, owner = composerScope, generation = state.selectionGeneration;
  const current = () => epoch === followupEpoch && composerOwns(owner) && generation === state.selectionGeneration && !state.runActive;
  if (action !== 'read') renderRoundFollowups({status:'pending',run,owner});
  try {
    const result = await rpc('prompt.followups', {run_id:run, action, expected_selection_generation:generation});
    if (current()) {
      renderRoundFollowups({...result, run:result.run_id || run, owner});
      if(result.status==='pending')setTimeout(()=>{if(current())void loadRoundFollowups(run,'read');},800);
    }
  } catch (_) { if (current() && action !== 'read') renderRoundFollowups({status:'failed',run,owner}); }
}
let usageEpoch = 0, usageValue = null, usageCompactionRequest = false, usageScope = null;
function syncUsageTrigger() {
  const trigger = document.getElementById('context-usage');
  if (!trigger) return;
  const busy = state.compactionActive || usageCompactionRequest;
  const recommended = Boolean(usageValue?.compaction_recommended) && !state.runActive;
  trigger.classList.toggle('compaction-recommended', recommended);
  trigger.classList.toggle('compacting', busy);
  trigger.setAttribute('aria-busy', String(busy));
  trigger.disabled = busy || state.switching || state.desktopUpdateLocked;
  const key = busy ? 'compacting' : recommended && !state.runActive ? 'compact_recommended' : 'usage';
  CLATI18n.attribute(trigger,'aria-label',()=>experienceText(key));
}
function paintUsageRing(ring, percent) {
  if (ring._frame) cancelAnimationFrame(ring._frame);
  const previous = Number(ring.dataset.percent);
  const target = percent === null ? 0 : Math.max(0, percent);
  const draw = value => {
    ring.style.setProperty('--usage', `${Math.min(100,value) * 3.6}deg`);
    ring.dataset.percent = String(value);
  };
  ring.classList.toggle('unknown', percent === null);
  if (percent === null || !Number.isFinite(previous) || matchMedia('(prefers-reduced-motion: reduce)').matches) { draw(target); return; }
  const start = performance.now();
  const step = now => {
    const progress = Math.min(1, (now - start) / 650);
    draw(previous + (target - previous) * (1 - (1 - progress) ** 3));
    if (progress < 1) ring._frame = requestAnimationFrame(step);
  };
  ring._frame = requestAnimationFrame(step);
}
function positionUsagePopover(popup, trigger, alignment = 'center') {
  if (popup.classList.contains('hidden')) return;
  const anchor = trigger.getBoundingClientRect(), inset = 12;
  const bounds = dom['composer-shell'].getBoundingClientRect();
  const desired = alignment === 'left' ? bounds.left : alignment === 'right' ? bounds.right - popup.offsetWidth
    : anchor.left + (anchor.width - popup.offsetWidth) / 2;
  const left = Math.max(inset, Math.min(desired, innerWidth - popup.offsetWidth - inset));
  popup.style.left = `${left}px`;
  popup.style.bottom = `${innerHeight - anchor.top + 12}px`;
  popup.style.maxHeight = `${Math.max(0, Math.min(innerHeight * .65, anchor.top - 24))}px`;
}
function installUsagePopover(actions) {
  const seat = el('div', 'usage-seat');
  const trigger = el('button', 'icon-button usage-trigger'); trigger.type = 'button'; trigger.id = 'context-usage';
  trigger.setAttribute('aria-expanded','false'); trigger.setAttribute('aria-haspopup','dialog');
  CLATI18n.attribute(trigger,'aria-label',() => experienceText('usage'));
  const ring = el('span', 'context-ring');
  trigger.append(ring);
  const popup = el('section','usage-popover context-usage-popover hidden'); popup.setAttribute('role','dialog');
  CLATI18n.attribute(popup,'aria-label',() => experienceText('usage'));
  seat.append(trigger,popup); actions.prepend(seat);
  const popover = attachComposerPopup(seat,trigger,popup,()=>renderUsage(popup,ring),'center',{hover:true});
  trigger.onclick = popover.toggle;
  CLATI18n.onChange(()=>{ renderUsage(popup,ring); syncUsageTrigger(); });
  syncUsageTrigger();
}
async function refreshUsage() {
  const epoch=++usageEpoch, session=state.sessionId, generation=state.selectionGeneration;
  const owner = JSON.stringify([session,generation,state.workbench?.model]);
  if (owner !== usageScope) {
    usageScope = owner;
    document.querySelector('.context-ring')?.removeAttribute('data-percent');
  }
  usageValue=null;
  const pendingPopup=document.querySelector('.context-usage-popover'), pendingRing=document.querySelector('.context-ring');
  if(pendingPopup&&pendingRing)renderUsage(pendingPopup,pendingRing);
  try {
    const value=await rpc('conversation.stats',{});
    if(epoch!==usageEpoch || session!==state.sessionId || generation!==state.selectionGeneration)return;
    usageValue=value;
  } catch (_) { if(epoch===usageEpoch)usageValue={unavailable:true}; }
  const popup=document.querySelector('.context-usage-popover'),ring=document.querySelector('.context-ring');
  if(popup&&ring)renderUsage(popup,ring);
}
async function requestUsageCompaction() {
  if (usageCompactionRequest || state.runActive || state.compactionActive || state.switching || state.desktopUpdateLocked) return;
  activeComposerPopup?.close(); usageCompactionRequest = true; syncUsageTrigger();
  try { await compactCurrentSession(); }
  finally { usageCompactionRequest = false; syncUsageTrigger(); }
}
function renderUsage(popup,ring) {
  popup.replaceChildren(); const value=usageValue;
  const row=(label,text) => { const node=el('div','usage-row');node.append(el('span','',()=>experienceText(label)),el('span','',text));popup.append(node); };
  const used=value?.context?.used, capacity=value?.context?.capacity;
  const percent=Number.isFinite(used)&&capacity>0 ? used/capacity*100 : null;
  if (value !== null || !ring.hasAttribute('data-percent')) paintUsageRing(ring,percent);
  syncUsageTrigger();
  row('context', value?.context ? `${used.toLocaleString(CLATI18n.locale)} / ${capacity?.toLocaleString(CLATI18n.locale) || experienceText('capacity_unknown')}${percent===null?'':` · ${percent.toFixed(1)}%`}` : experienceText(value===null?'loading':'unavailable'));
  popup.append(el('small','',()=>experienceText('estimate')));
  if (value?.compaction_recommended) {
    const compact = el('button', 'text-action usage-compact', () => experienceText('compact'));
    compact.type = 'button'; compact.disabled = state.runActive || state.compactionActive || usageCompactionRequest || state.switching || state.desktopUpdateLocked;
    compact.onclick = requestUsageCompaction;
    popup.append(el('p','usage-compaction-hint',()=>experienceText('compact_recommended')), compact);
  }
  for(const [scope,key] of [['last_request','last_request'],['route','route']]) {
    const usage=value?.[scope];if(!usage)continue;
    popup.append(el('h3','',()=>experienceText(key)));
    for(const [field,label] of [['input_tokens','input'],['output_tokens','output'],['cached_input_tokens','cached']]) {
      if(usage[field]!=null)row(label,Number(usage[field]).toLocaleString(CLATI18n.locale));
    }
    if(usage.input_tokens>0 && usage.cached_input_tokens!=null)row('cache_rate',(100*usage.cached_input_tokens/usage.input_tokens).toFixed(1)+'%');
  }
  const quota=value?.allowance;
  popup.append(el('h3','',()=>experienceText('allowance')));
  if(quota?.status==='available') {
    for(const item of quota.items)row(item.kind==='money'?'wallet':'tokens',`${item.value} ${item.unit || experienceText('unit_unknown')}`.trim());
  } else row('status',experienceText(quota?.status || 'unavailable'));
  if(quota?.updated_at)row('updated',new Date(quota.updated_at).toLocaleString(CLATI18n.locale));
}
