/* Views describe their identity and actions; application chrome never reads UI copy. */
const viewContexts=new Map();
const viewText=key=>CLATI18n.msg('view.'+key);
function registerView(node,description){
  const prior=viewContexts.get(node.id)||{};viewContexts.set(node.id,{id:node.id,...prior,...description,node});
  if(!prior.node)new MutationObserver(()=>refreshViewContext()).observe(node,{attributes:true,attributeFilter:['data-detail']});
  refreshViewContext();return patch=>registerView(node,patch);
}
function refreshViewContext(){if(workbenchShell?.toolbar)refreshApplicationToolbar(workbenchShell);}
function viewAction(id,icon,label,run,primary=true,enabled=()=>true){return {id,icon,label,run,primary,enabled};}
function activeViewContext(shell){
  if(shell.page)return viewContexts.get(shell.page)||{id:shell.page,node:shell.surfaces.get(shell.page)?.dialog,crumbs:()=>[{label:()=>shellText((dom.app.dataset.page||'').split(/[/?]/)[0])}]};
  const tool=shell.tools?.current();
  const project=state.workbench?.project,session=state.workbench?.session?.id===state.sessionId?state.workbench.session:state.sessions.find(row=>row.id===state.sessionId);
  const crumbs=[{label:()=>shellText('workbench'),run:()=>shell.tools.chat()},...(project?[{label:()=>project.root.split(/[\\/]/).filter(Boolean).pop(),run:()=>openSettings('projects')}]:[])];
  if(state.sessionId)crumbs.push({label:()=>session?.title||CLATI18n.msg('web.untitled_session'),session:true,run:tool?()=>shell.tools.chat():null});
  if(tool)crumbs.push({label:()=>experienceText(tool.key),close:()=>shell.tools.chat(),tool:true});
  const seat=tool?viewContexts.get(tool.id):null;
  return {...seat,id:tool?.id||'conversation',node:tool?.node||document.getElementById('conversation-page'),sidebar:seat?.sidebar||(!tool?dom.sidebar:null),crumbs:()=>crumbs,workspace:true,actions:seat?.actions||(()=>conversationViewActions(shell))};
}
function conversationViewActions(shell){
  const find=viewAction('find','search',()=>CLATI18n.msg('web.find_in_conversation'),event=>setConversationFindOpen(document.getElementById('find-bar').classList.contains('hidden'),event.detail===0));
  find.controls='find-bar';find.expanded=()=>!document.getElementById('find-bar').classList.contains('hidden');
  return [find,viewAction('inspector','panel',()=>viewText('workspace'),()=>dom['inspector-toggle'].click()),viewAction('session','more',()=>CLATI18n.msg('web.session_actions'),event=>{const row=state.sessions.find(item=>item.id===state.sessionId);if(!row)return;const source=document.getElementById('tools-open'),rect=source.getBoundingClientRect();toggleSessionMenu(row,rect.right,rect.bottom+4,source);},false,()=>Boolean(state.sessionId)&&!state.switching)];
}
function navigationViewContext(page,sidebar,title){
  const roots={'resources-dialog':['resources',true],'jobs-dialog':['tasks',true],'history-dialog':['history',false],'market-dialog':['extensions',false]};
  const [,project]=roots[page.id]||[];let selected=null,extra=[];
  const describe={id:page.id,sidebar,actions:()=>extra,crumbs:()=>[{label:title,run:()=>{delete page.dataset.detail;sidebar.querySelector('input')?.focus();}},...(project&&state.workbench?.project?[{label:()=>state.workbench.project.root.split(/[\\/]/).filter(Boolean).pop(),run:()=>openSettings('projects')}]:[]),...(selected?[{label:selected}]:[])]};
  registerView(page,describe);
  return {select(value){selected=value;refreshViewContext();},actions(value){extra=value;refreshViewContext();},context(patch){Object.assign(describe,patch);registerView(page,describe);}};
}
const viewListPreferences={
  read(id){try{const value=JSON.parse(localStorage.getItem('clat.view-lists.v1')||'{}');return value[id]??(id==='conversation'?state.sidebar==='collapsed':false);}catch(_){return id==='conversation'&&state.sidebar==='collapsed';}},
  write(id,collapsed){try{const value=JSON.parse(localStorage.getItem('clat.view-lists.v1')||'{}');if(!('conversation'in value))value.conversation=state.sidebar==='collapsed';value[id]=collapsed;localStorage.setItem('clat.view-lists.v1',JSON.stringify(value));}catch(_){}},
};
function syncViewList(view){
  const mobile=matchMedia('(max-width: 760px)').matches,collapsed=!mobile&&viewListPreferences.read(view.id);
  dom.app.dataset.viewSidebar=collapsed?'collapsed':'expanded';
  if(!view.sidebar)return false;
  view.sidebar.inert=collapsed||(mobile&&(view.id==='conversation'?dom.app.dataset.mobileSidebar!=='open':view.node.dataset.detail==='true'));
  view.sidebar.setAttribute('aria-hidden',String(view.sidebar.inert));return !view.sidebar.inert;
}
function toggleViewList(shell){
  const view=activeViewContext(shell);if(!view.sidebar)return;
  if(matchMedia('(max-width: 760px)').matches){if(view.id==='conversation')dom.app.dataset.mobileSidebar=dom.app.dataset.mobileSidebar==='open'?'closed':'open';else delete view.node.dataset.detail;}
  else viewListPreferences.write(view.id,!viewListPreferences.read(view.id));
  refreshApplicationToolbar(shell);view.node.dispatchEvent(new Event('clat:view-resize'));
}

function registerGlobalViewContexts(shell){
  const settings=document.getElementById('settings-dialog'),nav=settings.querySelector('.settings-nav');
  registerView(settings,{id:settings.id,sidebar:nav,crumbs:()=>[{label:()=>shellText('settings'),run:()=>{viewListPreferences.write(settings.id,false);refreshViewContext();nav.querySelector('button')?.focus();}},...(currentSettingsCategory?[{label:()=>settingsCategoryLabel(currentSettingsCategory)}]:[])],actions:()=>[]});
}
function settingsCategoryLabel(category){
  const keys={appearance:'web.appearance',projects:'web.projects',models:'web.models',utility:'web.companion',permissions:'web.permissions',wechat:'web.wechat',web:'webtool.title',desktop:'pages.updates_about',terminal:'dev.terminal',git:'dev.git'};
  return keys[category]?CLATI18n.msg(keys[category]):shellText('settings');
}
