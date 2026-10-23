let showArchivedSessions = false;
async function organizeSession(session, flags) {
  if (flags.archived === true && !confirm(CLATI18n.msg("web.archive_this_session_in_the_shared_project_list_chat_content_is_retained_and_can_b", {}, "Archive this session in the shared project list? Chat content is retained and can be restored. The selected conversation stays selected."))) return;
  await rpc('session.organize', { id: session.id, ...flags, confirmed: flags.archived === true });
  await refreshSessions();
}
function installSessionOrganization() {
  const toggle = el('button', 'ghost', () => (CLATI18n.msg("web.archived_sessions", {}, "Archived sessions"))); toggle.type = 'button'; toggle.id = 'archived-sessions'; toggle.setAttribute('aria-pressed', 'false');
  toggle.classList.add('sidebar-expanded-only');
  document.getElementById('session-search').closest('label').after(toggle);
  toggle.addEventListener('click', () => {
    showArchivedSessions = !showArchivedSessions;
    toggle.setAttribute('aria-pressed', String(showArchivedSessions));
    CLATI18n.bind(toggle, "textContent", () => showArchivedSessions ? CLATI18n.msg("web.back_to_active_list", {}, "Back to active list") : CLATI18n.msg("web.archived_sessions", {}, "Archived sessions")); renderSessions();
  });
}
