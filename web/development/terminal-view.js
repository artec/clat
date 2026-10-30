/* Conversation-style terminal navigation; no process ownership in the view. */
function terminalHeading(row){
  if(!row)return '';
  const shell=(row.shell||'').split(/[\\/]/).pop(),name=row.name||shell;
  const same=name.toLowerCase()===shell.toLowerCase()||name.toLowerCase()===shell.replace(/\.exe$/i,'').toLowerCase();
  return !shell||same?name:name+' · '+shell;
}
function terminalIcon(key,icon,action){return navigationIcon(()=>devText(key),icon,action);}
function createTerminalPage(actions){
  const node=el('section','dev-terminal-page navigation-layout');node.id='terminal-page';
  const sidebar=el('aside','sidebar terminal-sidebar navigation-sidebar'),head=el('div','sidebar-head navigation-head'),title=el('strong','',()=>devText('terminal'));
  head.append(title);
  const {element:search,input:query}=navigationSearch(null,()=>devText('terminal_search'));
  const list=el('nav','terminal-project-list');sidebar.append(head,search,list);
  const detail=el('section','dev-terminal-detail navigation-detail'),bar=el('header','terminal-toolbar navigation-toolbar'),name=el('strong',''),status=el('span','terminal-state'),mount=el('div','dev-terminal-mount'),empty=el('div','terminal-empty');
  const back=terminalIcon('back_list','previous',()=>delete node.dataset.detail);back.classList.add('navigation-back');
  bar.append(back,name,status,terminalIcon('take_control','permission-write',()=>actions.claim()),terminalIcon('copy','copy',()=>actions.copy()),terminalIcon('clear','trash',()=>actions.clear()));
  empty.append(svgIcon('terminal'),el('p','',()=>devText('terminal_empty')),devButton('new_terminal',()=>actions.create()));mount.append(empty);detail.append(bar,mount);node.append(sidebar,detail);
  registerView(node,{sidebar,actions:()=>[viewAction('create','plus',()=>devText('new_terminal'),()=>actions.create(),true,actions.canCreate),viewAction('find','search',()=>devText('find'),()=>actions.find(),true,actions.hasSelection),viewAction('settings','settings',()=>devText('terminal_settings'),()=>openSettings('terminal')),viewAction('shell','terminal',()=>devText('shell_choice'),event=>actions.shellMenu(event.currentTarget),false),viewAction('cleanup','trash',()=>devText('close_ended'),()=>actions.cleanup(),false)]});
  query.oninput=()=>actions.render();return {node,list,query,detail,status,mount,name,empty};
}
function terminalMenu(source,items){
  globalThis.CLATTerminalMenuClose?.();
  const menu=el('div','session-context-menu');menu.dataset.terminalMenu='true';menu.setAttribute('role','menu');
  for(const [key,icon,run]of items){const button=el('button');button.type='button';button.setAttribute('role','menuitem');button.append(svgIcon(icon),el('span','',()=>typeof key==='function'?key():devText(key)));button.onclick=()=>{dispose();void run();};menu.append(button);}
  document.body.append(menu);const bounds=source.getBoundingClientRect(),inset=12;menu.style.left=Math.max(inset,Math.min(bounds.right-menu.offsetWidth,innerWidth-menu.offsetWidth-inset))+'px';menu.style.top=Math.max(inset,Math.min(bounds.bottom+8,innerHeight-menu.offsetHeight-inset))+'px';
  function outside(event){if(!menu.contains(event.target)&&!source.contains(event.target))dispose();}function key(event){if(event.key==='Escape'){event.preventDefault();dispose();source.focus();}else if(['ArrowDown','ArrowUp'].includes(event.key)){event.preventDefault();const rows=[...menu.querySelectorAll('button')],index=rows.indexOf(document.activeElement);rows[(index+(event.key==='ArrowDown'?1:rows.length-1))%rows.length]?.focus();}}
  function dispose(){if(globalThis.CLATTerminalMenuClose===dispose)globalThis.CLATTerminalMenuClose=null;menu.remove();globalThis.document.removeEventListener('pointerdown',outside,true);globalThis.document.removeEventListener('keydown',key,true);}
  globalThis.CLATTerminalMenuClose=dispose;globalThis.document.addEventListener('pointerdown',outside,true);globalThis.document.addEventListener('keydown',key,true);menu.querySelector('button')?.focus();return dispose;
}
function renderTerminalDirectory(ui,projects,selected,actions,expanded){
  ui.list.replaceChildren();const query=ui.query.value.trim().toLocaleLowerCase();
  for(const project of projects){
    const rows=project.terminals.filter(row=>!query||[row.name,project.title,project.root].join(' ').toLocaleLowerCase().includes(query));if(query&&!rows.length)continue;
    const group=el('section','project-group'),head=el('div','project-group-head'),toggle=terminalIcon('expand_project','chevron-down',()=>{expanded.set(project.root,expanded.get(project.root)===false);actions.render();}),label=el('button','project-directory-row');label.type='button';label.append(svgIcon('folder'),el('span','project-directory-name',project.title));label.onclick=()=>{expanded.set(project.root,expanded.get(project.root)===false);actions.render();};CLATI18n.attribute(label,'title',()=>project.root);
    const extras=[[()=>devText('close_ended'),'trash',()=>actions.cleanup(project)]],more=context.organization.menuButton(project,extras);label.append(context.organization.pin(project));context.organization.bind(head,project,more,extras);
    head.append(toggle,label,more,terminalIcon('new_terminal','plus',()=>actions.create(project)));group.append(head);
    const body=el('ul','session-list');body.hidden=!query&&expanded.get(project.root)===false;toggle.setAttribute('aria-expanded',String(!body.hidden));
    for(const row of [...rows].reverse()){
      const item=el('li'),button=el('button','session-item'),copy=el('span','session-copy');button.type='button';button.dataset.phase=row.phase==='running'?'running':'ended';copy.append(el('strong','',row.name),el('small','',()=>devText(row.phase==='running'?'running':'ended')));button.append(svgIcon('terminal'),copy);button.onclick=()=>actions.select(project,row);item.classList.toggle('active',selected?.id===row.id&&selected?.root===project.root);
      const more=terminalIcon('more','more',event=>actions.menu(project,row,event.currentTarget));more.classList.add('session-more');item.append(button,more);item.oncontextmenu=event=>{event.preventDefault();actions.menu(project,row,more);};body.append(item);
    }
    if(project.error)body.append(el('li','error',()=>CLATI18n.present(project.presentation,devText('failed'))));group.append(body);ui.list.append(group);
  }
}
