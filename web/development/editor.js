/* Project document tabs keep their own CodeMirror states and undo histories. */
function createDevelopmentEditor(){
  const node=el('section','workspace-review dev-editor');node.id='file-browser';
  const header=el('div','review-header'),query=el('input','');query.type='search';CLATI18n.attribute(query,'placeholder',()=>devText('find_file'));
  const status=el('p','picker-hint');status.setAttribute('role','status');const note=el('p','picker-hint',()=>devText('manual_save'));
  const list=el('nav','review-files'),detail=el('section','review-patch dev-editor-detail'),tabs=el('nav','dev-file-tabs'),toolbar=el('div','dev-editor-toolbar'),mount=el('div','dev-editor-mount'),breadcrumb=el('span','dev-editor-path');
  const docs=new Map();let selected=null,view=null,epoch=0,active=false,selectedEntry='',selectedDirectory=false,openEpoch=0,recovered=false;const expanded=new Set();
  const drafts=createDevelopmentDrafts({docs,status,activate});const queueDraft=drafts.queue,recover=drafts.recover;
  const controller={node,docs,get selected(){return selected;},openFile,save,saveAll,refresh,createFile,selection:()=>view?view.view.state.selection.main:null,flush:drafts.flush};
  header.append(el('h2','',()=>devText('files')),query,devButton('new_file',()=>void createFile(false)),devButton('new_folder',()=>void createFile(true)),devButton('refresh',()=>void refresh()),devButton('recover_drafts',()=>void recover(true)));
  toolbar.append(devButton('save',()=>void save(selected)),devButton('save_all',()=>void saveAll()),devButton('find',()=>view?.search()),devButton('rename',()=>void rename()),devButton('delete',()=>void remove()),devButton('quote',()=>void quote()),devButton('terminal_here',()=>void terminalHere()));
  toolbar.prepend(breadcrumb);
  detail.append(tabs,toolbar,mount);node.append(header,note,status,list,detail);
  const tree={isOpen:()=>active,owns:()=>true,list,status,expanded,preview:path=>openFile(path),pin:path=>openFile(path,{fixed:true}),version:()=>epoch};
  function stash(){if(selected&&view){selected.state=view.state();selected.top=view.view.scrollDOM.scrollTop;selected.left=view.view.scrollDOM.scrollLeft;}view?.destroy();view=null;}
  function renderTabs(){
    breadcrumb.textContent=selected?.path||'';tabs.replaceChildren();for(const doc of docs.values()){
      const row=el('div','dev-file-tab');row.classList.toggle('is-selected',doc===selected);
      const button=el('button','ghost',()=>doc.path.split('/').pop()+(doc.dirty?' •':''));button.type='button';button.onclick=()=>activate(doc);button.ondblclick=()=>{doc.pinned=true;};
      const close=el('button','icon-button',()=> '×');close.type='button';CLATI18n.attribute(close,'aria-label',()=>devText('close_file'));close.onclick=()=>void closeDoc(doc);row.append(button,close);tabs.append(row);
    }
  }
  function activate(doc){
    if(selected===doc&&view)return;stash();selected=doc;selectedEntry=doc.path;selectedDirectory=false;breadcrumb.textContent=doc.path;mount.replaceChildren();node.dataset.detail='true';renderTabs();
    if(doc.readonly&&doc.reason)devStatus(status,doc.reason);else CLATI18n.bind(status,'textContent',()=>doc.path);
    view=globalThis.CLATDevelopmentWidgets.editor(mount,{text:doc.text,path:doc.path,readonly:doc.readonly,state:doc.state,phrases:devEditorPhrases(),onChange:text=>{doc.text=text;doc.dirty=text!==doc.baseText;doc.pinned=true;renderTabs();queueDraft(doc);},onSave:()=>void save(doc)});
    view.view.scrollDOM.scrollTop=doc.top||0;view.view.scrollDOM.scrollLeft=doc.left||0;
  }
  async function openFile(path,{fixed=false}={}){
    const request=++openEpoch;
    if(docs.has(path)){const doc=docs.get(path);doc.pinned||=fixed;activate(doc);return;}
    try{const result=await rpc('editor.open',{path});if(request!==openEpoch)return;
      for(const [key,doc]of docs){if(!doc.pinned&&!doc.dirty){if(doc===selected)stash();docs.delete(key);}}
      const doc={...result,text:result.text,baseText:result.text,dirty:false,pinned:fixed,readonly:!result.editable};docs.set(path,doc);activate(doc);
    }catch(error){devReport(status,error);}
  }
  async function refresh(){
    const generation=++epoch;list.replaceChildren();
    try{const found=await rpc(query.value.trim()?'files.search':'files.list',query.value.trim()?{query:query.value}:{});if(generation!==epoch)return;
      const entries=found.entries||found.paths.map(path=>({path,name:path,directory:false}));appendProjectTree(entries,list,tree,[]);
      list.onclick=event=>{const button=event.target.closest('[data-path]');if(button){selectedEntry=button.dataset.path;selectedDirectory=button.dataset.directory==='true';}};;
      await recheck();
    }catch(error){devReport(status,error);}
  }
  async function recheck(currentOnly=false){
    for(const doc of currentOnly?(selected?[selected]:[]):docs.values()){
      let disk;try{disk=await rpc('editor.open',{path:doc.path});}catch(error){doc.conflict=true;if(error.technical==='file_missing')doc.disk={version:'missing',text:''};if(doc===selected)showConflict(doc);continue;}
      if(disk.version===doc.version)continue;
      if(doc.dirty){doc.conflict=true;doc.disk=disk;if(doc===selected)showConflict(doc);}
      else{Object.assign(doc,disk,{baseText:disk.text,readonly:!disk.editable,conflict:false,state:null});if(doc===selected){stash();doc.state=null;selected=null;activate(doc);}}
    }
  }
  async function save(doc,overwrite=false){
    if(!doc||doc.readonly||!doc.dirty)return true;
    try{
      if(doc.conflict&&!overwrite){showConflict(doc);return false;}
      const version=overwrite?(doc.disk?.version==='missing'?'missing':(await rpc('editor.open',{path:doc.path})).version):doc.version;
      const text=doc.text;const result=await rpc('editor.save',{operationId:devId(),path:doc.path,version,text,bom:doc.bom,newline:doc.newline});
      doc.version=result.version;doc.baseText=text;doc.dirty=doc.text!==text;doc.conflict=false;renderTabs();queueDraft(doc);devStatus(status,'saved');return true;
    }catch(error){devReport(status,error);doc.conflict=true;showConflict(doc);return false;}
  }
  async function saveAll(){for(const doc of docs.values())if(!await save(doc))return false;return true;}
  async function closeDoc(doc){
    if(doc.dirty){const answer=await devChoice(devText('unsaved_file',{path:doc.path}),['save','discard','cancel']);if(answer==='cancel'||answer==='save'&&(!await save(doc)||doc.dirty))return;}
    if(doc===selected){stash();selected=null;breadcrumb.textContent='';mount.replaceChildren();}docs.delete(doc.path);queueDraft({...doc,dirty:false});renderTabs();if(!selected&&docs.size)activate([...docs.values()].at(-1));
  }
  installDevelopmentFileActions(controller,{status,list,toolbar,activate,stash,renderTabs,queueDraft,selectedPath:()=>selectedEntry||selected?.path,selectedDirectory:()=>selectedDirectory,setPath:path=>{selectedEntry=path;},clearSelection:()=>{stash();selected=null;selectedEntry='';selectedDirectory=false;mount.replaceChildren();}});
  async function createFile(directory){return controller.create(directory);}
  async function rename(){return controller.rename();}async function remove(){return controller.remove();}async function quote(){return controller.quote();}async function terminalHere(){return controller.terminalHere();}
  function showConflict(doc){controller.showConflict(doc,save);}
  query.oninput=()=>void refresh();node.addEventListener('clat:tool-enter',async()=>{active=true;if(!recovered){recovered=await recover();}void refresh();});node.addEventListener('clat:tool-leave',()=>{active=false;});
  CLATI18n.onChange(()=>view?.setPhrases(devEditorPhrases()));
  window.addEventListener('focus',()=>{if(active)void recheck();});
  document.addEventListener('keydown',event=>{if(!active||!context.isActive()||!(event.metaKey||event.ctrlKey)||event.key.toLowerCase()!=='s')return;event.preventDefault();event.stopImmediatePropagation();if(event.shiftKey)void saveAll();else void save(selected);},{capture:true});
  setInterval(()=>{if(active&&context.isActive())void recheck(true);},2500);return {element:node,open:()=>openWorkbenchTool(node),controller};
}
