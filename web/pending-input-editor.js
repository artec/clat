function pendingInputEditor(id) {
  return [...pendingInputEditors.values()].find(editor => editor.id === id && composerOwns(editor.owner));
}
function beginPendingInputEdit(item) {
  const key = JSON.stringify([composerScope, item.id]);
  pendingInputEditors.set(key, { key, id:item.id, owner:composerScope, value:item.text, version:item.version, original:item, keep:new Set(item.draft.uploads), added:[], scope:null, busy:false });
  renderPendingInputPopup(); pendingInputPopup.querySelector(`[data-input-editor="${CSS.escape(item.id)}"]`)?.focus();
}
function cancelPendingInputEdit(id) {
  const editor = pendingInputEditor(id);
  if(editor){editor.cancelled=true;editor.controller?.abort();}
  for (const image of editor?.added || []) URL.revokeObjectURL(image.url);
  if (editor) pendingInputEditors.delete(editor.key); renderPendingInputPopup();
}
function buildInputEditor(item, editor) {
  const box = el('div', 'pending-input-editor'), field = el('textarea');
  field.value = editor.value; field.rows = 3; field.dataset.inputEditor = item.id;
  CLATI18n.attribute(field, 'aria-label', () => inputText('edit')); field.oninput = () => { editor.value = field.value; };
  const current = item.phase === 'queued' && item.version === editor.version;
  const images = el('div', 'pending-input-attachments');
  editor.original.draft.uploads.forEach((id,index) => {
    if (!editor.keep.has(id)) return;
    const image = el('img', 'pending-input-image'); image.alt = String(inputText('image'));
    inputPreview(editor.original,index).then(value => { image.src=value.url; }).catch(()=>{});
    const remove = pendingButton('delete_image',()=>{editor.keep.delete(id);renderPendingInputPopup();},'trash-2'); remove.disabled = !current;
    images.append(image,remove);
  });
  editor.added.forEach((image,index)=>{
    const thumb=el('img','pending-input-image');thumb.src=image.url;thumb.alt=image.file.name;
    const remove=pendingButton('delete_image',()=>{URL.revokeObjectURL(image.url);editor.added.splice(index,1);renderPendingInputPopup();},'trash-2');
    remove.disabled=editor.busy || !current;images.append(thumb,remove);
  });
  const picker=el('input');picker.type='file';picker.accept='image/png,image/jpeg';picker.multiple=true;picker.hidden=true;
  picker.onchange=()=>void stagePendingInputFiles(editor,[...picker.files]);
  const add=el('button','text-action',()=>inputText('add_images'));add.type='button';add.disabled=!current || editor.busy;add.onclick=()=>picker.click();
  const actions=el('div','pending-input-actions');
  const save=el('button','text-action',()=>inputText('save'));save.type='button';save.disabled=!current || editor.busy || inputOperations.has(item.id);
  save.onclick=()=>void savePendingInputEdit(item,editor);
  const cancel=el('button','text-action',()=>inputText('cancel_edit'));cancel.type='button';cancel.onclick=()=>cancelPendingInputEdit(item.id);
  actions.append(save,cancel);
  if(!current){
    actions.append(el('small','',()=>inputText('received')));
    const recover=el('button','text-action',()=>inputText('recover_edit'));recover.type='button';
    recover.onclick=()=>{if(state.draft.sending || state.draft.images.length+editor.added.length>MAX_DRAFT_IMAGES || totalDraftBytes()+editor.added.reduce((sum,image)=>sum+image.file.size,0)>MAX_DRAFT_BATCH_BYTES){updateRunState(inputText('merge_limit'));return;}restoreTextSteering('',{owner:editor.owner,text:editor.value});void addDraftFiles(editor.added.map(image=>image.file));cancelPendingInputEdit(item.id);};actions.append(recover);
  }
  box.append(field,images,add,picker,actions);return box;
}
async function stagePendingInputFiles(editor,files) {
  const owner=composerScope;editor.busy=true;editor.controller=new AbortController();renderPendingInputPopup();
  try{
    if(editor.keep.size+editor.added.length+files.length>MAX_DRAFT_IMAGES)throw CLATI18n.error(inputText('merge_limit'));
    editor.scope ||= await rpc('draft.open',{clientDraftId:newOpaqueClientId('input-editor')});
    for(const file of files){
      if(!['image/png','image/jpeg'].includes(file.type)||!file.size||file.size>MAX_DRAFT_IMAGE_BYTES)throw CLATI18n.error(inputText('merge_limit'));
      const dimensions=await imageDimensions(file);if(dimensions.width*dimensions.height>MAX_DRAFT_PIXELS)throw CLATI18n.error(inputText('merge_limit'));
      const response=await fetch(`${workspacePrefix}/api/drafts/${encodeURIComponent(editor.scope.draftScopeId)}/images`,{method:'POST',headers:{...authHeaders(),'Content-Type':file.type,'X-CLAT-Display-Name':uploadDisplayName(file,editor.added.length)},body:file,signal:editor.controller.signal});
      const result=await response.json().catch(()=>null);if(!response.ok||!result?.ok)throw CLATI18n.error(CLATI18n.present(result?.error?.presentation,inputText('failed')));
      if(editor.cancelled)return;editor.added.push({file,uploadId:result.value.uploadId,url:URL.createObjectURL(file)});
    }
  }catch(error){if(composerOwns(owner))updateRunState(error.message);}
  finally{editor.busy=false;if(composerOwns(owner))renderPendingInputPopup();}
}
async function savePendingInputEdit(item,editor) {
  const extra={text:editor.value,keepUploads:[...editor.keep],attachments:editor.added.map(image=>image.uploadId)};
  if(editor.scope)extra.draftScopeId=editor.scope.draftScopeId;
  const result=await performInputAction('update',{...item,version:editor.version},extra);
  if(result)for(const image of editor.added)URL.revokeObjectURL(image.url);
}
