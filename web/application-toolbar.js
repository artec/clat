/* One renderer for page-owned breadcrumbs, list controls and action descriptors. */
function createApplicationToolbar(shell){
  const bar=el('header','application-toolbar');bar.id='application-toolbar';const navigation=el('nav','application-navigation');
  const back=shellButton('application-back','previous',()=>shellText('back'),()=>context.history.go(-1)),forward=shellButton('application-forward','next',()=>shellText('forward'),()=>context.history.go(1));
  const modifier=/Mac|iPhone|iPad/.test(navigator.platform||'')?'⌘':'Ctrl+';
  for(const [button,key,bracket]of [[back,'back','['],[forward,'forward',']']]){
    CLATI18n.attribute(button,'title',()=>shellText(key)+' ('+modifier+bracket+')');
    button.setAttribute('aria-keyshortcuts',(/Mac|iPhone|iPad/.test(navigator.platform||'')?'Meta':'Control')+'+'+bracket);
  }
  const list=navigationIcon(()=>viewText('toggle_list'),'panel',()=>toggleViewList(shell));list.id='project-list-toggle';navigation.append(back,forward,list);
  const location=el('nav','application-location');CLATI18n.attribute(location,'aria-label',()=>viewText('location'));
  // 右上角统一四段秩序：主操作 → 工具/更多菜单 → 此页帮助 → 布局面板开关（最右）
  const right=el('div','application-right'),actions=el('nav','application-actions'),utility=el('div','application-utility');
  const menu=el('div','workbench-tools hidden');menu.id='workbench-tools';menu.setAttribute('role','menu');
  const more=navigationIcon(()=>CLATI18n.msg('tui.tools'),'tools',()=>{closeSessionMenu();menu.classList.toggle('hidden');more.setAttribute('aria-expanded',String(!menu.classList.contains('hidden')));});more.id='tools-open';more.setAttribute('aria-controls',menu.id);more.setAttribute('aria-expanded','false');
  right.append(actions,more,utility);
  const technical=document.querySelector('.session-technical');technical.querySelector('summary').after(document.querySelector('.conversation-meta'));
  const legacy=document.querySelector('.header-actions');legacy.hidden=true;legacy.id='legacy-workbench-actions';legacy.append(dom['session-title'],dom['mobile-sidebar-open']);document.getElementById('conversation-page').append(legacy);dom['session-title'].hidden=true;dom['mobile-sidebar-open'].hidden=true;
  document.querySelector('.conversation-header').remove();bar.append(navigation,location,right,menu);dom.app.prepend(bar);
  shell.toolbar={back,forward,list,actions,utility,more,menu,breadcrumbs:createApplicationBreadcrumbs(location,shell),signature:'',view:null};
  const close=()=>{menu.classList.add('hidden');more.setAttribute('aria-expanded','false');};shell.toolbar.close=close;
  document.addEventListener('pointerdown',event=>{if(!menu.contains(event.target)&&!more.contains(event.target))close();});
  menu.onkeydown=event=>{if(event.key==='Escape'){event.preventDefault();close();more.focus();}else if(['ArrowDown','ArrowUp'].includes(event.key)){event.preventDefault();const rows=[...menu.querySelectorAll('button:not(:disabled)')],index=rows.indexOf(document.activeElement);rows[(index+(event.key==='ArrowDown'?1:rows.length-1))%rows.length]?.focus();}};
  context.history.subscribe(()=>refreshApplicationToolbar(shell));CLATI18n.onChange(()=>{shell.toolbar.signature='';refreshApplicationToolbar(shell);});refreshApplicationToolbar(shell);
  window.addEventListener('clat:builtin-availability',()=>refreshApplicationToolbar(shell));
}
function refreshApplicationToolbar(shell=workbenchShell){
  if(!shell?.toolbar)return;const chrome=shell.toolbar,view=activeViewContext(shell);
  if(chrome.view!==view.id){chrome.close();chrome.breadcrumbs.close();closeSessionMenu();setConversationFindOpen(false);window.dispatchEvent(new Event('clat:editor-menu-close'));chrome.view=view.id;chrome.signature='';globalThis.CLATTerminalMenuClose?.();closeGitFileMenu?.();}
  chrome.breadcrumbs.render(view);const expanded=syncViewList(view);chrome.list.hidden=!view.sidebar;chrome.list.setAttribute('aria-expanded',String(expanded));
  if(view.sidebar){view.sidebar.id||=view.id+'-list';chrome.list.setAttribute('aria-controls',view.sidebar.id);}else chrome.list.removeAttribute('aria-controls');
  renderApplicationActions(chrome,view,shell);chrome.back.disabled=!context.history.canGo(-1);chrome.forward.disabled=!context.history.canGo(1);syncPanelAccessibility();
}
function renderApplicationActions(chrome,view,shell){
  const capability=['file-browser','workspace-review','terminal-page'].includes(view.id)?'builtin.development':null;
  const featureReady=()=>!capability||builtinReady(capability);
  const raw=(view.actions?.()||[]).filter(action=>!action.hidden?.());
  // 尾槽：先独立放置「此页帮助」，再放置面板开关等全局尾槽操作（如右栏开关）
  const contextualHelp=view.id==='help-dialog'?[]:[{...viewAction('page-help','help',()=>helpText('page_help'),()=>openHelp(),true),slot:'utility'}];
  const utilityActions=[...contextualHelp,...raw.filter(action=>action.slot==='utility')];
  const pageActions=raw.filter(action=>action.slot!=='utility');
  const primary=pageActions.filter(action=>action.primary).slice(0,3),secondary=pageActions.filter(action=>!primary.includes(action));
  const signature=JSON.stringify([view.id,[...primary,...secondary,...utilityActions].map(a=>[a.id,typeof a.label==='function'?a.label():a.label,featureReady()&&a.enabled?.()!==false,Boolean(a.busy?.())]),Boolean(view.workspace),view.workspace?[builtinReady('builtin.development'),builtinUnavailableReason('builtin.development')]:null,shell.tools?.current()?.id]);
  if(signature===chrome.signature)return;chrome.signature=signature;chrome.actions.replaceChildren();chrome.utility?.replaceChildren();chrome.menu.replaceChildren();
  const button=(action,inline)=>{const run=event=>{const source=inline?event.currentTarget:chrome.more;chrome.close();void Promise.resolve().then(()=>{if(activeViewContext(shell).id===view.id&&featureReady()&&action.enabled?.()!==false)return action.run({currentTarget:source,detail:event.detail});}).catch(error=>{if(activeViewContext(shell).id===view.id)updateRunState(error.message||String(error));});};const node=inline?navigationIcon(action.label,action.icon,run):shellButton('',action.icon,action.label,run);node.dataset.action=action.id;if(action.controls)node.setAttribute('aria-controls',action.controls);if(action.expanded)node.setAttribute('aria-expanded',String(action.expanded()));node.disabled=!featureReady()||action.enabled?.()===false||Boolean(action.busy?.());if(capability&&!featureReady())CLATI18n.attribute(node,'title',()=>builtinUnavailableReason(capability));if(!inline)node.setAttribute('role','menuitem');return node;};
  for(const action of primary)chrome.actions.append(button(action,true));
  for(const action of secondary)chrome.menu.append(button(action,false));
  if(view.workspace){if(secondary.length)chrome.menu.append(el('hr','menu-separator'));const group=el('div','workspace-action-group');group.setAttribute('role','group');CLATI18n.attribute(group,'aria-label',()=>viewText('workspace'));chrome.menu.append(group);for(const tool of WORKSPACE_TOOLS){const node=button(viewAction('tool-'+tool.key,tool.icon,()=>experienceText(tool.key),()=>shell.tools.open(tool.id)),false);node.setAttribute('aria-current',String(shell.tools.current()?.id===tool.id));if(['files','changes'].includes(tool.key)){node.disabled=!builtinReady('builtin.development');CLATI18n.attribute(node,'title',()=>builtinReady('builtin.development')?'':builtinUnavailableReason('builtin.development'));}group.append(node);}}
  // 工作区页使用扳手图标（含工作区工具切换），非工作区页使用三点更多图标
  const moreKind=view.workspace?'tools':'more';
  if(chrome.more.dataset&&chrome.more.dataset.kind!==moreKind){
    chrome.more.dataset.kind=moreKind;
    if(typeof chrome.more.querySelector==='function'){chrome.more.querySelector('svg')?.replaceWith(svgIcon(moreKind));}
    const label=()=>moreKind==='tools'?CLATI18n.msg('tui.tools'):CLATI18n.msg('pages.actions');
    if(typeof CLATI18n?.attribute==='function'){CLATI18n.attribute(chrome.more,'title',label);CLATI18n.attribute(chrome.more,'aria-label',label);}
  }
  const tailHost=chrome.utility||chrome.actions;
  for(const action of utilityActions)tailHost.append(button(action,true));
  chrome.more.hidden=!chrome.menu.children.length;if(chrome.more.hidden)chrome.close();
}
