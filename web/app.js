/* clat web client — a pure projection of the serve event stream.
 *
 * Invariants (worklist PWA-4):
 * - INV-W2  zero business logic: every view state is derived from the
 *   stream (replay family builds the skeleton, live events mount under the
 *   active run). Nothing here is a second source of truth.
 * - INV-W3  refresh = rebuild: (re)connecting clears the view and replays
 *   from scratch. Session content is never stored locally — only the
 *   connection preference (base URL + per-run token) lives in
 *   sessionStorage, nothing in localStorage.
 * - INV-W5  unknown frame types / notice kinds are warned and ignored,
 *   never fatal.
 * - XSS discipline: model/tool output is rendered exclusively through
 *   createElement/textContent — never innerHTML.
 */

'use strict';

/* —— connection bootstrap ————————————————————————————————— */

const CONNECT_KEY = 'clat.connect';

function readConnectPreference() {
  const params = new URLSearchParams(location.search);
  const token = params.get('t');
  if (token) return { base: '', token };
  try {
    const saved = JSON.parse(sessionStorage.getItem(CONNECT_KEY) || 'null');
    if (saved && saved.token) {
      return { base: normalizeBase(saved.base), token: saved.token };
    }
  } catch (_) { /* corrupt preference: fall through to landing */ }
  return null;
}

function normalizeBase(base) {
  if (!base) return '';
  return base.replace(/\/+$/, '');
}

function saveConnectPreference(base, token) {
  sessionStorage.setItem(CONNECT_KEY, JSON.stringify({ base, token }));
}

/* —— tiny DOM helpers（XSS 纪律的落点）———————————————— */

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function show(node) { node.classList.remove('hidden'); }
function hide(node) { node.classList.add('hidden'); }

/* —— app state（全部可从流推导；不含持久会话内容）———— */

const state = {
  base: '',
  token: '',
  connected: false,
  runActive: false,
  sessionId: null,
  reconnectDelayMs: 1000,
  stream: null,        // active fetch controller
  // current-run render anchors（run_started 重置）
  run: null,           // { userText, assistant: {body, reasoning}, toolCount }
};

const dom = {};
for (const id of [
  'landing', 'connect-form', 'connect-url', 'connect-error',
  'app', 'session-title', 'conn-status', 'new-session', 'session-list',
  'transcript', 'prompt', 'send', 'cancel', 'run-state',
]) {
  dom[id] = document.getElementById(id);
}

/* —— RPC（象限①：POST + Bearer）—————————————————————— */

async function rpc(method, params) {
  const response = await fetch(state.base + '/api/' + method, {
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

/* —— SSE（fetch + ReadableStream 手解析；扫描偏移防巨帧 O(n²)）— */

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
  const response = await fetch(state.base + '/api/events', {
    headers: { Authorization: 'Bearer ' + state.token },
    signal: controller.signal,
  });
  if (!response.ok || !response.body) {
    throw Object.assign(new Error('event stream rejected: HTTP ' + response.status),
      { code: response.status === 401 || response.status === 403 ? 'unauthorized' : 'internal' });
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

/* —— 连接生命周期（断线重连 = 全量重建，INV-W3）——————— */

function setConnStatus(text, stateName) {
  dom['conn-status'].textContent = text;
  dom['conn-status'].dataset.state = stateName;
}

async function connect() {
  setConnStatus('connecting…', 'connecting');
  state.reconnectDelayMs = 1000;
  try {
    await openStream();
  } catch (error) {
    if (error.name === 'AbortError') return; // 主动切换：由切换方重建
    state.connected = false;
    state.runActive = false;
    updateRunState('');
    if (error.code === 'unauthorized') {
      // token 是进程级的：serve 重启/换 token → 回落地态重连。
      setConnStatus('unauthorized — reconnect via the URL clat serve prints', 'failed');
      sessionStorage.removeItem(CONNECT_KEY);
      stopApp();
      return;
    }
    setConnStatus('reconnecting…', 'reconnecting');
    setTimeout(connect, state.reconnectDelayMs);
    state.reconnectDelayMs = Math.min(state.reconnectDelayMs * 2, 10000);
  }
}

function stopApp() {
  if (state.stream) state.stream.abort();
  hide(dom.app);
  show(dom.landing);
}

/* —— 帧分派（重放族 / 实时族 / 控制族）———————————————— */

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
      // 重建起点：清空视图（INV-W3——视图只从流推导）。
      dom.transcript.replaceChildren();
      state.run = null;
      state.runActive = false;
      break;
    case 'replay.end':
      break;
    default:
      // INV-W5：未知帧 type 告警呈现，不崩不挂。
      console.warn('[clat] unknown frame type:', frame.event);
  }
}

/* —— 重放族 → 骨架（与 /resume 同源的 journal 事实）———— */

function handleReplay(event) {
  if (!event || !event.type) return;
  switch (event.type) {
    case 'user_message':
      addUserMessage(event.text);
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
      addNoticeLine('permission · ' + event.tool + ' → ' + decisionText(event.decision));
      break;
    case 'tool_requested':
      addToolCard(event.call && event.call.name, jsonText(event.call && event.call.arguments), null, false);
      break;
    case 'tool_finished':
      addToolCard(event.tool, '← ' + jsonText(event.output), null, event.is_error);
      break;
    case 'retry_scheduled':
      addNoticeLine('retry #' + event.retry + ' in ' + event.delay_ms + 'ms');
      break;
    case 'turn_ended':
      addNoticeLine('turn ' + event.turn + ' ended · ' + turnEndText(event.reason));
      break;
    case 'compaction':
      addNoticeLine('history compacted');
      break;
    default:
      console.warn('[clat] unknown replay type:', event.type);
  }
}

/* —— 实时族（RunEvent v1 wire 原样；按 run 归属挂载）———— */

function handleLive(event) {
  if (!event || !event.type) return;
  switch (event.type) {
    case 'run_started':
      state.runActive = true;
      updateRunState('running…');
      show(dom.cancel);
      state.run = null;
      // 中途订阅（刷新/重连）场景：journal 重放已渲染同一条 user
      // 骨架——run_started 只是活流从 run 头续播，不重复呈现。
      if (!lastUserMessageIs(event.prompt)) {
        addUserMessage(event.prompt);
      }
      break;
    case 'model_requested':
      ensureAssistant();
      break;
    case 'model_stream': {
      const bubble = ensureAssistant();
      const inner = event.event || {};
      if (inner.type === 'text_delta') bubble.appendBody(inner.delta || '');
      else if (inner.type === 'refusal_delta') bubble.appendBody(inner.delta || '');
      else if (inner.type === 'reasoning_delta') bubble.appendReasoning(inner.delta || '');
      else if (inner.type === 'reasoning_summary_delta') bubble.appendReasoning(inner.delta || '');
      break;
    }
    case 'model_responded':
      break;
    case 'tool_requested':
      state.run = state.run || {};
      addToolCard(event.call && event.call.name, jsonText(event.call && event.call.arguments), null, false);
      break;
    case 'permission_checked':
      addNoticeLine('permission · ' + event.tool + ' → ' + decisionText(event.decision));
      break;
    case 'permission_denied':
      addNoticeLine('permission denied · ' + event.tool + ' — ' + (event.reason || ''));
      break;
    case 'tool_started':
      break;
    case 'tool_finished': {
      const result = event.result || {};
      addToolCard(result.tool_name, '← ' + jsonText(result.output), null, result.is_error);
      break;
    }
    case 'steering_applied':
      addNoticeLine('steering applied');
      break;
    case 'run_completed':
    case 'run_cancelled':
    case 'run_failed':
      finishRun(event);
      break;
    default:
      // INV-W5：未知 RunEvent type——amend 政策下 v1 不会出现（wire 层
      // 词汇冻结），出现即协议异常，告警不挂。
      console.warn('[clat] unknown RunEvent type:', event.type);
  }
  scrollIfNearEnd();
}

function finishRun(event) {
  state.runActive = false;
  state.run = null;
  updateRunState('');
  hide(dom.cancel);
  if (event.type === 'run_failed') {
    addVerdict('failed', 'run failed — ' + (event.message || 'unknown error'));
  }
}

/* —— 控制族 ———————————————————————————————————————— */

function onSubscribed(ctl) {
  state.connected = true;
  setConnStatus('live', 'live');
  setSwitching(false); // 切换完成（若有）：composer 解锁
  if (ctl && ctl.session_id) {
    state.sessionId = ctl.session_id;
  }
  refreshSessionInfo();
  refreshSessions();
}

function onApprovalRequested(ctl) {
  if (!ctl || !ctl.rpc_id) return;
  const request = ctl.request || {};
  const card = el('div', 'approval-card');
  card.dataset.rpcId = ctl.rpc_id;

  const title = el('div', 'title', 'Permission required — ' + (request.tool || 'unknown tool'));
  const meta = el('div', 'note',
    (request.effect || 'side effect') + ' · ' + (request.reason || ''));
  const args = el('div', 'args', jsonText(request.arguments));
  const actions = el('div', 'actions');
  const note = el('div', 'note resolution', '');

  const resolve = (text) => {
    card.classList.add('resolved');
    note.textContent = text;
    actions.replaceChildren();
  };

  for (const decision of ['allow', 'deny']) {
    const button = el('button', decision === 'allow' ? 'primary' : 'ghost', decision === 'allow' ? 'Allow' : 'Deny');
    button.addEventListener('click', async () => {
      button.disabled = true;
      try {
        await rpc('approval.respond', { rpcId: ctl.rpc_id, decision });
        resolve('answered: ' + decision);
      } catch (error) {
        if (error.code === 'not-pending') {
          resolve('already answered elsewhere');
        } else {
          button.disabled = false;
          note.textContent = 'error: ' + error.message;
        }
      }
    });
    actions.appendChild(button);
  }

  card.append(title, meta, args, actions, note);
  dom.transcript.appendChild(card);
  scrollIfNearEnd();
}

function onSettled(ctl) {
  const outcome = (ctl && ctl.outcome) || {};
  switch (outcome.type) {
    case 'completed':
      addVerdict('completed', 'completed · ' + (outcome.turns || 0) + ' turns · ' +
        usageText(outcome.usage));
      break;
    case 'cancelled':
      addVerdict('cancelled', 'cancelled after ' + (outcome.turns || 0) + ' turns');
      break;
    case 'failed':
      addVerdict('failed', 'failed — ' + (outcome.error || 'unknown error'));
      break;
    default:
      console.warn('[clat] unknown settled outcome:', outcome.type);
  }
  refreshSessions();
  refreshSessionInfo();
  scrollIfNearEnd();
}

function onNotice(ctl) {
  // notice.kind 是开放枚举：未知 kind 按文档默认忽略（INV-W5）。
  const payload = ctl && ctl.payload;
  switch (ctl && ctl.kind) {
    case 'monitor':
      updateRunState(typeof payload === 'string' ? payload : '');
      break;
    case 'compaction':
      addNoticeLine(payload && payload.note ? String(payload.note) : 'compaction updated');
      break;
    case 'title':
      if (payload && payload.title) {
        dom['session-title'].textContent = payload.title;
        refreshSessions();
      }
      break;
    case 'mcp_startup':
      addNoticeLine('mcp startup: ' + jsonText(payload));
      break;
    default:
      break; // 未知 kind：文档化默认 = 忽略
  }
}

/* —— 转录渲染 ———————————————————————————————————— */

function lastUserMessageIs(text) {
  const messages = dom.transcript.querySelectorAll('.msg.user .body');
  if (messages.length === 0) return false;
  return messages[messages.length - 1].textContent === text;
}

function addUserMessage(text) {
  const msg = el('div', 'msg user');
  msg.append(el('span', 'marker', '❯'), el('div', 'body', text));
  dom.transcript.appendChild(msg);
  return msg;
}

function addAssistantMessage() {
  const msg = el('div', 'msg assistant');
  msg.append(el('span', 'marker', '⏺'));
  const body = el('div', 'body');
  const reasoning = el('div', 'reasoning hidden');
  msg.append(reasoning, body);
  dom.transcript.appendChild(msg);
  return {
    appendBody(text) { body.textContent += text; },
    appendReasoning(text) {
      reasoning.classList.remove('hidden');
      reasoning.textContent += text;
    },
    setReasoning(text) {
      reasoning.classList.remove('hidden');
      reasoning.textContent = text;
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
  const summary = el('summary', null, 'tool · ' + (name || 'unknown') + (isError ? ' ✗' : ''));
  const body = el('div', 'tool-body');
  if (argsText) body.appendChild(el('div', null, argsText));
  if (outputText) body.appendChild(el('div', null, outputText));
  card.append(summary, body);
  dom.transcript.appendChild(card);
  return card;
}

function addVerdict(kind, text) {
  const verdict = el('div', 'verdict ' + kind, text);
  dom.transcript.appendChild(verdict);
  scrollIfNearEnd();
}

function addNoticeLine(text) {
  const line = el('div', 'notice-line', '· ' + text);
  dom.transcript.appendChild(line);
}

function nearEnd() {
  const t = dom.transcript;
  return t.scrollHeight - t.scrollTop - t.clientHeight < 160;
}

function scrollIfNearEnd() {
  if (nearEnd()) dom.transcript.scrollTop = dom.transcript.scrollHeight;
}

function jsonText(value) {
  if (value === undefined || value === null) return '';
  if (typeof value === 'string') return value;
  try { return JSON.stringify(value, null, 2); } catch (_) { return String(value); }
}

function decisionText(decision) {
  if (decision === null || decision === undefined) return '?';
  if (typeof decision === 'string') return decision;
  if (typeof decision === 'object') {
    const key = Object.keys(decision)[0];
    return key + (decision[key] ? ` (${decision[key]})` : '');
  }
  return String(decision);
}

function turnEndText(reason) {
  if (typeof reason === 'string') return reason;
  if (reason && typeof reason === 'object') {
    const key = Object.keys(reason)[0];
    return key + (reason[key] ? ` (${reason[key]})` : '');
  }
  return '?';
}

function usageText(usage) {
  if (!usage) return '';
  const parts = [usage.input_tokens + ' in', usage.output_tokens + ' out'];
  if (usage.cached_input_tokens !== undefined) parts.push(usage.cached_input_tokens + ' cached');
  return parts.join(' · ');
}

/* —— 会话面（验收②）———————————————————————————————— */

async function refreshSessions() {
  try {
    const value = await rpc('session.list', {});
    dom['session-list'].replaceChildren();
    for (const session of value.sessions || []) {
      const item = el('li', session.id === state.sessionId ? 'active' : '');
      item.append(el('span', null, session.title || 'untitled'));
      const meta = el('span', 'meta', (session.turns || 0) + ' turns');
      item.appendChild(meta);
      item.addEventListener('click', () => switchSession(session.id));
      dom['session-list'].appendChild(item);
    }
  } catch (error) {
    console.warn('[clat] session.list failed:', error.message);
  }
}

async function refreshSessionInfo() {
  try {
    const info = await rpc('session.info', {});
    state.sessionId = info.session_id || null;
    dom['session-title'].textContent = info.title || 'untitled';
  } catch (error) {
    console.warn('[clat] session.info failed:', error.message);
  }
}

function setSwitching(active) {
  dom.send.disabled = active;
  dom.prompt.disabled = active;
  dom['new-session'].disabled = active;
  if (active) updateRunState('switching session…');
}

async function switchSession(id) {
  if (id === state.sessionId) return;
  setSwitching(true);
  try {
    await rpc('session.switch', { id });
    // 活跃会话变了：重订阅（重放族重建视图——与刷新同构，INV-W3）。
    // 中止旧流后由**这里**驱动新连接——connect 的 AbortError 路径只
    // 退出旧循环，不重连。
    resubscribe();
  } catch (error) {
    setSwitching(false);
    updateRunState('switch failed: ' + error.message);
  }
}

function resubscribe() {
  if (state.stream) state.stream.abort();
  connect();
}

dom['new-session'].addEventListener('click', async () => {
  setSwitching(true);
  try {
    await rpc('session.new', {});
    resubscribe(); // 新会话 = 重订阅重建
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
    refreshSessions();
  } catch (error) {
    updateRunState('rename failed: ' + error.message);
  }
});

/* —— composer（Enter=提交；run 活跃时 = steering）—————— */

function updateRunState(text) {
  dom['run-state'].textContent = text;
}

async function submitPrompt() {
  const text = dom.prompt.value.trim();
  if (!text) return;
  dom.prompt.value = '';
  try {
    if (state.runActive) {
      const value = await rpc('steer.send', { text });
      addNoticeLine('steering ' + (value && value.outcome === 'queued' ? 'queued' : 'not running'));
    } else {
      await rpc('prompt.send', { text });
    }
  } catch (error) {
    if (error.code === 'busy') {
      updateRunState('a run is already active — sending as steering');
      try {
        const value = await rpc('steer.send', { text });
        addNoticeLine('steering ' + (value && value.outcome === 'queued' ? 'queued' : 'not running'));
      } catch (steerError) {
        updateRunState('steering failed: ' + steerError.message);
      }
    } else {
      updateRunState('send failed: ' + error.message);
      dom.prompt.value = text;
    }
  }
}

dom.send.addEventListener('click', submitPrompt);
dom.prompt.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    submitPrompt();
  }
});

dom.cancel.addEventListener('click', async () => {
  try {
    await rpc('run.cancel', {});
  } catch (error) {
    updateRunState('cancel failed: ' + error.message);
  }
});

/* —— 落地态（无 token / serve 未运行）———————————————— */

dom['connect-form'].addEventListener('submit', async (event) => {
  event.preventDefault();
  const raw = dom['connect-url'].value.trim();
  dom['connect-error'].textContent = '';
  let parsed;
  try {
    parsed = new URL(raw);
  } catch (_) {
    dom['connect-error'].textContent = 'that is not a URL';
    return;
  }
  const token = new URLSearchParams(parsed.search).get('t');
  if (!token) {
    dom['connect-error'].textContent = 'the URL must carry the token (?t=…)';
    return;
  }
  saveConnectPreference(parsed.origin, token);
  state.base = parsed.origin;
  state.token = token;
  hide(dom.landing);
  show(dom.app);
  connect();
});

/* —— boot ————————————————————————————————————————— */

(function boot() {
  const preference = readConnectPreference();
  if (!preference) {
    show(dom.landing);
    return;
  }
  state.base = preference.base;
  state.token = preference.token;
  show(dom.app);
  connect();
})();
