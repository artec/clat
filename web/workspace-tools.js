/* One contextual workspace: conversation, or one list/detail tool. */
function toolViewIsOpen(node) { return Boolean(workbenchShell?.tools?.isActive(node.id)); }
function openWorkbenchTool(node) { workbenchShell.tools.open(node.id); }
function closeWorkbenchTool() { workbenchShell.tools.chat(); }
function installWorkspaceTools(shell) {
  const seats = new Map(); let selected = null, active = false, owner = state.sessionId, navigating = false;
  const label = () => selected ? experienceText(seats.get(selected).key) : '';
  const host = el('section', 'workspace-tool-host hidden'); host.id = 'workspace-tool-host';
  const tabs = el('nav', 'workspace-tabs hidden'); tabs.setAttribute('role', 'tablist');
  CLATI18n.attribute(tabs, 'aria-label', () => CLATI18n.msg('workflow.workspace_views'));
  const chat = shellButton('workspace-chat-tab', 'chat', () => CLATI18n.msg('web.conversation'), () => showChat());
  const tool = shellButton('workspace-tool-tab', 'tools', () => label(), () => {activate();route();});
  const chatStatus=el('small','workspace-chat-status');chatStatus.setAttribute('role','status');chat.append(chatStatus);
  let unread=false,pending=0;
  const close = shellButton('workspace-tool-close', 'close', () => CLATI18n.msg('common.close'), () => dispose());
  close.replaceChildren(document.createTextNode('×'));
  for (const tab of [chat, tool]) tab.setAttribute('role', 'tab');
  const group = el('div', 'workspace-tool-tab'); group.append(tool, close); tabs.append(chat, group);
  const content = document.getElementById('content-area'); content.prepend(tabs); content.append(host);
  const conversation = document.getElementById('conversation-page');
  function route() { if (!navigating && context.isActive()) context.navigatePage(active ? 'workbench?tool=' + seats.get(selected).key + '&session=' + encodeURIComponent(owner || '') : 'workbench'); }
  function sync() {
    tabs.classList.toggle('hidden', !selected || Boolean(shell.page));
    host.classList.toggle('hidden', !active); host.inert = !active;
    conversation.classList.toggle('hidden', active || Boolean(shell.page)); conversation.inert = active || Boolean(shell.page);
    if (active) dom.app.dataset.tool = seats.get(selected).key; else delete dom.app.dataset.tool;
    chat.setAttribute('aria-selected', String(!active)); tool.setAttribute('aria-selected', String(active));
    chat.tabIndex = active ? -1 : 0; tool.tabIndex = active ? 0 : -1;
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
    suspend(); unread=false; sync(); refreshStatus();restoreProjectReading(); if (update) route();
  }
  function open(id) {
    if (!seats.has(id)) return;
    if (shell.page) shell.chat(false);
    if (owner !== state.sessionId) resetOwner();
    if (selected !== id) suspend();
    selected = id; activate(); route();
  }
  function resetOwner(){ suspend();selected=null;owner=state.sessionId;unread=false;for(const seat of seats.values()){delete seat.node.dataset.loaded;delete seat.node.dataset.detail;seat.scrolls=[];seat.node.dispatchEvent(new Event('clat:tool-reset'));}sync(); }
  function dispose() { showChat(false); selected = null; sync(); route(); document.getElementById('tools-open')?.focus({preventScroll:true}); }
  installWorkspaceToolKeys({tabs,close,chat,tool,isActive:()=>active,showChat});
  window.addEventListener('clat:workbench-updated', () => {
    if (owner !== state.sessionId) resetOwner();
    pending = document.querySelectorAll('.question-card:not(.resolved),.approval-card:not(.resolved)').length;
    refreshStatus();
    CLATI18n.attribute(chat, 'aria-label', () => pending ? CLATI18n.msg('workflow.conversation_pending', {count:pending}) : CLATI18n.msg('web.conversation'));
    if (!active) return;
    seats.get(selected)?.node.dispatchEvent(new Event('clat:tool-refresh'));
  });
  function refreshStatus(){updateWorkspaceConversationStatus(chat,chatStatus,pending,state.runActive||state.compactionActive,unread);}
  const changes=new MutationObserver(()=>{if(active&&context.isActive()){unread=true;refreshStatus();}});
  changes.observe(dom.transcript,{childList:true});
  const api = {
    location:()=>active&&selected?'workbench?tool='+seats.get(selected).key+'&session='+encodeURIComponent(owner||''):'workbench',
    open, chat: showChat, suspend: () => { suspend(); sync(); },
    isActive: id => active && selected === id && owner === state.sessionId,
    register(id, key) {
      const node=document.getElementById(id);mountWorkspaceToolRegion(node,host,tool.id);seats.set(id,{node,key});
    },
    restore(params) {
      navigating = true;
      try { const seat = [...seats].find(([,value]) => value.key === params.get('tool'));
        if (seat && (params.get('session') || '') === (state.sessionId || '')) open(seat[0]); else showChat(false);
      } finally { navigating = false; }
    },
    global() { suspend(); tabs.classList.add('hidden'); host.classList.add('hidden'); delete dom.app.dataset.tool; },
  };
  return api;
}

function installWorkspaceToolKeys({tabs,close,chat,tool,isActive,showChat}){
  tabs.addEventListener('keydown', event => {
    if (event.target === close || !['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) return;
    event.preventDefault(); const next = event.key === 'Home' ? chat : event.key === 'End' ? tool : event.target === chat ? tool : chat;
    next.click(); next.focus({preventScroll:true});
  });
  document.addEventListener('keydown', event => {
    if (!isActive() || event.key !== 'Escape' || event.defaultPrevented || event.isComposing) return;
    if (document.querySelector('#workbench-tools:not(.hidden), dialog[open]:not(.shell-surface)')) return;
    event.preventDefault(); event.stopImmediatePropagation(); showChat(); chat.focus({preventScroll:true});
  });
}

function mountWorkspaceToolRegion(node,host,tabId){
  host.append(node);node.classList.add('workspace-tool');node.hidden=true;
  const back=shellButton('','previous',()=>workflowText('back_to_list'),()=>{delete node.dataset.detail;});back.classList.add('tool-list-back');node.querySelector('.review-header')?.prepend(back);
  node.addEventListener('click',event=>{if(event.target.closest('.review-file')&&!event.target.closest('[aria-expanded]'))node.dataset.detail='true';});
  node.setAttribute('role','tabpanel');node.setAttribute('aria-labelledby',tabId);
}

function updateWorkspaceConversationStatus(chat,status,pending,running,unread){
  chat.dataset.pending=String(Boolean(pending));
  CLATI18n.bind(status,'textContent',()=>pending?CLATI18n.msg('workflow.pending_count',{count:pending}):running?CLATI18n.msg('web.running_short'):unread?workflowText('updated'):'');
}
