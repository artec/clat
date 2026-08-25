/* CLAT web workbench — a thin projection of Application/RPC/SSE facts.
 *
 * RF-1/RF-5: browser persistence contains presentation preferences and one
 * origin-scoped pairing token. The origin includes the serve port, so another
 * local HTTP service cannot read it. Session content, run state, permission mode, model and MCP
 * state are rebuilt from serve. Dynamic model/tool text is always written
 * through textContent; this file never uses innerHTML.
 */

'use strict';

const PRESENTATION_KEY = 'clat.presentation.v1';
const AUTH_KEY = 'clat.auth.v1';
const MOBILE_BREAKPOINT = 760;
const INSPECTOR_DRAWER_BREAKPOINT = 1180;

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

const ICON_PATHS = {
  user: ['M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z', 'M5 21c.8-4 3.1-6 7-6s6.2 2 7 6'],
  agent: ['M7 8h10v9H7z', 'M9 4h6M12 4v4M4 11h3m10 0h3M10 13h.01m4-.01h.01'],
  tool: ['m14 6 4 4-8 8-4 1 1-4z', 'm13 7 4 4'],
  trace: ['M5 17 9 9l4 6 3-10 3 12', 'M4 20h16'],
  info: ['M12 8h.01M11 12h1v5h1'],
  check: ['m5 12 4 4L19 6'],
  wasm: ['M5 5h14v14H5z', 'M9 9h6v6H9z'],
  mcp: ['M4 12h5m6-5h5m-5 10h5', 'M9 12l6-5m-6 5 6 5'],
};

function svgIcon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  for (const data of ICON_PATHS[name] || ICON_PATHS.info) {
    const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    path.setAttribute('d', data);
    svg.appendChild(path);
  }
  return svg;
}

const EVENT_LABELS = {
  run_started: 'Run started',
  model_requested: 'Model request started',
  model_stream: 'Model response streaming',
  model_responded: 'Model response ready',
  text_delta: 'Answer text',
  refusal_delta: 'Model refusal',
  reasoning_delta: 'Reasoning trace',
  reasoning_summary_delta: 'Reasoning summary',
  tool_requested: 'Tool requested',
  permission_checked: 'Permission checked',
  permission_denied: 'Permission denied',
  tool_started: 'Tool running',
  tool_finished: 'Tool finished',
  steering_applied: 'Steering applied',
  retry_scheduled: 'Retry scheduled',
  turn_ended: 'Turn ended',
  compaction: 'History compacted',
  run_completed: 'Run completed',
  run_cancelled: 'Run cancelled',
  run_failed: 'Run failed',
};

function humanEventName(id) {
  if (!id) return 'Runtime event';
  return EVENT_LABELS[id] || String(id)
    .replace(/[._-]+/g, ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function show(node) { node.classList.remove('hidden'); }
function hide(node) { node.classList.add('hidden'); }

function readAuthToken() {
  const token = localStorage.getItem(AUTH_KEY);
  return token && token.trim() ? token : '';
}

function saveAuthToken(token) {
  localStorage.setItem(AUTH_KEY, token);
}

function clearAuthToken() {
  localStorage.removeItem(AUTH_KEY);
}

function readPresentationPreference() {
  try {
    const value = JSON.parse(localStorage.getItem(PRESENTATION_KEY) || 'null');
    if (value && typeof value === 'object') return value;
  } catch (_) { /* corrupt preference: use defaults */ }
  return {};
}

function savePresentationPreference() {
  localStorage.setItem(PRESENTATION_KEY, JSON.stringify({
    theme: state.theme,
    sidebar: state.sidebar,
    inspector: state.inspector,
  }));
}

const presentation = readPresentationPreference();
const state = {
  token: '',
  connected: false,
  runActive: false,
  sessionId: null,
  sessions: [],
  workbench: null,
  lastUsage: null,
  reconnectDelayMs: 1000,
  stream: null,
  run: null,
  theme: ['system', 'light', 'dark'].includes(presentation.theme) ? presentation.theme : 'system',
  sidebar: presentation.sidebar === 'collapsed' ? 'collapsed' : 'expanded',
  inspector: window.innerWidth <= INSPECTOR_DRAWER_BREAKPOINT
    ? 'closed'
    : (presentation.inspector === 'closed' ? 'closed' : 'open'),
  marketPackages: [],
  marketLoaded: false,
  marketFallback: false,
  workbenchRequest: 0,
};

const dom = {};
for (const id of [
  'landing', 'connect-form', 'connect-token', 'connect-error', 'app', 'sidebar',
  'sidebar-toggle', 'mobile-sidebar-open', 'mobile-sidebar-close', 'sidebar-backdrop',
  'project-name', 'project-root', 'session-title', 'conn-status', 'sidebar-connection',
  'header-model', 'header-permission', 'new-session', 'session-search', 'session-count',
  'session-list', 'session-empty', 'transcript-scroll', 'empty-state', 'transcript',
  'prompt', 'send', 'cancel', 'run-state', 'composer-permission',
  'composer-permission-label', 'inspector', 'inspector-toggle', 'inspector-close',
  'detail-run', 'detail-seq', 'detail-session', 'detail-model', 'detail-protocol',
  'detail-context', 'detail-budget', 'capability-list', 'detail-mcp', 'mcp-servers',
  'settings-open', 'settings-dialog', 'theme-options', 'permission-options',
  'full-access-confirm-row', 'full-access-confirm', 'settings-error', 'settings-saved',
  'permission-save', 'market-open', 'market-dialog', 'market-close', 'market-search',
  'market-status', 'market-list',
]) {
  dom[id] = document.getElementById(id);
}

function applyPresentation() {
  document.documentElement.dataset.theme = state.theme;
  dom.app.dataset.sidebar = state.sidebar;
  dom.app.dataset.inspector = state.inspector;
  dom['sidebar-toggle'].setAttribute('aria-expanded', String(state.sidebar === 'expanded'));
  dom['sidebar-toggle'].setAttribute(
    'aria-label', state.sidebar === 'expanded' ? 'Collapse sidebar' : 'Expand sidebar',
  );
  dom['inspector-toggle'].setAttribute('aria-expanded', String(state.inspector === 'open'));
  const themeRadio = document.querySelector(`input[name="theme"][value="${state.theme}"]`);
  if (themeRadio) themeRadio.checked = true;
  syncPanelAccessibility();
}

applyPresentation();

/* —— RPC and stream transport ————————————————————————————— */

async function rpc(method, params) {
  const response = await fetch('/api/' + method, {
    method: 'POST',
    headers: {
      Authorization: 'Bearer ' + state.token,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(params || {}),
  });
  const body = await response.json().catch(() => null);
  if (!body || typeof body.ok !== 'boolean') {
    throw Object.assign(new Error('malformed rpc reply'), { code: 'internal' });
  }
  if (body.ok) return body.value;
  const error = new Error((body.error && body.error.message) || 'request failed');
  error.code = (body.error && body.error.code) || 'internal';
  throw error;
}

function parseSseBlock(block) {
  let event = null;
  const dataLines = [];
  for (const rawLine of block.split('\n')) {
    const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;
    if (line === '' || line.startsWith(':')) continue;
    if (line.startsWith('event:')) event = line.slice(6).trim();
    else if (line.startsWith('data:')) dataLines.push(line.slice(5).replace(/^ /, ''));
  }
  if (dataLines.length === 0) return null;
  return { event: event || 'message', data: dataLines.join('\n') };
}

function parseJson(text) {
  try { return JSON.parse(text); } catch (_) { return null; }
}

async function openStream() {
  const controller = new AbortController();
  state.stream = controller;
  const response = await fetch('/api/events', {
    headers: { Authorization: 'Bearer ' + state.token },
    signal: controller.signal,
  });
  if (!response.ok || !response.body) {
    throw Object.assign(new Error('event stream rejected: HTTP ' + response.status), {
      code: response.status === 401 || response.status === 403 ? 'unauthorized' : 'internal',
    });
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let scanned = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let boundary;
    while ((boundary = buffer.indexOf('\n\n', scanned)) !== -1) {
      const block = buffer.slice(0, boundary + 2);
      buffer = buffer.slice(boundary + 2);
      scanned = 0;
      const frame = parseSseBlock(block);
      if (frame) handleFrame(frame);
    }
    scanned = Math.max(0, buffer.length - 1);
  }
  throw Object.assign(new Error('event stream ended'), { code: 'stream-ended' });
}

function setConnStatus(text, stateName) {
  dom['conn-status'].textContent = text;
  dom['conn-status'].dataset.state = stateName;
  dom['sidebar-connection'].textContent = stateName === 'live'
    ? 'Local runtime connected'
    : text.replace('…', '');
}

async function connect() {
  setConnStatus('connecting…', 'connecting');
  try {
    await openStream();
  } catch (error) {
    if (error.name === 'AbortError') return;
    state.connected = false;
    state.runActive = false;
    updateRunState('');
    updateRunDetail();
    if (error.code === 'unauthorized') {
      setConnStatus('pairing required', 'failed');
      state.token = '';
      clearAuthToken();
      stopApp();
      return;
    }
    setConnStatus('reconnecting…', 'reconnecting');
    const delay = state.reconnectDelayMs;
    state.reconnectDelayMs = Math.min(delay * 2, 10000);
    setTimeout(connect, delay);
  }
}

function stopApp() {
  if (state.stream) state.stream.abort();
  hide(dom.app);
  show(dom.landing);
  dom['connect-token'].focus();
}

function resubscribe() {
  if (state.stream) state.stream.abort();
  state.reconnectDelayMs = 1000;
  connect();
}

/* —— SSE projection ——————————————————————————————————————— */

function handleFrame(frame) {
  const payload = parseJson(frame.data);
  if (!payload) {
    console.warn('[clat] non-JSON frame dropped:', frame.event);
    return;
  }
  switch (frame.event) {
    case 'replay': handleReplay(payload.replay); break;
    case 'event': handleLive(payload.event); break;
    case 'subscribed': onSubscribed(payload.ctl || payload); break;
    case 'approval.requested': onApprovalRequested(payload.ctl); break;
    case 'prompt.settled': onSettled(payload.ctl); break;
    case 'notice': onNotice(payload.ctl); break;
    case 'replay.begin':
      dom.transcript.replaceChildren();
      show(dom['empty-state']);
      state.run = null;
      state.runActive = false;
      state.lastUsage = null;
      updateRunDetail();
      break;
    case 'replay.end':
      syncEmptyState();
      break;
    default:
      console.warn('[clat] unknown frame type:', frame.event);
  }
}

function handleReplay(event) {
  if (!event || !event.type) return;
  switch (event.type) {
    case 'user_message': addUserMessage(event.text); break;
    case 'assistant_message': {
      const bubble = addAssistantMessage();
      if (event.reasoning) bubble.setReasoning(event.reasoning);
      bubble.appendBody(event.text || '');
      for (const call of event.tool_calls || []) {
        addToolCard(call.name, jsonText(call.arguments), null, false);
      }
      break;
    }
    case 'permission_checked':
      addNoticeLine(event.tool + ' → ' + decisionText(event.decision), event.type);
      break;
    case 'tool_requested':
      addToolCard(event.call && event.call.name, jsonText(event.call && event.call.arguments), null, false);
      break;
    case 'tool_finished':
      addToolCard(event.tool, '← ' + jsonText(event.output), null, event.is_error);
      break;
    case 'retry_scheduled':
      addNoticeLine('#' + event.retry + ' in ' + event.delay_ms + 'ms', event.type);
      break;
    case 'turn_ended':
      addNoticeLine('turn ' + event.turn + ' · ' + turnEndText(event.reason), event.type);
      break;
    case 'compaction': addNoticeLine('', event.type); break;
    default: console.warn('[clat] unknown replay type:', event.type);
  }
}

function handleLive(event) {
  if (!event || !event.type) return;
  switch (event.type) {
    case 'run_started':
      state.runActive = true;
      updateRunState('running');
      updateRunDetail();
      show(dom.cancel);
      state.run = null;
      if (!lastUserMessageIs(event.prompt)) addUserMessage(event.prompt);
      break;
    case 'model_requested':
      addTraceEvent(event.type, [event.provider, event.model].filter(Boolean).join(' · '));
      ensureAssistant();
      break;
    case 'model_stream': {
      const bubble = ensureAssistant();
      const inner = event.event || {};
      if (inner.type === 'text_delta' || inner.type === 'refusal_delta') {
        bubble.appendBody(inner.delta || '');
      } else if (inner.type === 'reasoning_delta' || inner.type === 'reasoning_summary_delta') {
        bubble.setTraceKind(inner.type);
        bubble.appendReasoning(inner.delta || '');
      }
      break;
    }
    case 'model_responded':
      addTraceEvent(event.type, turnEndText(event.finish_reason));
      break;
    case 'tool_requested':
      state.run = state.run || {};
      addToolCard(event.call && event.call.name, jsonText(event.call && event.call.arguments), null, false);
      break;
    case 'permission_checked':
      addNoticeLine(event.tool + ' → ' + decisionText(event.decision), event.type);
      break;
    case 'permission_denied':
      addNoticeLine(event.tool + ' — ' + (event.reason || ''), event.type);
      break;
    case 'tool_started': break;
    case 'tool_finished': {
      const result = event.result || {};
      addToolCard(result.tool_name, '← ' + jsonText(result.output), null, result.is_error);
      break;
    }
    case 'steering_applied': addNoticeLine('', event.type); break;
    case 'run_completed':
    case 'run_cancelled':
    case 'run_failed':
      finishRun(event);
      break;
    default:
      console.warn('[clat] unknown RunEvent type:', event.type);
  }
  scrollIfNearEnd();
}

function finishRun(event) {
  state.runActive = false;
  state.run = null;
  updateRunState('');
  updateRunDetail();
  hide(dom.cancel);
  if (event.type === 'run_failed') {
    addVerdict('failed', 'run failed — ' + (event.message || 'unknown error'));
  }
}

function onSubscribed(ctl) {
  state.connected = true;
  state.reconnectDelayMs = 1000;
  setConnStatus('live', 'live');
  setSwitching(false);
  if (ctl && ctl.session_id) state.sessionId = ctl.session_id;
  refreshWorkbench();
  refreshSessions();
}

function onApprovalRequested(ctl) {
  if (!ctl || !ctl.rpc_id) return;
  const request = ctl.request || {};
  const card = el('div', 'approval-card');
  card.dataset.rpcId = ctl.rpc_id;
  const title = el('div', 'title', 'Permission required — ' + (request.tool || 'unknown tool'));
  const meta = el('div', 'note', (request.effect || 'side effect') + ' · ' + (request.reason || ''));
  const args = el('div', 'args', jsonText(request.arguments));
  const actions = el('div', 'actions');
  const note = el('div', 'note resolution', '');
  const resolve = (text) => {
    card.classList.add('resolved');
    note.textContent = text;
    actions.replaceChildren();
  };

  for (const decision of ['allow', 'deny']) {
    const button = el(
      'button', decision === 'allow' ? 'primary' : 'ghost', decision === 'allow' ? 'Allow' : 'Deny',
    );
    button.type = 'button';
    button.addEventListener('click', async () => {
      button.disabled = true;
      try {
        await rpc('approval.respond', { rpcId: ctl.rpc_id, decision });
        resolve('answered: ' + decision);
      } catch (error) {
        if (error.code === 'not-pending') resolve('already answered elsewhere');
        else {
          button.disabled = false;
          note.textContent = 'error: ' + error.message;
        }
      }
    });
    actions.appendChild(button);
  }
  card.append(title, meta, args, actions, note);
  appendTranscript(card);
  scrollIfNearEnd();
}

function onSettled(ctl) {
  const outcome = (ctl && ctl.outcome) || {};
  state.lastUsage = outcome.usage || null;
  switch (outcome.type) {
    case 'completed':
      addVerdict('completed', 'completed · ' + (outcome.turns || 0) + ' turns · ' + usageText(outcome.usage));
      break;
    case 'cancelled': addVerdict('cancelled', 'cancelled after ' + (outcome.turns || 0) + ' turns'); break;
    case 'failed': addVerdict('failed', 'failed — ' + (outcome.error || 'unknown error')); break;
    default: console.warn('[clat] unknown settled outcome:', outcome.type);
  }
  refreshSessions();
  refreshWorkbench();
  scrollIfNearEnd();
}

function onNotice(ctl) {
  const payload = ctl && ctl.payload;
  switch (ctl && ctl.kind) {
    case 'monitor': updateRunState(typeof payload === 'string' ? payload : ''); break;
    case 'compaction':
      addNoticeLine(payload && payload.note ? String(payload.note) : 'compaction updated');
      refreshWorkbench();
      break;
    case 'title':
      if (payload && payload.title) {
        dom['session-title'].textContent = payload.title;
        refreshSessions();
        refreshWorkbench();
      }
      break;
    case 'mcp_startup':
      addNoticeLine('mcp startup: ' + jsonText(payload));
      refreshWorkbench();
      break;
    case 'process_finished': {
      const id = payload && payload.session_id;
      const state = payload && payload.timed_out
        ? 'timed out'
        : payload && payload.cancelled
          ? 'cancelled'
          : payload && payload.terminated
            ? 'terminated'
            : payload && payload.signal
              ? 'signal ' + payload.signal
            : payload && payload.exit_code !== null && payload.exit_code !== undefined
              ? 'exit ' + payload.exit_code
              : 'finished';
      addNoticeLine('process ' + id + ': ' + state);
      break;
    }
    default: break;
  }
}

/* —— Transcript rendering ———————————————————————————————— */

function appendTranscript(node) {
  hide(dom['empty-state']);
  dom.transcript.appendChild(node);
  return node;
}

function syncEmptyState() {
  if (dom.transcript.childElementCount === 0) show(dom['empty-state']);
  else hide(dom['empty-state']);
}

function lastUserMessageIs(text) {
  const messages = dom.transcript.querySelectorAll('.msg.user .body');
  return messages.length > 0 && messages[messages.length - 1].textContent === text;
}

function addUserMessage(text) {
  const msg = el('div', 'msg user');
  const marker = el('span', 'marker');
  marker.appendChild(svgIcon('user'));
  msg.append(marker, el('div', 'body', text));
  return appendTranscript(msg);
}

function addAssistantMessage() {
  const msg = el('div', 'msg assistant');
  const marker = el('span', 'marker');
  marker.appendChild(svgIcon('agent'));
  msg.append(marker);
  const body = el('div', 'body');
  const reasoning = el('details', 'reasoning hidden');
  const reasoningSummary = el('summary');
  reasoningSummary.append(svgIcon('trace'), el('span', null, humanEventName('reasoning_delta')));
  const reasoningCopy = el('pre', 'reasoning-copy');
  reasoning.append(reasoningSummary, reasoningCopy);
  msg.append(reasoning, body);
  appendTranscript(msg);
  return {
    appendBody(text) { body.textContent += text; },
    appendReasoning(text) {
      show(reasoning);
      reasoningCopy.textContent += text;
    },
    setReasoning(text) {
      show(reasoning);
      reasoningCopy.textContent = text;
    },
    setTraceKind(eventId) {
      reasoningSummary.querySelector('span').textContent = humanEventName(eventId);
      reasoningSummary.title = 'Event ID: ' + eventId;
    },
  };
}

function ensureAssistant() {
  if (!state.run || !state.run.assistant) {
    state.run = state.run || {};
    state.run.assistant = addAssistantMessage();
  }
  return state.run.assistant;
}

function addToolCard(name, argsText, outputText, isError) {
  const card = el('details', 'tool-card' + (isError ? ' is-error' : ''));
  const summary = el('summary');
  summary.append(
    svgIcon('tool'),
    el('span', null, name || 'Unknown tool'),
    el('span', 'tool-state', isError ? 'Failed' : 'Tool trace'),
  );
  const body = el('div', 'tool-body');
  if (argsText) body.appendChild(el('div', null, argsText));
  if (outputText) body.appendChild(el('div', null, outputText));
  card.append(summary, body);
  return appendTranscript(card);
}

function addVerdict(kind, text) {
  appendTranscript(el('div', 'verdict ' + kind, text));
  scrollIfNearEnd();
}

function addNoticeLine(text, eventId) {
  const line = el('div', 'notice-line');
  line.appendChild(svgIcon(eventId ? 'trace' : 'info'));
  const label = eventId ? humanEventName(eventId) : text;
  const copy = el('span');
  if (eventId) {
    copy.append(el('b', null, label));
    if (text) copy.append(el('small', null, ' · ' + text));
    line.classList.add('trace-event');
    line.title = 'Event ID: ' + eventId;
  } else {
    copy.textContent = text;
  }
  line.appendChild(copy);
  appendTranscript(line);
}

function addTraceEvent(eventId, detail) {
  addNoticeLine(detail || '', eventId);
}

function nearEnd() {
  const viewport = dom['transcript-scroll'];
  return viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 180;
}

function scrollIfNearEnd() {
  const viewport = dom['transcript-scroll'];
  if (nearEnd()) viewport.scrollTop = viewport.scrollHeight;
}

function jsonText(value) {
  if (value === undefined || value === null) return '';
  if (typeof value === 'string') return value;
  try { return JSON.stringify(value, null, 2); } catch (_) { return String(value); }
}

function decisionText(decision) {
  if (decision === null || decision === undefined) return '?';
  if (typeof decision === 'string') return humanEventName(decision);
  if (typeof decision === 'object') {
    const key = Object.keys(decision)[0];
    return humanEventName(key) + (decision[key] ? ` (${decision[key]})` : '');
  }
  return String(decision);
}

function turnEndText(reason) {
  if (typeof reason === 'string') return humanEventName(reason);
  if (reason && typeof reason === 'object') {
    const key = Object.keys(reason)[0];
    return humanEventName(key) + (reason[key] ? ` (${reason[key]})` : '');
  }
  return '?';
}

function usageText(usage) {
  if (!usage) return 'usage unavailable';
  const parts = [(usage.input_tokens || 0) + ' in', (usage.output_tokens || 0) + ' out'];
  if (usage.cached_input_tokens !== undefined) parts.push(usage.cached_input_tokens + ' cached');
  return parts.join(' · ');
}

function compactNumber(value) {
  if (value === null || value === undefined) return '—';
  if (value >= 1_000_000) return (value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1) + 'm';
  if (value >= 1_000) return (value / 1_000).toFixed(value >= 10_000 ? 0 : 1) + 'k';
  return String(value);
}

/* —— Workbench and session read models ——————————————————— */

const PERMISSION_LABELS = {
  'read-only': 'Read Only',
  'workspace-write': 'Project Write',
  'danger-full-access': 'Full Access',
};

function updateDocumentTitle(sessionTitle) {
  const session = (state.workbench && state.workbench.session) || {};
  const hasSession = sessionTitle !== undefined || Boolean(session.id);
  const name = sessionTitle !== undefined ? sessionTitle : (session.title || 'Untitled session');
  document.title = hasSession ? name + ' - CLAT' : 'CLAT · local agent workbench';
}

async function refreshWorkbench() {
  const request = ++state.workbenchRequest;
  try {
    const info = await rpc('workbench.info', {});
    if (request !== state.workbenchRequest) return;
    state.workbench = info;
    const project = info.project || {};
    const session = info.session || {};
    const model = info.model || {};
    const permission = info.permission || {};
    state.sessionId = session.id || null;

    dom['project-name'].textContent = project.name || 'Current project';
    dom['project-root'].textContent = project.root || '';
    dom['project-root'].title = project.root || '';
    dom['session-title'].textContent = session.title || 'Untitled session';
    updateDocumentTitle();
    dom['header-model'].textContent = model.model || 'model unavailable';
    const modeLabel = permission.label || PERMISSION_LABELS[permission.mode] || 'Permission mode';
    dom['header-permission'].textContent = modeLabel;
    dom['composer-permission-label'].textContent = modeLabel;

    dom['detail-seq'].textContent = session.committed_seq === null || session.committed_seq === undefined
      ? '—' : String(session.committed_seq);
    dom['detail-session'].textContent = session.id ? shortenId(session.id) : 'Fresh';
    dom['detail-session'].title = session.id || '';
    dom['detail-model'].textContent = model.model || '—';
    dom['detail-protocol'].textContent = [model.protocol, model.active_profile || model.preset]
      .filter(Boolean).join(' / ') || '—';
    dom['detail-context'].textContent = model.max_context_tokens
      ? compactNumber(model.max_context_tokens) + ' tok' : 'manual only';
    dom['detail-budget'].textContent = model.run_token_budget === 0
      ? 'off' : compactNumber(model.run_token_budget) + ' tok';

    renderCapabilities(info.capabilities || []);
    renderMcp(info.mcp || {});
    selectPermissionMode(permission.mode || 'workspace-write');
    updateRunDetail(Boolean(info.active_run));
    renderSessions();
  } catch (error) {
    console.warn('[clat] workbench.info failed:', error.message);
  }
}

function shortenId(id) {
  if (!id || id.length <= 16) return id;
  return id.slice(0, 8) + '…' + id.slice(-6);
}

function renderCapabilities(capabilities) {
  dom['capability-list'].replaceChildren();
  for (const capability of capabilities) {
    dom['capability-list'].appendChild(el('span', null, String(capability).replaceAll('-', ' ')));
  }
}

function renderMcp(mcp) {
  const configured = mcp.configured || 0;
  const connected = mcp.connected || 0;
  const connecting = mcp.connecting || 0;
  dom['detail-mcp'].textContent = configured === 0
    ? 'No servers configured'
    : `${connected}/${configured} connected${connecting ? ` · ${connecting} connecting` : ''}`;
  dom['mcp-servers'].replaceChildren();
  for (const server of mcp.servers || []) {
    const card = el('div', 'mcp-server');
    card.append(
      el('strong', null, server.name || 'unnamed'),
      el('span', null, `${server.transport || 'transport'} · ${server.tools || 0} tools`),
    );
    dom['mcp-servers'].appendChild(card);
  }
}

function updateRunDetail(serverActive) {
  const active = serverActive === undefined ? state.runActive : serverActive;
  dom['detail-run'].textContent = active ? 'Running' : (state.connected ? 'Idle' : 'Disconnected');
}

async function refreshSessions() {
  try {
    const value = await rpc('session.list', {});
    state.sessions = value.sessions || [];
    renderSessions();
  } catch (error) {
    console.warn('[clat] session.list failed:', error.message);
  }
}

function renderSessions() {
  const query = dom['session-search'].value.trim().toLocaleLowerCase();
  const filtered = state.sessions.filter((session) => {
    const title = session.title || 'untitled';
    return !query || title.toLocaleLowerCase().includes(query) || session.id.toLocaleLowerCase().includes(query);
  });
  dom['session-list'].replaceChildren();
  dom['session-count'].textContent = String(state.sessions.length);
  for (const session of filtered) {
    const item = el('li', session.id === state.sessionId ? 'active' : '');
    const button = el('button', 'session-item');
    button.type = 'button';
    button.title = session.title || 'Untitled session';
    const title = session.title || 'Untitled session';
    const glyph = el('span', 'session-glyph', title.trim().slice(0, 1).toLocaleUpperCase() || '·');
    const copy = el('span', 'session-copy');
    copy.append(
      el('strong', null, title),
      el('small', null, `${session.turns || 0} turns · ${session.message_count || 0} messages`),
    );
    button.append(glyph, copy);
    button.addEventListener('click', () => switchSession(session.id));
    item.appendChild(button);
    dom['session-list'].appendChild(item);
  }
  if (filtered.length === 0 && state.sessions.length > 0) show(dom['session-empty']);
  else hide(dom['session-empty']);
}

function setSwitching(active) {
  dom.send.disabled = active;
  dom.prompt.disabled = active;
  dom['new-session'].disabled = active;
  if (active) updateRunState('switching session');
}

async function switchSession(id) {
  if (id === state.sessionId) {
    closeMobileSidebar();
    return;
  }
  setSwitching(true);
  try {
    await rpc('session.switch', { id });
    closeMobileSidebar();
    resubscribe();
  } catch (error) {
    setSwitching(false);
    updateRunState('switch failed: ' + error.message);
  }
}

dom['new-session'].addEventListener('click', async () => {
  setSwitching(true);
  try {
    await rpc('session.new', {});
    closeMobileSidebar();
    resubscribe();
  } catch (error) {
    setSwitching(false);
    updateRunState('new session failed: ' + error.message);
  }
});

dom['session-title'].addEventListener('click', async () => {
  if (!state.sessionId) return;
  const title = window.prompt('Session title', dom['session-title'].textContent || '');
  if (title === null || title.trim() === '') return;
  try {
    await rpc('session.rename', { id: state.sessionId, title });
    dom['session-title'].textContent = title.trim();
    updateDocumentTitle(title.trim());
    refreshSessions();
    refreshWorkbench();
  } catch (error) {
    updateRunState('rename failed: ' + error.message);
  }
});

dom['session-search'].addEventListener('input', renderSessions);

/* —— Layout and settings —————————————————————————————————— */

const MARKET_CATALOG_URL = 'https://pi.at.cn/catalog.json';
const MARKET_FALLBACK = [
  {
    id: 'dev.clat.digest', name: 'Digest Lab', runtime: 'wasm-component', status: 'preview',
    summary: 'A minimal-permission Rust/WASM component for deterministic digests.',
    tags: ['WASM', 'Pure', 'Rust'],
  },
  {
    id: 'dev.clat.greeter', name: 'Greeter Component', runtime: 'wasm-component', status: 'preview',
    summary: 'An end-to-end starter for the Rust SDK, configuration and host context.',
    tags: ['WASM', 'SDK', 'Starter'],
  },
  {
    id: 'cn.at.clat.dsh-port', name: 'DSH Porting Bridge', runtime: 'mcp-stdio', status: 'preview',
    summary: 'Project DSH/Cordis tools, prompts, sampling and elicitation into CLAT.',
    tags: ['DSH', 'Cordis', 'MCP'],
  },
];

function renderMarket() {
  const query = dom['market-search'].value.trim().toLocaleLowerCase();
  const packages = state.marketPackages.filter((plugin) => {
    const haystack = [plugin.id, plugin.name, plugin.summary, ...(plugin.tags || [])]
      .join(' ').toLocaleLowerCase();
    return !query || haystack.includes(query);
  });
  dom['market-list'].replaceChildren();
  for (const plugin of packages) {
    const card = el('article', 'market-item');
    const top = el('div', 'market-item-top');
    const icon = el('span', 'market-item-icon');
    icon.appendChild(svgIcon(plugin.runtime === 'wasm-component' ? 'wasm' : 'mcp'));
    const statusLabel = { available: 'Available', preview: 'Preview', withdrawn: 'Withdrawn' };
    top.append(
      icon,
      el('span', 'market-item-state', statusLabel[plugin.status]),
    );
    const tags = el('div', 'market-tags');
    for (const tag of plugin.tags || []) tags.appendChild(el('span', null, tag));
    card.append(
      top,
      el('h3', null, plugin.name || plugin.id),
      el('p', 'market-id', plugin.id),
      el('p', 'market-summary', plugin.summary || 'No summary provided.'),
      tags,
    );
    dom['market-list'].appendChild(card);
  }
  if (packages.length === 0) {
    dom['market-list'].appendChild(el('div', 'market-item market-empty', 'No matching plugins.'));
  }
  const source = state.marketFallback ? 'built-in preview · pi.at.cn unavailable' : 'public catalog';
  dom['market-status'].textContent = `${packages.length} of ${state.marketPackages.length} · ${source}`;
}

async function loadMarket() {
  if (state.marketLoaded) return;
  dom['market-status'].textContent = 'Loading public catalog…';
  try {
    const response = await fetch(MARKET_CATALOG_URL, {
      method: 'GET',
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
      cache: 'no-cache',
      headers: { Accept: 'application/json' },
    });
    if (!response.ok) throw new Error('HTTP ' + response.status);
    const catalog = await readBoundedCatalog(response);
    state.marketPackages = normalizeMarketCatalog(catalog);
    state.marketFallback = false;
  } catch (error) {
    state.marketPackages = MARKET_FALLBACK;
    state.marketFallback = true;
    console.warn('[clat] public plugin catalog unavailable:', error.message);
  }
  state.marketLoaded = true;
  renderMarket();
}

async function readBoundedCatalog(response) {
  const cap = 1024 * 1024;
  const advertised = Number(response.headers.get('content-length') || 0);
  if (advertised > cap) throw new Error('catalog exceeds 1 MiB');
  if (!response.body) throw new Error('catalog response has no body');
  const reader = response.body.getReader();
  const chunks = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > cap) {
      reader.cancel();
      throw new Error('catalog exceeds 1 MiB');
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try { return JSON.parse(new TextDecoder().decode(bytes)); }
  catch (_) { throw new Error('catalog is not valid JSON'); }
}

function normalizeMarketCatalog(catalog) {
  if (!catalog || catalog.schemaVersion !== 1 || !Array.isArray(catalog.packages)
      || catalog.packages.length > 256) {
    throw new Error('unsupported catalog schema');
  }
  return catalog.packages.map((plugin) => {
    if (!plugin || typeof plugin.id !== 'string' || !/^[a-z0-9][a-z0-9._-]{0,127}$/.test(plugin.id)
        || typeof plugin.name !== 'string' || plugin.name.length > 128
        || typeof plugin.summary !== 'string' || plugin.summary.length > 1024
        || !['wasm-component', 'mcp-stdio'].includes(plugin.runtime)
        || !['preview', 'available', 'withdrawn'].includes(plugin.status)) {
      throw new Error('catalog contains an invalid package record');
    }
    return {
      id: plugin.id,
      name: plugin.name,
      summary: plugin.summary,
      runtime: plugin.runtime,
      status: plugin.status,
      tags: Array.isArray(plugin.tags)
        ? plugin.tags.slice(0, 12).filter((tag) => typeof tag === 'string').map((tag) => tag.slice(0, 48))
        : [],
    };
  });
}

function openMarket() {
  dom['market-search'].value = '';
  if (!dom['market-dialog'].open) dom['market-dialog'].showModal();
  loadMarket();
  setTimeout(() => dom['market-search'].focus(), 0);
}

dom['market-open'].addEventListener('click', openMarket);
dom['market-close'].addEventListener('click', () => dom['market-dialog'].close());
dom['market-search'].addEventListener('input', renderMarket);

function isMobile() { return window.innerWidth <= MOBILE_BREAKPOINT; }

function syncPanelAccessibility() {
  const sidebarVisible = !isMobile() || dom.app.dataset.mobileSidebar === 'open';
  dom.sidebar.inert = !sidebarVisible;
  dom.sidebar.setAttribute('aria-hidden', String(!sidebarVisible));
  const inspectorVisible = state.inspector === 'open';
  dom.inspector.inert = !inspectorVisible;
  dom.inspector.setAttribute('aria-hidden', String(!inspectorVisible));
}

function closeMobileSidebar() {
  dom.app.dataset.mobileSidebar = 'closed';
  syncPanelAccessibility();
}

dom['sidebar-toggle'].addEventListener('click', () => {
  state.sidebar = state.sidebar === 'expanded' ? 'collapsed' : 'expanded';
  applyPresentation();
  savePresentationPreference();
});

dom['mobile-sidebar-open'].addEventListener('click', () => {
  dom.app.dataset.mobileSidebar = 'open';
  syncPanelAccessibility();
});
dom['mobile-sidebar-close'].addEventListener('click', closeMobileSidebar);
dom['sidebar-backdrop'].addEventListener('click', closeMobileSidebar);

function setInspector(next) {
  state.inspector = next;
  applyPresentation();
  savePresentationPreference();
}

dom['inspector-toggle'].addEventListener('click', () => {
  setInspector(state.inspector === 'open' ? 'closed' : 'open');
});
dom['inspector-close'].addEventListener('click', () => setInspector('closed'));

function selectPermissionMode(mode) {
  const radio = document.querySelector(`input[name="permission-mode"][value="${mode}"]`);
  if (radio) radio.checked = true;
  updateFullAccessConfirmation();
}

function selectedPermissionMode() {
  const radio = document.querySelector('input[name="permission-mode"]:checked');
  return radio ? radio.value : 'workspace-write';
}

function updateFullAccessConfirmation() {
  const fullAccess = selectedPermissionMode() === 'danger-full-access';
  if (fullAccess) show(dom['full-access-confirm-row']);
  else {
    hide(dom['full-access-confirm-row']);
    dom['full-access-confirm'].checked = false;
  }
}

function openSettings() {
  const mode = state.workbench && state.workbench.permission && state.workbench.permission.mode;
  selectPermissionMode(mode || 'workspace-write');
  dom['settings-error'].textContent = '';
  dom['settings-saved'].textContent = '';
  if (!dom['settings-dialog'].open) dom['settings-dialog'].showModal();
}

dom['settings-open'].addEventListener('click', openSettings);
dom['composer-permission'].addEventListener('click', openSettings);
dom['permission-options'].addEventListener('change', updateFullAccessConfirmation);

dom['theme-options'].addEventListener('change', () => {
  const radio = document.querySelector('input[name="theme"]:checked');
  state.theme = radio ? radio.value : 'system';
  applyPresentation();
  savePresentationPreference();
});

dom['permission-save'].addEventListener('click', async () => {
  const mode = selectedPermissionMode();
  dom['settings-error'].textContent = '';
  dom['settings-saved'].textContent = '';
  if (mode === 'danger-full-access' && !dom['full-access-confirm'].checked) {
    dom['settings-error'].textContent = 'Confirm the Full Access warning before applying it.';
    return;
  }
  dom['permission-save'].disabled = true;
  try {
    const params = { mode };
    if (mode === 'danger-full-access') params.confirm = mode;
    await rpc('permission.set', params);
    await refreshWorkbench();
    dom['settings-saved'].textContent = 'Permission mode updated.';
  } catch (error) {
    dom['settings-error'].textContent = error.message;
  } finally {
    dom['permission-save'].disabled = false;
  }
});

for (const button of document.querySelectorAll('[data-prompt]')) {
  button.addEventListener('click', () => {
    dom.prompt.value = button.dataset.prompt || '';
    resizePrompt();
    dom.prompt.focus();
  });
}

document.addEventListener('keydown', (event) => {
  const target = event.target;
  const typing = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
  if (event.key === '/' && !typing && !dom['settings-dialog'].open && !dom['market-dialog'].open) {
    event.preventDefault();
    if (isMobile()) dom.app.dataset.mobileSidebar = 'open';
    else if (state.sidebar === 'collapsed') {
      state.sidebar = 'expanded';
      applyPresentation();
    }
    setTimeout(() => dom['session-search'].focus(), 0);
  }
  if (event.key === 'Escape' && dom['market-dialog'].open) {
    dom['market-dialog'].close();
  } else if (event.key === 'Escape' && !dom['settings-dialog'].open) {
    closeMobileSidebar();
    if (isMobile() && state.inspector === 'open') setInspector('closed');
  }
});

window.addEventListener('resize', () => {
  if (!isMobile()) closeMobileSidebar();
  if (window.innerWidth <= INSPECTOR_DRAWER_BREAKPOINT && state.inspector === 'open') {
    setInspector('closed');
  } else {
    syncPanelAccessibility();
  }
});

/* —— Composer ————————————————————————————————————————————— */

function updateRunState(text) {
  dom['run-state'].textContent = text;
}

function resizePrompt() {
  dom.prompt.style.height = 'auto';
  dom.prompt.style.height = Math.min(dom.prompt.scrollHeight, 220) + 'px';
}

async function submitPrompt() {
  const text = dom.prompt.value.trim();
  if (!text) return;
  dom.prompt.value = '';
  resizePrompt();
  try {
    if (state.runActive) {
      const value = await rpc('steer.send', { text });
      addNoticeLine('steering ' + (value && value.outcome === 'queued' ? 'queued' : 'not running'));
    } else if (text.startsWith('/')) {
      const value = await rpc('command.run', { command: text });
      if (value && value.kind === 'status') {
        addNoticeLine(value.message || 'command completed');
      } else if (value && value.kind === 'context') {
        addNoticeLine(JSON.stringify(value.context));
      } else if (value && value.kind === 'session_reset') {
        addNoticeLine('new conversation');
        await loadSessions();
        resubscribe();
      }
    } else {
      await rpc('prompt.send', { text });
    }
  } catch (error) {
    if (error.code === 'busy') {
      updateRunState('run active · sending as steering');
      try {
        const value = await rpc('steer.send', { text });
        addNoticeLine('steering ' + (value && value.outcome === 'queued' ? 'queued' : 'not running'));
      } catch (steerError) {
        updateRunState('steering failed: ' + steerError.message);
      }
    } else {
      updateRunState('send failed: ' + error.message);
      dom.prompt.value = text;
      resizePrompt();
    }
  }
}

dom.send.addEventListener('click', submitPrompt);
dom.prompt.addEventListener('input', resizePrompt);
dom.prompt.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    submitPrompt();
  }
});

dom.cancel.addEventListener('click', async () => {
  try { await rpc('run.cancel', {}); }
  catch (error) { updateRunState('cancel failed: ' + error.message); }
});

/* —— Landing and boot ————————————————————————————————————— */

dom['connect-form'].addEventListener('submit', async (event) => {
  event.preventDefault();
  const token = dom['connect-token'].value.trim();
  dom['connect-error'].textContent = '';
  if (!token) {
    dom['connect-error'].textContent = 'Paste the token from ~/.clat/web-token.';
    return;
  }
  const button = dom['connect-form'].querySelector('button[type="submit"]');
  button.disabled = true;
  try {
    const response = await fetch('/auth', {
      method: 'POST',
      headers: { Authorization: 'Bearer ' + token },
      body: '',
    });
    if (!response.ok) {
      dom['connect-error'].textContent = response.status === 401
        ? 'That token does not match this CLAT server.'
        : 'Pairing failed: HTTP ' + response.status + '.';
      return;
    }
  } catch (error) {
    dom['connect-error'].textContent = 'Pairing failed: ' + error.message;
    return;
  } finally {
    button.disabled = false;
  }
  saveAuthToken(token);
  state.token = token;
  dom['connect-token'].value = '';
  hide(dom.landing);
  show(dom.app);
  connect();
});

(function boot() {
  // Migrate an already-installed pre-clean-URL PWA without treating its old
  // query token as a credential. The public shell can now load and pair.
  sessionStorage.removeItem('clat.connect');
  const current = new URL(location.href);
  if (current.searchParams.has('t')) {
    current.searchParams.delete('t');
    history.replaceState(null, '', current.pathname + current.search + current.hash);
  }
  state.token = readAuthToken();
  if (!state.token) {
    show(dom.landing);
    return;
  }
  show(dom.app);
  connect();
})();
