let showArchivedSessions = false;
async function organizeSession(session, flags) {
  if (flags.archived === true && !await CLATDialog.confirm(CLATI18n.msg("web.archive_this_session_in_the_shared_project_list_chat_content_is_retained_and_can_b", {}, "Archive this session in the shared project list? Chat content is retained and can be restored."))) return;
  const wasCurrent = session.id === state.sessionId;
  await rpc('session.organize', { id: session.id, ...flags, confirmed: flags.archived === true });
  // INV-SL3（SL-U1）：归档当前会话 → 跳转全新会话（与 /new 完全同
  // 流程：懒物化，首条消息前零 journal/列表条目）；归档非当前会话则
  // 选择不变。
  if (flags.archived === true && wasCurrent) { await newSession(); return; }
  await refreshSessions();
}
// SL-U3：归档域两段式删除——两次确认，第二确认点名会话标题；入口
// 只出现在已归档行（活跃域零删除，core 侧 INV-SL1 兜底）。删除的是
// 当前浏览中的归档会话时服务器已把选择腾空到 Fresh 并推进了选择
// 代际——此处**不得**再补发 session.new（rpc 会注入过期的
// expected_selection_generation，必撞 "session selection changed"，
// 2026-11-19 负责人真机首版即此症）；改走纯重订阅跟随服务端跳转。
async function deleteArchivedSession(session) {
  if (state.runActive || state.switching || state.compactionActive) return;
  if (!await CLATDialog.confirm(CLATI18n.msg('web.delete_archived_session_first_confirm', {}, "Delete this archived conversation? This cannot be undone."))) return;
  const title = session.title || shortenId(session.id);
  if (!await CLATDialog.confirm(CLATI18n.msg('web.delete_archived_session_second_confirm', { title }, "Permanently delete “{title}”? Its whole chat history is removed. This cannot be undone."))) return;
  const result = await rpc('session.delete', { id: session.id, confirmed: true });
  if (result?.jumped_to_new) { await followServerSelectionJump(); return; }
  await refreshSessions();
}
function setArchivedView(shown) {
  showArchivedSessions = shown;
  const toggle = document.getElementById('archived-sessions');
  toggle.setAttribute('aria-pressed',String(shown));
  CLATI18n.refresh(); renderSessions();
}
function installSessionOrganization() {
  const toggle = document.getElementById('archived-sessions');
  for (const attribute of ['aria-label','title']) CLATI18n.attribute(toggle, attribute, () => CLATI18n.msg(showArchivedSessions ? 'web.show_active_sessions' : 'web.show_archived_sessions'));
  toggle.addEventListener('click',()=>setArchivedView(!showArchivedSessions));
  document.getElementById('project-list-add').addEventListener('click',()=>{
    if (window.__TAURI__) document.querySelector('#clat-desktop-settings [data-action="open"]')?.click();
    else openSettings('projects');
  });
}
