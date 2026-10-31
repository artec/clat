/* Presentation only: retain original nodes, message identities and chronological order. */
const roundText=(key,params={})=>CLATI18n.msg('round.'+key,params);
function roundDuration(milliseconds){
  if(!Number.isFinite(milliseconds)||milliseconds<0)return roundText('unknown');
  const seconds=Math.floor(milliseconds/1000),minutes=Math.floor(seconds/60);
  return roundText('duration',{minutes,seconds:seconds%60});
}
function toolPresentationIcon(name){
  const icons={run_command:'terminal',exec_command:'terminal',write_stdin:'terminal',read_file:'file',list_files:'folder',search:'search',web_search:'search',web_search_prime:'search',web_fetch:'file',edit_file:'edit',write_file:'edit',apply_patch:'edit',view_image:'file',update_plan:'plan',exit_plan_mode:'plan'};
  return icons[name]||'tool'; // External identifiers never imply an action.
}
function createConversationRounds(){
  let seats=new Map(),owners=new WeakMap(),live=null,timer=null,serial=0,scopeId=null;
  const choices=new Map();
  const key=seat=>(scopeId??'pending')+':'+(seat.nodes.find(n=>n.matches('.msg.user'))?.dataset.seq||seat.id);
  function stop(){clearInterval(timer);timer=null;}
  function reset(){stop();seats.clear();owners=new WeakMap();live=null;scopeId=null;}
  function seat(id){
    if(!seats.has(id))seats.set(id,{id,nodes:[],start:null,end:null,ended:false,manual:null,header:null});
    return seats.get(id);
  }
  function header(value){
    const node=el('button','round-process-heading');node.type='button';
    node.append(el('span','round-clock'),svgIcon('next'));
    node.onclick=()=>{captureProjectReading();value.manual=node.getAttribute('aria-expanded')!=='true';choices.set(key(value),value.manual);paint(value);restoreProjectReading();};
    value.header=node;return node;
  }
  function paint(value){
    const nodes=value.nodes.filter(n=>dom.transcript.contains(n));if(!nodes.length)return;
    if(!value.header)header(value);
    const first=nodes[0];
    if(first.matches('.msg.user')){if(first.nextSibling!==value.header)first.after(value.header);}
    else if(first.previousSibling!==value.header)first.before(value.header);
    const manual=value.manual??choices.get(key(value));
    const expanded=!value.ended||manual===true||(manual==null&&state.processView==='expanded');
    value.header.setAttribute('aria-expanded',String(expanded));
    value.header.disabled=!value.ended;
    value.header.dataset.running=String(!value.ended);
    if(first.dataset.turn)value.header.dataset.turn=first.dataset.turn;
    const final=value.ended?nodes.findLast(n=>n.matches('.msg.assistant')&&n.querySelector('.body')?.textContent.trim()):null;
    for(const node of nodes){
      const pinned=node.matches('.msg.user,.is-error,.verdict,.approval-card,.question-card,.interaction-anchor,.compaction-disclosure')||node.matches('.is-warning:not([data-event=retry_scheduled])')||node.querySelector('.is-error,.is-warning');
      const hiding=!expanded&&!pinned&&node!==final;
      if(hiding&&node.contains(document.activeElement))value.header.focus({preventScroll:true});
      node.classList.toggle('round-process-hidden',hiding);
      if(!expanded&&node===final&&node.querySelector('.reasoning')?.contains(document.activeElement))value.header.focus({preventScroll:true});
      node.classList.toggle('round-hide-reasoning',!expanded&&node===final);
    }
    clock(value);
  }
  function clock(value){
    if(!value?.header)return;
    const duration=roundDuration((value.ended?value.end:Date.now())-value.start);
    const known=value.start!==null&&(!value.ended||value.end!==null);
    CLATI18n.bind(value.header.querySelector('.round-clock'),'textContent',()=>known?roundText(value.ended?(value.approximate?'elapsed_estimated':'elapsed'):'processing',{duration}):roundText('unknown'));
  }

  function add(value,node){
    if(!node||node.classList.contains('compaction-disclosure'))return;
    if(!owners.has(node)){owners.set(node,value);value.nodes.push(node);}
  }
  function begin(){scopeId=state.sessionId;live=seat('live:'+ ++serial);live.start=Date.now();live.approximate=true;tick();}
  function capture(node){if(live&&!live.ended&&!state.history.replaying&&!state.history.replayTarget){add(live,node);if(!live.header)paint(live);}}
  function finish(){
    if(!live||live.ended)return;const follow=nearEnd();
    live.ended=true;live.end=Date.now();paint(live);stop();
    if(follow)dom['transcript-scroll'].scrollTop=dom['transcript-scroll'].scrollHeight;
  }
  function replay(event,node){
    if(event.type==='compaction')return;
    const value=seat('turn:'+event.turn);add(value,node);
    if(event.type==='user_message'&&value.start===null&&event.time_ms>0){value.start=event.time_ms;value.approximate=true;}
    if(event.type==='turn_ended'){value.ended=true;value.end=event.time_ms>0?event.time_ms:null;}
  }
  function refresh(){for(const value of seats.values())paint(value);if(!live)live=[...seats.values()].findLast(value=>!value.ended);}
  function tick(){stop();if(live&&!live.ended&&state.connected&&context.isActive())timer=setInterval(()=>clock(live),1000);}
  function active(info){
    if(info&&(!live||live.ended))live=[...seats.values()].findLast(value=>!value.ended);
    if(info?.started&&live&&!live.ended){live.start=info.started;live.approximate=false;paint(live);}
    if(!info)stop();else tick();
  }
  function reveal(node){const value=owners.get(node);if(value&&node.classList.contains('round-process-hidden')){value.manual=true;choices.set(key(value),true);paint(value);}}
  function preference(){for(const value of seats.values())value.manual=null;choices.clear();refresh();}
  function scope(id){scopeId=id;refresh();}
  return {reset,begin,capture,finish,replay,refresh,stop,tick,active,reveal,preference,scope};
}
const conversationRounds=createConversationRounds();

function conversationMapTurns(outline){
  const turns=new Map();
  for(const item of outline){
    if(!item||!Number.isSafeInteger(item.seq)||item.renderable===false||!['user','assistant'].includes(item.role))continue;
    if(item.renderable!==true&&item.role==='assistant'&&!String(item.preview||'').trim())continue;
    const id=item.turn||0,prior=turns.get(id);
    if(!prior||prior.role!=='user'&&item.role==='user')turns.set(id,item);
  }
  return [...turns.values()];
}
function renderCompactionDisclosure(event){
  const target=state.history.replayTarget||dom.transcript;
  const id=event.disclosure?.id||'seq:'+event.seq;
  const existing=[...target.querySelectorAll('.compaction-disclosure')].find(n=>n.dataset.compactionId===id);
  if(existing)return existing;
  const card=el('details','compaction-disclosure'),summary=el('summary');card.dataset.compactionId=id;
  const value=event.disclosure||{},body=el('div','compaction-disclosure-body');
  summary.append(svgIcon('collapse-all'),el('span','',()=>value.records==null?roundText('compacted'):roundText('compacted_records',{count:value.records})));card.append(summary,body);
  if(value.range)body.append(el('p','',()=>roundText('range',{start:value.range[0],end:value.range[1]})));
  if(value.estimated_tokens!=null)body.append(el('p','',()=>roundText('tokens',{count:value.estimated_tokens})));
  if(event.summary_text){body.append(el('h4','',()=>roundText('summary')));const text=el('div','rich-text');renderMarkdown(text,event.summary_text);body.append(text);}
  return appendTranscript(card);
}
function conversationMapActiveSeq(){
  const edge=dom['transcript-scroll'].getBoundingClientRect().top+72;
  const visible=[...dom.transcript.children].filter(node=>node.dataset.turn&&node.getClientRects().length&&!node.classList.contains('round-process-hidden'));
  const node=visible.findLast(node=>node.getBoundingClientRect().top<=edge)||visible[0];
  return state.history.mapItems.find(item=>String(item.turn||0)===node?.dataset.turn)?.seq?.toString()||null;
}
async function refreshCompactionDisclosure(){
  const generation=state.selectionGeneration,stream=state.stream;
  const marker=el('div','compaction-disclosure',()=>roundText('compacted'));
  appendTranscript(marker); // Reserve the completion position before awaiting history.
  try{
    const page=await rpc('session.history',{before_seq:null,max_messages:50,expected_selection_generation:generation});
    if(generation!==state.selectionGeneration||stream!==state.stream)return;
    const event=(page.events||[]).findLast(event=>event.type==='compaction');if(!event)return;
    const id=event.disclosure?.id||'seq:'+event.seq;
    const existing=[...dom.transcript.querySelectorAll('[data-compaction-id]')].find(node=>node.dataset.compactionId===id);
    if(existing){marker.remove();return;}
    const card=renderCompactionDisclosure(event);card.dataset.seq=String(event.seq);marker.replaceWith(card);
  }catch(_){ /* Keep the completion label; cold replay restores disclosure metadata. */ }
}

/* One live status label, independent of recorded reasoning and message bodies. */
function createRunActivity(){
  let element=null,label=null,signature='',waiting=null;
  function hide(){element?.remove();}
  function clear(){hide();label=null;signature='';waiting=null;}
  function sync(){
    if(!state.connected||!state.runActive||state.switching||!context.isActive()){hide();return;}
    if(!element){
      element=el('div','run-activity');element.setAttribute('role','status');
      element.append(svgIcon('activity-log'),el('span','run-activity-label'));
    }
    if(!label)label=CLATI18n.msg('web.running');
    if(element.parentNode!==dom.transcript||element.nextSibling)dom.transcript.append(element);
    const next=JSON.stringify(label);
    if(next!==signature){signature=next;const text=label;CLATI18n.bind(element.querySelector('span'),'textContent',()=>text);}
  }
  function set(value){label=value;waiting=null;sync();}
  function wait(value,id){label=value;waiting=id;sync();}
  function resume(key,id){if(label?.key===key&&waiting===id)set(CLATI18n.msg('web.running'));}
  return {set,sync,hide,clear,wait,resume};
}
const runActivity=createRunActivity();
