/* Fixed-height, view-local virtualization. Original records and their order are retained. */
const trajectoryText=(key,params={})=>CLATI18n.msg('trajectory.'+key,params);
const trajectoryGroup=row=>row.turn==null?row.id:'turn:'+row.turn;
const trajectoryLabel=row=>row.kind==='tool'?toolDisplayName(row.tool||''):trajectoryText(row.final_answer?'final_answer':row.kind);
function trajectoryIcon(row){return row.kind==='tool'?toolPresentationIcon(row.tool):({user:'user',reasoning:'brain',message:'agent',terminal:'trace',request:'trace',approval:'permission-write',retry:'refresh',compaction:'collapse-all'})[row.kind]||'info';}
function createTrajectoryList(parent,{choose,loadTurn,loadPage,viewportChanged}){
  const host=el('div','trajectory-ledger'),content=el('div','trajectory-virtual-content');host.tabIndex=0;
  CLATI18n.attribute(host,'aria-label',()=>trajectoryText('records'));host.append(content);parent.append(host);
  let rows=[],turns=[],records=new Map(),expanded=new Set(),selected=null,first=0,last=0,frame=null;
  const HEIGHT=40,OVERSCAN=10;
  function flatten(){
    const grouped=new Map();for(const row of records.values()){const key=trajectoryGroup(row);if(!grouped.has(key))grouped.set(key,[]);grouped.get(key).push(row);}
    rows=[];for(const turn of turns){
      if(turn.turn==null&&records.has(turn.id)){rows.push({id:turn.id,record:records.get(turn.id)});continue;}
      rows.push({id:turn.id,turn});
      if(expanded.has(turn.id)){
        const children=grouped.get(turn.id)||[];
        for(const record of children.sort((a,b)=>a.seq-b.seq))rows.push({id:record.id,record});
      }
    }
    content.style.height=rows.length*HEIGHT+'px';
  }
  function anchor(){const index=Math.floor(host.scrollTop/HEIGHT);return {id:rows[index]?.id,offset:host.scrollTop-index*HEIGHT};}
  function update(nextTurns,nextRecords,id,reveal=false){
    const saved=anchor();turns=nextTurns;records=nextRecords;selected=id;
    if(reveal&&records.has(id))expanded.add(trajectoryGroup(records.get(id)));
    flatten();const index=rows.findIndex(row=>row.id===saved.id);if(index>=0)host.scrollTop=index*HEIGHT+saved.offset;
    render(true);
  }
  function select(id,scroll=false,reveal=true){
    selected=id;
    if(scroll){
      let index=rows.findIndex(row=>row.id===id);
      if(index<0&&records.has(id)){
        const group=trajectoryGroup(records.get(id));
        if(reveal){expanded.add(group);flatten();index=rows.findIndex(row=>row.id===id);}
        else index=rows.findIndex(row=>row.id===group);
      }
      if(index>=0)host.scrollTop=Math.max(0,index*HEIGHT-host.clientHeight/3);
    }
    render(true);
  }
  function toggle(turn){
    const saved=anchor();if(expanded.has(turn.id))expanded.delete(turn.id);else{expanded.add(turn.id);loadTurn(turn);}
    flatten();const index=rows.findIndex(row=>row.id===saved.id);
    const fallback=rows.findIndex(row=>row.id===turn.id);
    if(index>=0||fallback>=0)host.scrollTop=(index>=0?index:fallback)*HEIGHT+saved.offset;
    render(true);
  }
  function collapse(){
    const saved=anchor(),item=rows.find(row=>row.id===saved.id),id=item?.record?trajectoryGroup(item.record):saved.id;
    expanded.clear();flatten();const index=rows.findIndex(row=>row.id===id);if(index>=0)host.scrollTop=index*HEIGHT+saved.offset;render(true);
  }
  function rowNode(item,index){
    const button=el('button','trajectory-row'+(item.turn?' trajectory-turn':''));button.type='button';button.style.top=index*HEIGHT+'px';button.dataset.recordId=item.id;
    if(item.turn){
      const turn=item.turn,open=expanded.has(turn.id),selectedRow=records.get(selected);
      button.setAttribute('aria-expanded',String(open));button.setAttribute('aria-current',String(selectedRow&&trajectoryGroup(selectedRow)===turn.id));
      const caret=svgIcon('next');caret.classList.toggle('is-expanded',open);
      button.append(caret,el('strong','',()=>turn.turn==null?trajectoryText('between_turns'):CLATI18n.msg('view.turn_number',{turn:turn.turn})),el('span','trajectory-row-preview',turn.question||trajectoryText('no_question')),el('small','',()=>trajectoryText(turn.steps==null?'counts':'step_counts',{steps:turn.steps??turn.records,tools:turn.tools})));
      button.onclick=()=>turn.turn==null?choose(turn.id):toggle(turn);
    }else{
      const row=item.record;button.setAttribute('aria-current',String(row.id===selected));button.dataset.status=row.status||'';
      button.append(svgIcon(trajectoryIcon(row)),el('span','trajectory-row-label',()=>trajectoryLabel(row)),el('span','trajectory-row-preview',row.preview));
      if(row.status)button.append(el('small','trajectory-record-status',()=>trajectoryText('status_'+row.status)));
      button.onclick=()=>choose(row.id);
    }
    return button;
  }
  function render(force=false){
    const visibleStart = Math.floor(host.scrollTop / HEIGHT);
    const visibleEnd = host.clientHeight ? Math.ceil((host.scrollTop + host.clientHeight) / HEIGHT) : visibleStart;
    viewportChanged(rows.slice(visibleStart, visibleEnd).map(item => ({
      id: item.id,
      turn: item.turn?.turn,
      wholeTurn: !expanded.has(item.id),
    })));
    const start = Math.max(0, visibleStart - OVERSCAN);
    const end = Math.min(rows.length, visibleEnd + OVERSCAN);
    content.style.height = rows.length * HEIGHT + 'px';
    if (!force && start === first && end === last) return;
    first = start;
    last = end;
    const focus=document.activeElement?.dataset.recordId;content.replaceChildren(...rows.slice(start,end).map((row,i)=>rowNode(row,start+i)));
    if(focus)content.querySelectorAll('[data-record-id]').forEach(node=>{if(node.dataset.recordId===focus)node.focus({preventScroll:true});});
  }
  host.onscroll=()=>{if(frame==null)frame=requestAnimationFrame(()=>{frame=null;render();});};
  host.onkeydown=event=>{
    if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;event.preventDefault();
    const focused=document.activeElement?.dataset.recordId,index=rows.findIndex(row=>row.id===focused),target=event.key==='Home'?0:event.key==='End'?rows.length-1:Math.max(0,Math.min(rows.length-1,index+(event.key==='ArrowDown'?1:-1)));
    const item=rows[target];if(!item)return;host.scrollTop=Math.max(0,target*HEIGHT-host.clientHeight/2);render(true);[...content.children].find(node=>node.dataset.recordId===item.id)?.focus({preventScroll:true});
  };
  const observer=new ResizeObserver(()=>render(true));observer.observe(host);
  const paging=el('div','trajectory-paging'),earlier=el('button','',()=>trajectoryText('earlier')),later=el('button','',()=>trajectoryText('later'));
  earlier.type=later.type='button';earlier.onclick=()=>loadPage('before');later.onclick=()=>loadPage('after');paging.append(earlier,later);parent.append(paging);
  return {update,select,pages(before,after,busy){earlier.hidden=!before;later.hidden=!after;earlier.disabled=later.disabled=busy;},collapse,dispose(){observer.disconnect();cancelAnimationFrame(frame);},focus(){host.focus({preventScroll:true});}};
}
