// Composer ownership and live UI state. Receipts and upload leases remain Host-owned.
const composerDrafts = new Map();
const composerOwnerAliases = new Map();
let composerSession = undefined;
let composerSelection = undefined;
let composerScope = '';
let freshComposer = 0;

function installComposerGuard() {
  if (typeof CLATWorkbenchState === 'undefined') return;
  let composing = false;
  document.addEventListener('compositionstart', event => { if (event.target === dom.prompt) composing = true; });
  document.addEventListener('compositionend', event => { if (event.target === dom.prompt) composing = false; });
  CLATWorkbenchState.register(workspacePrefix, action => {
    const protect = action !== 'quit' || composerDraftNeedsProtection();
    const text = Boolean((inputHistory.draft ?? dom.prompt.value).trim()) ||
      [...composerDrafts.values()].some(draft => Boolean(draft.text.trim()));
    return {
      text: (protect && text) || [...pendingSessionShares.values()].some(shares => shares.length > 0)
        || [...document.querySelectorAll('.question-card:not(.resolved) textarea')].some(input => Boolean(input.value.trim())),
      images: protect && (state.draft.images.length > 0 || composerImageDrafts.size > 0),
      submitting: composerGoalPending !== null || state.draft.sending || state.draft.queuedClientMessageId !== null || pendingTextSteering.size > 0,
      composing,
      busy: Boolean(document.querySelector('[data-unsaved=true]')) || Boolean(document.querySelector('[data-saving=true]')) || state.switching || state.compactionActive,
    };
  });
}

function setComposerTextDraft(owner, draft) {
  if (draft) composerDrafts.set(owner, draft);
  else composerDrafts.delete(owner);
  void saveComposerDraftText(composerDraftKeys.get(owner), draft);
}

function saveComposerText() {
  if (!composerScope || state.composerDraftLoading) return;
  const text = dom.prompt.value;
  exitInputHistoryBrowse();
  const previous = composerDrafts.get(composerScope);
  const goal = composerGoalDraft;
  const revision = previous?.generation === state.composerGeneration && previous?.revision
    || newOpaqueClientId('draft-edit');
  setComposerTextDraft(composerScope, {text, generation: state.composerGeneration, goal, revision});
}

function materializeComposer(oldScope, scope, session) {
  composerOwnerAliases.set(oldScope, scope);
  for (const map of [composerDrafts, composerImageDrafts, inputSubmissions, pendingSessionShares]) {
    if (map.has(oldScope)) map.set(scope, map.get(oldScope));
    map.delete(oldScope);
  }
  migrateComposerDraft(oldScope, scope, session);
}

function observeComposerSession(id, selection) {
  const next = id || null;
  if (composerSession === next && composerSelection === selection) return;
  const materializing = composerSession === null && next !== null && composerSelection === selection
    && !state.composerDraftLoading;
  const oldScope = composerScope;
  const oldSession = composerSession;
  const scope = workspacePrefix + ':' + (next || 'new-' + (++freshComposer));
  composerSession = next;
  composerSelection = selection;
  composerScope = scope;
  if (materializing) {
    materializeComposer(oldScope, scope, next);
  } else {
    composerDraftKey = '';
    state.composerDraftLoading = true;
    dom.prompt.value = composerDrafts.get(scope)?.text || '';
    composerGoalDraft = composerDrafts.get(scope)?.goal || null;
    state.composerGeneration += 1;
    if (oldSession !== undefined) clearDraft(true);
    invalidateSuggestion();
    clearRoundFollowups();
  }
  renderSessionShareDraft();
  refreshRecalledSessionShares(scope, selection);
  // HIST-PWA-1/W1 + B8-R2-1：会话/selection 变化是**作废**不是迁移——
  // 旧会话的浏览游标与待还原暂存一并退役，不得经新会话的 ↑↓ 复原；
  // 新会话列表只能由匹配它的 workbench 快照重新提供。与同会话刷新的
  // “保留暂存 + 值锚迁移”（observeInputHistory）是两条不同规则。页面内
  // 各会话自己的文本草稿（composerDrafts）不受影响。
  retireInputHistory();
  resizePrompt();
}

function clearOwnedSubmittedText(owner, generation, revision) {
  const canonical = composerOwnerAliases.get(owner) || owner;
  const draft = composerDrafts.get(canonical);
  const sameRevision = revision && draft?.revision === revision;
  if (canonical === composerScope) {
    clearSubmittedPrompt(sameRevision && inputHistory.draft === null ? state.composerGeneration : generation);
    return;
  }
  if (sameRevision || draft?.generation === generation) setComposerTextDraft(canonical, null);
}

function settleComposerGoalDraft(owner, generation, goal) {
  if (composerOwns(owner)) {
    if (composerGoalDraft !== goal) return;
    composerGoalDraft = null;
    clearOwnedSubmittedText(owner, generation);
    saveComposerText();
    return;
  }
  const canonical = composerOwnerAliases.get(owner) || owner;
  const draft = composerDrafts.get(canonical);
  if (draft?.goal !== goal) return;
  if (draft.generation === generation) setComposerTextDraft(canonical, null);
  else setComposerTextDraft(canonical, { ...draft, goal: null });
}

function composerOwns(owner) {
  return (composerOwnerAliases.get(owner) || owner) === composerScope;
}

function composerSubmissionParams(text, images, clientMessageId = state.draft.clientMessageId) {
  return images.length === 0 ? { text, clientMessageId } : {
    text, draftScopeId: state.draft.scope && state.draft.scope.draftScopeId,
    attachments: images.map((image) => image.uploadId), clientMessageId,
  };
}

const pendingTextSteering = new Map();
// 乐观回显注册表（docs/todo/web-steering-optimistic-echo-2026-11-07.md）。
// 插话被接受（queued / input.submit 入队）即在转录尾部出现 dim 的
// provisional 用户块；claim（steering_applied）原地转正，未 claim 的
// run 终态移除。纯视图态：不持久化、不进事件；legacy steer.send 与现代
// input.submit 两条提交路径共用同一套结算/丢弃语义。
const optimisticSteering = new Map();

/// INV-OE2：claim 原地升级。命中返回 true（调用方不再 push 正式节点，
/// 避免同一插话双节点）；未命中（他端发送 / 重放 / 未回显路径）返回
/// false，调用方维持现状 addUserMessage。
function settleOptimisticSteering(clientMessageId) {
  const provisional = optimisticSteering.get(clientMessageId);
  if (!provisional) return false;
  optimisticSteering.delete(clientMessageId);
  provisional.node.classList.remove('provisional');
  return true;
}

/// INV-OE3：未 claim 而终结——移除幽灵节点并退出 live 序列（seq 绑定
/// 依赖 liveMessageNodes 与 journal 项一一对应）。
function discardOptimisticSteering(clientMessageId) {
  const provisional = optimisticSteering.get(clientMessageId);
  if (!provisional) return;
  optimisticSteering.delete(clientMessageId);
  const index = liveMessageNodes.indexOf(provisional.node);
  if (index !== -1) liveMessageNodes.splice(index, 1);
  provisional.node.remove();
}

/// run 终态扫尾：注册表剩余条目全部移除（legacy 的 restoreTextSteering
/// 已逐条处置自己的条目；这里兜底现代 input.submit 路径——其条目仍留在
/// Host 队列里，但本轮未投递，转录不再显示，下一 run_started 重新渲染）。
function finishOptimisticSteering() {
  for (const id of [...optimisticSteering.keys()]) discardOptimisticSteering(id);
}

/// 队列权威快照对账（renderPendingInputs 每次刷新调用）：条目已离队
/// （claim 走 settle 已自清；被删除/召回）→ 移除回显；条目文本被编辑 →
/// 原地刷新，回显与队列永不漂移。
function reconcileOptimisticSteering(items) {
  for (const [id, provisional] of optimisticSteering) {
    const item = items.find((entry) => entry.id === id);
    if (!item) { discardOptimisticSteering(id); continue; }
    if (typeof item.text === 'string' && item.text !== provisional.text) {
      provisional.text = item.text;
      userMessageReader.update(provisional.node, item.text);
    }
  }
}

/// INV-OE6：转录重建（replay.begin）/新 run 清注册表（节点已随重建
/// 消失或已由终态扫尾处置）。
function resetOptimisticSteering() {
  optimisticSteering.clear();
}

/// INV-OE1：接受即回显——转录尾部 dim provisional 用户块（流式
/// assistant / 工具卡之后，INV-OE4），入 liveMessageNodes 以便 claim 后
/// seq 绑定（INV-OE6）。幂等：同一 id 只回显一次。
function echoOptimisticSteering(clientMessageId, text) {
  if (!clientMessageId || optimisticSteering.has(clientMessageId)) return;
  const node = addUserMessage(text, null);
  node.classList.add('provisional');
  optimisticSteering.set(clientMessageId, { node, text });
  liveMessageNodes.push(node);
  scrollIfFollowing();
}

function restoreTextSteering(id, entry) {
  discardOptimisticSteering(id);
  pendingTextSteering.delete(id);
  const canonical = composerOwnerAliases.get(entry.owner) || entry.owner;
  const current = composerOwns(entry.owner) ? (inputHistory.draft ?? dom.prompt.value) : (composerDrafts.get(canonical)?.text || '');
  const text = entry.text + (current ? '\n' + current : '');
  if (composerOwns(entry.owner)) {
    dom.prompt.value = text;
    state.composerGeneration += 1;
    resizePrompt();
    saveComposerText();
  } else {
    setComposerTextDraft(canonical, { ...composerDrafts.get(canonical), text, revision: newOpaqueClientId('draft-edit'), generation: (composerDrafts.get(canonical)?.generation || 0) + 1 });
  }
  if (id) finishComposerSubmission(entry.owner, id);
}

function finishTextSteering() {
  let restored = false;
  for (const [id, entry] of pendingTextSteering) {
    entry.ended = true;
    if (entry.accepted) { restoreTextSteering(id, entry); restored = true; }
  }
  return restored;
}

async function submitComposerSteering({ text, images, owner, draftEpoch, composerGeneration }) {
  const clientMessageId = images.length ? state.draft.clientMessageId : newOpaqueClientId('steering');
  const entry = { text, owner, accepted: false, ended: false };
  if (!images.length) pendingTextSteering.set(clientMessageId, entry);
  let value;
  try {
    const saved = persistComposerSubmission(clientMessageId, images);
    if (saved) await saved;
    if (!composerOwns(owner) || composerGeneration !== state.composerGeneration) {
      pendingTextSteering.delete(clientMessageId);
      return;
    }
    const params={...composerSubmissionParams(text,images),clientMessageId};bindFileReferenceSubmission(owner,text,params);
    value = await rpc('steer.send', params);
  } catch (error) {
    pendingTextSteering.delete(clientMessageId);
    throw error;
  }
  if (value && value.outcome === 'queued' && !images.length) {
    clearOwnedSubmittedText(owner, composerGeneration);
  }
  if (!images.length) {
    if (!value || value.outcome !== 'queued') pendingTextSteering.delete(clientMessageId);
    else if (pendingTextSteering.has(clientMessageId)) {
      entry.accepted = true;
      // run 已在等待期间终结：退还编辑框、移除回显；否则立即乐观回显
      // （INV-OE1）。claim 已先到时 entry 已被 steering_applied 分支删除，
      // has() 为 false，天然不重复回显。
      if (entry.ended) restoreTextSteering(clientMessageId, entry);
      else echoOptimisticSteering(clientMessageId, text);
    }
  }
  if (!composerOwns(owner) || draftEpoch !== state.draft.epoch) return;
  if (!value || value.outcome !== 'queued') {
    finishComposerSubmission(owner, clientMessageId);
    updateRunState(CLATI18n.msg("web.run_ended_before_steering_was_accepted_draft_retained", {}, "run ended before steering was accepted; draft retained"));
    return;
  }
  // 文本路径的发送反馈就是回显本体（INV-OE1）；「排队」通知行只保留给
  // 图片 draft-hold 路径（INV-OE5）。
  if (images.length > 0) addNoticeLine(CLATI18n.msg("web.steering_queued", {}, "steering queued"));
  if (images.length > 0 && state.draft.clientMessageId === clientMessageId) {
    state.draft.queuedClientMessageId = clientMessageId;
    for (const image of images) image.status = 'queued';
    state.draft.notice = CLATI18n.msg("web.steering_accepted_holding_the_local_draft_until_durable_claim", {}, "steering accepted; holding the local draft until durable claim");
  }
}

async function submitComposerCommand({ text, owner }) {
  if ((text === '/new' || text === '/clear') && borrowedConversation) {updateRunState(pageText('fixed_task_session'));return false;}
  if (text === '/new' || text === '/clear') {
    if (!await confirmAttachmentSelection()) return false;
    await rpc('session.new', {});
    if (!composerOwns(owner)) return true;
    addNoticeLine(CLATI18n.msg("tui.new_conversation", {}, "new conversation"));
    await loadSessions();
    resubscribe();
    return true;
  }
  const value = await rpc('command.run', { command: text });
  if (!composerOwns(owner)) return true;
  if (value?.kind === 'plugin_manager') {
    dom['market-open'].click();
    return true;
  }
  if (value && ['status', 'help', 'memory', 'goal', 'subagent_status', 'goal_run', 'task', 'task_run', 'capabilities', 'ui_consents'].includes(value.kind)) {
    const notice = addNoticeLine(value.presentation
      ? CLATI18n.present(value.presentation, value.message)
      : value.message || CLATI18n.msg("web.command_completed", {}, "command completed"));
    if (['memory', 'goal', 'subagent_status', 'task', 'capabilities', 'ui_consents'].includes(value.kind)) {
      notice.classList.add('content-notice');
      CLATI18n.attribute(notice,'aria-label',()=>value.kind === 'ui_consents'
        ? CLATI18n.msg('cua.consent_list') : value.kind.replaceAll('_', ' '));
    }
  } else if (value && value.kind === 'context') {
    addContextSnapshot(value.context);
  } else if (value && value.kind === 'session_reset') {
    addNoticeLine(CLATI18n.msg("tui.new_conversation", {}, "new conversation"));
    await loadSessions();
    resubscribe();
  }
  await refreshWorkbench();
  return true;
}

async function recoverComposerSubmission(error, { owner, text, images, composerGeneration }) {
  if (!composerOwns(owner)) return;
  if (error.code !== 'busy' || images.length !== 0) {
    updateRunState(CLATI18n.msg("web.line_send_failed_value",{value1:(error.message)},"send failed: {value1}")); return;
  }
  updateRunState(CLATI18n.msg("web.run_active_sending_as_steering", {}, "run active · sending as steering"));
  try {
    await submitComposerSteering({ owner, text, images, composerGeneration, draftEpoch: state.draft.epoch });
  } catch (steerError) {
    if (composerOwns(owner)) updateRunState(CLATI18n.msg("web.line_steering_failed_value",{value1:(steerError.message)},"steering failed: {value1}"));
  }
}

// —— 已提交输入历史（HIST-PWA-1）————————————————————————————
//
// 列表只来自 Host 的 session.recent_inputs（与 TUI recent_inputs 同一
// journal transcript 权威，含其它客户端的提交）；浏览态是页面本地的，
// 永不写入 localStorage/sessionStorage/IndexedDB。语义与 TUI
// InputBuffer 的 ↑↓ 召回对齐：首次 ↑ 暂存草稿，越过最新项 ↓ 原样还原。

const inputHistory = { list: [], index: null, draft: null };

function resetInputHistoryBrowse() {
  inputHistory.index = null;
  inputHistory.draft = null;
}

/// 会话/selection 变化：历史浏览整体退役（列表、游标、待还原暂存），
/// 旧会话的任何浏览状态都不得进入新会话（B8-R2-1）。
function retireInputHistory() {
  inputHistory.list = [];
  resetInputHistoryBrowse();
}

/// 编辑即退出浏览态（与 TUI leave_history 同语义：待还原暂存一并丢弃，
/// 被编辑的召回项成为新草稿——含被列表变化打断后的编辑，B8-R3）。
function exitInputHistoryBrowse() {
  if (inputHistory.index === null && inputHistory.draft === null) return;
  resetInputHistoryBrowse();
}

/// Host 快照落地（当前 Session 匹配时由调用方保证）。
/// - 列表未变（同 Session 重复快照）：完全不动——浏览游标与待还原草稿
///   均保持有效（B8-R3）。
/// - 列表真变化（其它客户端新提交/重连/切会话）：正在浏览时按“当前
///   召回值”锚点迁移游标（锚点滑出有界窗口时复位为 null），**待还原
///   草稿保留**，↓ 仍可精确取回；正文不动。
function observeInputHistory(recentInputs) {
  const list = Array.isArray(recentInputs)
    ? recentInputs.filter((text) => typeof text === 'string' && text.trim() !== '')
    : [];
  const unchanged = list.length === inputHistory.list.length
    && list.every((text, index) => text === inputHistory.list[index]);
  if (unchanged) return;
  if (inputHistory.index !== null) {
    const anchor = inputHistory.list[inputHistory.index];
    // B8-R2-2：锚点滑出有界窗口时归一到无游标态（lastIndexOf 找不到返回
    // -1，不得当作有效游标保存——否则 ↑ 无法重新进入历史）。
    if (anchor !== undefined) {
      const found = list.lastIndexOf(anchor);
      inputHistory.index = found >= 0 ? found : null;
    } else {
      inputHistory.index = null;
    }
  }
  inputHistory.list = list;
}

/// 自身成功提交后的即时入史（最终权威以随后的快照为准）；slash 命令与
/// steering 提交不在此列（后者等 durable claim 后由快照补录）。调用方
/// 必须已用提交时的 owner 归属校验过滤迟到回执（B8-R1）。
function rememberSubmittedInput(text) {
  const value = String(text || '').trim();
  if (!value || value.startsWith('/')) return;
  inputHistory.index = null;
  inputHistory.draft = null;
  if (inputHistory.list[inputHistory.list.length - 1] !== value) inputHistory.list.push(value);
}

function applyInputHistoryText(text) {
  dom.prompt.value = text;
  state.composerGeneration += 1;
  if (state.suggestionPending || state.suggestion) invalidateSuggestion();
  resizePrompt();
  dom.prompt.setSelectionRange(text.length, text.length);
}

/// ↑/↓ 召回（W2 边界由调用方判定）。返回 true 表示已消费该键。
/// B8-R2-3 + B8-R3-1：游标被列表变化打断时，待还原草稿仍可经 ↓ 取回
///（仅一次）——即使权威窗口已**空**（同会话滑空/压缩）也不例外；空历史
/// 且无暂存时不拦截方向键（返回 false，原生行为）。
function navigateInputHistory(direction) {
  if (direction < 0) {
    if (!inputHistory.list.length) return false;
    if (!inputHistory.list.length) return false;
    if (inputHistory.index === null) {
      // 首次 ↑ 暂存原草稿；被打断后重新进入浏览时不覆盖仍待还原的原
      // 草稿（当前正文是召回项，不是用户的原草稿）。
      if (inputHistory.draft === null) inputHistory.draft = dom.prompt.value;
      inputHistory.index = inputHistory.list.length - 1;
    } else if (inputHistory.index > 0) {
      inputHistory.index -= 1;
    } else {
      return false;
    }
  } else {
    if (inputHistory.index === null || !inputHistory.list.length) {
      // 游标无效（含空窗口）：有暂存则取回一次，否则不拦截。
      if (inputHistory.draft === null) return false;
      const draft = inputHistory.draft;
      inputHistory.draft = null;
      applyInputHistoryText(draft);
      return true;
    }
    if (inputHistory.index + 1 < inputHistory.list.length) {
      inputHistory.index += 1;
    } else {
      inputHistory.index = null;
      const draft = inputHistory.draft === null ? '' : inputHistory.draft;
      inputHistory.draft = null;
      applyInputHistoryText(draft);
      return true;
    }
  }
  applyInputHistoryText(inputHistory.list[inputHistory.index]);
  return true;
}

async function confirmAttachmentSelection() {
  if (state.draft.sending || state.draft.queuedClientMessageId !== null) {
    updateRunState(CLATI18n.msg("web.wait_for_the_submitted_attachment_receipt_before_changing_sessions", {}, "Wait for the submitted attachment receipt before changing sessions."));
    return false;
  }
  return true;
}

async function copyMessageText(button, text) {
  if (button.disabled) return;
  const iconOnly = button.classList.contains('message-action-icon');
  const label = button.textContent;
  button.disabled = true;
  let feedback;
  try {
    if (!navigator.clipboard) throw CLATI18n.error(CLATI18n.msg('web.clipboard_unavailable'));
    await navigator.clipboard.writeText(text);
    feedback = 'web.copied';
    if (iconOnly) button.replaceChildren(svgIcon('check'));
  } catch (_) { feedback = 'web.copy_failed'; }
  if (iconOnly) {
    CLATI18n.attribute(button,'aria-label',()=>CLATI18n.msg(feedback));
    CLATI18n.attribute(button,'title',()=>CLATI18n.msg(feedback));
    const status=el('span','visually-hidden',()=>CLATI18n.msg(feedback));status.setAttribute('role','status');button.append(status);
  } else CLATI18n.bind(button,'textContent',()=>CLATI18n.msg(feedback));
  setTimeout(() => {
    button.disabled = false;
    if(iconOnly){button.replaceChildren(svgIcon('copy'));button.restoreCopyLabel();}
    else if(button.restoreCopyLabel)button.restoreCopyLabel();
    else CLATI18n.bind(button,'textContent',()=>label);
  }, 1600);
}

function copyTextButton(label, source, iconOnly = false) {
  const readLabel=()=>typeof label==='function'?label():label;
  const button = iconOnly ? el('button','message-action-icon message-copy icon-button') : el('button','text-action',readLabel);
  button.type = 'button';
  button.restoreCopyLabel=()=>{
    CLATI18n.attribute(button,'aria-label',readLabel);
    CLATI18n.attribute(button,'title',readLabel);
    if(!iconOnly)CLATI18n.bind(button,'textContent',readLabel);
  };
  button.restoreCopyLabel();if(iconOnly)button.append(svgIcon('copy'));
  button.addEventListener('click', () => { void copyMessageText(button, source()); });
  return button;
}

function addMessageCopyAction(node, source, reply = false) {
  const action = reply ? 'copy-reply' : 'copy-message';
  if (node.querySelector('[data-action="'+action+'"]')) return;
  let actions = node.querySelector(':scope > .message-actions');
  if (!actions) { actions = el('div', 'message-actions'); node.appendChild(actions); }
  const copy = copyTextButton(() => CLATI18n.msg(reply ? 'web.copy_reply' : 'pages.copy'), source, true);
  copy.dataset.action = action; actions.prepend(copy);
}

function quoteSelection(text, role, seq) {
  if (state.switching || state.compactionActive || dom.prompt.disabled) return;
  const header = `[Quote from ${role === 'user' ? 'your message' : 'agent reply'}${seq ? ' · message ' + seq : ''}]`;
  const quote = header + '\n' + text.split(/\r?\n/).map((line) => '> ' + line).join('\n');
  dom.prompt.value += (dom.prompt.value ? '\n\n' : '') + quote + '\n\n';
  state.composerGeneration += 1;
  saveComposerText();
  invalidateSuggestion();
  resizePrompt();
  dom.prompt.focus();
}

function installSelectionQuote() {
  const button = el('button', 'selection-quote hidden', () => (CLATI18n.msg("web.quote_selection", {}, "Quote selection")));
  button.type = 'button';
  let chosen = null;
  button.addEventListener('pointerdown', (event) => event.preventDefault());
  button.addEventListener('click', () => {
    if (chosen && chosen.owner === composerScope) quoteSelection(chosen.text, chosen.role, chosen.seq);
    hide(button);
  });
  document.body.appendChild(button);
  document.addEventListener('selectionchange', () => {
    hide(button); chosen = null;
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount !== 1 || state.switching) return;
    const range = selection.getRangeAt(0);
    const node = range.commonAncestorContainer;
    const element = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
    const body = element?.closest('.msg > .body');
    if (!body || !dom.transcript.contains(body) || !body.contains(range.startContainer)
      || !body.contains(range.endContainer)) return;
    const text = selection.toString().slice(0, 16000);
    if (!text.trim()) return;
    const message = body.parentElement;
    chosen = { text, role: message.classList.contains('user') ? 'user' : 'assistant',
      seq: message.dataset.seq, owner: composerScope };
    const rect = range.getBoundingClientRect();
    show(button);
    button.style.left = Math.max(8, Math.min(innerWidth - button.offsetWidth - 8, rect.left)) + 'px';
    button.style.top = Math.max(8, Math.min(innerHeight - button.offsetHeight - 8, rect.bottom + 6)) + 'px';
  });
}
