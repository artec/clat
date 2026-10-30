/* One contextual workspace: conversation, or one list/detail tool. */
const WORKSPACE_TOOLS = [
  {id:'file-browser',trigger:'files-open',entry:'context-files-open',key:'files',icon:'folder'},
  {id:'workspace-review',trigger:'review-open',entry:'context-changes-open',key:'changes',icon:'branch'},
  {id:'turn-review',trigger:'turn-review-open',entry:'context-turn-open',key:'turn',icon:'activity-log'},
  {id:'task-review',trigger:'tasks-open',entry:'context-tasks-open',key:'tasks',icon:'task-list'},
  {id:'workflow-details',trigger:'workflow-open',entry:'context-workflow-open',key:'workflow',icon:'plan'},
];
function toolViewIsOpen(node) { return Boolean(workbenchShell?.tools?.isActive(node.id)); }
function openWorkbenchTool(node) { workbenchShell.tools.open(node.id); }
function closeWorkbenchTool() { workbenchShell.tools.chat(); }
function installWorkspaceTools(shell) {
  const seats = new Map(); let selected = null, active = false, owner = state.sessionId, navigating = false;
  const host = el('section', 'workspace-tool-host hidden'); host.id = 'workspace-tool-host';
  let unread=false,pending=0;
  const content = document.getElementById('content-area'); content.append(host);
  const conversation = document.getElementById('conversation-page');
  const motion=createWorkspaceToolMotion();
  function route() { if (!navigating && context.isActive()) context.navigatePage(active ? 'workbench?tool=' + seats.get(selected).key + '&session=' + encodeURIComponent(owner || '') : 'workbench'); }
  function sync() {
    host.classList.toggle('hidden', !active); host.inert = !active;
    conversation.classList.toggle('hidden', active || Boolean(shell.page)); conversation.inert = active || Boolean(shell.page);
    if (active) dom.app.dataset.tool = seats.get(selected).key; else delete dom.app.dataset.tool;
    for (const [id, seat] of seats) { seat.node.hidden = !(active && selected === id); seat.node.inert = seat.node.hidden; }
    CLATI18n.render(); refreshApplicationToolbar(shell); syncPanelAccessibility();
  }
  function suspend() {
    if (!active) return;
    const seat = seats.get(selected); seat.scrolls=[...seat.node.querySelectorAll('*')].filter(node=>node.scrollHeight>node.clientHeight).map(node=>[node,node.scrollTop,node.scrollLeft]); seat.node.dispatchEvent(new Event('clat:tool-leave')); active = false;
  }
  function activate() {
    if (!selected || shell.page) return;
    if (!active) { captureProjectReading(); activeComposerPopup?.close(); setConversationFindOpen(false); active = true; sync(); const seat=seats.get(selected);for(const [node,top,left]of seat.scrolls||[]){node.scrollTop=top;node.scrollLeft=left;}seat.node.dispatchEvent(new Event('clat:tool-enter')); }
    else sync();
  }
  function showChat(update = true) {
    motion.cancel();const returning=active;
    suspend(); unread=false; sync(); refreshStatus();restoreProjectReading(); if (update) route();
    if(returning&&!shell.page)motion.close();
  }
  function open(id) {
    if (!seats.has(id)) return;
    motion.cancel();
    if (shell.page) shell.chat(false);
    if (owner !== state.sessionId) resetOwner();
    if (selected !== id) suspend();
    selected = id; activate(); route();
  }
  function resetOwner(){
    motion.cancel();
    const projectTool=selected&&['files','changes'].includes(seats.get(selected).key);
    if(!projectTool){suspend();selected=null;}owner=state.sessionId;unread=false;
    for(const seat of seats.values()){
      if(['files','changes'].includes(seat.key))continue;
      delete seat.node.dataset.loaded;delete seat.node.dataset.detail;seat.scrolls=[];seat.node.dispatchEvent(new Event('clat:tool-reset'));
    }sync();
  }
  installWorkspaceToolKeys(()=>active,showChat);
  window.addEventListener('clat:workbench-updated', () => {
    if (owner !== state.sessionId) resetOwner();
    pending = document.querySelectorAll('.question-card:not(.resolved),.approval-card:not(.resolved)').length;
    refreshStatus();
    if (!active) return;
    seats.get(selected)?.node.dispatchEvent(new Event('clat:tool-refresh'));
  });
  function refreshStatus(){refreshApplicationToolbar(shell);}
  const changes=new MutationObserver(()=>{if(active&&context.isActive()){unread=true;refreshStatus();}});
  changes.observe(dom.transcript,{childList:true});
  const api = {
    current:()=>active&&selected?{id:selected,...seats.get(selected)}:null,
    status:()=>({pending,unread,running:state.runActive||state.compactionActive}),
    location:()=>active&&selected?'workbench?tool='+seats.get(selected).key+'&session='+encodeURIComponent(owner||''):'workbench',
    open, chat: showChat, suspend: () => { motion.cancel();suspend(); sync(); },
    isActive: id => active && selected === id && owner === state.sessionId,
    register(id, key) {
      const node=document.getElementById(id);mountWorkspaceToolRegion(node,host,key);seats.set(id,{node,key});
    },
    restore(params) {
      navigating = true;
      try { const seat = [...seats].find(([,value]) => value.key === params.get('tool'));
        if (seat && (['files','changes'].includes(seat[1].key)||(params.get('session') || '') === (state.sessionId || ''))) open(seat[0]); else showChat(false);
      } finally { navigating = false; }
    },
    global() { motion.cancel();suspend();host.inert=true; host.classList.add('hidden'); delete dom.app.dataset.tool; },
  };
  return api;
}

function installWorkspaceToolKeys(isActive,showChat){
  document.addEventListener('keydown',event=>{
    if(!isActive()||event.key!=='Escape'||event.defaultPrevented||event.isComposing)return;
    if(event.target.closest('input,textarea,.cm-editor')||document.querySelector('#workbench-tools:not(.hidden),.breadcrumb-overflow[open],dialog[open]:not(.shell-surface),.session-context-menu'))return;
    event.preventDefault();event.stopImmediatePropagation();showChat();document.getElementById('tools-open')?.focus({preventScroll:true});
  });
}
function mountWorkspaceToolRegion(node,host,key){
  host.append(node);node.classList.add('workspace-tool');node.hidden=true;
  node.setAttribute('role','region');node.removeAttribute('aria-labelledby');CLATI18n.attribute(node,'aria-label',()=>experienceText(key));
}

/* Motion owns temporary paint only; lifecycle, selection and execution switch immediately. */
function createWorkspaceToolMotion(){
  const reduced=matchMedia('(prefers-reduced-motion: reduce)');
  const viewport=dom['transcript-scroll'];
  let animations=[],resizing=[],frame=0,epoch=0,restoring=false;
  function cancel(){
    if(frame)cancelAnimationFrame(frame);frame=0;
    if(restoring){
      for(const animation of resizing){try{animation.finish();}catch(_){/* Already cancelled. */}}
      restoreProjectReading();restoring=false;
    }
    epoch++;for(const animation of animations)animation.cancel();animations=[];
    resizing=[];
  }
  function close(){
    if(!context.isActive()||reduced.matches||typeof viewport.animate!=='function')return;
    const version=++epoch;restoring=true;
    // Only history is concealed; the composer, sidebars and tools keep their geometry motion.
    const cover=viewport.animate([{opacity:0},{opacity:0}],{duration:1,fill:'forwards'});
    animations.push(cover);
    // Resolve styles before collecting the actual CSS transitions, avoiding a second timing constant.
    void getComputedStyle(dom.app).gridTemplateColumns;
    resizing=dom.app.getAnimations().filter(animation=>animation.transitionProperty==='grid-template-columns');
    Promise.allSettled(resizing.map(animation=>animation.finished)).then(()=>{
      if(version!==epoch)return;
      frame=requestAnimationFrame(()=>{
        frame=0;if(version!==epoch)return;
        restoreProjectReading();restoring=false;resizing=[];
        const fade=viewport.animate([{opacity:0},{opacity:1}],{duration:160,easing:'ease-out',fill:'both'});
        cover.cancel();animations=[fade];
        fade.finished.then(()=>{if(version===epoch)cancel();},()=>{});
      });
    });
  }
  reduced.addEventListener('change',()=>{if(reduced.matches)cancel();});
  return {close,cancel};
}
