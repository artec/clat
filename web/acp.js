/* ACP views share the project RPC and leave process authority in core. */
const acpText = (key, args = {}) => CLATI18n.msg('acp.' + key, args);
let acpRail = null, acpRailVersion = 0, acpSettingsRefresh = null;
let acpRailSnapshot = '';
let acpRailActive = new Set(), acpRailNotice = null, acpRailStarts = null;
const ACP_COMPLETION_NOTICE_MS = 15_000;

function acpButton(key, run, primary = false) {
  const button = el('button', primary ? 'primary' : 'secondary', () => acpText(key));
  button.type = 'button';
  button.onclick = run;
  return button;
}

function acpDialog(title, args = {}) {
  const dialog = el('dialog', 'workbench-dialog session-share-dialog');
  const head = el('header', 'workbench-dialog-header');
  const close = acpButton('close', () => dialog.close());
  head.append(el('h2', '', () => acpText(title, args)), close);
  const fields = el('div', 'session-share-fields');
  const status = el('p', 'settings-note');
  status.setAttribute('role', 'status');
  const actions = el('footer', 'workbench-dialog-actions');
  CLATI18n.attribute(dialog, 'aria-label', () => acpText(title, args));
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
  const project = state.workbench?.project?.root;
  let agents = [];
  let busy = false;
  const action = acpButton('start', act, true);
  view.actions.append(acpButton('title', () => { view.dialog.close(); openSettings('acp'); }), action);
  if (origin) view.actions.prepend(acpButton('delegation_history', () => {
    if (!current()) return;
    view.dialog.close();
    openAcpDelegationHistory(origin);
  }));
  view.fields.append(el('p', 'settings-note', () => acpText(delegate ? 'send_notice' : 'authority')));
  if (project) view.fields.append(el('p', 'settings-note', project));
  const runtimeHelp = el('div', 'acp-start-runtime');
  runtimeHelp.hidden = true;
  view.fields.append(runtimeHelp);
  const selectedAgent = () => agents.find(agent => agent.id === picker.value);
  picker.onchange = () => { view.status.textContent = ''; runtimeHelp.hidden = true; sync(); };
  sync();
  rpc('acp.agents.list', {}).then(agents => {
    if (!current()) return;
    updateAgents(agents);
    if (!agents.length) view.status.textContent = acpText('no_agents');
  }).catch(error => { if (current()) acpShowError(view.status, error); });

  function updateAgents(list, selectedId) {
    agents = list;
    picker.replaceChildren(...agents.map(agent => {
      const option = el('option', '', () => agent.trusted ? agent.label : acpText('untrusted_option', { agent:agent.label }));
      option.value = agent.id;
      return option;
    }));
    picker.value = selectedId || agents.find(agent => agent.trusted)?.id || agents[0]?.id || '';
    sync();
  }

  function sync() {
    const agent = selectedAgent();
    picker.disabled = busy || !agents.length;
    action.disabled = busy || !agent;
    CLATI18n.bind(action, 'textContent', () => acpText(agent && !agent.trusted ? 'trust' : 'start'));
  }

  async function act() {
    const agent = selectedAgent();
    if (!current() || busy || !agent) return;
    busy = true;
    sync();
    view.status.textContent = '';
    runtimeHelp.hidden = true;
    try {
      if (agent.trusted) {
        await startAcpFromPicker(view, { delegate, origin, agent, text, current, draftGeneration, generation });
      } else {
        const updated = await requestAcpTrust(agent, project, generation, current);
        if (updated && current()) {
          updateAgents(agents.map(entry => entry.id === updated.id ? updated : entry), updated.id);
          void acpSettingsRefresh?.();
        }
      }
    } catch (error) {
      if (current()) {
        acpShowError(view.status, error);
        const help = acpRuntimeHelp(error.presentation);
        if (help) {
          runtimeHelp.replaceChildren(help);
          runtimeHelp.hidden = false;
        }
      }
    } finally {
      busy = false;
      if (current()) sync();
    }
  }
}

async function startAcpFromPicker(view, options) {
  const { delegate, origin, agent, text, current, draftGeneration, generation } = options;
  if (delegate && !text.value.trim()) { view.status.textContent = acpText('empty_task'); return; }
  const value = await rpc(delegate ? 'acp.delegations.start' : 'acp.sessions.start', {
    agent:agent.id, requestId:crypto.randomUUID(), text:text?.value || '',
    continueWithClat:delegate, origin, expected_selection_generation:generation,
  });
  if (!current()) return;
  view.dialog.close();
  if (delegate && state.composerGeneration === draftGeneration && text.value === dom.prompt.value) {
    dom.prompt.value = '';
    dom.prompt.dispatchEvent(new Event('input', { bubbles:true }));
    resizePrompt();
  }
  if (delegate) {
    const acceptedOrigin = value.origin || origin;
    if (acceptedOrigin && value.id) {
      if (acpRailStarts?.origin !== acceptedOrigin) acpRailStarts = {origin:acceptedOrigin, ids:new Set()};
      acpRailStarts.ids.add(value.id);
    }
    if (!origin) resubscribe();
    else void refreshAcpRail();
  } else openAcpSession(value.id);
}

function installAcpSettings() {
  const page = dom['settings-dialog'];
  const nav = acpButton('title', () => selectSettingsCategory('acp'));
  nav.className = ''; nav.dataset.settingsCategory = 'acp';
  page.querySelector('.settings-nav').append(nav);
  const section = el('section', 'settings-section');
  section.hidden = true; section.dataset.settingsPanel = 'acp';
  section.append(el('h2', '', () => acpText('title')), el('p', 'settings-note', () => acpText('authority')));
  const heading = el('div', 'settings-group-heading');
  heading.append(el('h3', '', () => acpText('configured_agents')), acpButton('sessions', openAcpSessions));
  const list = el('div', 'settings-card managed-settings-list');
  const status = el('p', 'settings-note');
  status.setAttribute('role', 'status');
  section.append(heading, list, status);
  page.querySelector('.settings-content').append(section);
  const editor = installAcpAgentEditor(section);
  const active = () => !section.hidden && pageIsOpen(page);
  const added = new Set();
  let presets = null;
  editor.add.onclick = () => {
    const generation = state.selectionGeneration;
    const current = () => active() && context.isActive() && state.selectionGeneration === generation;
    presets = openAcpPresets({
      current, generation,
      custom: () => editor.open(null),
      added: agents => {
        for (const agent of agents) added.add(agent.id);
        return acpSettingsRefresh();
      },
    });
  };
  let serial = 0, rendered = 0;
  acpSettingsRefresh = async () => {
    if (!active()) return;
    const request = ++serial;
    const generation = state.selectionGeneration;
    const current = () => active() && serial === request && generation === state.selectionGeneration;
    try {
      const agents = await rpc('acp.agents.list', {});
      if (!current()) return;
      rendered = request;
      const ownsRows = () => active() && rendered === request && generation === state.selectionGeneration;
      list.replaceChildren(...agents.map(agent => {
        const row = acpAgentRow(agent, editor, status, ownsRows);
        if (added.has(agent.id)) row.classList.add('acp-agent-added');
        return row;
      }));
      if (!agents.length) list.append(el('p', 'settings-note', () => acpText('no_agents')));
      status.textContent = '';
    } catch (error) { if (current()) acpShowError(status, error); }
  };
  page.addEventListener('clat:page-enter', () => void acpSettingsRefresh());
  const leave = () => {
    serial++;
    rendered = 0;
    if (presets?.open) presets.close();
    added.clear();
    editor.close();
    list.replaceChildren();
    status.textContent = '';
  };
  page.addEventListener('clat:settings-leave', leave);
  page.addEventListener('clat:page-leave', leave);
}

function openAcpPresets(owner) {
  const view = acpDialog('add');
  view.dialog.classList.add('acp-preset-dialog');
  const heading = el('div', 'acp-preset-heading');
  const list = el('div', 'acp-preset-list');
  list.hidden = true;
  const selected = new Set(), errors = new Map(), controls = new Map();
  let rows = [], loading = false, saving = false;
  const scan = acpButton('preset_rescan', () => void refresh());
  const custom = acpButton('preset_custom', () => {
    if (!current() || saving) return;
    view.dialog.close();
    owner.custom();
  });
  const count = el('span', 'acp-preset-count');
  count.setAttribute('aria-live', 'polite');
  const add = acpButton('preset_add_selected', save, true);
  heading.append(el('p', 'settings-note', () => acpText('preset_host')), scan);
  view.fields.append(heading, list);
  view.actions.append(custom, count, add);
  sync();
  void refresh();
  return view.dialog;

  function current() {
    return view.dialog.open && owner.current();
  }

  function sync() {
    scan.disabled = loading || saving;
    custom.disabled = saving;
    add.disabled = loading || saving || !selected.size;
    CLATI18n.bind(count, 'textContent', () => acpText('preset_selected', {count:selected.size}));
    for (const [id, checkbox] of controls) {
      const row = rows.find(row => row.id === id);
      checkbox.disabled = loading || saving || row.status !== 'ready';
    }
  }

  function render() {
    controls.clear();
    list.replaceChildren(...rows.map(row => acpPresetRow(row, {
      checked: selected.has(row.id), error: errors.get(row.id), controls, refresh,
      change(checked) {
        if (checked) selected.add(row.id);
        else selected.delete(row.id);
        sync();
      },
    })));
    list.hidden = !rows.length;
    sync();
  }

  async function refresh(agentId) {
    if (!current() || loading || saving) return;
    loading = true; sync();
    CLATI18n.bind(view.status, 'textContent', () => acpText('preset_checking'));
    try {
      const fresh = await rpc('acp.agents.presets', {});
      if (!current()) return;
      const previous = new Map(rows.map(row => [row.id, row.detection]));
      rows = fresh;
      for (const id of selected) {
        if (!rows.some(row => row.id === id && row.status === 'ready' && row.detection === previous.get(id))) {
          selected.delete(id);
        }
      }
      errors.clear();
      CLATI18n.bind(view.status, 'textContent', () => '');
    } catch (error) {
      if (current()) CLATI18n.bind(view.status, 'textContent', () => error.message);
    } finally {
      loading = false;
      if (current()) {
        render();
        const checkbox = controls.get(agentId);
        if (checkbox && !checkbox.disabled) checkbox.focus();
      }
    }
  }

  async function save() {
    if (!current() || saving || loading || !selected.size) return;
    saving = true; errors.clear(); sync();
    CLATI18n.bind(view.status, 'textContent', () => acpText('preset_adding'));
    try {
      const presets = rows.filter(row => selected.has(row.id)).map(({id, detection}) => ({id, detection}));
      const results = await rpc('acp.agents.addPresets', {presets, expected_selection_generation:owner.generation});
      const added = [];
      for (const result of results) {
        if (result.agent) {
          added.push(result.agent);
          rows.find(row => row.id === result.id).status = 'added';
          selected.delete(result.id);
        } else {
          errors.set(result.id, result.error);
        }
      }
      if (added.length && owner.current()) void owner.added(added);
      if (!current()) return;
      if (!errors.size) { view.dialog.close(); return; }
      CLATI18n.bind(view.status, 'textContent', () => acpText('preset_partial_failure', {count:errors.size}));
    } catch (error) {
      if (current()) CLATI18n.bind(view.status, 'textContent', () => error.message);
    } finally {
      saving = false;
      if (current()) render();
    }
  }
}

function acpPresetRow(row, {checked, error, controls, refresh, change}) {
  const item = el('div', 'acp-preset-row');
  item.dataset.status = row.status;
  const label = el('label', 'acp-preset-choice');
  const checkbox = el('input'); checkbox.type = 'checkbox'; checkbox.checked = checked;
  checkbox.onchange = () => change(checkbox.checked);
  controls.set(row.id, checkbox);
  const name = el('strong', '', row.label);
  name.id = 'acp-preset-name-' + row.id;
  const state = el('small', 'acp-preset-status', () =>
    CLATI18n.present(row.statusDetail, acpText('preset_status_' + row.status)));
  state.id = 'acp-preset-status-' + row.id;
  checkbox.setAttribute('aria-labelledby', name.id);
  label.append(checkbox, name);
  const info = el('div', 'acp-preset-info');
  info.append(state);
  if (row.install?.url?.startsWith('https://')) {
    const runtimeIssue = row.status === 'missing_runtime';
    const guideLabel = runtimeIssue ? 'preset_runtime_guide' : 'preset_guide';
    const guideTitle = runtimeIssue ? 'preset_runtime_guide_for' : 'preset_guide_for';
    const guide = acpButton(guideLabel, () => openAcpPresetSetup(row, refresh, item.closest('dialog')));
    guide.className = 'acp-preset-guide';
    CLATI18n.attribute(guide, 'aria-label', () => acpText(guideTitle, {agent:row.label}));
    info.append(guide);
  }
  item.append(label, info);
  const detail = error && el('small', 'acp-preset-error', () => CLATI18n.present(error));
  if (detail) {
    detail.id = 'acp-preset-error-' + row.id;
    item.append(detail);
  }
  checkbox.setAttribute('aria-describedby', detail ? state.id + ' ' + detail.id : state.id);
  return item;
}

const ACP_RUNTIME_INSTALL = new Map([
  ['node', {name:'Node.js', url:'https://nodejs.org/en/download'}],
  ['python3', {name:'Python', url:'https://www.python.org/downloads/'}],
  ['bun', {name:'Bun', url:'https://bun.sh/docs/installation'}],
  ['deno', {name:'Deno', url:'https://docs.deno.com/runtime/getting_started/installation/'}],
]);

function acpRuntimeHelp(presentation, fallback) {
  if (presentation?.key !== 'acp.runtime_unavailable') return null;
  const runtime = ACP_RUNTIME_INSTALL.get(presentation.args?.runtime);
  const link = runtime ? acpSetupLink(runtime.url, 'preset_install_runtime', {runtime:runtime.name}) : fallback;
  if (!link) return null;
  const help = el('details', 'acp-setup-advanced');
  help.append(el('summary', '', () => acpText('runtime_install_help')), link);
  return help;
}

function openAcpPresetSetup(row, refresh, parent) {
  const runtimeIssue = row.status === 'missing_runtime';
  const title = runtimeIssue ? 'preset_runtime_guide_for' : 'preset_guide_for';
  const {dialog, fields, status, actions} = acpDialog(title, {agent:row.label});
  dialog.classList.add('acp-setup-dialog');
  const close = () => dialog.close();
  parent.addEventListener('close', close, {once:true});
  dialog.addEventListener('close', () => parent.removeEventListener('close', close), {once:true});

  const install = row.install;
  const official = acpSetupLink(install.url, 'preset_setup_source');
  const readyToCheck = install.method !== 'wsl' && install.method !== 'unavailable';
  if (runtimeIssue) {
    const hint = row.statusDetail?.key === 'acp.runtime_ambiguous'
      ? 'runtime_lookup_ambiguous' : 'runtime_lookup_host';
    fields.append(el('p', 'acp-setup-issue', () => CLATI18n.present(row.statusDetail)),
      el('p', 'settings-note', () => acpText(hint)));
    const help = acpRuntimeHelp(row.statusDetail, official);
    if (help) fields.append(help);
  } else {
    const note = row.requiresAdapter ? 'preset_setup_adapter' : 'preset_setup_builtin';
    fields.append(el('p', 'settings-note', () => acpText(note)));
    if (install.method === 'unavailable' || install.method === 'wsl') {
      const key = install.method === 'unavailable' ? 'preset_setup_unavailable' : 'preset_setup_detect_wsl';
      fields.append(el('p', 'settings-note', () => acpText(key)));
    } else {
      fields.append(el('p', 'settings-note', () => acpText('preset_setup_detect')));
    }
    official.classList.add('acp-setup-install');
    fields.append(official);
    if (install.command || install.method === 'download') fields.append(acpSetupAdvanced(install, status));
  }
  if (readyToCheck) {
    actions.append(acpButton('preset_rescan', () => { dialog.close(); void refresh(row.id); }, true));
  }
}

function acpSetupAdvanced({method, command, launcher, windows}, status) {
  const options = el('details', 'acp-setup-advanced');
  const key = method === 'terminal' ? (windows ? 'powershell' : 'terminal') : method;
  options.append(el('summary', '', () => acpText('preset_setup_advanced')),
    el('p', 'settings-note', () => acpText('preset_setup_' + key, {launcher})));
  if (command) options.append(acpSetupCommand(command, status));
  const links = el('div', 'acp-setup-links');
  if (method === 'npm' || method === 'wsl') {
    links.append(acpSetupLink(ACP_RUNTIME_INSTALL.get('node').url, 'preset_setup_node'));
  }
  if (method === 'wsl') links.append(acpSetupLink('https://learn.microsoft.com/windows/wsl/install', 'preset_setup_wsl_guide'));
  if (links.childElementCount) options.append(links);
  return options;
}

function acpSetupLink(url, key, args = {}) {
  const link = el('a', 'acp-preset-guide', () => acpText(key, args));
  link.href = url;
  link.rel = 'noopener noreferrer';
  if (!window.__TAURI__) link.target = '_blank';
  return link;
}

function acpSetupCommand(command, status) {
  const row = el('div', 'acp-setup-command');
  const copy = el('button', 'secondary', () => CLATI18n.msg('web.copy_code'));
  copy.type = 'button';
  copy.onclick = async () => {
    let feedback;
    try {
      await navigator.clipboard.writeText(command);
      feedback = 'web.copied';
    } catch (_) { feedback = 'web.copy_failed'; }
    CLATI18n.bind(status, 'textContent', () => CLATI18n.msg(feedback));
  };
  row.append(el('code', '', command), copy);
  return row;
}

function acpAgentRow(agent, editor, status, current) {
  const row = el('div', 'settings-row');
  const copy = el('span', 'settings-row-copy');
  copy.append(el('strong', '', agent.label), el('small', '', agent.program));
  const actions = el('div', 'settings-row-actions');
  actions.append(acpButton('edit', () => editor.open(agent)));
  actions.append(acpButton(agent.trusted ? 'revoke' : 'trust', () => confirmAcpTrust(agent, status, current)));
  actions.append(acpButton('remove', async () => {
    if (!await CLATDialog.confirm(acpText('remove_agent', { agent:agent.label })) || !current()) return;
    try {
      await rpc('acp.agents.remove', { id:agent.id, revision:agent.revision });
      if (current() && editor.editing(agent.id)) editor.close();
      if (current()) void acpSettingsRefresh();
    } catch (error) { if (current()) acpShowError(status, error); }
  }));
  row.append(copy, actions);
  return row;
}

async function confirmAcpTrust(agent, status, current) {
  const project = state.workbench?.project?.root;
  const generation = state.selectionGeneration;
  try {
    if (await requestAcpTrust(agent, project, generation, current) && current()) void acpSettingsRefresh();
  } catch (error) { if (current()) acpShowError(status, error); }
}

async function requestAcpTrust(agent, project, generation, current) {
  const message = acpText(agent.trusted ? 'revoke_notice' : 'trust_notice', {
    agent:agent.label, program:agent.program, root:project || '',
  });
  if (!await CLATDialog.confirm(message) || !current()) return;
  return rpc('acp.agents.trust', {
    id:agent.id, revision:agent.revision, allow:!agent.trusted, expected_selection_generation:generation,
  });
}

function installAcpAgentEditor(section) {
  const editor = managedSettingsEditor(section, acpText);
  const { add, form, message, field } = editor;
  const label = el('input'), program = el('input');
  const args = el('textarea'), env = el('textarea');
  label.autocomplete = 'off'; program.autocomplete = 'off'; args.rows = 2; env.rows = 2;
  field('label', label); field('program', program); field('arguments', args); field('environment', env);
  const notice = el('p', 'settings-note', () => acpText('update_arguments'));
  form.append(notice);
  let agent = null, version = 0;
  function close() {
    version++;
    agent = null;
    editor.close();
    for (const input of [label, program, args, env]) input.value = '';
    message.textContent = '';
    acknowledgeSettingsSave(section);
  }
  function open(entry) {
    close();
    agent = entry;
    label.value = entry?.label || '';
    program.value = entry?.program || '';
    args.value = entry?.argumentsHidden ? '' : JSON.stringify(entry?.args || []);
    env.value = JSON.stringify(entry?.env || []);
    notice.hidden = !entry?.argumentsHidden;
    save.disabled = false;
    editor.open(label, !!entry);
  }
  function asArray(value) {
    try {
      const parsed = JSON.parse(value);
      if (Array.isArray(parsed) && parsed.every(item => typeof item === 'string')) return parsed;
    } catch (_) {}
    throw Error(acpText('array_required'));
  }
  const save = acpButton('save', async () => {
    const request = version;
    const token = settingsSaveToken(section);
    save.disabled = true;
    message.textContent = '';
    try {
      const saved = await rpc('acp.agents.save', { id:agent?.id || null, agent:{
        label:label.value, program:program.value, args:asArray(args.value), env:asArray(env.value),
      } });
      if (request !== version) {
        if (context.isActive()) void acpSettingsRefresh();
        return;
      }
      if (settingsSaveToken(section) === token) { close(); add.focus(); }
      else agent = saved;
      await acpSettingsRefresh();
    } catch (error) { if (request === version) acpShowError(message, error); }
    finally { if (request === version) save.disabled = false; }
  }, true);
  const actions = el('div', 'settings-actions');
  actions.append(save, acpButton('cancel', () => { close(); add.focus(); }));
  form.append(actions);
  return { add, open, close, editing: id => agent?.id === id };
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
function openAcpSession(id, {delegation = false} = {}) {
  const view = acpDialog(delegation ? 'delegation_details' : 'session');
  const owner = composerScope;
  let version = '', pending = false, timer = null, session = null;
  const ui = createAcpSessionContent(view, id, {send, stop});
  const current = () => view.dialog.open && context.isActive() && composerOwns(owner);

  async function send() {
    if (ui.send.disabled) return;
    const files = Array.from(ui.attachments.files || []);
    const text = ui.editor.value;
    ui.send.disabled = true;
    let content;
    try { content = await acpPromptAttachments(files); }
    catch (error) { acpShowError(view.status, error); ui.send.disabled = false; return; }
    if (!current()) return;
    if (ui.editor.value === text && files.every((file, index) => ui.attachments.files[index] === file) && files.length === ui.attachments.files.length) {
      await command('prompt', { text, content });
    } else void refresh();
  }

  function stop() {
    if (!current() || !acpBusy(session?.phase)) return;
    ui.stop.disabled = true;
    rpc('acp.sessions.cancel', { id }).then(refresh).catch(error => {
      if (current()) { acpShowError(view.status, error); ui.stop.disabled = false; }
    });
  }

  async function command(action, params = {}) {
    if (pending || !current() || !session || acpBusy(session.phase)) return;
    if (action === 'prompt' && (session.origin || (!params.text.trim() && !params.content?.length))) {
      ui.send.disabled = false;
      return;
    }
    pending = true;
    ui.send.disabled = true;
    try {
      await rpc('acp.sessions.action', { id, action, requestId:crypto.randomUUID(), ...params });
      if (action === 'prompt') {
        if (ui.editor.value === params.text) ui.editor.value = '';
        if (params.content?.length) ui.attachments.value = '';
      }
    } catch (error) { if (current()) acpShowError(view.status, error); }
    finally { pending = false; await refresh(); }
  }

  async function refresh() {
    clearTimeout(timer);
    if (!current()) {
      if (view.dialog.open) view.dialog.close();
      return;
    }
    try {
      const fresh = await rpc('acp.sessions.status', { id });
      if (!current()) return;
      const previous = session;
      session = fresh;
      updateAcpSessionControls(view, ui, session, pending);
      const next = JSON.stringify(session);
      if (next !== version) {
        version = next;
        renderAcpSessionContent(view, ui, session, previous, command);
      }
    } catch (error) { if (current()) acpShowError(view.status, error); }
    if (current()) timer = setTimeout(refresh, acpBusy(session?.phase) ? 650 : 1800);
  }

  view.dialog.addEventListener('close', () => {
    clearTimeout(timer);
    acpReleaseMedia(ui.transcript);
    for (const card of [...ui.interactions.children]) {
      if (!card.classList.contains('resolved')) appendAcpInteraction(id, card);
    }
  }, { once:true });
  void refresh();
}

function createAcpSessionContent(view, id, {send:sendPrompt, stop:stopSession}) {
  view.dialog.dataset.acpSession = id;
  view.dialog.classList.add('acp-session-dialog');
  const meta = el('div', 'acp-session-meta');
  const agent = el('span', 'acp-session-agent');
  view.status.classList.add('acp-session-state');
  meta.append(agent, view.status);
  view.dialog.insertBefore(meta, view.fields);

  const interactions = el('div', 'acp-interactions');
  for (const card of document.querySelectorAll('[data-acp-interaction]')) {
    if (card.dataset.acpInteraction !== id) continue;
    const parent = card.parentNode;
    interactions.append(card);
    if (parent.classList.contains('acp-detached-interaction')) parent.remove();
  }
  const summary = el('div', 'acp-session-summary');
  const log = el('details', 'acp-session-log');
  log.append(el('summary', '', () => acpText('execution_log')));
  const transcript = el('div', 'acp-transcript');
  const tools = el('div', 'acp-session-tools');
  const composer = el('div', 'acp-session-composer');
  const editor = acpField(composer, 'task', el('textarea'));
  editor.rows = 2;
  const attachments = el('input'); attachments.type = 'file'; attachments.multiple = true;
  const attachmentField = acpField(composer, 'attachments', attachments).parentElement;
  view.fields.append(interactions, summary, log, transcript, tools, composer);
  const send = acpButton('send', sendPrompt, true);
  const stop = acpButton('cancel', stopSession);
  view.actions.append(stop, send);
  for (const element of [agent, summary, log, transcript, tools, composer, send, stop, view.actions]) {
    element.hidden = true;
  }
  return {agent, interactions, summary, log, transcript, tools, composer, editor, attachments, attachmentField, send, stop};
}

function updateAcpSessionControls(view, ui, session, pending) {
  const busy = acpBusy(session.phase);
  const canCompose = !session.origin && session.phase !== 'closed';
  ui.composer.hidden = ui.send.hidden = !canCompose;
  ui.send.disabled = pending || busy || !canCompose;
  ui.stop.hidden = !busy;
  ui.stop.disabled = !busy;
  view.actions.hidden = !canCompose && !busy;
  const prompt = session.capabilities?.promptCapabilities;
  ui.attachments.accept = [
    prompt?.image && 'image/png,image/jpeg,image/gif,image/webp',
    prompt?.audio && 'audio/mpeg,audio/wav,audio/ogg,audio/mp4',
  ].filter(Boolean).join(',');
  ui.attachments.disabled = ui.send.disabled;
  ui.attachmentField.hidden = !ui.attachments.accept;
}

function renderAcpSessionContent(view, ui, session, previous, command) {
  view.dialog.dataset.phase = session.phase;
  ui.agent.hidden = !session.label;
  ui.agent.textContent = session.label || '';
  CLATI18n.bind(view.status, 'textContent', () => session.error || acpText('phase_' + session.phase));

  const delegated = Boolean(session.origin);
  if (delegated) {
    const cards = [];
    if (session.task) cards.push(acpSessionCard('task', session.task));
    if (session.result?.trim()) cards.push(acpSessionCard('agent_result', session.result));
    ui.summary.replaceChildren(...cards);
    ui.summary.hidden = !cards.length;
    if (!previous) ui.log.open = !session.result?.trim();
    if (ui.transcript.parentElement !== ui.log) ui.log.append(ui.transcript);
  }
  acpRenderTranscript(ui.transcript, session);
  ui.transcript.hidden = !ui.transcript.childElementCount;
  ui.log.hidden = !delegated || ui.transcript.hidden;
  acpRenderActions(ui.tools, session, command);
}

function acpSessionCard(key, text) {
  const card = el('section', 'acp-session-card');
  const body = el('div', 'rich-text');
  renderMarkdown(body, text, {preserveLineBreaks:true});
  card.append(el('h3', '', () => acpText(key)), body);
  return card;
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
    const row = el('div', 'acp-transcript-row');
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
  const open = root.querySelector('details')?.open ?? false;
  root.replaceChildren();
  root.hidden = true;
  if (acpBusy(phase)) return;

  const auth = phase !== 'closed' && (!session.origin || session.canAuthenticate)
    ? acpAuthenticationButtons(capabilities, command) : [];
  if (session.origin) {
    root.replaceChildren(...auth);
    root.hidden = !auth.length;
    return;
  }

  const settings = el('details', 'acp-session-log');
  settings.open = open;
  settings.append(el('summary', '', () => acpText('session_controls')));
  const items = el('div', 'acp-session-management');
  if (phase !== 'closed') {
    if (capabilities.auth?.logout) auth.push(acpButton('logout', () => command('logout')));
    items.append(...auth, acpButton('clear_permissions', () => command('clear_permissions')),
      acpButton('close_session', () => command('close')));
  } else {
    items.append(acpButton('reopen', () => command('reopen')));
  }
  if (capabilities.sessionCapabilities?.delete) items.append(acpButton('delete_session', async () => {
    if (await CLATDialog.confirm(acpText('delete_notice'))) void command('delete');
  }));
  settings.append(items);
  if (phase !== 'closed') acpRenderSessionOptions(settings, options, command);
  const commandPicker = phase !== 'closed' && acpCommandPicker(session, command);
  if (commandPicker) root.append(commandPicker);
  root.append(settings);
  root.hidden = false;
}

function acpAuthenticationButtons(capabilities, command) {
  return (capabilities.authMethods || [])
    .filter(method => !method.type || method.type === 'agent')
    .map(method => {
      const button = acpButton('authenticate', () => command('authenticate', {option:method.id}));
      if (typeof method.name === 'string' && method.name.trim()) {
        CLATI18n.bind(button, 'textContent', () => acpText('authenticate_with', {method:method.name}));
      }
      return button;
    });
}

function acpCommandPicker(session, command) {
  const available = session.updates?.findLast(update => update.sessionUpdate === 'available_commands_update')?.availableCommands || [];
  const names = new Set(available.map(item => item?.name)
    .filter(name => typeof name === 'string' && /^[a-zA-Z0-9_-]+$/.test(name)));
  if (!names.size) return null;
  const root = el('div', 'acp-command-picker');
  const menu = acpField(root, 'agent_commands', el('select'));
  menu.append(el('option', '', () => acpText('choose_command')));
  menu.firstElementChild.value = '';
  for (const name of names) {
    const option = el('option', '', '/' + name);
    option.value = name;
    menu.append(option);
  }
  const run = acpButton('run_command', async () => {
    const name = menu.value;
    if (!name) return;
    const args = await CLATDialog.prompt('/' + name);
    if (args !== null) void command('prompt', {text:('/' + name + ' ' + args).trim()});
  });
  run.disabled = true;
  menu.onchange = () => { run.disabled = !menu.value; };
  root.append(run);
  return root;
}

function acpRenderSessionOptions(root, options, command) {
  const modes = options.modes?.availableModes || [];
  const controls = el('div', 'acp-session-options');
  if (modes.length) {
    acpField(controls, 'session_mode', acpSelect(modes, options.modes.currentModeId,
      value => command('mode', {option:value})));
  }
  for (const option of options.configOptions || []) {
    const label = el('label');
    label.append(el('span', '', option.name));
    if (option.type === 'boolean') {
      const toggle = el('input'); toggle.type = 'checkbox'; toggle.checked = option.currentValue;
      toggle.onchange = () => command('config', {option:option.id, value:toggle.checked});
      label.append(toggle);
    } else if (Array.isArray(option.options)) {
      const list = option.options.flatMap(item => item.options || item);
      label.append(acpSelect(list, option.currentValue, value => command('config', {option:option.id, value})));
    } else continue;
    controls.append(label);
  }
  if (controls.childElementCount) root.append(controls);
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
    acpRailSnapshot = '';
    acpRailActive.clear();
    acpRailNotice = null;
    if (origin && acpRailStarts?.origin !== origin) acpRailStarts = null;
  }
  if (!origin) return;
  try {
    const records = await rpc('acp.delegations.list', { origin });
    if (version !== acpRailVersion || state.sessionId !== origin) return;
    const rows = acpRailRows(origin, records);
    const snapshot = JSON.stringify(rows);
    if (snapshot !== acpRailSnapshot) {
      acpRail.replaceChildren(...rows.map(record => acpDelegationRow(record)));
      acpRailSnapshot = snapshot;
    }
    acpRail.hidden = !rows.length;
  } catch (_) { if (version === acpRailVersion) acpRail.hidden = true; }
}

function acpDelegationActive(record) {
  return acpBusy(record.phase) || record.handoff === 'pending';
}

function acpRailRows(origin, records) {
  const rows = records.map(({id, label, phase, handoff}) => ({id, label, phase, handoff}));
  const active = rows.filter(acpDelegationActive);
  const starts = acpRailStarts?.origin === origin ? acpRailStarts.ids : null;
  const finished = rows.findLast(row => !acpDelegationActive(row) && (acpRailActive.has(row.id) || starts?.has(row.id)));
  acpRailActive = new Set(active.map(row => row.id));
  for (const row of rows) starts?.delete(row.id);
  const now = performance.now();
  if (finished) acpRailNotice = {id:finished.id, expires:now + ACP_COMPLETION_NOTICE_MS};
  const notice = acpRailNotice?.expires > now && rows.find(row => row.id === acpRailNotice.id && !acpDelegationActive(row));
  if (!notice) acpRailNotice = null;
  return notice ? [...active, notice] : active;
}

function openAcpDelegationHistory(origin) {
  const view = acpDialog('delegation_history');
  const owner = composerScope;
  const current = () => view.dialog.open && context.isActive() && composerOwns(owner) && state.sessionId === origin;
  const list = el('div', 'settings-card acp-delegation-history');
  view.actions.hidden = true;
  view.fields.append(list);
  rpc('acp.delegations.list', {origin}).then(records => {
    if (!current()) return;
    const finished = records.filter(record => !acpDelegationActive(record));
    list.hidden = !finished.length;
    if (!finished.length) CLATI18n.bind(view.status, 'textContent', () => acpText('no_delegation_history'));
    list.replaceChildren(...finished.map(record => acpDelegationRow(record, () => {
      if (!current()) return;
      view.dialog.close();
      openAcpSession(record.id, {delegation:true});
    })));
  }).catch(error => { if (current()) acpShowError(view.status, error); });
}

function acpDelegationRow(record, open = () => openAcpSession(record.id, {delegation:true})) {
  const row = el('div', 'acp-delegation-row');
  row.dataset.phase = record.phase;
  const copy = el('span', 'acp-delegation-copy');
  copy.append(
    el('strong', '', record.label),
    el('small', 'acp-delegation-phase', () => acpText('phase_' + record.phase)),
  );
  if (record.handoff !== 'none') copy.append(el('small', '', () => acpText('handoff_' + record.handoff)));
  const details = acpButton('details', open);
  details.classList.add('shell-button');
  row.append(copy, details);
  if (record.task) row.append(el('small', 'acp-delegation-task', record.task));
  if (acpDelegationActive(record)) {
    const cancel = acpButton('cancel', async () => {
      try { await rpc('acp.delegations.cancel', {id:record.id}); void refreshAcpRail(); }
      catch (error) { updateRunState(error.message); }
    });
    cancel.classList.add('shell-button');
    row.append(cancel);
  }
  return row;
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
