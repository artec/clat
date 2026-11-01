/* Repository-bound intents retain existing core idempotency and outcome reconciliation. */
function createGitActions({seat,call,refresh,render,report,result}){
  async function start(action,extra={}){
    const owner=seat();if(owner.preparing&&['commit','generate'].includes(action))return;
    if(owner.operation||owner.pending){await reconcile(owner);return;}
    if(['commit','generate'].includes(action)){
      if(!await prepareSubmission(owner,action)||seat()!==owner||owner.operation||owner.pending)return;
    }
    if(['discard','delete_untracked','branch.delete'].includes(action)&&!window.confirm(devText('git_destructive')))return;
    if(['branch.switch','pull'].includes(action)&&developmentEditor?.docs&&[...developmentEditor.docs.values()].some(doc=>doc.dirty)){report({message:devText('save_before_switch')});return;}
    if(action==='delete_untracked'){try{const path=[owner.path,extra.paths[0]].filter(Boolean).join('/'),stat=await rpc('editor.inspect',{path});await rpc('editor.delete',{operationId:devId(),path,version:stat.version,confirmed:true});await refresh();}catch(error){report(error);}return;}
    const request={operationId:devId(),repositoryId:owner.id,action,expected:owner.snapshot,message:owner.draft,language:owner.language,systemLanguage:navigator.language,...extra};
    owner.operation=request.operationId;owner.pending={operationId:request.operationId,repositoryId:owner.id,action,automatic:action==='generate'||action==='commit'&&!owner.draft.trim()};save(owner);render();const edit=owner.edit;
    let uncertain=true;
    try{const view=await devOperation(request,progress=>{owner.phase=progress.phase;if(seat()===owner)render();});uncertain=Boolean(view.localUnknown);finish(owner,view,edit,action);return view;}
    catch(error){if(seat()===owner)report(error);}
    finally{if(!uncertain){owner.operation=null;owner.pending=null;save(owner);}if(seat()===owner){render();await refresh();}}
  }
  async function prepareSubmission(owner,action){
    const files=owner.snapshot?.files||[];
    if(files.some(f=>f.staged))return true;
    if(!files.length){report({message:devText('nothing_staged')});return false;}
    if(files.some(gitConflicted)){report({message:devText('submission_conflicts')});return false;}
    const paths=files.filter(f=>f.unstaged||f.untracked).map(f=>f.path);
    if(!paths.length)return false;
    if(!window.confirm(devText(action==='generate'?'stage_generate_confirm':'stage_commit_confirm')))return false;
    const edit=owner.edit;owner.preparing=true;render();
    try{
      const view=await start('stage',{paths});
      return view?.phase==='completed'&&!view.localUnknown&&seat()===owner&&owner.edit===edit
        &&!owner.operation&&!owner.pending&&Boolean(owner.snapshot?.files.some(f=>f.staged));
    }finally{owner.preparing=false;if(seat()===owner)render();}
  }
  function finish(owner,view,edit,action){
    owner.result=view;owner.phase=view.phase;
    if(view.message&&owner.edit===edit&&!owner.pending?.edited)owner.draft=view.message;
    if(view.phase==='completed'&&action==='commit'&&owner.edit===edit&&!owner.pending?.edited){owner.draft='';owner.edit++;}
    if(seat()===owner){result(view);if(view.phase==='unknown'||view.localUnknown)report({message:devText('operation_unknown')});else if(view.phase==='failed'||view.phase==='cancelled')report({message:CLATI18n.present(view.presentation,devText(view.phase==='cancelled'?'cancelled':'failed'))});}
  }
  async function reconcile(owner=seat()){
    if(!owner.pending||owner.reconciling)return;owner.reconciling=true;owner.operation=owner.pending.operationId;render();
    const edit=owner.edit;try{const view=await devOperation(owner.pending,progress=>{owner.phase=progress.phase;if(seat()===owner)render();},true);finish(owner,view,edit,owner.pending.action);if(!view.localUnknown){owner.operation=null;owner.pending=null;save(owner);}}
    catch(error){if(seat()===owner)report(error);}finally{owner.reconciling=false;if(seat()===owner){render();await refresh();}}
  }
  function save(owner){try{if(owner.pending)localStorage.setItem(owner.pendingKey,JSON.stringify(owner.pending));else localStorage.removeItem(owner.pendingKey);}catch(_){} }
  function cancel(owner=seat()){if(owner.operation)void call('git.cancel',{operationId:owner.operation},owner).catch(report);}
  return {start,reconcile,cancel};
}

// Projection of local repository facts; no implicit fetch, pull or push.
function gitPrimaryAction(snapshot){
  if(!snapshot||snapshot.files?.length)return {action:'commit',key:'commit',enabled:Boolean(snapshot?.files?.length)};
  const tracking=snapshot.tracking;
  if(tracking?.behind>0)return {action:'pull',key:'pull_count',count:tracking.behind,enabled:true};
  if(tracking?.ahead>0)return {action:'push',key:'push_count',count:tracking.ahead,enabled:true};
  return {action:'commit',key:'commit',enabled:false};
}
