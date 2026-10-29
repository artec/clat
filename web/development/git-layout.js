/* Shared shell, compact source-control rows and device-only layout preferences. */
function createGitLayout(node){
  const view=createNavigationPage(node,{title:()=>devText('git')});view.list.remove();view.filters.remove();
  const repo=el('div','git-repository-heading'),name=el('strong'),branch=el('span');repo.append(svgIcon('branch'),name,branch);view.sidebar.append(repo);
  const form=el('div','git-submit'),message=el('textarea'),tools=el('div','git-submit-tools'),locale=devLanguageSelect('system');
  CLATI18n.attribute(message,'placeholder',()=>devText('commit_placeholder'));CLATI18n.attribute(message,'aria-label',()=>devText('commit'));message.rows=1;
  const generate=navigationIcon(()=>devText('generate_message'),'sparkles',()=>{}),commit=devButton('commit',()=>{},'primary'),cancel=navigationIcon(()=>devText('cancel'),'close',()=>{});cancel.hidden=true;
  tools.append(generate,locale,cancel);form.append(message,tools,commit);view.sidebar.append(form);
  const status=el('p','navigation-status');status.setAttribute('role','status');view.sidebar.append(status);
  const changes=el('section','git-changes'),split=el('div','git-splitter'),bottom=el('section','git-bottom');split.tabIndex=0;split.setAttribute('role','separator');split.setAttribute('aria-orientation','horizontal');split.setAttribute('aria-valuemin','25');split.setAttribute('aria-valuemax','80');CLATI18n.attribute(split,'aria-label',()=>devText('resize_graph'));
  const repositories=el('details','git-repositories'),repos=el('nav'),graph=el('details','git-history'),history=el('section');graph.open=true;
  const repositoryStatus=el('p','navigation-status');repositoryStatus.hidden=true;repositoryStatus.setAttribute('role','status');repositories.append(el('summary','',()=>devText('repositories')),repositoryStatus,repos);graph.append(el('summary','',()=>devText('graph')),history);bottom.append(repositories,graph);view.sidebar.append(changes,split,bottom);
  const tabs=el('nav','dev-file-tabs'),toolbar=el('div','dev-editor-toolbar'),mount=el('section','git-diff-content');view.body.replaceChildren(tabs,toolbar,mount);view.body.classList.add('git-body');
  view.bar.remove();gitLayoutPreference(node,changes,split,repositories,graph);
  return {...view,repoName:name,branch,message,locale,generate,commit,cancel,status,repositoryStatus,changes,repos,history,tabs,toolbar,mount};
}
function gitLayoutPreference(node,changes,split,repositories,graph){
  const key='clat.git.layout';let saved={ratio:60,repositories:false,graph:true};try{Object.assign(saved,JSON.parse(localStorage.getItem(key)||'{}'));}catch(_){}
  const ratio=value=>{saved.ratio=Math.max(25,Math.min(80,Number(value)||60));node.style.setProperty('--git-list-ratio',saved.ratio+'%');split.setAttribute('aria-valuenow',String(Math.round(saved.ratio)));};
  const store=()=>{try{localStorage.setItem(key,JSON.stringify(saved));}catch(_){}};ratio(saved.ratio);repositories.open=saved.repositories;graph.open=saved.graph;
  repositories.ontoggle=()=>{saved.repositories=repositories.open;store();};graph.ontoggle=()=>{saved.graph=graph.open;store();};
  split.onpointerdown=event=>{split.setPointerCapture(event.pointerId);const rect=changes.getBoundingClientRect(),total=rect.height+node.querySelector('.git-bottom').clientHeight;split.onpointermove=e=>ratio((e.clientY-rect.top)/total*100);};
  split.onpointerup=()=>{split.onpointermove=null;store();};split.onpointercancel=()=>{split.onpointermove=null;store();};
  split.onkeydown=event=>{if(['ArrowUp','ArrowDown'].includes(event.key)){event.preventDefault();ratio(saved.ratio+(event.key==='ArrowUp'?-5:5));store();}};
}
function gitFileRow(file,{selected=false,open,pin,actions=[],openFile}={}){
  const row=el('div','git-file-row'),button=el('button','git-file-main'),copy=el('span','git-file-copy');button.type='button';CLATI18n.attribute(button,'aria-label',()=>file.path+' · '+devGitFileStatus(file));const parts=file.path.split('/'),name=parts.pop();
  copy.append(el('strong','',name),el('small','',parts.join('/')));button.append(svgIcon('file'),copy);button.onclick=open;button.ondblclick=pin;button.setAttribute('aria-current',String(selected));CLATI18n.attribute(button,'title',()=>file.path);row.append(button);
  const tools=el('span','git-file-tools');for(const action of actions)tools.append(navigationIcon(()=>devText(action.label),action.icon,action.run));if(openFile){const more=navigationIcon(()=>devText('git_actions'),'more',event=>{const rect=event.currentTarget.getBoundingClientRect();showGitFileMenu(file,actions,openFile,more,rect.left,rect.bottom);});tools.append(more);row.oncontextmenu=event=>{event.preventDefault();showGitFileMenu(file,actions,openFile,button,event.clientX,event.clientY);};}const badge=el('span','git-file-state '+(gitConflicted(file)?'git-status-conflict':file.status?.includes('D')?'git-status-deleted':file.untracked||file.status?.startsWith('A')?'git-status-added':'git-status-modified'),file.untracked||file.status==='??'?'U':file.status?.trim()||'M');CLATI18n.attribute(badge,'title',()=>devGitFileStatus(file));CLATI18n.attribute(badge,'aria-label',()=>devGitFileStatus(file));row.append(tools,badge);return row;
}
function gitSection(label,rows,actions=[]){
  const section=el('details','git-file-group');section.open=true;const heading=el('summary'),count=el('span','git-count',String(rows.length));heading.append(el('span','',()=>devText(label)),count);section.append(heading);
  const tools=el('span','git-group-tools');for(const action of actions)tools.append(navigationIcon(()=>devText(action.label),action.icon,event=>{event.preventDefault();event.stopPropagation();action.run();}));heading.append(tools);return section;
}

let closeGitFileMenu=null;
function showGitFileMenu(file,actions,openFile,source,x,y){
  closeGitFileMenu?.();const menu=el('div','session-context-menu'),listeners=new AbortController();menu.setAttribute('role','menu');CLATI18n.attribute(menu,'aria-label',()=>devText('git_actions'));
  const close=()=>{listeners.abort();menu.remove();closeGitFileMenu=null;};closeGitFileMenu=close;
  const items=[...actions,{label:'open_file',icon:'open',run:()=>openFile(file.path)},{label:'copy_path',icon:'copy',run:()=>navigator.clipboard.writeText(file.path)}];
  for(const item of items){const button=el('button');button.type='button';button.setAttribute('role','menuitem');button.append(svgIcon(item.icon),el('span','',()=>devText(item.label)));button.onclick=()=>{close();source.focus();void Promise.resolve().then(item.run).catch(error=>updateRunState(error.message));};menu.append(button);}
  document.body.append(menu);positionSessionMenu(menu,x,y,source);menu.firstChild?.focus();
  document.addEventListener('pointerdown',event=>{if(!menu.contains(event.target))close();},{signal:listeners.signal});
  window.addEventListener('resize',close,{signal:listeners.signal});
  menu.onkeydown=event=>{const buttons=[...menu.querySelectorAll('button')],index=buttons.indexOf(document.activeElement);if(['Escape','Tab'].includes(event.key)){close();if(event.key==='Escape'){event.preventDefault();source.focus();}}else if(['ArrowUp','ArrowDown'].includes(event.key)){event.preventDefault();buttons[(index+(event.key==='ArrowDown'?1:buttons.length-1))%buttons.length]?.focus();}};
}
