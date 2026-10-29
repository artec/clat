/* Human workspace presentation. Every side effect is a core intent. */
function devText(key,args={}) { return CLATI18n.msg('dev.'+key,args); }
function devButton(key,run,kind='ghost') {
  const button=el('button',kind,()=>devText(key));button.type='button';button.onclick=run;return button;
}
function devId(){return crypto.randomUUID();}
const developmentClientSession=(()=>{
  if(globalThis.CLATDevelopmentClientSession)return globalThis.CLATDevelopmentClientSession;
  let id;try{id=sessionStorage.getItem('clat.development.client');}catch(_){}id||=devId();
  let ready;const result={id,ready:new Promise(resolve=>{ready=resolve;})};globalThis.CLATDevelopmentClientSession=result;
  const store=()=>{try{sessionStorage.setItem('clat.development.client',result.id);}catch(_){}};
  if(navigator.locks){
    const hold=id=>{void navigator.locks.request('clat.development.'+id,{ifAvailable:true},async lock=>{
      if(!lock){result.id=devId();store();hold(result.id);return;}
      store();ready();await new Promise(resolve=>globalThis.addEventListener('pagehide',resolve,{once:true}));
    }).catch(()=>{result.id=devId();store();ready();});};hold(id);
  }else{result.id=devId();store();ready();}
  return result;
})();
const developmentClient=developmentClientSession.id;

function devStatus(target,key,args={}){CLATI18n.bind(target,'textContent',()=>devText(key,args));}
function devReport(target,error){CLATI18n.bind(target,'textContent',()=>error?.message||devText('failed'));target.classList.add('error');}
async function devChoice(message,choices){
  const dialog=el('dialog','dev-confirm');const copy=el('p','',message),actions=el('div','dev-actions');dialog.append(copy,actions);document.body.append(dialog);
  return new Promise(resolve=>{
    let settled=false;const finish=value=>{if(settled)return;settled=true;dialog.close();dialog.remove();resolve(value);};
    for(const key of choices)actions.append(devButton(key,()=>finish(key)));
    dialog.addEventListener('cancel',event=>{event.preventDefault();finish('cancel');});dialog.showModal();
  });
}
async function devOperation(request,onProgress=()=>{},resume=false){
  let view;
  if(!resume)view=await devStartOperation(request);
  else try{view=await rpc('git.operation',{operationId:request.operationId,...(request.repositoryId?{repositoryId:request.repositoryId}:{})});}catch(_){return {id:request.operationId,phase:'unknown',localUnknown:true};}
  while(['pending','running','generating','committing'].includes(view.phase)){
    onProgress(view);await new Promise(resolve=>setTimeout(resolve,200));
    try{view=await rpc('git.operation',{operationId:request.operationId,...(request.repositoryId?{repositoryId:request.repositoryId}:{})});}
    catch(_){return {id:request.operationId,phase:'unknown',localUnknown:true};}
  }
  onProgress(view);return view;
}
async function devStartOperation(request){
  for(let attempt=0;attempt<2;attempt++){
    try{return await rpc('git.start',request);}
    catch(error){
      try{return await rpc('git.operation',{operationId:request.operationId,...(request.repositoryId?{repositoryId:request.repositoryId}:{})});}
      catch(outcome){if(outcome.technical==='operation_missing'&&error.technical)throw error;}
    }
  }
  return {id:request.operationId,phase:'unknown',localUnknown:true};
}

let developmentEditor=null,developmentGit=null,developmentTerminal=null;

function devEditorPhrases(){
  const keys={Find:'cm_find',Replace:'cm_replace',next:'cm_next',previous:'cm_previous',all:'cm_all','match case':'cm_match_case','by word':'cm_by_word',regexp:'cm_regexp',replace:'cm_replace','replace all':'cm_replace_all',close:'cm_close','Go to line':'cm_goto',go:'cm_go','current match':'cm_current','on line':'cm_on_line','No matches found':'cm_no_matches','Fold line':'cm_fold','Unfold line':'cm_unfold'};
  return Object.fromEntries(Object.entries(keys).map(([phrase,key])=>[phrase,devText(key)]));
}
