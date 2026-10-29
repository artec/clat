function installDevelopmentFileActions(editor,ui){
  const {node,docs}=editor;let conflictBar=null,diff=null;
  const parent=()=>{const path=ui.selectedPath();return ui.selectedDirectory()?path:path?path.split('/').slice(0,-1).join('/'):'';};
  const fail=error=>devReport(ui.status,error);
  CLATI18n.onChange(()=>diff?.setPhrases(devEditorPhrases()));
  editor.create=async directory=>{
    const name=window.prompt(devText(directory?'folder_name':'file_name'));if(!name)return;
    const path=[parent(),name].filter(Boolean).join('/');
    try{await rpc('editor.create',{operationId:devId(),path,directory});await editor.refresh();if(!directory)await editor.openFile(path,{fixed:true});}catch(error){fail(error);}
  };
  editor.rename=async()=>{
    const path=ui.selectedPath();if(!path)return;const target=window.prompt(devText('rename_path'),path);if(!target||target===path)return;
    try{const snapshot=await rpc('editor.inspect',{path});await rpc('editor.rename',{operationId:devId(),path,target,version:snapshot.version});
      for(const [key,doc]of [...docs])if(key===path||key.startsWith(path+'/')){docs.delete(key);doc.path=target+key.slice(path.length);docs.set(doc.path,doc);ui.queueDraft({...doc,path:key,dirty:false});ui.queueDraft(doc);}ui.setPath(target);ui.renderTabs();await editor.refresh();
    }catch(error){fail(error);}
  };
  editor.remove=async()=>{
    const path=ui.selectedPath();if(!path)return;
    try{const snapshot=await rpc('editor.inspect',{path});const dirty=[...docs.values()].some(doc=>(doc.path===path||doc.path.startsWith(path+'/'))&&doc.dirty);
      if(await devChoice(devText(dirty?'delete_unsaved':'delete_confirm',{path,count:snapshot.entries.length}),['delete','cancel'])!=='delete')return;
      const result=await rpc('editor.delete',{operationId:devId(),path,version:snapshot.version,confirmed:true});
      for(const [key,doc]of [...docs])if(key===path||key.startsWith(path+'/')){docs.delete(key);ui.queueDraft({...doc,dirty:false});}
      if(editor.selected?.path===path||editor.selected?.path.startsWith(path+'/')){ui.clearSelection();}
      ui.renderTabs();await editor.refresh();
      const restore=devButton('restore',async()=>{try{await rpc('editor.restore',{operationId:devId(),path,backupId:result.backupId});restore.remove();await editor.refresh();}catch(error){fail(error);}});ui.toolbar.append(restore);
    }catch(error){fail(error);}
  };
  editor.quote=async()=>{
    const doc=editor.selected;if(!doc)return;
    if(doc.dirty){
      const selection=editor.selection();const excerpt=selection&&selection.from!==selection.to?doc.text.slice(selection.from,selection.to):doc.text.split('\n').slice(0,200).join('\n');
      if(new TextEncoder().encode(excerpt).length>65536){devStatus(ui.status,'quote_large');return;}
      appendQuote(devText('unsaved_quote',{path:doc.path})+'\n```text\n'+excerpt+'\n```');return;
    }
    const session=state.sessionId;
    try{const preview=await rpc('files.preview',{path:doc.path,start_line:1,end_line:200});const reference=await rpc('files.reference',{path:doc.path,start_line:preview.start_line,end_line:preview.end_line,version:preview.version});
      if(session!==state.sessionId||!context.isActive())return;
      queueFileReference(reference);appendQuote(reference.reference);
    }catch(error){fail(error);}
  };
  function appendQuote(text){dom.prompt.value+=(dom.prompt.value?'\n\n':'')+text+'\n';state.composerGeneration++;saveComposerText();invalidateSuggestion();resizePrompt();closeWorkbenchTool();dom.prompt.focus();}
  editor.terminalHere=async()=>{await developmentTerminal?.openDirectory(parent());};
  editor.showConflict=(doc,save)=>{
    conflictBar?.remove();conflictBar=el('div','dev-conflict');conflictBar.append(el('span','',()=>devText('external_change')));
    const compare=devButton('compare',()=>{
      ui.stash();const mount=node.querySelector('.dev-editor-mount');mount.replaceChildren();diff?.destroy();diff=globalThis.CLATDevelopmentWidgets.diff(mount,{left:doc.disk?.text||'',right:doc.text,path:doc.path,phrases:devEditorPhrases()});
      conflictBar.append(devButton('back_editor',()=>{diff?.destroy();diff=null;ui.activate(doc);}));
    });
    const reload=devButton('reload',async()=>{if(await devChoice(devText('reload_confirm'),['reload','cancel'])!=='reload')return;
      try{const disk=await rpc('editor.open',{path:doc.path});ui.stash();Object.assign(doc,disk,{baseText:disk.text,readonly:!disk.editable,dirty:false,conflict:false,state:null});ui.queueDraft(doc);conflictBar.remove();ui.activate(doc);}catch(error){fail(error);}
    });
    const overwrite=devButton('overwrite',async()=>{if(await devChoice(devText('overwrite_confirm'),['overwrite','cancel'])==='overwrite'&&await save(doc,true))conflictBar.remove();});
    conflictBar.append(compare,reload,overwrite);ui.toolbar.after(conflictBar);
  };
}
