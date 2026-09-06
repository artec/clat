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
  // PU-9（2026-09-06 负责人令）：品牌像素机器人剪影（fill + evenodd 挖出面部）。
  agent: ['M6 4h12v4h4v4h-4v4h4v4h-4v-4h-4v4h-4v-4h-4v4H2v-4h4v-4H2V8h4V4z M6.2 8.2h11.6v3.8h-3.8v4H10.2v-4H6.2z'],
  tool: ['m14 6 4 4-8 8-4 1 1-4z', 'm13 7 4 4'],
  trace: ['M5 17 9 9l4 6 3-10 3 12', 'M4 20h16'],
  info: ['M12 8h.01M11 12h1v5h1'],
  check: ['m5 12 4 4L19 6'],
  wasm: ['M5 5h14v14H5z', 'M9 9h6v6H9z'],
  mcp: ['M4 12h5m6-5h5m-5 10h5', 'M9 12l6-5m-6 5 6 5'],
  vision: ['M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6S2.5 12 2.5 12Z', 'M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z'],
};

// Fill-based glyphs (pixel silhouettes) bypass the global stroke pipeline.
const FILL_ICONS = new Set(['agent']);

function svgIcon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  for (const data of ICON_PATHS[name] || ICON_PATHS.info) {
    const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    path.setAttribute('d', data);
    if (FILL_ICONS.has(name)) {
      path.setAttribute('fill', 'currentColor');
      path.setAttribute('fill-rule', 'evenodd');
      path.setAttribute('stroke', 'none');
    }
    svg.appendChild(path);
  }
  return svg;
}

function renderModelIdentity(target, name, imageInput) {
  target.replaceChildren(el('span', 'model-name', name));
  if (!imageInput) return;
  const badge = el('span', 'vision-capability');
  badge.title = 'Accepts images';
  badge.setAttribute('aria-label', 'Accepts images');
  badge.appendChild(svgIcon('vision'));
  target.appendChild(badge);
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
  compactionActive: false,
  switching: false,
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
  transcriptAttachmentUrls: new Set(),
  draft: {
    clientDraftId: newOpaqueClientId('draft'),
    clientMessageId: newOpaqueClientId('message'),
    scope: null,
    images: [],
    epoch: 0,
    sending: false,
    queuedClientMessageId: null,
    notice: '',
  },
  wechatBindingPoll: 0,
  wechatQrUrl: null,
};

const dom = {};
for (const id of [
  'landing', 'connect-form', 'connect-token', 'connect-error', 'app', 'sidebar',
  'sidebar-toggle', 'mobile-sidebar-open', 'mobile-sidebar-close', 'sidebar-backdrop',
  'project-name', 'project-root', 'session-title', 'conn-status', 'sidebar-connection', 'sidebar-footnote',
  'header-model', 'header-permission', 'new-session', 'session-search', 'session-count',
  'session-list', 'session-empty', 'transcript-scroll', 'empty-state', 'transcript',
  'prompt', 'send', 'cancel', 'run-state', 'composer-permission', 'composer-shell',
  'attachment-input', 'attachment-open', 'attachment-rail', 'attachment-summary', 'drop-overlay',
  'composer-permission-label', 'plan-mode-badge', 'goal-badge', 'inspector', 'inspector-toggle', 'inspector-close',
  'detail-run', 'detail-seq', 'detail-session', 'detail-model', 'detail-protocol',
  'detail-context', 'detail-budget', 'compact-session', 'capability-list', 'detail-mcp', 'mcp-servers',
  'settings-open', 'settings-dialog', 'theme-options', 'permission-options',
  'full-access-confirm-row', 'full-access-confirm', 'settings-error', 'settings-saved',
  'wechat-status', 'wechat-counts', 'wechat-qr', 'wechat-qr-image', 'wechat-qr-state',
  'wechat-verify-row', 'wechat-verify-code', 'wechat-verify-submit', 'wechat-pairing',
  'wechat-pairing-code', 'wechat-error', 'wechat-bind', 'wechat-pair', 'wechat-unbind',
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
  dom['sidebar-footnote'].dataset.state = stateName;
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
  clearTranscriptAttachmentUrls();
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
      clearTranscriptAttachmentUrls();
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
    case 'user_message':
      settleQueuedDraft(event.client_message_id);
      addUserMessage(event.text, event.content_blocks);
      break;
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
      syncInteractionControls();
      show(dom.cancel);
      state.run = null;
      if (!lastUserMessageIs(event.prompt)) addUserMessage(event.prompt, event.content_blocks);
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
      if (result.tool_name === 'exit_plan_mode' && !result.is_error) {
        refreshWorkbench();
      }
      break;
    }
    case 'steering_applied':
      settleQueuedDraft(event.client_message_id);
      addUserMessage(event.text || '', event.content_blocks);
      addNoticeLine('', event.type);
      break;
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
  const restoredDraft = restoreQueuedDraft();
  updateRunState(restoredDraft ? 'run ended before steering was applied; draft restored' : '');
  updateRunDetail();
  hide(dom.cancel);
  syncInteractionControls();
  if (event.type === 'run_failed') {
    addVerdict('failed', 'run failed — ' + (event.message || 'unknown error'));
  }
}

function onSubscribed(ctl) {
  state.connected = true;
  state.reconnectDelayMs = 1000;
  setConnStatus('live', 'live');
  setSwitching(false);
  if (ctl && Object.prototype.hasOwnProperty.call(ctl, 'session_id')) {
    state.sessionId = ctl.session_id || null;
  }
  syncPlanModeBadge();
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
      if (payload && payload.status === 'started') {
        state.compactionActive = true;
        updateRunState('compacting history');
      } else if (payload && payload.status === 'finished') {
        state.compactionActive = false;
        updateRunState(payload.note ? String(payload.note) : 'compaction finished');
        if (payload.succeeded) {
          addNoticeLine(payload.note ? String(payload.note) : 'compaction finished', 'compaction');
        } else {
          addNoticeLine(payload.note ? String(payload.note) : 'compaction did not change history');
        }
        refreshWorkbench();
      }
      syncInteractionControls();
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

function attachmentBlocks(blocks) {
  if (!Array.isArray(blocks)) return [];
  return blocks
    .filter((block) => block && block.type === 'image' && block.attachment)
    .map((block) => block.attachment)
    .filter((attachment) => attachment && typeof attachment.attachment_id === 'string'
      && ['image/png', 'image/jpeg'].includes(attachment.media_type));
}

function clearTranscriptAttachmentUrls() {
  for (const url of state.transcriptAttachmentUrls) URL.revokeObjectURL(url);
  state.transcriptAttachmentUrls.clear();
}

async function loadTranscriptImage(img, attachment) {
  try {
    let response;
    // A steering descriptor is forwarded only after its journal flush. The
    // projection fold is normally in that same call, but retry a tiny bounded
    // window so an independently scheduled reader never turns that handoff
    // race into a permanent “unavailable” thumbnail.
    for (let attempt = 0; attempt < 8; attempt += 1) {
      response = await fetch(`/api/attachments/${encodeURIComponent(attachment.attachment_id)}`, {
        headers: { Authorization: 'Bearer ' + state.token },
        cache: 'no-store',
      });
      if (response.ok || (response.status !== 404 && response.status !== 503)) break;
      await new Promise((resolve) => setTimeout(resolve, 80 * (attempt + 1)));
    }
    if (!response || !response.ok) throw new Error(`HTTP ${response && response.status}`);
    const blob = await response.blob();
    if (!['image/png', 'image/jpeg'].includes(blob.type)) throw new Error('unexpected image type');
    const url = URL.createObjectURL(blob);
    if (!img.isConnected) {
      URL.revokeObjectURL(url);
      return;
    }
    state.transcriptAttachmentUrls.add(url);
    img.src = url;
    img.classList.remove('is-loading');
    img.closest('.message-attachment')?.classList.remove('is-unavailable');
  } catch (_) {
    img.alt = `${attachment.display_name || 'image'} unavailable`;
    img.closest('.message-attachment')?.classList.add('is-unavailable');
  }
}

function addMessageAttachments(attachments) {
  if (attachments.length === 0) return null;
  const rail = el('div', 'message-attachments');
  for (const attachment of attachments) {
    const card = el('button', 'message-attachment');
    card.type = 'button';
    const image = el('img', 'message-attachment-preview is-loading');
    image.alt = `${attachment.display_name || 'attached image'} loading`;
    const label = el('span', 'message-attachment-label');
    const name = attachment.display_name || 'image';
    const dimensions = attachment.width && attachment.height ? `${attachment.width} × ${attachment.height}` : 'image';
    label.textContent = `${name} · ${dimensions}`;
    card.append(image, label);
    card.addEventListener('click', () => {
      if (image.src) openImageLightbox(image.src, image.alt);
    });
    rail.appendChild(card);
    void loadTranscriptImage(image, attachment);
  }
  return rail;
}

function openImageLightbox(src, label) {
  let dialog = document.getElementById('image-lightbox');
  if (!dialog) {
    dialog = document.createElement('dialog');
    dialog.id = 'image-lightbox';
    dialog.className = 'image-lightbox';
    const close = el('button', 'icon-button image-lightbox-close', '×');
    close.type = 'button';
    close.title = 'Close image preview';
    close.addEventListener('click', () => dialog.close());
    const image = el('img', 'image-lightbox-image');
    dialog.append(close, image);
    dialog.addEventListener('click', (event) => { if (event.target === dialog) dialog.close(); });
    document.body.appendChild(dialog);
  }
  const image = dialog.querySelector('img');
  image.src = src;
  image.alt = label;
  if (!dialog.open) dialog.showModal();
}

function addUserMessage(text, blocks) {
  const msg = el('div', 'msg user');
  const marker = el('span', 'marker');
  marker.appendChild(svgIcon('user'));
  const body = el('div', 'body' + (text ? '' : ' image-only'), text);
  const attachments = addMessageAttachments(attachmentBlocks(blocks));
  msg.append(marker, body);
  if (attachments) msg.appendChild(attachments);
  return appendTranscript(msg);
}

function addAssistantMessage() {
  const msg = el('div', 'msg assistant');
  const marker = el('span', 'marker');
  marker.appendChild(svgIcon('agent'));
  msg.append(marker);
  const body = el('div', 'body');
  const bodyText = document.createTextNode('');
  body.appendChild(bodyText);
  const reasoning = el('details', 'reasoning hidden');
  const reasoningSummary = el('summary');
  reasoningSummary.append(svgIcon('trace'), el('span', null, humanEventName('reasoning_delta')));
  const reasoningCopy = el('pre', 'reasoning-copy');
  const reasoningText = document.createTextNode('');
  reasoningCopy.appendChild(reasoningText);
  reasoning.append(reasoningSummary, reasoningCopy);
  msg.append(reasoning, body);
  appendTranscript(msg);
  return {
    // `textContent += delta` serializes and reparses the entire growing
    // transcript on every stream chunk. Native Text append keeps long local
    // streams linear, so attachment fetch completion and input remain live.
    appendBody(text) { bodyText.appendData(text); },
    appendReasoning(text) {
      show(reasoning);
      reasoningText.appendData(text);
    },
    setReasoning(text) {
      show(reasoning);
      reasoningText.data = text;
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
  return line;
}

function contextAmount(value) {
  return Number.isFinite(Number(value)) ? String(value) : '—';
}

function injectionState(value) {
  return Number(value) > 0 ? 'injected' : 'not injected';
}

function formatContextSnapshot(snapshot) {
  const context = snapshot && typeof snapshot === 'object' ? snapshot : {};
  const unit = typeof context.unit === 'string' && context.unit ? context.unit : 'units';
  const estimate = (label, key) => `${label}: ${contextAmount(context[key])} ${unit}`;
  const tools = Array.isArray(context.tools) && context.tools.length > 0
    ? context.tools.join(', ') : 'none';
  const skills = Array.isArray(context.skills) && context.skills.length > 0
    ? context.skills.join(', ') : 'none';
  const diagnostics = Array.isArray(context.skill_diagnostics)
    ? context.skill_diagnostics : [];
  const lines = [
    `Context estimate · ${unit}`,
    estimate('Base prompt', 'base_prompt'),
    estimate('Project instructions', 'project_instructions'),
    `${estimate('Plan policy', 'plan_policy')} · ${injectionState(context.plan_policy)}`,
    estimate('Skill catalog', 'skill_catalog'),
    `${estimate('Goal policy', 'goal_policy')} · ${injectionState(context.goal_policy)}`,
    `Memory injection: ${contextAmount(context.memory)} / ${contextAmount(context.memory_budget_bytes)} bytes · ${injectionState(context.memory)}`,
    estimate('Tool schemas', 'tool_schemas'),
    estimate('History / compaction view', 'history'),
    `Images: ${contextAmount(context.image_count)}`,
    `Images before projection: ${contextAmount(context.image_original_count)}`,
    `Older images omitted: ${contextAmount(context.image_offloaded_count)}`,
    `Image bytes: ${contextAmount(context.image_bytes)} bytes`,
    estimate('Visual token estimate', 'image_tokens'),
    `Visual safety factor: ${contextAmount(context.image_safety_factor)}.0x`,
    estimate('Output reserve', 'output_reserve'),
    estimate('Input estimate', 'input'),
    estimate('Total estimate', 'total'),
    `Tools: ${tools}`,
    `Skills: ${skills}`,
  ];
  if (diagnostics.length === 0) {
    lines.push('Skill diagnostics: none');
  } else {
    lines.push('Skill diagnostics:');
    for (const diagnostic of diagnostics) {
      const source = diagnostic && diagnostic.source ? diagnostic.source : '-';
      const name = diagnostic && diagnostic.name ? diagnostic.name : '-';
      const kind = diagnostic && diagnostic.kind ? diagnostic.kind : '-';
      const message = diagnostic && diagnostic.message ? diagnostic.message : '-';
      lines.push(`! ${source} / ${name} / ${kind}: ${message}`);
    }
  }
  if (context.estimator) lines.push(`Estimator: ${context.estimator}`);
  return lines.join('\n');
}

function addContextSnapshot(snapshot) {
  const line = addNoticeLine(formatContextSnapshot(snapshot));
  line.classList.add('context-notice');
  line.setAttribute('aria-label', 'Context estimate');
}

function syncPlanModeBadge() {
  const info = state.workbench;
  const sameSession = info && (info.session?.id || null) === state.sessionId;
  dom['plan-mode-badge'].classList.toggle('hidden', !(sameSession && info.plan_mode_active));
  dom['goal-badge'].classList.toggle('hidden', !(sameSession && info.goal_armed));
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
    state.runActive = Boolean(info.active_run);
    state.compactionActive = Boolean(info.active_compaction);
    syncPlanModeBadge();

    dom['project-name'].textContent = project.name || 'Current project';
    dom['project-root'].textContent = project.root || '';
    dom['project-root'].title = project.root || '';
    dom['session-title'].textContent = session.title || 'Untitled session';
    updateDocumentTitle();
    renderModelIdentity(
      dom['header-model'],
      model.model || 'model unavailable',
      Boolean(model.image_input),
    );
    const modeLabel = permission.label || PERMISSION_LABELS[permission.mode] || 'Permission mode';
    dom['header-permission'].textContent = modeLabel;
    dom['composer-permission-label'].textContent = modeLabel;
    // PU-8：Full Access 与 TUI 同语——权限文字转警示黄（CSS 按 data-mode 着色）。
    dom['header-permission'].dataset.mode = permission.mode || '';
    dom['composer-permission'].dataset.mode = permission.mode || '';

    dom['detail-seq'].textContent = session.committed_seq === null || session.committed_seq === undefined
      ? '—' : String(session.committed_seq);
    dom['detail-session'].textContent = session.id ? shortenId(session.id) : 'Fresh';
    dom['detail-session'].title = session.id || '';
    renderModelIdentity(dom['detail-model'], model.model || '—', Boolean(model.image_input));
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
    syncInteractionControls();
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
  dom['detail-run'].textContent = state.compactionActive
    ? 'Compacting'
    : active ? 'Running' : (state.connected ? 'Idle' : 'Disconnected');
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
    button.disabled = state.switching || state.compactionActive;
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
  state.switching = active;
  syncInteractionControls();
  renderSessions();
  if (active) updateRunState('switching session');
}

function syncInteractionControls() {
  const locked = state.switching || state.compactionActive;
  dom.send.disabled = locked;
  dom.prompt.disabled = locked;
  dom['attachment-open'].disabled = locked;
  dom['new-session'].disabled = locked;
  dom['session-title'].disabled = locked || !state.sessionId;
  dom['compact-session'].textContent = state.compactionActive
    ? 'Cancel compaction'
    : 'Compact history';
  dom['compact-session'].disabled = state.switching
    || !state.sessionId
    || (state.runActive && !state.compactionActive);
  renderDraft();
}

async function switchSession(id) {
  if (id === state.sessionId) {
    closeMobileSidebar();
    return;
  }
  setSwitching(true);
  try {
    await rpc('session.switch', { id });
    clearDraft();
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
    clearDraft();
    closeMobileSidebar();
    resubscribe();
  } catch (error) {
    setSwitching(false);
    updateRunState('new session failed: ' + error.message);
  }
});

dom['session-title'].addEventListener('click', async () => {
  if (!state.sessionId || state.switching || state.compactionActive) return;
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

dom['compact-session'].addEventListener('click', async () => {
  const action = state.compactionActive ? 'cancel' : 'start';
  dom['compact-session'].disabled = true;
  try {
    const value = await rpc('session.compact', { action });
    if (action === 'start') {
      // HTTP and SSE are independent lanes: a very fast compaction may finish
      // before this response is observed. Re-read the server-owned slot rather
      // than resurrecting stale local "active" state after a finished notice.
      await refreshWorkbench();
      if (state.compactionActive) updateRunState('compacting history');
    } else if (value && value.status === 'cancelling') {
      updateRunState('cancelling compaction');
    } else {
      state.compactionActive = false;
      updateRunState('');
    }
  } catch (error) {
    updateRunState('compaction failed: ' + error.message);
    await refreshWorkbench();
  } finally {
    syncInteractionControls();
    renderSessions();
  }
});

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
  refreshWechatStatus();
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

function clearWechatQr() {
  if (state.wechatQrUrl) URL.revokeObjectURL(state.wechatQrUrl);
  state.wechatQrUrl = null;
  dom['wechat-qr-image'].removeAttribute('src');
  hide(dom['wechat-qr']);
  hide(dom['wechat-verify-row']);
  dom['wechat-verify-code'].value = '';
}

async function refreshWechatStatus() {
  dom['wechat-error'].textContent = '';
  try {
    const status = await rpc('wechat.binding.status', {});
    dom['wechat-status'].textContent = status.bound ? 'Bound' : 'Not bound';
    dom['wechat-counts'].textContent = `${status.paired_users || 0} paired · ${status.mapped_chats || 0} chats`;
    dom['wechat-bind'].textContent = status.bound ? 'Replace binding' : 'Bind WeChat';
    dom['wechat-pair'].disabled = !status.bound;
    dom['wechat-unbind'].disabled = !status.bound;
  } catch (error) {
    dom['wechat-status'].textContent = 'Unavailable';
    dom['wechat-error'].textContent = error.message;
  }
}

async function pollWechatBinding(pollId, verifyCode) {
  if (pollId !== state.wechatBindingPoll) return;
  try {
    const params = verifyCode ? { verifyCode } : {};
    const value = await rpc('wechat.binding.poll', params);
    if (pollId !== state.wechatBindingPoll) return;
    const labels = {
      waiting: 'Waiting for scan…',
      scanned: 'Scanned; confirm on the phone…',
      need_verify_code: 'Enter the verification code shown by WeChat.',
      verify_code_blocked: 'Verification was blocked. Wait before trying again.',
      expired: 'QR code expired. Start a new binding.',
      already_bound: 'The account is already bound and returned no new credential.',
      confirmed: 'Binding confirmed. Create a one-time user pairing code next.',
    };
    dom['wechat-qr-state'].textContent = labels[value.state] || value.state;
    if (value.state === 'need_verify_code') {
      show(dom['wechat-verify-row']);
      dom['wechat-verify-code'].focus();
      return;
    }
    hide(dom['wechat-verify-row']);
    if (['confirmed', 'expired', 'verify_code_blocked', 'already_bound'].includes(value.state)) {
      state.wechatBindingPoll += 1;
      if (value.state === 'confirmed') await refreshWechatStatus();
      return;
    }
    window.setTimeout(() => pollWechatBinding(pollId), 500);
  } catch (error) {
    if (pollId === state.wechatBindingPoll) dom['wechat-error'].textContent = error.message;
  }
}

dom['wechat-bind'].addEventListener('click', async () => {
  dom['wechat-error'].textContent = '';
  hide(dom['wechat-pairing']);
  const replacing = dom['wechat-bind'].textContent === 'Replace binding';
  if (replacing && !window.confirm('Replace the current binding? Existing user grants are kept until the new QR is confirmed, then cleared.')) return;
  dom['wechat-bind'].disabled = true;
  try {
    const value = await rpc('wechat.binding.start', { replace: replacing });
    clearWechatQr();
    const blob = new Blob([value.qr_svg], { type: 'image/svg+xml' });
    state.wechatQrUrl = URL.createObjectURL(blob);
    dom['wechat-qr-image'].src = state.wechatQrUrl;
    dom['wechat-qr-state'].textContent = 'Waiting for scan…';
    show(dom['wechat-qr']);
    const pollId = ++state.wechatBindingPoll;
    pollWechatBinding(pollId);
  } catch (error) {
    dom['wechat-error'].textContent = error.message;
  } finally {
    dom['wechat-bind'].disabled = false;
  }
});

dom['wechat-verify-submit'].addEventListener('click', () => {
  const code = dom['wechat-verify-code'].value.trim();
  if (!/^[A-Za-z0-9]{1,32}$/.test(code)) {
    dom['wechat-error'].textContent = 'Verification code must be 1–32 letters or digits.';
    return;
  }
  hide(dom['wechat-verify-row']);
  pollWechatBinding(state.wechatBindingPoll, code);
});

dom['wechat-pair'].addEventListener('click', async () => {
  dom['wechat-error'].textContent = '';
  try {
    const value = await rpc('wechat.pairing.create', {});
    dom['wechat-pairing-code'].textContent = value.code;
    show(dom['wechat-pairing']);
  } catch (error) {
    dom['wechat-error'].textContent = error.message;
  }
});

dom['wechat-unbind'].addEventListener('click', async () => {
  if (!window.confirm('Remove the WeChat credential, paired users, and chat mappings?')) return;
  dom['wechat-error'].textContent = '';
  try {
    await rpc('wechat.binding.unbind', { confirm: 'unbind-wechat' });
    state.wechatBindingPoll += 1;
    clearWechatQr();
    hide(dom['wechat-pairing']);
    await refreshWechatStatus();
  } catch (error) {
    dom['wechat-error'].textContent = error.message;
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

const MAX_DRAFT_IMAGES = 8;
const MAX_DRAFT_IMAGE_BYTES = 8 * 1024 * 1024;
const MAX_DRAFT_BATCH_BYTES = 32 * 1024 * 1024;
const MAX_DRAFT_PIXELS = 16 * 1024 * 1024;

function newOpaqueClientId(prefix) {
  if (globalThis.crypto && typeof globalThis.crypto.randomUUID === 'function') {
    return `${prefix}-${globalThis.crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 14)}`;
}

function formatBytes(bytes) {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.ceil(bytes / 1024))} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(bytes >= 10 * 1024 * 1024 ? 0 : 1)} MB`;
}

function imageLabel(image) {
  const dimensions = image.width && image.height ? `${image.width} × ${image.height}` : 'checking dimensions';
  return `${image.name} · ${dimensions} · ${formatBytes(image.bytes)}`;
}

function uploadDisplayName(file, index) {
  const extension = file.type === 'image/jpeg' ? 'jpg' : 'png';
  const original = String(file.name || '').replace(/[^\x20-\x7e]/g, '_').replace(/[\\/\r\n]/g, '_');
  const stem = original.replace(/\.[^.]*$/, '').replace(/[^A-Za-z0-9._ -]/g, '_').slice(0, 170) || `image-${index + 1}`;
  return `${stem}.${extension}`;
}

async function imageDimensions(file) {
  if (typeof createImageBitmap === 'function') {
    const bitmap = await createImageBitmap(file);
    try { return { width: bitmap.width, height: bitmap.height }; }
    finally { bitmap.close(); }
  }
  const url = URL.createObjectURL(file);
  try {
    const image = new Image();
    image.src = url;
    await image.decode();
    return { width: image.naturalWidth, height: image.naturalHeight };
  } finally {
    URL.revokeObjectURL(url);
  }
}

function totalDraftBytes() {
  return state.draft.images.reduce((total, image) => total + image.bytes, 0);
}

function renderDraft() {
  const images = state.draft.images;
  dom['attachment-rail'].replaceChildren();
  if (images.length === 0) {
    hide(dom['attachment-rail']);
    dom['attachment-summary'].textContent = state.draft.notice;
    return;
  }
  show(dom['attachment-rail']);
  for (const [index, image] of images.entries()) {
    const chip = el('article', `attachment-chip is-${image.status}`);
    chip.dataset.imageId = image.id;
    const thumb = el('img', 'attachment-thumb');
    thumb.src = image.url;
    thumb.alt = `${image.name} preview`;
    const indexMark = el('span', 'attachment-index', String(index + 1).padStart(2, '0'));
    const copy = el('div', 'attachment-copy');
    copy.append(
      el('strong', null, image.name),
      el('small', null, imageLabel(image)),
      el('small', 'attachment-state', image.status === 'uploaded'
        ? 'staged locally'
        : image.status === 'queued'
          ? 'steering queued · held for recovery'
        : image.status === 'uploading'
          ? 'staging…'
          : image.status === 'failed'
            ? `not staged · ${image.error || 'retry required'}`
            : 'ready to stage'),
    );
    const controls = el('div', 'attachment-controls');
    const moveEarlier = el('button', 'attachment-order', '↑');
    moveEarlier.type = 'button';
    moveEarlier.title = 'Move image earlier';
    moveEarlier.disabled = state.compactionActive || index === 0 || state.draft.sending || state.draft.queuedClientMessageId !== null;
    moveEarlier.addEventListener('click', () => moveDraftImage(index, index - 1));
    const moveLater = el('button', 'attachment-order', '↓');
    moveLater.type = 'button';
    moveLater.title = 'Move image later';
    moveLater.disabled = state.compactionActive || index === images.length - 1 || state.draft.sending || state.draft.queuedClientMessageId !== null;
    moveLater.addEventListener('click', () => moveDraftImage(index, index + 1));
    if (image.status === 'failed') {
      const retry = el('button', 'attachment-retry', '↻');
      retry.type = 'button';
      retry.title = `Retry staging ${image.name}`;
      retry.disabled = state.compactionActive;
      retry.addEventListener('click', () => retryDraftImage(image));
      controls.appendChild(retry);
    }
    const remove = el('button', 'attachment-remove', '×');
    remove.type = 'button';
    remove.title = `Remove ${image.name}`;
    remove.disabled = state.compactionActive || state.draft.sending || state.draft.queuedClientMessageId !== null;
    remove.addEventListener('click', () => removeDraftImage(image.id));
    controls.append(moveEarlier, moveLater, remove);
    chip.append(thumb, indexMark, copy, controls);
    dom['attachment-rail'].appendChild(chip);
  }
  const staged = images.filter((image) => image.status === 'uploaded').length;
  const failed = images.filter((image) => image.status === 'failed').length;
  let summary = failed
    ? `${failed} image${failed === 1 ? '' : 's'} need attention`
    : state.draft.queuedClientMessageId !== null
      ? `${images.length} image${images.length === 1 ? '' : 's'} queued with the active run · draft held until claimed`
    : staged === images.length
      ? `${staged} image${staged === 1 ? '' : 's'} ready · ${formatBytes(totalDraftBytes())}`
      : `${staged}/${images.length} images staged`;
  if (state.draft.notice) summary += ` · ${state.draft.notice}`;
  dom['attachment-summary'].textContent = summary;
}

function clearDraft() {
  state.draft.epoch += 1;
  for (const image of state.draft.images) URL.revokeObjectURL(image.url);
  state.draft.clientDraftId = newOpaqueClientId('draft');
  state.draft.clientMessageId = newOpaqueClientId('message');
  state.draft.scope = null;
  state.draft.images = [];
  state.draft.sending = false;
  state.draft.queuedClientMessageId = null;
  state.draft.notice = '';
  dom['attachment-input'].value = '';
  renderDraft();
}

function removeDraftImage(id) {
  if (state.draft.queuedClientMessageId !== null) return;
  const index = state.draft.images.findIndex((image) => image.id === id);
  if (index < 0) return;
  const [image] = state.draft.images.splice(index, 1);
  URL.revokeObjectURL(image.url);
  if (state.draft.images.length === 0) state.draft.scope = null;
  renderDraft();
}

function moveDraftImage(from, to) {
  if (to < 0 || to >= state.draft.images.length || state.draft.sending || state.draft.queuedClientMessageId !== null) return;
  const [image] = state.draft.images.splice(from, 1);
  state.draft.images.splice(to, 0, image);
  renderDraft();
}

async function ensureDraftScope() {
  if (state.draft.scope) return state.draft.scope;
  const scope = await rpc('draft.open', { clientDraftId: state.draft.clientDraftId });
  if (!scope || typeof scope.draftScopeId !== 'string') throw new Error('invalid draft scope response');
  state.draft.scope = scope;
  return scope;
}

async function uploadDraftImage(image, epoch) {
  image.status = 'uploading';
  image.error = '';
  renderDraft();
  try {
    const scope = await ensureDraftScope();
    const response = await fetch(`/api/drafts/${encodeURIComponent(scope.draftScopeId)}/images`, {
      method: 'POST',
      headers: {
        Authorization: 'Bearer ' + state.token,
        'Content-Type': image.type,
        'X-CLAT-Display-Name': image.uploadName,
      },
      body: image.file,
    });
    const body = await response.json().catch(() => null);
    if (!response.ok || !body || !body.ok || !body.value || typeof body.value.uploadId !== 'string') {
      const message = body && body.error && body.error.message ? body.error.message : `upload rejected (HTTP ${response.status})`;
      throw new Error(message);
    }
    if (epoch !== state.draft.epoch || !state.draft.images.includes(image)) return;
    image.uploadId = body.value.uploadId;
    image.status = 'uploaded';
  } catch (error) {
    if (epoch !== state.draft.epoch || !state.draft.images.includes(image)) return;
    image.status = 'failed';
    image.error = error.message || 'upload failed';
  }
  renderDraft();
}

async function addDraftFiles(files) {
  if (state.compactionActive || state.switching) {
    updateRunState('wait for history compaction to finish');
    return;
  }
  if (state.draft.queuedClientMessageId !== null) {
    updateRunState('waiting for queued steering to be claimed or the run to end');
    return;
  }
  const list = Array.from(files || []);
  if (list.length === 0) return;
  const issues = [];
  const available = MAX_DRAFT_IMAGES - state.draft.images.length;
  const accepted = [];
  let nextBytes = totalDraftBytes();
  for (const file of list) {
    if (accepted.length >= available) { issues.push(`at most ${MAX_DRAFT_IMAGES} images per message`); break; }
    if (!(file instanceof File) || !['image/png', 'image/jpeg'].includes(file.type)) {
      issues.push(`${file && file.name ? file.name : 'file'} is not a PNG or JPEG`);
      continue;
    }
    if (file.size === 0 || file.size > MAX_DRAFT_IMAGE_BYTES) {
      issues.push(`${file.name} must be 1..${formatBytes(MAX_DRAFT_IMAGE_BYTES)}`);
      continue;
    }
    if (nextBytes + file.size > MAX_DRAFT_BATCH_BYTES) {
      issues.push(`all images together may use at most ${formatBytes(MAX_DRAFT_BATCH_BYTES)}`);
      continue;
    }
    nextBytes += file.size;
    accepted.push(file);
  }
  state.draft.notice = issues.join(' · ');
  const epoch = state.draft.epoch;
  const added = accepted.map((file, offset) => ({
    id: newOpaqueClientId('image'), file, name: file.name || `image-${state.draft.images.length + offset + 1}`,
    uploadName: uploadDisplayName(file, state.draft.images.length + offset), type: file.type,
    bytes: file.size, width: 0, height: 0, url: URL.createObjectURL(file),
    status: 'ready', uploadId: null, error: '',
  }));
  state.draft.images.push(...added);
  renderDraft();
  for (const image of added) {
    try {
      const dimensions = await imageDimensions(image.file);
      if (dimensions.width * dimensions.height > MAX_DRAFT_PIXELS) throw new Error(`exceeds the ${MAX_DRAFT_PIXELS}-pixel limit`);
      if (epoch !== state.draft.epoch || !state.draft.images.includes(image)) continue;
      image.width = dimensions.width;
      image.height = dimensions.height;
      renderDraft();
      await uploadDraftImage(image, epoch);
    } catch (error) {
      if (epoch !== state.draft.epoch || !state.draft.images.includes(image)) continue;
      image.status = 'failed';
      image.error = error.message || 'image validation failed';
      renderDraft();
    }
  }
}

async function retryDraftImage(image) {
  if (!image || state.draft.sending || state.draft.queuedClientMessageId !== null) return;
  image.uploadId = null;
  await uploadDraftImage(image, state.draft.epoch);
}

function updateRunState(text) {
  dom['run-state'].textContent = text;
}

function resizePrompt() {
  dom.prompt.style.height = 'auto';
  dom.prompt.style.height = Math.min(dom.prompt.scrollHeight, 220) + 'px';
}

async function submitPrompt() {
  if (state.compactionActive || state.switching) {
    updateRunState('wait for history compaction to finish');
    return;
  }
  const text = dom.prompt.value.trim();
  const images = state.draft.images;
  if (!text && images.length === 0) return;
  if (state.draft.queuedClientMessageId !== null) {
    updateRunState('steering is queued; waiting for the active run');
    return;
  }
  if (images.some((image) => image.status === 'uploading' || image.status === 'ready')) {
    updateRunState('waiting for images to stage');
    return;
  }
  if (images.some((image) => image.status !== 'uploaded')) {
    updateRunState('resolve image staging failures before sending');
    return;
  }
  if (text.startsWith('/') && images.length > 0) {
    updateRunState('commands do not accept draft images; draft retained');
    return;
  }
  state.draft.sending = images.length > 0;
  renderDraft();
  try {
    if (state.runActive) {
      const clientMessageId = state.draft.clientMessageId;
      const params = images.length === 0
        ? { text }
        : {
          text,
          draftScopeId: state.draft.scope && state.draft.scope.draftScopeId,
          attachments: images.map((image) => image.uploadId),
          clientMessageId: state.draft.clientMessageId,
        };
      const value = await rpc('steer.send', params);
      if (!value || value.outcome !== 'queued') {
        updateRunState('run ended before steering was accepted; draft retained');
        return;
      }
      addNoticeLine('steering queued');
      dom.prompt.value = '';
      resizePrompt();
      if (images.length > 0 && state.draft.clientMessageId === clientMessageId) {
        state.draft.queuedClientMessageId = clientMessageId;
        for (const image of images) image.status = 'queued';
        state.draft.notice = 'steering accepted; holding the local draft until durable claim';
      }
      return;
    } else if (text === '/new' || text === '/clear') {
      await rpc('session.new', {});
      addNoticeLine('new conversation');
      await loadSessions();
      resubscribe();
    } else if (text.startsWith('/')) {
      const value = await rpc('command.run', { command: text });
      if (value && ['status', 'memory', 'goal', 'subagent_status', 'goal_run'].includes(value.kind)) {
        const notice = addNoticeLine(value.message || 'command completed');
        if (['memory', 'goal', 'subagent_status'].includes(value.kind)) {
          notice.classList.add('content-notice');
          notice.setAttribute('aria-label', value.kind.replaceAll('_', ' '));
        }
      } else if (value && value.kind === 'context') {
        addContextSnapshot(value.context);
      } else if (value && value.kind === 'session_reset') {
        addNoticeLine('new conversation');
        await loadSessions();
        resubscribe();
      }
      await refreshWorkbench();
    } else {
      const params = images.length === 0
        ? { text }
        : {
          text,
          draftScopeId: state.draft.scope && state.draft.scope.draftScopeId,
          attachments: images.map((image) => image.uploadId),
          clientMessageId: state.draft.clientMessageId,
        };
      await rpc('prompt.send', params);
      dom.prompt.value = '';
      resizePrompt();
      if (images.length > 0) clearDraft();
      return;
    }
    dom.prompt.value = '';
    resizePrompt();
  } catch (error) {
    if (error.code === 'busy' && images.length === 0) {
      updateRunState('run active · sending as steering');
      try {
        const value = await rpc('steer.send', { text });
        addNoticeLine('steering ' + (value && value.outcome === 'queued' ? 'queued' : 'not running'));
      } catch (steerError) {
        updateRunState('steering failed: ' + steerError.message);
      }
    } else {
      updateRunState('send failed: ' + error.message);
    }
  } finally {
    state.draft.sending = false;
    renderDraft();
  }
}

function settleQueuedDraft(clientMessageId) {
  if (!clientMessageId) return false;
  const queuedClaim = state.draft.queuedClientMessageId === clientMessageId;
  // The durable steering event and the RPC response travel over independent
  // connections. A fast run may claim/flush the message and deliver SSE while
  // `steer.send` is still awaiting its HTTP response. In that window the local
  // draft is still marked `sending`, not `queued`; the durable event is already
  // authoritative and must retire the matching draft instead of letting the
  // later acknowledgement resurrect it as unclaimed.
  const inFlightClaim = state.draft.sending
    && state.draft.images.length > 0
    && state.draft.clientMessageId === clientMessageId;
  if (!queuedClaim && !inFlightClaim) return false;
  clearDraft();
  return true;
}

function restoreQueuedDraft() {
  if (state.draft.queuedClientMessageId === null) return false;
  state.draft.queuedClientMessageId = null;
  for (const image of state.draft.images) {
    if (image.status === 'queued') image.status = 'uploaded';
  }
  state.draft.notice = 'the run ended before this steering was claimed; draft restored';
  renderDraft();
  return true;
}

dom.send.addEventListener('click', submitPrompt);
dom.prompt.addEventListener('input', resizePrompt);
dom.prompt.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    submitPrompt();
  }
});

dom['attachment-open'].addEventListener('click', () => dom['attachment-input'].click());
dom['attachment-input'].addEventListener('change', async () => {
  await addDraftFiles(dom['attachment-input'].files);
  dom['attachment-input'].value = '';
});

for (const eventName of ['dragenter', 'dragover']) {
  dom['composer-shell'].addEventListener(eventName, (event) => {
    if (!event.dataTransfer || !Array.from(event.dataTransfer.types || []).includes('Files')) return;
    event.preventDefault();
    show(dom['drop-overlay']);
  });
}
for (const eventName of ['dragleave', 'dragend']) {
  dom['composer-shell'].addEventListener(eventName, () => hide(dom['drop-overlay']));
}
dom['composer-shell'].addEventListener('drop', async (event) => {
  hide(dom['drop-overlay']);
  if (!event.dataTransfer || !event.dataTransfer.files.length) return;
  event.preventDefault();
  await addDraftFiles(event.dataTransfer.files);
});
dom.prompt.addEventListener('paste', async (event) => {
  const items = Array.from(event.clipboardData ? event.clipboardData.items : []);
  const files = items.filter((item) => item.kind === 'file').map((item) => item.getAsFile()).filter(Boolean);
  if (files.length === 0) return;
  event.preventDefault();
  await addDraftFiles(files);
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
