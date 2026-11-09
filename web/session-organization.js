let showArchivedSessions = false;
async function organizeSession(session, flags) {
  if (flags.archived === true && !confirm(CLATI18n.msg("web.archive_this_session_in_the_shared_project_list_chat_content_is_retained_and_can_b", {}, "Archive this session in the shared project list? Chat content is retained and can be restored. The selected conversation stays selected."))) return;
  await rpc('session.organize', { id: session.id, ...flags, confirmed: flags.archived === true });
  await refreshSessions();
}
function installSessionOrganization() {
  const toggle = document.getElementById('archived-sessions');
  for (const attribute of ['aria-label','title']) CLATI18n.attribute(toggle, attribute, () => CLATI18n.msg(showArchivedSessions ? 'web.show_active_sessions' : 'web.show_archived_sessions'));
  toggle.addEventListener('click',()=>{
    showArchivedSessions = !showArchivedSessions;
    toggle.setAttribute('aria-pressed',String(showArchivedSessions));
    CLATI18n.refresh(); renderSessions();
  });
  document.getElementById('project-list-add').addEventListener('click',()=>{
    if (window.__TAURI__) document.querySelector('#clat-desktop-settings [data-action="open"]')?.click();
    else openSettings('projects');
  });
}
