/* Project document tabs keep their own CodeMirror states and undo histories. */
function createDevelopmentEditor(){
  const docs=new Map();let selected=null,view=null,active=false,openEpoch=0;
  const ui=createEditorLayout({create:directory=>createFile(directory),refresh:()=>refresh(),collapse:()=>tree.collapse(),recover:()=>drafts.recover(true),save:()=>save(selected),saveAll,canSave:()=>Boolean(selected?.dirty&&!selected.readonly&&!selected.saving),canSaveAll:()=>[...docs.values()].some(doc=>doc.dirty&&!doc.readonly&&!doc.saving),canFind:()=>Boolean(view),find:()=>view?.search(),menu:()=>fileMenu(tree.directory())});
  const {node,status,list,toolbar,mount,tabs,breadcrumb,footer}=ui;
  const controller={node,docs,get selected(){return selected;},openFile,save,saveAll,refresh,createFile,selection:()=>view?view.view.state.selection.main:null,flush:()=>drafts.flush()};
  const drafts=createDevelopmentDrafts({docs,status,activate});const queueDraft=drafts.queue,recover=drafts.recover;
  const tree=installEditorTree(ui,{active:()=>active,recheck,openFile,menu:(entry,source,x,y)=>{tree.select(entry.path,Boolean(entry.directory));editorMenu(fileMenu(Boolean(entry.directory)),source,x,y);}});
  function fileMenu(directory){return [
    {key:'new_file',icon:'file-plus',run:()=>createFile(false)},
    {key:'new_folder',icon:'folder-plus',run:()=>createFile(true)},
    {key:'rename',icon:'edit',disabled:!tree.path(),run:rename},
    {key:'delete',icon:'trash',disabled:!tree.path(),run:remove},
    {key:'quote',icon:'chat',disabled:directory||!tree.path(),run:async()=>{if(tree.path()!==selected?.path)await openFile(tree.path(),{fixed:true});if(selected?.path===tree.path())await quote();}},
    {key:'terminal_here',icon:'terminal',run:terminalHere},
    {key:'copy_path',icon:'copy',disabled:!tree.path(),run:()=>navigator.clipboard.writeText(tree.path())},
  ];}
  function stash(){if(selected&&view){selected.state=view.state();selected.top=view.view.scrollDOM.scrollTop;selected.left=view.view.scrollDOM.scrollLeft;}view?.destroy();view=null;}
  function renderTabs(){
    refreshViewContext();
    renderEditorTabs({tabs,docs,selected,activate,closeDoc,render:renderTabs});editorBreadcrumb(breadcrumb,selected,tree.reveal);
    if(!selected){footer.replaceChildren();editorEmpty(mount);}
  }
  function updateStatus(){
    if(!selected||!view)return;
    selected.indentTab=view.indent()==='\t';editorStatus(footer,selected,view,mode=>{selected.mode=mode;view.setLanguage(mode||globalThis.CLATDevelopmentWidgets.formatFor(selected.path,selected.text).id);});refreshViewContext();
  }
  function updatePosition(){if(!selected||!view||!footer.firstChild)return;const selection=view.view.state.selection.main,line=view.view.state.doc.lineAt(selection.head);CLATI18n.bind(footer.firstChild,'textContent',()=>devText('cursor_position',{line:line.number,column:selection.head-line.from+1}));}
  function activate(doc){
    tree.select(doc.path);if(selected===doc&&view){renderTabs();return;}controller.clearConflict?.();stash();selected=doc;mount.replaceChildren();node.dataset.detail='true';renderTabs();
    status.classList.remove('error');if(doc.readonly&&doc.reason)devStatus(status,doc.reason);else status.textContent='';
    view=globalThis.CLATDevelopmentWidgets.editor(mount,{text:doc.text,path:doc.path,readonly:doc.readonly,mode:doc.mode,state:doc.state,phrases:devEditorPhrases(),onChange:text=>{doc.text=text;doc.dirty=text!==doc.baseText;doc.pinned=true;renderTabs();queueDraft(doc);},onSelection:updatePosition,onSave:()=>void save(doc)});
    updateStatus();view.view.scrollDOM.scrollTop=doc.top||0;view.view.scrollDOM.scrollLeft=doc.left||0;
    if(doc.conflict)showConflict(doc);
  }
  async function openFile(path,{fixed=false}={}){
    const request=++openEpoch;
    if(docs.has(path)){const doc=docs.get(path);doc.pinned||=fixed;activate(doc);return;}
    try{const result=await rpc('editor.open',{path});if(request!==openEpoch)return;
      for(const [key,doc]of docs){if(!doc.pinned&&!doc.dirty){if(doc===selected)stash();docs.delete(key);}}
      const doc={...result,text:result.text,baseText:result.text,dirty:false,pinned:fixed,readonly:!result.editable};docs.set(path,doc);activate(doc);
    }catch(error){devReport(status,error);}
  }
  async function refresh(){return tree.refresh();}
  async function recheck(currentOnly=false){
    for(const doc of currentOnly?(selected?[selected]:[]):docs.values()){
      if(doc.saving)continue;const version=doc.version;
      let disk;try{disk=await rpc('editor.open',{path:doc.path});}catch(error){if(docs.get(doc.path)!==doc||doc.version!==version||doc.saving)continue;doc.conflict=true;if(error.technical==='file_missing')doc.disk={version:'missing',text:''};if(doc===selected)showConflict(doc);continue;}
      if(docs.get(doc.path)!==doc||doc.version!==version||doc.saving)continue;
      if(disk.version===doc.version)continue;
      if(doc.dirty){doc.conflict=true;doc.disk=disk;if(doc===selected)showConflict(doc);}
      else{Object.assign(doc,disk,{baseText:disk.text,readonly:!disk.editable,conflict:false,state:null});if(doc===selected){stash();doc.state=null;selected=null;activate(doc);}}
    }
  }
  async function save(doc,overwrite=false){
    if(!doc||doc.readonly||!doc.dirty)return true;
    if(doc.saving)return false;doc.saving=true;refreshViewContext();
    try{
      if(doc.conflict&&!overwrite){showConflict(doc);return false;}
      const version=overwrite?(doc.disk?.version==='missing'?'missing':(await rpc('editor.open',{path:doc.path})).version):doc.version;
      const text=doc.text;const result=await rpc('editor.save',{operationId:devId(),path:doc.path,version,text,bom:doc.bom,newline:doc.newline});
      doc.version=result.version;doc.baseText=text;doc.dirty=doc.text!==text;doc.conflict=false;if(doc===selected){controller.clearConflict?.();if(!view){selected=null;activate(doc);}}renderTabs();queueDraft(doc);devStatus(status,'saved');return true;
    }catch(error){devReport(status,error);doc.conflict=true;if(doc===selected)showConflict(doc);return false;}finally{doc.saving=false;refreshViewContext();}
  }
  async function saveAll(){for(const doc of docs.values())if(!await save(doc))return false;return true;}
  async function closeDoc(doc){
    if(doc.dirty){const answer=await devChoice(devText('unsaved_file',{path:doc.path}),['save','discard','cancel']);if(answer==='cancel'||answer==='save'&&(!await save(doc)||doc.dirty))return;}
    if(doc===selected){controller.clearConflict?.();stash();selected=null;breadcrumb.replaceChildren();mount.replaceChildren();}docs.delete(doc.path);queueDraft({...doc,dirty:false});renderTabs();if(!selected&&docs.size)activate([...docs.values()].at(-1));
  }
  installDevelopmentFileActions(controller,{status,list,toolbar,activate,stash,renderTabs,queueDraft,selectedPath:()=>tree.path()||selected?.path,selectedDirectory:tree.directory,setPath:(path,directory=tree.directory())=>{tree.select(path,directory);},reveal:tree.reveal,refreshDocument:()=>{if(selected){const doc=selected;stash();selected=null;activate(doc);}},clearSelection:()=>{controller.clearConflict?.();stash();selected=null;tree.select('');mount.replaceChildren();}});
  async function createFile(directory){return controller.create(directory);}
  async function rename(){return controller.rename();}async function remove(){return controller.remove();}async function quote(){return controller.quote();}async function terminalHere(){return controller.terminalHere();}
  function showConflict(doc){controller.showConflict(doc,save);}
  editorEmpty(mount);
  installEditorLifecycle({node,tabs,docs,activate,selected:()=>selected,view:()=>view,refresh,recheck,recover,updateStatus,save,saveAll,enter:()=>{active=true;},leave:()=>{active=false;openEpoch++;tree.suspend();},active:()=>active});
  return {element:node,open:()=>openWorkbenchTool(node),controller};
}
