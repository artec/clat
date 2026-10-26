/* The library stores provenance, never copies of file or message bodies. */
const pendingFileReferences = new Map();
function queueFileReference(snapshot) {
  if (!pendingFileReferences.has(composerScope)) pendingFileReferences.set(composerScope, []);
  pendingFileReferences.get(composerScope).push({path:snapshot.path, version:snapshot.version, quote:snapshot.reference});
}
function bindFileReferenceSubmission(owner, text, params) {
  const pending=pendingFileReferences.get(owner);if(!pending?.length)return;
  if(!params.clientMessageId)params.clientMessageId=newOpaqueClientId('reference');
  for(const snapshot of pending){
    const exact=text.includes(snapshot.quote)?snapshot.quote:text.endsWith(snapshot.quote.trimEnd())?snapshot.quote.trimEnd():null;
    if(exact){snapshot.quote=exact;snapshot.clientMessageIds=[...new Set([...(snapshot.clientMessageIds||[]),params.clientMessageId])];}
  }
}
let referenceReconciliation = false;
async function reconcileFileReferences() {
  if (!state.sessionId || referenceReconciliation) return;
  const owner=composerScope, session=state.sessionId;
  for(const [previous,target]of composerOwnerAliases)if(target===owner&&pendingFileReferences.has(previous)){pendingFileReferences.set(owner,[...(pendingFileReferences.get(owner)||[]),...pendingFileReferences.get(previous)]);pendingFileReferences.delete(previous);}
  const pending=pendingFileReferences.get(owner);
  if(!pending?.length)return;referenceReconciliation=true;
  const reference={project:state.workbench.project.root,session,seq:null};
  try{let after=null;do{
    const value=await rpc('history.conversation',{reference,after_seq:after});
    for(const message of value.messages.filter(m=>m.role==='user'))for(const snapshot of [...pending])if(snapshot.clientMessageIds?.includes(message.client_message_id) && message.text.includes(snapshot.quote)){
      await rpc('library.reference.add',{reference:message.reference,snapshot});const index=pending.indexOf(snapshot);if(index>=0)pending.splice(index,1);
    }
    after=value.next_seq;
  }while(after!==null&&pending.length);}catch(_){}finally{referenceReconciliation=false;}
}

function showHistoricalAttachment(reference, attachment, trusted = false) {
  const dialog=el('dialog','image-preview'); const status=el('p','settings-note',()=>pageText('loading'));
  const close=pageAction('cancel',()=>dialog.close());dialog.append(close,status);document.body.append(dialog);dialog.showModal();let url=null;
  dialog.addEventListener('close',()=>{if(url)URL.revokeObjectURL(url);dialog.remove();},{once:true});
  void rpc('history.attachment',{reference,attachment_id:attachment.attachment_id,trusted_projects:trusted}).then(value=>{
    if(!dialog.open)return;const bytes=Uint8Array.from(atob(value.base64),c=>c.charCodeAt(0));url=URL.createObjectURL(new Blob([bytes],{type:value.media_type}));
    const image=el('img');image.src=url;image.alt=attachment.display_name||attachment.attachment_id;status.replaceWith(image);
  }).catch(error=>{if(dialog.open)status.textContent=error.message;});
}
function createResourceLibrary(shell) {
  const page=el('section','resources-page');page.id='resources-dialog';
  pageHeader(page,()=>shellText('resources'),()=>pageText('project_scope'));
  const tabs=el('nav','page-tabs'), content=el('section','resource-content');
  const files=createProjectFileBrowser({id:'resource-files',inline:true,onClose:quoted=>{if(quoted){shell.chat();dom.prompt.focus();}}});
  let tab='files', epoch=0, reader=null;
  const controls=[];
  function select(key){tab=key;epoch++;reader?.dispose();files.suspend();content.replaceChildren();
    controls.forEach(button=>button.setAttribute('aria-selected',String(button.dataset.tab===key)));
    if(key==='files'){content.append(files.element);files.open();}else void loadMetadata(key);
  }
  for(const key of ['files','materials','bookmarks']){const button=pageAction(key,()=>select(key));button.dataset.tab=key;controls.push(button);tabs.append(button);}
  async function loadMetadata(key){
    const request=epoch;content.replaceChildren(pageEmpty('loading'));
    try{const [library,sessions]=await Promise.all([rpc('library.list',{}),rpc('session.list',{})]);if(request!==epoch||!pageIsOpen(page))return;
      let selection=0;const listing=el('div','page-object-list'), detail=el('section','page-detail');const body=el('div','page-list-detail');body.append(listing,detail);content.replaceChildren(body);
      const entries=library.entries.filter(e=>e.kind===(key==='bookmarks'?'bookmark':'snapshot'));
      for(const entry of entries){const button=el('button','page-object');button.type='button';button.append(el('strong','',entry.path||sessions.sessions.find(s=>s.id===entry.reference.session)?.title||entry.reference.session),el('small','',entry.reference.session+' · #'+entry.reference.seq+(entry.version?' · '+entry.version.slice(0,12):'')));
        button.onclick=()=>{selection++;reader?.dispose();detail.replaceChildren();reader=createConversationReader(detail,entry.reference,{current:()=>epoch===request&&pageIsOpen(page)});
          if(key==='bookmarks')detail.append(pageAction('remove_bookmark',async()=>{await rpc('library.bookmark.remove',{reference:entry.reference});select(tab);}));};listing.append(button);}
      if(key==='materials'){
        const picker=el('select');CLATI18n.attribute(picker,'aria-label',()=>pageText('session'));
        for(const session of sessions.sessions){const o=el('option','',session.title||session.id);o.value=session.id;picker.append(o);}picker.value=state.sessionId||picker.options[0]?.value||'';
        listing.prepend(el('p','settings-note',()=>pageText('material_scope')),picker);
        const attachments=async()=>{const selected=++selection;detail.replaceChildren();if(!picker.value){detail.append(pageEmpty('empty_materials'));return;}
          let after=null;do{const value=await rpc('history.conversation',{reference:{project:state.workbench.project.root,session:picker.value,seq:null},attachments_only:true,after_seq:after});if(epoch!==request||selected!==selection)return;
            for(const message of value.messages)for(const attachment of message.attachments){const button=pageAction('view_attachment',()=>showHistoricalAttachment(message.reference,attachment));button.append(el('small','',attachment.display_name||attachment.attachment_id));detail.append(button,pageAction('source_message',()=>{selection++;reader?.dispose();detail.replaceChildren();reader=createConversationReader(detail,message.reference,{current:()=>epoch===request});}));}
            after=value.next_seq;
          }while(after!==null);if(!detail.children.length)detail.append(pageEmpty('empty_materials'));
        };
        picker.onchange=()=>void attachments().catch(error=>detail.replaceChildren(el('p','error',error.message)));void attachments().catch(error=>detail.replaceChildren(el('p','error',error.message)));
      }else if(!entries.length)detail.append(pageEmpty('empty_bookmarks'));
    }catch(error){if(request===epoch)content.replaceChildren(el('p','error',error.message),pageAction('retry',()=>select(tab)));}
  }
  page.append(tabs,content);page.addEventListener('clat:page-leave',()=>{epoch++;reader?.dispose();files.suspend();});
  const trigger=shellButton('resources-open','folder',()=>shellText('resources'),()=>{presentPage(page);select(tab);});trigger.hidden=true;document.body.append(trigger,page);
  window.addEventListener('clat:workbench-updated',()=>void reconcileFileReferences());
}

function decorateConversationBookmarks() {
  if(!state.sessionId||!state.workbench?.project)return;
  for(const node of dom.transcript.querySelectorAll('.msg[data-seq]')){
    if(node.querySelector('[data-action=bookmark]'))continue;
    const reference={project:state.workbench.project.root,session:state.sessionId,seq:Number(node.dataset.seq)};
    const button=pageAction('bookmark',async()=>{
      try{await rpc('library.bookmark.add',{reference});button.setAttribute('aria-pressed','true');CLATI18n.bind(button,'textContent',()=>pageText('bookmarked'));}
      catch(error){updateRunState(error.message);}
    });button.classList.add('message-bookmark');button.setAttribute('aria-pressed','false');node.append(button);
  }
}
