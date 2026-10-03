let showArchivedSessions = false;
async function organizeSession(session, flags) {
  if (flags.archived === true && !confirm('Archive this session in the shared project list? Chat content is retained and can be restored. The selected conversation stays selected.')) return;
  await rpc('session.organize', { id: session.id, ...flags, confirmed: flags.archived === true });
  await refreshSessions();
}
function installSessionOrganization() {
  const toggle = el('button', 'ghost', 'Archived sessions'); toggle.type = 'button'; toggle.id = 'archived-sessions'; toggle.setAttribute('aria-pressed', 'false');
  toggle.classList.add('sidebar-expanded-only');
  document.getElementById('session-search').closest('label').after(toggle);
  toggle.addEventListener('click', () => {
    showArchivedSessions = !showArchivedSessions;
    toggle.setAttribute('aria-pressed', String(showArchivedSessions));
    toggle.textContent = showArchivedSessions ? 'Back to active list' : 'Archived sessions'; renderSessions();
  });
}
