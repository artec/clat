/* Capture belongs to a whole turn, never an individual shell/MCP/tool call. */
function installOperationCapture(host,turn,{current:owned,owner}){
  const section=el('details','operation-capture'),body=el('section');section.append(el('summary','',()=>CLATI18n.msg('view.turn_capture',{turn})),body);host.append(section);const current=()=>owned()&&host.contains(section);let loaded=false,selectedPath=null,epoch=0;section.addEventListener('clat:capture-refresh',()=>{if(section.open)void load(selectedPath);});
  section.ontoggle=()=>{if(section.open&&!loaded){loaded=true;void load();}};
  async function load(path=null){
    selectedPath=path;const ticket=++epoch,owns=()=>current()&&ticket===epoch;
    body.replaceChildren(pageEmpty('loading'));
    try{const value=await rpc('turn.changes',{turn,...(path?{path}:{}),expected_selection_generation:owner[1]});if(!owns())return;
      body.replaceChildren(el('p','picker-hint',()=>CLATI18n.present(value.presentation,value.note)));
      if(!value.files.length){body.append(el('p','',()=>viewText('no_capture')));return;}
      if(value.preview)renderOperationCapturePreview(body,value.preview);
      for(const file of value.files)body.append(navigationRow({title:file.path,meta:[file.status,file.disk_state].join(' · '),icon:'file',action:()=>void load(file.path)}));
      const restore=el('button','danger-button',()=>CLATI18n.msg('web.restore_captured_files'));restore.type='button';restore.disabled=state.runActive||state.compactionActive||!value.files.some(file=>file.disk_state==='safe');restore.onclick=()=>void recover(value,restore);body.append(restore);
    }catch(error){if(owns())body.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void load(path)));}
  }
  async function recover(value,button){
    if(!current()||!window.confirm(CLATI18n.msg('web.restore_only_captured_native_files_whose_current_identity_and_contents_still_match')))return;button.disabled=true;
    try{const result=await rpc('turn.restore',{turn,revision:value.revision,confirmed:true,expected_selection_generation:owner[1]});if(!current())return;body.replaceChildren(el('h3','',()=>CLATI18n.msg('web.recovery_results')));for(const file of result.files)body.append(el('p','',()=>[file.path,file.status,file.note,file.recovery_path].filter(Boolean).join(' · ')));}
    catch(error){if(current()){body.append(el('p','error',error.message));button.disabled=false;}}
  }
}

function renderOperationCapturePreview(body,preview){
  const after=preview.status==='prepared'?'web.expected_native_result_commit_unconfirmed':'web.captured_native_result_not_current_disk';
  const rows=[['web.pre_operation',preview.before]];
  if(preview.status==='prepared'&&preview.captured_after!==null)rows.push(['web.last_confirmed_native_result_before_unfinished_operation',preview.captured_after]);
  rows.push([after,preview.after]);
  for(const [key,text]of rows)body.append(el('h4','',()=>CLATI18n.msg(key)),el('pre','review-diff',text??CLATI18n.msg('web.not_existed')));
  if(preview.truncated)body.append(el('p','picker-hint',()=>CLATI18n.msg('web.preview_capped_at_128_kib_per_side')));
}
