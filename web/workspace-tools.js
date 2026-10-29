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
  const motion=createWorkspaceToolMotion(host,conversation,sync);
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
    motion.cancel();const departing=active?seats.get(selected)?.node:null;
    suspend(); unread=false; sync(); refreshStatus();restoreProjectReading(); if (update) route();
    if(departing&&!shell.page)motion.close(departing);
  }
  function open(id) {
    if (!seats.has(id)) return;
    motion.cancel();const changed=!active||selected!==id;
    if (shell.page) shell.chat(false);
    if (owner !== state.sessionId) resetOwner();
    if (selected !== id) suspend();
    selected = id; activate(); route();if(changed)motion.open();
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
function createWorkspaceToolMotion(host,conversation,settle){
  const reduced=matchMedia('(prefers-reduced-motion: reduce)');let animations=[],departing=null,epoch=0;
  function cancel(){
    epoch++;for(const animation of animations)animation.cancel();animations=[];
    if(departing){departing=null;host.classList.remove('workspace-tool-departing');host.removeAttribute('aria-hidden');settle();}
  }
  function enabled(){return context.isActive()&&!reduced.matches&&typeof host.animate==='function';}
  function play(node,frames,duration){const animation=node.animate(frames,{duration,easing:'cubic-bezier(.2,.75,.25,1)',fill:'both'});animations.push(animation);return animation;}
  function open(){
    if(!enabled())return;const version=++epoch;
    const animation=play(host,[{opacity:0,transform:'translateY(10px) scale(.992)',transformOrigin:'50% 0'},{opacity:1,transform:'none',transformOrigin:'50% 0'}],200);
    animation.finished.then(()=>{if(version===epoch){for(const item of animations)item.cancel();animations=[];}},()=>{});
  }
  function close(node){
    if(!enabled())return;const version=++epoch;departing=node;
    host.classList.remove('hidden');host.classList.add('workspace-tool-departing');host.inert=true;host.setAttribute('aria-hidden','true');node.hidden=false;node.inert=true;
    play(conversation,[{opacity:.55},{opacity:1}],180);
    const animation=play(host,[{opacity:1,transform:'none',transformOrigin:'50% 0'},{opacity:0,transform:'translateY(10px) scale(.992)',transformOrigin:'50% 0'}],160);
    animation.finished.then(()=>{if(version===epoch)cancel();},()=>{});
  }
  reduced.addEventListener('change',()=>{if(reduced.matches)cancel();});
  return {open,close,cancel};
}
