let showArchivedSessions = false;
async function organizeSession(session, flags) {
  if (flags.archived === true && !confirm(CLATI18n.msg("web.archive_this_session_in_the_shared_project_list_chat_content_is_retained_and_can_b", {}, "Archive this session in the shared project list? Chat content is retained and can be restored. The selected conversation stays selected."))) return;
  await rpc('session.organize', { id: session.id, ...flags, confirmed: flags.archived === true });
  await refreshSessions();
}
function installSessionOrganization() {
  const tabs = ['active-sessions','archived-sessions'].map(id=>document.getElementById(id));
  const select = index => {
    showArchivedSessions = index === 1;
    tabs.forEach((tab,i)=>{tab.setAttribute('aria-selected',String(i===index));tab.tabIndex=i===index?0:-1;});
    document.getElementById('session-panel').setAttribute('aria-labelledby',tabs[index].id);
    renderSessions();
  };
  tabs.forEach((tab,index)=>{
    tab.addEventListener('click',()=>select(index));
    tab.addEventListener('keydown',event=>{
      if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;
      event.preventDefault();
      const next=event.key==='Home'?0:event.key==='End'?1:1-index;
      select(next);tabs[next].focus();
    });
  });
}
