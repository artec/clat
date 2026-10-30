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
  const view=createNavigationPage(page,{title:()=>shellText('resources'),searchLabel:()=>pageText('search_resources')});
  const seats=new Map();let tab='files';
  const fileSeat={list:el('nav'),body:el('section'),status:el('div'),query:view.query,show:path=>{fileSeat.title=path;view.show(path);}};
  const files=createProjectFileBrowser({id:'resource-files',inline:true,navigationView:fileSeat,onClose:quoted=>{if(quoted){shell.chat();dom.prompt.focus();}}});
  seats.set('files',fileSeat);
  const tabs=navigationTabs(['files','materials','bookmarks'].map(key=>[key,()=>pageText(key)]),select);view.filters.append(tabs.element);
  const introduction=key=>navigationOverview({title:()=>pageText(key),description:()=>pageText('resource_'+key+'_intro'),icon:key==='bookmarks'?'bookmark':'folder',items:['files','materials','bookmarks'].filter(item=>item!==key).map(item=>({title:()=>pageText(item),description:()=>pageText('resource_'+item+'_intro'),icon:item==='bookmarks'?'bookmark':item==='files'?'folder':'file',action:()=>select(item)}))});
  fileSeat.body.append(introduction('files'));
  const status=el('div','navigation-status');view.sidebar.insertBefore(status,view.list);
  const refresh=()=>{if(tab==='files')void files.refresh();else{seats.get(tab)?.dispose?.();seats.delete(tab);select(tab);}};
  view.actions([viewAction('refresh','refresh',()=>pageText('refresh'),refresh)]);
  view.context({crumbs:()=>[{label:()=>shellText('resources'),run:()=>{delete page.dataset.detail;}},{label:()=>projectDisplayName(state.workbench?.project?.root,state.workbench?.project?.name),run:()=>openSettings('projects')},{label:()=>pageText(tab)},...(seats.get(tab)?.title?[{label:()=>seats.get(tab).title}]:[])]});
  function select(key){
    const prior=seats.get(tab);if(prior){prior.scroll=[view.list.scrollTop,view.body.scrollTop];prior.query=view.query.value;}
    files.suspend();tab=key;tabs.select(key);
    let seat=seats.get(key);if(!seat){seat={list:el('nav'),body:el('section'),status:el('div'),query:'',scroll:[0,0],filter:()=>{},overview:()=>introduction(key)};seat.body.append(seat.overview());seats.set(key,seat);}
    view.query.value=typeof seat.query==='string'?seat.query:'';view.list.replaceChildren(seat.list);view.body.replaceChildren(seat.body);status.replaceChildren(seat.status);
    CLATI18n.bind(view.name,'textContent',()=>seat.title||pageText(key));if(!seat.title)delete page.dataset.detail;view.list.scrollTop=seat.scroll?.[0]||0;view.body.scrollTop=seat.scroll?.[1]||0;
    refreshViewContext();if(key==='files'){fileSeat.query=view.query;files.open();}else if(!seat.loaded)void loadResourceMetadata(view,seat,key,()=>pageIsOpen(page)&&tab===key&&seats.get(key)===seat&&context.isActive());else seat.resume?.();
  }
  view.query.addEventListener('input',()=>{if(tab!=='files')seats.get(tab)?.filter?.();});
  page.addEventListener('clat:page-leave',()=>{files.suspend();});
  const trigger=shellButton('resources-open','folder',()=>shellText('resources'),()=>{presentPage(page);select(tab);});trigger.hidden=true;document.body.append(trigger,page);
  window.addEventListener('clat:workbench-updated',()=>void reconcileFileReferences());
}
async function loadResourceMetadata(view,seat,key,current){
  seat.list.replaceChildren(pageEmpty('loading'));let selection=0,reader=null;
  try{
    const [library,sessions]=await Promise.all([rpc('library.list',{}),rpc('session.list',{})]);if(!current())return;seat.loaded=true;seat.dispose=()=>{selection++;reader?.dispose();seat.disposeAttachment?.();};seat.resume=()=>{if(reader&&!seat.body.querySelector('.reader-messages')?.children.length)void reader.refresh();};
    const entries=library.entries.filter(e=>e.kind===(key==='bookmarks'?'bookmark':'snapshot'));seat.list.replaceChildren();
    const listing=el('nav');seat.list.append(listing);
    const open=entry=>{const version=++selection;reader?.dispose();seat.selected=JSON.stringify(entry.reference);seat.body.replaceChildren();seat.title=entry.path||sessions.sessions.find(s=>s.id===entry.reference.session)?.title||entry.reference.session;view.show(seat.title);seat.filter();reader=createConversationReader(seat.body,entry.reference,{current:()=>current()&&version===selection});
      if(key==='bookmarks')seat.body.append(pageAction('remove_bookmark',async()=>{try{await rpc('library.bookmark.remove',{reference:entry.reference});const i=entries.indexOf(entry);if(i>=0)entries.splice(i,1);if(current()&&seat.selected===JSON.stringify(entry.reference)){reader?.dispose();seat.selected=null;seat.title=null;view.select(null);seat.body.replaceChildren(seat.overview());CLATI18n.bind(view.name,'textContent',()=>pageText('bookmarks'));delete view.page.dataset.detail;}if(current())seat.filter();}catch(error){seat.status.textContent=error.message;}}));};
    seat.filter=()=>{const query=view.query.value.trim().toLocaleLowerCase();listing.replaceChildren();for(const entry of entries){const title=entry.path||sessions.sessions.find(s=>s.id===entry.reference.session)?.title||entry.reference.session;if(query&&![title,entry.reference.session].join(' ').toLocaleLowerCase().includes(query))continue;listing.append(navigationRow({title,meta:entry.reference.session+' · #'+entry.reference.seq,icon:key==='bookmarks'?'bookmark':'file',selected:seat.selected===JSON.stringify(entry.reference),action:()=>open(entry)}));}};
    seat.filter();if(key==='materials')installResourceAttachments(view,seat,sessions.sessions,current);
  }catch(error){if(current())seat.list.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void loadResourceMetadata(view,seat,key,current)));}
}
function installResourceAttachments(view,seat,sessions,current){
  const picker=el('select');CLATI18n.attribute(picker,'aria-label',()=>pageText('session'));for(const session of sessions){const option=el('option','',session.title||session.id);option.value=session.id;picker.append(option);}picker.value=state.sessionId||picker.options[0]?.value||'';seat.list.prepend(picker,el('p','settings-note',()=>pageText('material_scope')));
  const attachments=el('nav');seat.list.append(attachments);const referenceFilter=seat.filter;let generation=0,rows=[],finished=false;const resumeReader=seat.resume;seat.resume=()=>{resumeReader?.();if(!finished)void load();};
  seat.filter=()=>{referenceFilter();attachments.replaceChildren();const query=view.query.value.trim().toLocaleLowerCase();for(const {message,attachment}of rows){const title=attachment.display_name||attachment.attachment_id;if(query&&!title.toLocaleLowerCase().includes(query))continue;attachments.append(navigationRow({title,icon:'file',selected:seat.attachment===attachment.attachment_id,action:()=>{seat.disposeAttachment?.();seat.title=title;seat.attachment=attachment.attachment_id;view.show(title);seat.filter();seat.body.replaceChildren();const preview=el('section');seat.body.append(pageAction('source_message',()=>{seat.disposeAttachment?.();seat.body.replaceChildren();createConversationReader(seat.body,message.reference,{current});}),preview);seat.disposeAttachment=renderLibraryAttachment(preview,message.reference,attachment,()=>current()&&seat.attachment===attachment.attachment_id);}}));}};
  async function load(){const request=++generation;rows=[];attachments.replaceChildren(pageEmpty('loading'));try{let after=null;do{if(!picker.value)break;const value=await rpc('history.conversation',{reference:{project:state.workbench.project.root,session:picker.value,seq:null},attachments_only:true,after_seq:after});if(!current()||request!==generation)return;for(const message of value.messages)for(const attachment of message.attachments)rows.push({message,attachment});after=value.next_seq;}while(after!==null);seat.filter();finished=true;if(!rows.length)attachments.append(pageEmpty('empty_materials'));}catch(error){if(current()&&request===generation)attachments.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void load()));}}
  picker.onchange=()=>{seat.disposeAttachment?.();seat.attachment=null;seat.title=null;view.select(null);finished=false;generation++;seat.body.replaceChildren(seat.overview());CLATI18n.bind(view.name,'textContent',()=>pageText('materials'));void load();};void load();
}

function decorateConversationBookmarks() {
  if(!state.sessionId||!state.workbench?.project)return;
  for(const node of dom.transcript.querySelectorAll('.msg[data-seq]')){
    if(node.querySelector('[data-action=bookmark]'))continue;
    const reference={project:state.workbench.project.root,session:state.sessionId,seq:Number(node.dataset.seq)};
    const button=pageAction('bookmark',async()=>{
      try{await rpc('library.bookmark.add',{reference});button.setAttribute('aria-pressed','true');CLATI18n.bind(button,'textContent',()=>pageText('bookmarked'));}
      catch(error){updateRunState(error.message);}
    });button.classList.add('message-bookmark');button.setAttribute('aria-pressed','false');
    let actions=node.querySelector('.message-actions');
    if(!actions){actions=el('div','message-actions');node.append(actions);}actions.insertBefore(button,actions.querySelector('.message-time'));
  }
}

function renderLibraryAttachment(host,reference,attachment,current){
  let url=null,active=true;host.append(pageEmpty('loading'));
  async function load(){try{const value=await rpc('history.attachment',{reference,attachment_id:attachment.attachment_id,trusted_projects:false});if(!active||!current())return;const bytes=Uint8Array.from(atob(value.base64),c=>c.charCodeAt(0));url=URL.createObjectURL(new Blob([bytes],{type:value.media_type}));const image=el('img','navigation-attachment');image.src=url;image.alt=attachment.display_name||attachment.attachment_id;host.replaceChildren(image);}catch(error){if(active&&current())host.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void load()));}}
  void load();return()=>{active=false;if(url)URL.revokeObjectURL(url);};
}
