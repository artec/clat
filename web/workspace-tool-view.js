/* Compact two-column presentation shared by operation, process and workflow tools. */
function createWorkspaceToolView(page,key){
  const view=createNavigationPage(page,{title:()=>experienceText(key)});
  view.list.classList.add('review-files');view.body.classList.add('review-patch');
  const status=el('p','navigation-status');status.setAttribute('role','status');view.sidebar.insertBefore(status,view.list);
  const overview=()=>view.body.replaceChildren(navigationOverview({title:()=>experienceText(key),description:()=>viewText('select_detail'),icon:WORKSPACE_TOOLS.find(tool=>tool.key===key)?.icon||'info'}));overview();
  return {...view,status,overview};
}
