/* ACP views share the project RPC and leave process authority in core. */
const acpText = (key, args = {}) => CLATI18n.msg('acp.' + key, args);
let acpRail = null, acpRailVersion = 0, acpSettingsRefresh = null;

function acpButton(key, run, primary = false) {
  const button = el('button', primary ? 'primary' : 'secondary', () => acpText(key));
  button.type = 'button';
  button.onclick = run;
  return button;
}

function acpDialog(title) {
  const dialog = el('dialog', 'workbench-dialog session-share-dialog');
  const head = el('header', 'workbench-dialog-header');
  const close = acpButton('close', () => dialog.close());
  head.append(el('h2', '', () => acpText(title)), close);
  const fields = el('div', 'session-share-fields');
  const status = el('p', 'settings-note');
  status.setAttribute('role', 'status');
  const actions = el('footer', 'workbench-dialog-actions');
  CLATI18n.attribute(dialog, 'aria-label', () => acpText(title));
  dialog.append(head, fields, status, actions);
  dom.app.append(dialog);
  dialog.addEventListener('close', () => dialog.remove(), { once: true });
  dialog.showModal();
  return { dialog, fields, status, actions };
}

function acpField(root, title, input) {
  const label = el('label', 'settings-field');
  label.append(el('span', '', () => acpText(title)), input);
  root.append(label);
  return input;
}

function acpShowError(node, error) {
  node.textContent = error.message;
}

function openAcpPicker(delegate) {
  const view = acpDialog(delegate ? 'delegate' : 'new_session');
  const owner = composerScope;
  const origin = delegate ? state.sessionId : null;
  const generation = state.selectionGeneration;
  const draftGeneration = state.composerGeneration;
  const picker = acpField(view.fields, 'agent', el('select'));
  const text = delegate ? acpField(view.fields, 'task', el('textarea')) : null;
  if (text) { text.value = dom.prompt.value; text.rows = 6; }
  const current = () => view.dialog.open && context.isActive() && composerOwns(owner) && state.selectionGeneration === generation;
  const start = acpButton('start', () => startAcpFromPicker(view, { delegate, origin, picker, text, current, draftGeneration, start }), true);
  start.disabled = true;
  view.actions.append(acpButton('title', () => { view.dialog.close(); openSettings('acp'); }), start);
  view.fields.append(el('p', 'settings-note', () => acpText(delegate ? 'send_notice' : 'authority')));
  const project = state.workbench?.project?.root;
  if (project) view.fields.append(el('p', 'settings-note', project));
  rpc('acp.agents.list', {}).then(agents => {
    if (!current()) return;
    for (const agent of agents) {
      const option = el('option', '', agent.label); option.value = agent.id; option.disabled = !agent.trusted;
      picker.append(option);
    }
    const first = agents.find(agent => agent.trusted);
    if (first) picker.value = first.id;
    else view.status.textContent = acpText('trust_first');
    start.disabled = !first;
  }).catch(error => { if (current()) acpShowError(view.status, error); });
}

async function startAcpFromPicker(view, options) {
  const { delegate, origin, picker, text, current, draftGeneration, start } = options;
  if (!current() || !picker.value) return;
  if (delegate && !text.value.trim()) { view.status.textContent = acpText('empty_task'); return; }
  start.disabled = true;
  options.requestId ||= crypto.randomUUID();
  try {
    const value = await rpc(delegate ? 'acp.delegations.start' : 'acp.sessions.start', {
      agent:picker.value, requestId:options.requestId, text:text?.value || '',
      continueWithClat:delegate, origin, expected_selection_generation:state.selectionGeneration,
    });
    if (!current()) return;
    view.dialog.close();
    if (delegate && state.composerGeneration === draftGeneration && text.value === dom.prompt.value) {
      dom.prompt.value = '';
      dom.prompt.dispatchEvent(new Event('input', { bubbles:true }));
      resizePrompt();
    }
    if (delegate) {
      if (!origin) resubscribe();
      else void refreshAcpRail();
    } else openAcpSession(value.id);
  } catch (error) {
    if (current()) { acpShowError(view.status, error); start.disabled = false; }
  }
}

function installAcpSettings() {
  const page = dom['settings-dialog'];
  const nav = acpButton('title', () => selectSettingsCategory('acp'));
  nav.className = ''; nav.dataset.settingsCategory = 'acp';
  page.querySelector('.settings-nav').append(nav);
  const section = el('section', 'settings-section');
  section.hidden = true; section.dataset.settingsPanel = 'acp';
  section.append(el('h2', '', () => acpText('title')), el('p', 'settings-note', () => acpText('authority')));
  page.querySelector('.settings-content').append(section);
  const list = el('div', 'settings-card');
  const status = el('p', 'settings-note');
  const actions = el('div', 'settings-actions');
  actions.append(acpButton('add', () => openAcpAgentEditor(null)), acpButton('sessions', openAcpSessions));
  section.append(list, actions, status);
  acpSettingsRefresh = async () => {
    const generation = state.selectionGeneration;
    try {
      const agents = await rpc('acp.agents.list', {});
      if (section.hidden || generation !== state.selectionGeneration) return;
      list.replaceChildren(...agents.map(agent => acpAgentRow(agent, status)));
      if (!agents.length) list.append(el('p', 'settings-note', () => acpText('no_agents')));
    } catch (error) { if (!section.hidden) acpShowError(status, error); }
  };
}

function acpAgentRow(agent, status) {
  const row = el('div', 'settings-row');
  const copy = el('span', 'settings-row-copy');
  copy.append(el('strong', '', agent.label), el('small', '', agent.program));
  const actions = el('div', 'settings-actions');
  actions.append(acpButton('edit', () => openAcpAgentEditor(agent)));
  actions.append(acpButton(agent.trusted ? 'revoke' : 'trust', () => confirmAcpTrust(agent)));
  actions.append(acpButton('remove', async () => {
    if (!await CLATDialog.confirm(acpText('remove_agent', { agent:agent.label }))) return;
    try { await rpc('acp.agents.remove', { id:agent.id, revision:agent.revision }); void acpSettingsRefresh(); }
    catch (error) { acpShowError(status, error); }
  }));
  row.append(copy, actions);
  return row;
}

async function confirmAcpTrust(agent) {
  const root = state.workbench?.project?.root || '';
  const message = acpText(agent.trusted ? 'revoke_notice' : 'trust_notice', { agent:agent.label, program:agent.program, root });
  if (!await CLATDialog.confirm(message)) return;
  rpc('acp.agents.trust', { id:agent.id, revision:agent.revision, allow:!agent.trusted })
    .then(() => acpSettingsRefresh()).catch(error => updateRunState(error.message));
}

function openAcpAgentEditor(agent) {
  const view = acpDialog(agent ? 'edit' : 'add');
  const label = acpField(view.fields, 'label', el('input'));
  const program = acpField(view.fields, 'program', el('input'));
  const args = acpField(view.fields, 'arguments', el('textarea'));
  const env = acpField(view.fields, 'environment', el('textarea'));
  label.value = agent?.label || '';
  program.value = agent?.program || '';
  args.value = JSON.stringify(agent?.args || []);
  env.value = JSON.stringify(agent?.env || []);
  if (agent?.argumentsHidden) view.status.textContent = acpText('update_arguments');
  const save = acpButton('save', async () => {
    try {
      save.disabled = true;
      const asArray = value => {
        const parsed = JSON.parse(value);
        if (!Array.isArray(parsed) || parsed.some(item => typeof item !== 'string')) throw Error(acpText('array_required'));
        return parsed;
      };
      await rpc('acp.agents.save', { id:agent?.id || null, agent:{
        label:label.value, program:program.value, args:asArray(args.value), env:asArray(env.value),
      } });
      view.dialog.close();
      void acpSettingsRefresh();
    } catch (error) { acpShowError(view.status, error); save.disabled = false; }
  }, true);
  view.actions.append(save);
}

function openAcpSessions() {
  const view = acpDialog('sessions');
  view.actions.append(acpButton('remote_sessions', () => { view.dialog.close(); openRemoteAcpSessions(); }),
    acpButton('new_session', () => { view.dialog.close(); openAcpPicker(false); }, true));
  rpc('acp.sessions.list', {}).then(sessions => {
    if (!view.dialog.open) return;
    view.fields.replaceChildren(...sessions.map(session => {
      const button = el('button', 'secondary', session.label);
      button.type = 'button';
      button.onclick = () => { view.dialog.close(); openAcpSession(session.id); };
      return button;
    }));
    if (!sessions.length) view.status.textContent = acpText('no_sessions');
  }).catch(error => { if (view.dialog.open) acpShowError(view.status, error); });
}

const acpBusy = phase => ['starting', 'running'].includes(phase);
function openAcpSession(id) {
  const view = acpDialog('session');
  view.dialog.dataset.acpSession = id;
  const interactions = el('div', 'acp-interactions');
  view.fields.prepend(interactions);
  for (const card of document.querySelectorAll('[data-acp-interaction]')) {
    if (card.dataset.acpInteraction !== id) continue;
    const parent = card.parentNode; interactions.append(card);
    if (parent.classList.contains('acp-detached-interaction')) parent.remove();
  }
  const owner = composerScope;
  const transcript = el('div', 'settings-card acp-transcript');
  const editor = acpField(view.fields, 'task', el('textarea'));
  const attachments = el('input'); attachments.type = 'file'; attachments.multiple = true;
  const attachmentField = acpField(view.fields, 'attachments', attachments).parentElement;
  view.fields.prepend(transcript);
  const send = acpButton('send', async () => {
    const files = Array.from(attachments.files || []);
    const text = editor.value;
    send.disabled = true;
    let content;
    try { content = await acpPromptAttachments(files); }
    catch (error) { acpShowError(view.status, error); send.disabled = false; return; }
    if (editor.value === text && files.every((file, index) => attachments.files[index] === file) && files.length === attachments.files.length) {
      await command('prompt', { text, content });
    }
    if (!pending) await refresh();
  }, true);
  const stop = acpButton('cancel', () => {
    rpc('acp.sessions.cancel', { id }).then(refresh).catch(error => acpShowError(view.status, error));
  });
  const tools = el('div', 'settings-actions');
  view.fields.append(tools);
  view.actions.append(stop, send);
  let version = '', pending = false, timer = null, session = null;
  async function command(action, params = {}) {
    if (pending || !view.dialog.open || !composerOwns(owner)) return;
    if (action === 'prompt' && !params.text.trim() && !params.content?.length) return;
    pending = true; send.disabled = true;
    try {
      await rpc('acp.sessions.action', { id, action, requestId:crypto.randomUUID(), ...params });
      if (action === 'prompt') {
        if (editor.value === params.text) editor.value = '';
        if (params.content?.length) attachments.value = '';
      }
    } catch (error) { acpShowError(view.status, error); }
    finally { pending = false; await refresh(); }
  }
  async function refresh() {
    clearTimeout(timer);
    if (!view.dialog.open || !context.isActive() || !composerOwns(owner)) {
      if (view.dialog.open) view.dialog.close();
      return;
    }
    try {
      session = await rpc('acp.sessions.status', { id });
      if (!view.dialog.open || !context.isActive() || !composerOwns(owner)) return;
      send.disabled = pending || acpBusy(session.phase) || session.phase === 'closed' || !!session.origin;
      stop.disabled = !acpBusy(session.phase);
      const prompt = session.capabilities?.promptCapabilities;
      attachments.accept = [prompt?.image && 'image/png,image/jpeg,image/gif,image/webp', prompt?.audio && 'audio/mpeg,audio/wav,audio/ogg,audio/mp4'].filter(Boolean).join(',');
      attachments.disabled = send.disabled;
      attachmentField.hidden = !attachments.accept || !!session.origin;
      const next = JSON.stringify(session);
      if (next !== version) {
        version = next;
        acpRenderTranscript(transcript, session);
        acpRenderActions(tools, session, command);
        view.status.textContent = session.error || acpText('phase_' + session.phase);
      }
    } catch (error) { if (view.dialog.open) acpShowError(view.status, error); }
    if (view.dialog.open) timer = setTimeout(refresh, session && acpBusy(session.phase) ? 650 : 1800);
  }
  view.dialog.addEventListener('close', () => {
    clearTimeout(timer);
    acpReleaseMedia(transcript);
    for (const card of [...interactions.children]) {
      if (!card.classList.contains('resolved')) appendAcpInteraction(id, card);
    }
  }, { once:true });
  void refresh();
}

const ACP_RASTER_TYPES = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp']);
const ACP_AUDIO_TYPES = new Set(['audio/mpeg', 'audio/wav', 'audio/ogg', 'audio/mp4']);

async function acpPromptAttachments(files) {
  if (files.some(file => !ACP_RASTER_TYPES.has(file.type) && !ACP_AUDIO_TYPES.has(file.type))) throw Error(acpText('attachment_type'));
  if (files.length > 8 || files.reduce((sum, file) => sum + file.size, 0) > 2 * 1024 * 1024) throw Error(acpText('attachment_limit'));
  return Promise.all(files.map(file => new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error);
    reader.onload = () => resolve({
      type:ACP_RASTER_TYPES.has(file.type) ? 'image' : 'audio',
      mimeType:file.type, data:String(reader.result).split(',', 2)[1],
    });
    reader.readAsDataURL(file);
  })));
}

function acpReleaseMedia(root) {
  for (const node of root.querySelectorAll('[data-acp-blob]')) URL.revokeObjectURL(node.dataset.acpBlob);
}

function acpRenderTranscript(root, session) {
  acpReleaseMedia(root);
  const fragments = [];
  for (const update of session.updates || []) {
    const rowContent = acpUpdateContent(update);
    if (!rowContent) continue;
    const row = el('div', 'settings-row');
    row.append(rowContent);
    if (update.sessionUpdate === 'user_message_chunk') row.dataset.author = 'user';
    fragments.push(row);
  }
  root.replaceChildren(...fragments);
}

function acpUpdateContent(update) {
  const type = update.sessionUpdate;
  const copy = el('div', 'settings-row-copy');
  if (type === 'plan' && Array.isArray(update.entries)) {
    copy.append(el('strong', '', () => acpText('agent_plan')));
    const list = el('ul');
    for (const item of update.entries) {
      if (typeof item?.content !== 'string') continue;
      const state = ['pending', 'in_progress', 'completed'].includes(item.status) ? item.status : 'pending';
      list.append(el('li', '', () => acpText('plan_' + state, { item:item.content })));
    }
    copy.append(list);
  } else if (type === 'usage_update' && Number.isFinite(update.used) && Number.isFinite(update.size)) {
    copy.append(el('small', '', () => acpText('usage', { used:update.used, size:update.size })));
  } else if (update.content?.type) {
    const content = acpContentBlock(update.content);
    if (content) copy.append(content);
  } else if (typeof update.title === 'string' || typeof update.message === 'string') {
    copy.append(el('span', '', update.title || update.message));
  }
  return copy.childNodes.length ? copy : null;
}

function acpContentBlock(content) {
  if (content.type === 'text' && typeof content.text === 'string') return el('p', '', content.text);
  if (content.type === 'resource_link' && typeof content.uri === 'string') {
    const item = el('span', 'acp-resource');
    item.append(el('strong', '', content.name || content.uri), el('small', '', content.uri));
    return item;
  }
  if (content.type === 'resource' && typeof content.resource?.uri === 'string') {
    const resource = content.resource;
    const item = el('details');
    item.append(el('summary', '', resource.uri));
    if (typeof resource.text === 'string') item.append(el('pre', '', resource.text));
    else item.append(el('p', 'settings-note', () => acpText('embedded_binary')));
    return item;
  }
  const image = content.type === 'image' && ACP_RASTER_TYPES.has(content.mimeType);
  const audio = content.type === 'audio' && ACP_AUDIO_TYPES.has(content.mimeType);
  if ((!image && !audio) || typeof content.data !== 'string' || content.data.length > 4 * 1024 * 1024) return null;
  try {
    const bytes = Uint8Array.from(atob(content.data), character => character.charCodeAt(0));
    const media = el(image ? 'img' : 'audio');
    const url = URL.createObjectURL(new Blob([bytes], { type:content.mimeType }));
    media.dataset.acpBlob = url; media.src = url;
    if (image) { media.loading = 'lazy'; CLATI18n.attribute(media, 'alt', () => acpText('agent_image')); }
    else { media.controls = true; media.preload = 'none'; CLATI18n.attribute(media, 'aria-label', () => acpText('agent_audio')); }
    return media;
  } catch (_) { return null; }
}

function acpRenderActions(root, session, command) {
  const { capabilities = {}, options = {}, phase } = session;
  if (acpBusy(phase)) { root.replaceChildren(); return; }
  const items = [];
  for (const method of capabilities.authMethods || []) {
    if (method.type && method.type !== 'agent') continue;
    const button = acpButton('authenticate', () => command('authenticate', { option:method.id }));
    if (typeof method.name === 'string') button.title = method.name;
    items.push(button);
  }
  if (!session.origin && phase !== 'closed') items.push(acpButton('clear_permissions', () => command('clear_permissions')));
  if (capabilities.auth?.logout) items.push(acpButton('logout', () => command('logout')));
  if (!session.origin && phase === 'closed') items.push(acpButton('reopen', () => command('reopen')));
  else if (!session.origin) items.push(acpButton('close_session', () => command('close')));
  for (const item of session.updates?.filter(update => update.sessionUpdate === 'available_commands_update').at(-1)?.availableCommands || []) {
    if (typeof item.name !== 'string' || !/^[a-zA-Z0-9_-]+$/.test(item.name)) continue;
    const button = el('button', 'secondary', '/' + item.name); button.type = 'button';
    button.onclick = async () => {
      const args = await CLATDialog.prompt('/' + item.name);
      if (args !== null) void command('prompt', { text:('/' + item.name + ' ' + args).trim() });
    };
    items.push(button);
  }
  if (!session.origin && capabilities.sessionCapabilities?.delete) items.push(acpButton('delete_session', async () => {
    if (await CLATDialog.confirm(acpText('delete_notice'))) void command('delete');
  }));
  const modes = options.modes?.availableModes || [];
  if (modes.length) items.push(acpSelect(modes, options.modes.currentModeId, value => command('mode', { option:value })));
  for (const option of options.configOptions || []) {
    if (option.type === 'boolean') {
      const toggle = el('input'); toggle.type = 'checkbox'; toggle.checked = option.currentValue;
      toggle.onchange = () => command('config', { option:option.id, value:toggle.checked });
      const label = el('label'); label.append(el('span', '', option.name), toggle); items.push(label);
    } else if (Array.isArray(option.options)) {
      const list = option.options.flatMap(item => item.options || item);
      const choice = acpSelect(list, option.currentValue, value => command('config', { option:option.id, value }));
      const label = el('label'); label.append(el('span', '', option.name), choice); items.push(label);
    }
  }
  root.replaceChildren(...items);
}

function acpSelect(items, value, changed) {
  const select = el('select');
  for (const item of items) {
    const option = el('option', '', item.name); option.value = item.id ?? item.value;
    select.append(option);
  }
  select.value = value;
  select.onchange = () => changed(select.value);
  return select;
}

function installAcpRail() {
  acpRail = el('section', 'acp-delegations');
  acpRail.hidden = true;
  dom['composer-shell'].before(acpRail);
  setInterval(() => { if (state.connected && context.isActive() && !globalThis.document.hidden) void refreshAcpRail(); }, 2200);
  window.addEventListener('clat:workbench-updated', () => {
    if (context.isActive() && acpRail.dataset.origin !== (state.sessionId || '')) void refreshAcpRail();
  });
}

async function refreshAcpRail() {
  if (!acpRail) return;
  const origin = state.sessionId;
  const version = ++acpRailVersion;
  if (!origin || acpRail.dataset.origin !== origin) {
    acpRail.hidden = true;
    acpRail.replaceChildren();
    acpRail.dataset.origin = origin || '';
  }
  if (!origin) return;
  try {
    const records = await rpc('acp.delegations.list', { origin });
    if (version !== acpRailVersion || state.sessionId !== origin) return;
    acpRail.replaceChildren(...records.map(record => {
      const row = el('div', 'settings-row');
      const copy = el('span', 'settings-row-copy');
      copy.append(el('strong', '', record.label), el('small', '', () => acpText('phase_' + record.phase)));
      if (record.handoff !== 'none') copy.append(el('small', '', () => acpText('handoff_' + record.handoff)));
      row.append(copy, acpButton('details', () => openAcpSession(record.id)));
      if (acpBusy(record.phase) || record.handoff === 'pending') row.append(acpButton('cancel', async () => {
        try { await rpc('acp.delegations.cancel', {id:record.id}); void refreshAcpRail(); }
        catch (error) { updateRunState(error.message); }
      }));
      return row;
    }));
    acpRail.hidden = !records.length;
  } catch (_) { if (version === acpRailVersion) acpRail.hidden = true; }
}

function appendAcpInteraction(id, card) {
  card.dataset.acpInteraction = id;
  const dialog = [...document.querySelectorAll('dialog[data-acp-session]')]
    .find(node => node.dataset.acpSession === id && node.open);
  if (dialog) dialog.querySelector('.acp-interactions').append(card);
  else {
    const slot = el('div', 'acp-detached-interaction');
    slot.append(card);
    dom['composer-shell'].before(slot);
  }
}

function openRemoteAcpSessions() {
  const view = acpDialog('remote_sessions');
  const picker = acpField(view.fields, 'agent', el('select'));
  const list = el('div', 'settings-card');
  view.fields.append(list);
  let cursor = null, request = 0;
  const owner = composerScope;
  const more = acpButton('more', () => load(true));
  more.hidden = true;
  view.actions.append(more);
  async function load(next) {
    const probe = ++request;
    if (!next) { cursor = null; list.replaceChildren(); }
    try {
      const agent = picker.value;
      const response = await rpc('acp.sessions.remote', { agent, cursor });
      if (!view.dialog.open || !composerOwns(owner) || probe !== request) return;
      cursor = response.nextCursor;
      more.hidden = !cursor;
      view.status.textContent = response.sessions.length ? '' : acpText('no_sessions');
      for (const session of response.sessions) {
        const row = el('div', 'settings-row');
        const label = typeof session.title === 'string' && session.title.trim() ? session.title : session.id;
        const open = el('button', 'secondary', label);
        open.type = 'button';
        const requestId = crypto.randomUUID();
        open.onclick = async () => {
          open.disabled = true;
          try {
            const accepted = await rpc('acp.sessions.start', { agent, remote:session.id, requestId });
            if (view.dialog.open && composerOwns(owner)) { view.dialog.close(); openAcpSession(accepted.id); }
          } catch (error) { if (view.dialog.open) acpShowError(view.status, error); }
          finally { open.disabled = false; }
        };
        row.append(open);
        list.append(row);
      }
    } catch (error) { if (view.dialog.open && probe === request) acpShowError(view.status, error); }
  }
  picker.onchange = () => load(false);
  rpc('acp.agents.list', {}).then(agents => {
    if (!view.dialog.open || !context.isActive() || !composerOwns(owner)) return;
    for (const agent of agents.filter(item => item.trusted)) {
      const option = el('option', '', agent.label); option.value = agent.id; picker.append(option);
    }
    if (picker.value) void load(false);
    else view.status.textContent = acpText('trust_first');
  }).catch(error => { if (view.dialog.open) acpShowError(view.status, error); });
}
