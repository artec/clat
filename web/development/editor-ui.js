/* Editor presentation; document state, permissions and writes remain with their owners. */
function developmentFormatName(format){const keys={text:'plain_text',jsonc:'json_comments',ignore:'ignore_patterns',attributes:'git_attributes',batch:'batch_script'};return keys[format.id]?devText(keys[format.id]):format.name;}
function developmentFileIcon(path){
  const format=globalThis.CLATDevelopmentWidgets.formatFor(path),icon=svgIcon(format.icon);
  icon.classList.add('file-kind-icon');icon.dataset.kind=format.id;return icon;
}
function editorMenu(items,source,x,y){
  const menu=el('div','session-context-menu'),listeners=new AbortController();menu.setAttribute('role','menu');
  let closed=false;const close=()=>{if(closed)return;closed=true;listeners.abort();menu.remove();};
  for(const item of items){const button=el('button');button.type='button';button.setAttribute('role','menuitem');button.append(svgIcon(item.icon),el('span','',()=>devText(item.key)));button.disabled=Boolean(item.disabled);button.onclick=()=>{close();if(!context.isActive())return;source.focus({preventScroll:true});void Promise.resolve().then(item.run).catch(error=>updateRunState(error.message));};menu.append(button);}
  document.body.append(menu);positionSessionMenu(menu,x,y,source);
  document.addEventListener('pointerdown',event=>{if(!menu.contains(event.target))close();},{signal:listeners.signal});
  window.addEventListener('resize',close,{signal:listeners.signal});
  window.addEventListener('clat:editor-menu-close',close,{signal:listeners.signal});
  menu.onkeydown=event=>{const buttons=[...menu.querySelectorAll('button:not(:disabled)')],index=buttons.indexOf(document.activeElement);if(['Escape','Tab'].includes(event.key)){close();if(event.key==='Escape'){event.preventDefault();event.stopPropagation();source.focus();}}else if(['ArrowUp','ArrowDown'].includes(event.key)){event.preventDefault();buttons[(index+(event.key==='ArrowDown'?1:buttons.length-1))%buttons.length]?.focus();}};
  menu.querySelector('button:not(:disabled)')?.focus();return close;
}
function editorPopup(items,source){const rect=source.getBoundingClientRect();return editorMenu(items,source,rect.left,rect.bottom);}
function createEditorLayout(actions){
  const node=el('section','workspace-review dev-editor');node.id='file-browser';
  const sidebar=el('aside','sidebar editor-sidebar navigation-sidebar'),head=el('header','sidebar-head navigation-head');
  head.append(el('strong','',()=>devText('files')));
  const icons=el('div','editor-head-actions');
  for(const [key,icon,run]of [['new_file','file-plus',()=>actions.create(false)],['new_folder','folder-plus',()=>actions.create(true)],['refresh','refresh',()=>actions.refresh()],['collapse_all','collapse-all',()=>actions.collapse()]])icons.append(navigationIcon(()=>devText(key),icon,run));
  const more=navigationIcon(()=>devText('file_actions'),'more',()=>editorPopup([{key:'recover_drafts',icon:'restore',run:()=>actions.recover()}],more));icons.append(more);
  const {element:search,input:query}=navigationSearch(null,()=>devText('find_file'));
  const root=el('button','editor-project');root.type='button';root.append(svgIcon('chevron-down'),svgIcon('folder'),el('strong','',editorProjectName));root.setAttribute('aria-expanded','true');
  const list=el('nav','review-files editor-tree');CLATI18n.attribute(list,'aria-label',()=>devText('files'));list.setAttribute('role','tree');
  root.onclick=()=>{list.hidden=!list.hidden;root.setAttribute('aria-expanded',String(!list.hidden));};
  const status=el('p','editor-feedback');status.setAttribute('role','status');sidebar.append(head,icons,search,root,list,status);
  const detail=el('section','review-patch dev-editor-detail'),tabs=el('nav','dev-file-tabs');tabs.setAttribute('role','tablist');
  CLATI18n.attribute(tabs,'aria-label',()=>devText('open_files'));
  const bar=el('header','dev-editor-toolbar'),breadcrumb=el('nav','dev-editor-path');CLATI18n.attribute(breadcrumb,'aria-label',()=>devText('file_path'));
  const back=navigationIcon(()=>workflowText('back_to_list'),'previous',()=>{delete node.dataset.detail;});back.classList.add('editor-list-back');
  const tools=el('div','editor-tools');
  registerView(node,{sidebar,actions:()=>[viewAction('save','save',()=>devText('save'),()=>actions.save(),true,actions.canSave),viewAction('save-all','save-all',()=>devText('save_all'),()=>actions.saveAll(),true,actions.canSaveAll),viewAction('find','search',()=>devText('find'),()=>actions.find(),true,actions.canFind)]});
  const menu=navigationIcon(()=>devText('file_actions'),'more',()=>editorPopup(actions.menu(),menu));tools.append(menu);bar.append(back,breadcrumb,tools);
  const mount=el('div','dev-editor-mount'),footer=el('footer','editor-statusbar');detail.append(tabs,bar,mount,footer);node.append(sidebar,detail);
  const syncProjectName=()=>{CLATI18n.bind(root.querySelector('strong'),'textContent',editorProjectName);const title=mount.querySelector('.editor-empty>strong');if(title)CLATI18n.bind(title,'textContent',editorProjectName);};
  window.addEventListener('clat:workbench-updated',syncProjectName);window.addEventListener('clat:projects-updated',syncProjectName);
  return {node,sidebar,query,list,status,detail,tabs,toolbar:bar,mount,breadcrumb,footer,root};
}
function createEditorTab(doc,activate,closeDoc,render){
  const row=el('div','dev-file-tab'),button=el('button','editor-tab-label'),name=el('span','editor-tab-name'),meta=el('small'),dirty=el('span','editor-dirty');
  button.type='button';button.setAttribute('role','tab');button.append(developmentFileIcon(doc.path),name,meta,dirty);
  CLATI18n.attribute(dirty,'aria-label',()=>devText('unsaved'));CLATI18n.attribute(button,'title',()=>doc.path);
  button.onclick=()=>activate(doc);button.ondblclick=()=>{doc.pinned=true;render();};
  const close=navigationIcon(()=>devText('close_file'),'close',()=>void closeDoc(doc));row.append(button,close);
  row.editorTab={doc,button,name,meta,dirty,path:doc.path};return row;
}
function renderEditorTabs({tabs,docs,selected,activate,closeDoc,render}){
  const wanted=new Set(docs.values());for(const row of [...tabs.children])if(!wanted.has(row.editorTab?.doc))row.remove();
  let index=0;for(const doc of docs.values()){
    const row=[...tabs.children].find(row=>row.editorTab.doc===doc)||createEditorTab(doc,activate,closeDoc,render),parts=row.editorTab;
    const name=doc.path.split('/').pop(),duplicate=[...docs.values()].some(other=>other!==doc&&other.path.split('/').pop()===name);
    row.classList.toggle('is-selected',doc===selected);row.classList.toggle('is-preview',!doc.pinned);parts.button.setAttribute('aria-selected',String(doc===selected));parts.button.tabIndex=doc===selected?0:-1;
    parts.name.textContent=name;parts.meta.hidden=!duplicate;parts.meta.textContent=doc.path.split('/').slice(0,-1).join('/')||'.';parts.dirty.hidden=!doc.dirty;
    if(parts.path!==doc.path){parts.button.querySelector('svg').replaceWith(developmentFileIcon(doc.path));parts.path=doc.path;}
    if(tabs.children[index]!==row)tabs.insertBefore(row,tabs.children[index]||null);index++;
  }
}
function editorBreadcrumb(target,doc,reveal){
  target.replaceChildren();if(!doc)return;const parts=doc.path.split('/');
  parts.forEach((part,index)=>{if(index)target.append(svgIcon('next'));if(index===parts.length-1){const name=el('span','editor-path-file',part);target.append(developmentFileIcon(doc.path),name);}else{const button=el('button','',part);button.type='button';button.onclick=()=>reveal(parts.slice(0,index+1).join('/'),true);target.append(button);}});
  CLATI18n.attribute(target,'title',()=>doc.path);
}
function editorProjectName(){return projectDisplayName(state.workbench?.project?.root,state.workbench?.project?.name)||CLATI18n.msg('web.current_project');}
function editorEmpty(mount){
  const empty=el('div','editor-empty');empty.append(svgIcon('code'),el('strong','',editorProjectName),el('p','',()=>devText('editor_empty')),el('small','',()=>devText('editor_shortcuts')));mount.replaceChildren(empty);
}
function editorStatus(footer,doc,widget,changeMode){
  footer.replaceChildren();if(!doc||!widget)return;
  const selection=widget.view.state.selection.main,line=widget.view.state.doc.lineAt(selection.head),indent=widget.view.state.tabSize;
  footer.append(el('span','',()=>devText('cursor_position',{line:line.number,column:selection.head-line.from+1})),el('span','',()=>devText(doc.indentTab?'indent_tabs':'indent_spaces',{count:indent})),el('span','',doc.bom?'UTF-8 BOM':'UTF-8'),el('span','',(doc.newline||'lf').toUpperCase()));
  if(doc.readonly)footer.append(el('span','',()=>devText('read_only')));
  const language=el('select');CLATI18n.attribute(language,'aria-label',()=>devText('syntax_language'));
  const auto=el('option');auto.value='';CLATI18n.bind(auto,'textContent',()=>devText('language_auto',{language:developmentFormatName(globalThis.CLATDevelopmentWidgets.formatFor(doc.path,doc.text))}));language.append(auto);
  for(const format of globalThis.CLATDevelopmentWidgets.formats){const option=el('option','',()=>developmentFormatName(format));option.value=format.id;language.append(option);}
  language.value=doc.mode||'';language.onchange=()=>changeMode(language.value);footer.append(language);
}

function installEditorLifecycle(api){
  let recovered=false;
  api.tabs.onkeydown=event=>{if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key)||event.target.getAttribute('role')!=='tab')return;event.preventDefault();const entries=[...api.docs.values()],index=entries.indexOf(api.selected()),next=event.key==='Home'?0:event.key==='End'?entries.length-1:(index+(event.key==='ArrowRight'?1:entries.length-1))%entries.length;api.activate(entries[next]);api.tabs.querySelector('[aria-selected=true]')?.focus();};
  api.node.addEventListener('clat:tool-enter',async()=>{api.enter();if(!recovered)recovered=await api.recover();if(api.active())void api.refresh();});
  api.node.addEventListener('clat:tool-leave',api.leave);
  CLATI18n.onChange(()=>{api.view()?.setPhrases(devEditorPhrases());api.updateStatus();});
  window.addEventListener('focus',()=>{if(api.active())void api.recheck();});
  document.addEventListener('keydown',event=>{if(!api.active()||!context.isActive()||!(event.metaKey||event.ctrlKey)||event.key.toLowerCase()!=='s')return;event.preventDefault();event.stopImmediatePropagation();if(event.shiftKey)void api.saveAll();else void api.save(api.selected());},{capture:true});
  setInterval(()=>{if(api.active()&&context.isActive())void api.recheck(true);},2500);
}
