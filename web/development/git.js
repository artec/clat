/* Git intent UI. Core owns snapshots, generation, mutation and retry receipts. */
function installDevelopmentGit(){
  const node=el('section','workspace-review dev-git');node.id='workspace-review';
  const header=el('div','review-header'),note=el('p','picker-hint',()=>devText('git_commit_scope')),status=el('p','picker-hint'),list=el('nav','review-files'),detail=el('section','review-patch dev-git-detail');
  status.setAttribute('role','status');let snapshot=null,selected=null,diff=null,active=false,epoch=0,operation=null,language='system',page=0,mode='changes';
  const commitForm=el('div','dev-commit-form'),message=el('textarea',''),locale=devLanguageSelect('system');CLATI18n.attribute(message,'placeholder',()=>devText('commit_placeholder'));
  const pendingKey='clat.git.pending.'+context.route;let remembered=null;try{remembered=JSON.parse(localStorage.getItem(pendingKey)||'null');}catch(_){}
  let draftEpoch=0;message.oninput=()=>{draftEpoch++;if(operation&&remembered?.automatic)void rpc('git.cancel',{operationId:operation});};
  locale.onchange=()=>{language=locale.value;draftEpoch++;if(operation&&remembered?.automatic)void rpc('git.cancel',{operationId:operation});void rpc('git.settings.set',{language}).catch(error=>devReport(status,error));};
  const commit=devButton('commit',()=>void start('commit')),generate=devButton('generate_message',()=>void start('generate')),cancel=devButton('cancel',()=>operation&&void rpc('git.cancel',{operationId:operation}));cancel.hidden=true;
  commitForm.append(message,locale,generate,commit,cancel);list.append(commitForm);
  const result=el('details','dev-git-result');result.hidden=true;const resultBody=el('pre','');result.append(el('summary','',()=>devText('operation_result')),resultBody);commitForm.append(result);
  function showResult(view){const text=[view.message,view.result?.output,view.error].filter(Boolean).join('\n\n');result.hidden=!text;resultBody.textContent=text;}
  header.append(el('h2','',()=>devText('git')),devButton('refresh',()=>void refresh()),devButton('fetch',()=>void start('fetch')),devButton('pull',()=>void start('pull')),devButton('push',()=>void start('push')));
  header.append(devButton('restore',async()=>{try{const data=await rpc('git.backups',{});clearDetail();for(const backup of data.backups){const button=el('button','review-file',()=>backup.paths.join(', '));button.type='button';button.onclick=()=>void start('restore',{backupId:backup.id,confirmed:true});detail.append(button);}if(!data.backups.length)detail.append(el('p','picker-hint',()=>devText('no_backups')));node.dataset.detail='true';}catch(error){devReport(status,error);}}));
  const navigation=el('div','dev-actions');for(const name of ['changes','branches','history'])navigation.append(devButton(name,()=>{mode=name;page=0;void refresh();}));
  node.append(header,note,status,list,detail);document.body.append(node);
  const api={node,refresh,start,get snapshot(){return snapshot;},get busy(){return Boolean(operation);},open:()=>openWorkbenchTool(node),openFile};
  function clearDetail(){diff?.destroy();diff=null;detail.replaceChildren();}
  async function refresh(){
    const request=++epoch;try{const [next,settings]=await Promise.all([rpc('git.snapshot',{}),rpc('git.settings',{})]);if(request!==epoch)return;snapshot=next;language=settings.language;locale.value=language;CLATI18n.bind(note,'textContent',()=>[snapshot.branch||devText('detached'),devText('git_commit_scope')].join(' · '));
      list.replaceChildren(commitForm,navigation);commit.disabled=!snapshot.files.some(file=>file.staged)||Boolean(operation);
      if(mode==='changes')renderFiles();else if(mode==='branches')await renderBranches(request);else await renderHistory(request);
    }catch(error){devReport(status,error);}
  }
  function renderFiles(){
    for(const staged of [false,true]){
      const rows=snapshot.files.filter(file=>staged?file.staged:file.unstaged||file.untracked);if(!rows.length)continue;
      const heading=el('div','dev-group-heading');heading.append(el('h3','',()=>devText(staged?'staged':'changes')),devButton(staged?'unstage_all':'stage_all',()=>void start(staged?'unstage':'stage',{paths:rows.map(file=>file.path)})));list.append(heading);
      const tracked=rows.filter(file=>!file.untracked);if(!staged&&tracked.length)heading.append(devButton('discard_all',()=>void start('discard',{paths:tracked.map(file=>file.path),confirmed:true})));
      for(const file of rows){
        const row=el('div','dev-git-file'),button=el('button','review-file',()=>file.path);button.type='button';button.onclick=()=>void showFile(file,staged);button.append(el('small','',()=>devGitFileStatus(file)));
        const action=devButton(staged?'unstage':'stage',()=>void start(staged?'unstage':'stage',{paths:[file.path]}));row.append(button,action);list.append(row);
      }
    }
    if(!snapshot.files.length)list.append(el('p','picker-hint',()=>devText('clean')));
    if(selected){const file=snapshot.files.find(file=>file.path===selected.path);if(file)void showFile(file,selected.staged);else{selected=null;clearDetail();}}
  }
  async function showFile(file,staged){
    selected={path:file.path,staged};node.dataset.detail='true';const request=++epoch;clearDetail();
    try{const value=await rpc('git.diff',{path:file.path,staged});if(request!==epoch)return;
      const toolbar=el('div','dev-actions');toolbar.append(el('strong','',file.path),el('small','',()=>devText(value.leftLabel)+' → '+devText(value.rightLabel)),devButton('open_file',()=>void openFile(file.path)));
      if(!staged)toolbar.append(devButton(file.untracked?'delete':'discard',()=>void start(file.untracked?'delete_untracked':'discard',{paths:[file.path],confirmed:true})));
      detail.append(toolbar);if(value.binary){detail.append(el('p','picker-hint',()=>devText('binary_diff')));return;}
      const mount=el('div','dev-diff-mount');detail.append(mount);diff=globalThis.CLATDevelopmentWidgets.diff(mount,{...value,phrases:devEditorPhrases()});
    }catch(error){devReport(status,error);}
  }
  async function openFile(path){document.getElementById('files-open').click();await developmentEditor.openFile(path,{fixed:true});}
  async function renderBranches(request){
    const result=await rpc('git.branches',{});if(request!==epoch)return;list.append(devButton('new_branch',()=>{const branch=window.prompt(devText('branch_name'));if(branch)void start('branch.create',{branch});}));
    for(const row of result.branches){const wrap=el('div','dev-git-file'),button=el('button','review-file',()=>row.name+(row.current?' ✓':''));button.type='button';button.onclick=()=>void start('branch.switch',{branch:row.name});wrap.append(button);if(!row.current&&!row.remote)wrap.append(devButton('delete',()=>void start('branch.delete',{branch:row.name,confirmed:true})));list.append(wrap);}
  }
  async function renderHistory(request){
    const result=await rpc('git.history',{skip:page*50});if(request!==epoch)return;for(const row of result.commits){const button=el('button','review-file');button.type='button';button.append(el('strong','',row.subject),el('small','',row.id.slice(0,8)+' · '+row.author));button.onclick=async()=>{const selected=++epoch;try{const value=await rpc('git.commit.detail',{commit:row.id});if(selected!==epoch)return;clearDetail();detail.append(el('pre','review-diff',value.text));node.dataset.detail='true';}catch(error){devReport(status,error);}};list.append(button);}
    const pages=el('div','dev-actions');if(page>0)pages.append(devButton('previous',()=>{page--;void refresh();}));if(result.hasMore)pages.append(devButton('next',()=>{page++;void refresh();}));list.append(pages);
  }
  async function start(action,extra={}){
    if(operation||remembered){await reconcile();return;}
    if(['discard','delete_untracked','branch.delete'].includes(action)&&!window.confirm(devText('git_destructive')))return;
    if(['branch.switch','pull'].includes(action)&&developmentEditor?.docs&&[...developmentEditor.docs.values()].some(doc=>doc.dirty)){devStatus(status,'save_before_switch');return;}
    if(action==='delete_untracked'){try{const path=extra.paths[0],stat=await rpc('editor.inspect',{path});await rpc('editor.delete',{operationId:devId(),path,version:stat.version,confirmed:true});await refresh();}catch(error){devReport(status,error);}return;}
    const id=devId(),captured=draftEpoch,request={operationId:id,action,expected:snapshot,message:message.value,language,systemLanguage:navigator.language,...extra};operation=id;cancel.hidden=false;remembered={operationId:id,action,automatic:action==='generate'||action==='commit'&&!message.value.trim()};localStorage.setItem(pendingKey,JSON.stringify(remembered));
    let uncertain=false;
    try{const view=await devOperation(request,progress=>{devStatus(status,'phase_'+progress.phase);});uncertain=Boolean(view.localUnknown);showResult(view);
      if(view.message&&captured===draftEpoch){message.value=view.message;}
      if(view.phase==='completed'){if(action==='commit'&&captured===draftEpoch){message.value='';draftEpoch++;}}
      else{CLATI18n.bind(status,'textContent',()=>CLATI18n.present(view.presentation,devText(view.phase==='unknown'?'operation_unknown':view.phase==='cancelled'?'cancelled':'failed')));if(view.error){const details=el('details','dev-technical');details.append(el('summary','',()=>devText('technical_details')),el('pre','',view.error));detail.append(details);}}
    }catch(error){devReport(status,error);}finally{if(!uncertain){operation=null;remembered=null;localStorage.removeItem(pendingKey);}cancel.hidden=!operation;await refresh();}
  }
  async function reconcile(){
    if(!remembered)return;operation=remembered.operationId;cancel.hidden=false;
    const view=await devOperation(remembered,progress=>devStatus(status,'phase_'+progress.phase),true);
    showResult(view);
    if(view.message&&!message.value)message.value=view.message;
    if(!view.localUnknown){operation=null;remembered=null;localStorage.removeItem(pendingKey);cancel.hidden=true;}
    devStatus(status,view.phase==='unknown'?'operation_unknown':'phase_'+view.phase);
    if(view.localUnknown&&!detail.querySelector('[data-reconcile]')){const button=devButton('reconcile',()=>void reconcile());button.dataset.reconcile='true';detail.append(button);}
    await refresh();
  }
  node.addEventListener('clat:tool-enter',()=>{active=true;void refresh();if(remembered)void reconcile();});node.addEventListener('clat:tool-leave',()=>{active=false;});window.addEventListener('focus',()=>{if(active&&!operation)void refresh();});
  CLATI18n.onChange(()=>diff?.setPhrases(devEditorPhrases()));
  void rpc('git.settings',{}).then(value=>{language=value.language;locale.value=language;}).catch(()=>{});return api;
}
function devLanguageSelect(value){const select=el('select','');for(const language of ['system','en','zh-Hans','zh-Hant']){const option=el('option','',()=>devText('language_'+language));option.value=language;select.append(option);}select.value=value;return select;}

function devGitFileStatus(file){const status=file.status||'';const kind=file.untracked?'untracked':status.includes('U')||['AA','DD'].includes(status)?'conflicted':status.includes('D')?'deleted':status.includes('R')?'renamed':status.includes('A')?'added':'modified';return devText('git_'+kind);}
