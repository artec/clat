/* Shared lazy tree rows; optional editor intents never grant write authority. */
function createProjectTreeRow(entry,tree){
  const row=el('div','file-tree-row'),button=el('button','review-file');button.type='button';button.dataset.path=entry.path;button.dataset.directory=String(Boolean(entry.directory));
  button.setAttribute('role','treeitem');button.setAttribute('aria-selected',String(tree.selected?.()===entry.path));
  const chevron=svgIcon('chevron-down');chevron.classList.add('file-tree-chevron');chevron.classList.toggle('is-leaf',!entry.directory);
  const copy=el('span','file-tree-copy');copy.append(el('span','file-tree-name',entry.name.split('/').pop()));
  if(tree.searching?.())copy.append(el('small','',entry.path.split('/').slice(0,-1).join('/')));
  button.append(chevron,entry.directory?svgIcon('folder'):developmentFileIcon(entry.path),copy);button.classList.toggle('is-selected',tree.selected?.()===entry.path);CLATI18n.attribute(button,'title',()=>entry.path);
  row.append(button);const choose=()=>{for(const item of tree.list.querySelectorAll('.review-file')){item.classList.toggle('is-selected',item===button);item.setAttribute('aria-selected',String(item===button));}tree.select?.(entry);};
  if(tree.menu)button.oncontextmenu=event=>{event.preventDefault();choose();tree.menu(entry,button,event.clientX,event.clientY);};
  return {row,button,choose};
}
function appendProjectTree(entries,parent,tree,captured){
  if(parent===tree.list&&!parent.hasAttribute('role'))parent.setAttribute('role','tree');
  const pending=[];
  for(const entry of [...entries].sort((a,b)=>Number(Boolean(b.directory))-Number(Boolean(a.directory))||a.name.localeCompare(b.name,undefined,{numeric:true}))){
    const {row,button,choose}=createProjectTreeRow(entry,tree);parent.append(row);
    if(!entry.directory){button.onclick=()=>{choose();void tree.preview(entry.path,captured);};button.ondblclick=()=>tree.pin?.(entry.path);continue;}
    const children=el('div','file-tree-children');children.setAttribute('role','group');children.hidden=true;row.append(children);button.setAttribute('aria-expanded','false');let loading=false;
    async function toggle(open){
      children.hidden=!open;button.setAttribute('aria-expanded',String(open));
      if(!open){tree.expanded.delete(entry.path);return;}tree.expanded.add(entry.path);
      if(children.childElementCount||loading)return;loading=true;const request=tree.version();
      try{const result=await rpc('files.list',{directory:entry.path});if(!tree.isOpen()||request!==tree.version()||!tree.owns(captured))return;await appendProjectTree(result.entries||[],children,tree,captured);}
      catch(error){if(tree.isOpen()&&request===tree.version()&&tree.owns(captured))devReport(tree.status,error);}
      finally{loading=false;}
    }
    button.onclick=()=>{choose();void toggle(children.hidden);};if(tree.expanded.has(entry.path))pending.push(toggle(true));
  }
  return Promise.all(pending);
}
function installEditorTree(ui,{active,menu,openFile,recheck}){
  let epoch=0,selected='',directory=false,searching=false,searchScroll=0,searchTimer;
  const expanded=new Set();
  const tree={isOpen:active,owns:()=>true,list:ui.list,status:ui.status,expanded,preview:path=>openFile(path),pin:path=>openFile(path,{fixed:true}),version:()=>epoch,selected:()=>selected,searching:()=>searching,select:entry=>{selected=entry.path;directory=Boolean(entry.directory);},menu};
  function select(path,isDirectory=false){selected=path;directory=isDirectory;for(const button of ui.list.querySelectorAll('[data-path]')){const same=button.dataset.path===path;button.classList.toggle('is-selected',same);button.setAttribute('aria-selected',String(same));}}
  async function refresh(){
    const request=++epoch,query=ui.query.value.trim(),top=ui.list.scrollTop,wasSearching=searching;
    if(Boolean(query)!==searching){if(query)searchScroll=top;searching=Boolean(query);}
    if(query){ui.list.hidden=false;ui.root.setAttribute('aria-expanded','true');}
    const current=()=>request===epoch&&active();
    try{const result=await rpc(query?'files.search':'files.list',query?{query}:{});if(!current())return;
      ui.list.replaceChildren();await appendProjectTree(result.entries||result.paths?.map(path=>({path,name:path,directory:false}))||[],ui.list,tree,[]);if(!current())return;
      ui.list.scrollTop=wasSearching&&!query?searchScroll:top;select(selected,directory);
      if(!ui.list.childElementCount)ui.list.append(el('p','picker-hint',()=>devText('no_files')));
      if(result.truncated)devStatus(ui.status,'file_results_partial');else if(!ui.status.classList.contains('error'))ui.status.textContent='';
      await recheck();
    }catch(error){if(current())devReport(ui.status,error);}
  }
  async function reveal(path,isDirectory=false){
    ui.query.value='';ui.list.hidden=false;ui.root.setAttribute('aria-expanded','true');
    const parts=path.split('/');for(let i=1;i<parts.length+(isDirectory?1:0);i++)expanded.add(parts.slice(0,i).join('/'));
    select(path,isDirectory);await refresh();const button=[...ui.list.querySelectorAll('[data-path]')].find(b=>b.dataset.path===path);button?.scrollIntoView({block:'nearest'});
  }
  ui.query.oninput=()=>{epoch++;clearTimeout(searchTimer);searchTimer=setTimeout(()=>void refresh(),150);};
  ui.list.onkeydown=event=>{
    const button=event.target.closest('.review-file');if(!button||!['ArrowUp','ArrowDown','ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;event.preventDefault();
    const rows=[...ui.list.querySelectorAll('.review-file')].filter(b=>!b.closest('[hidden]')),index=rows.indexOf(button),isDirectory=button.dataset.directory==='true',open=button.getAttribute('aria-expanded')==='true';
    if(event.key==='ArrowRight'){if(isDirectory&&!open)button.click();else rows[index+1]?.focus();}
    else if(event.key==='ArrowLeft'){if(isDirectory&&open)button.click();else button.parentElement.parentElement.closest('.file-tree-row')?.querySelector('.review-file')?.focus();}
    else rows[event.key==='Home'?0:event.key==='End'?rows.length-1:Math.max(0,Math.min(rows.length-1,index+(event.key==='ArrowDown'?1:-1)))]?.focus();
  };
  return {refresh,reveal,select,path:()=>selected,directory:()=>directory,collapse(){expanded.clear();return refresh();},suspend(){epoch++;clearTimeout(searchTimer);document.dispatchEvent(new Event('clat:editor-menu-close'));}};
}
