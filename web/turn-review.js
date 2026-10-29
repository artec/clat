/* Operation records consume the read-only core port, never raw journal events. */
function installTurnReview(){
  const trigger=el('button','ghost',()=>experienceText('turn'));trigger.id='turn-review-open';trigger.type='button';document.getElementById('review-open').after(trigger);
  const page=el('section','workspace-review');page.id='turn-review';const ui=createWorkspaceToolView(page,'turn');document.body.append(page);
  let epoch=0,detailEpoch=0,selected=null,selectedRow=null,detailStamp='',filterStamp='',cursor=null,rows=[],tools=[],turns=[],loading=false,refreshAgain=false;
  const owner=()=>[state.sessionId,state.selectionGeneration,state.stream,workspacePrefix],same=saved=>saved.every((v,i)=>v===owner()[i]);
  const filters={};for(const key of ['turn','tool','status']){const select=el('select');CLATI18n.attribute(select,'aria-label',()=>viewText('filter_'+key));select.onchange=()=>void load();filters[key]=select;ui.filters.append(select);}
  const more=el('button','',()=>CLATI18n.msg('history.more'));more.type='button';more.onclick=()=>void load(true);more.hidden=true;ui.sidebar.append(more);
  const filter=()=>Object.fromEntries(Object.entries(filters).map(([key,input])=>[key,input.value?(key==='turn'?Number(input.value):input.value):null]));
  function options(){for(const [key,values]of [['turn',turns],['tool',tools],['status',['unfinished','succeeded','failed','unknown']]]){const select=filters[key],prior=select.value;select.replaceChildren();const any=el('option','',()=>viewText('all_'+key));any.value='';select.append(any);for(const value of values){const option=el('option','',()=>key==='turn'?CLATI18n.msg('view.turn_number',{turn:value}):key==='tool'?toolDisplayName(value):viewText('status_'+value));option.value=String(value);select.append(option);}select.value=prior;}}
  function render(){
    const top=ui.list.scrollTop;ui.list.replaceChildren();let turn=null;
    for(const row of rows){if(row.turn!==turn){turn=row.turn;ui.list.append(el('h3','operation-turn',()=>CLATI18n.msg('view.turn_number',{turn:row.turn})));}ui.list.append(navigationRow({title:()=>toolDisplayName(row.tool),meta:()=>[row.target,viewText('status_'+row.status)].filter(Boolean).join(' · '),icon:'activity-log',selected:selected===row.id,action:()=>void detail(row)}));}
    if(!rows.length)ui.list.append(el('p','picker-hint',()=>viewText('no_operations')));ui.list.scrollTop=top;more.hidden=!cursor;
  }
  async function load(append=false){
    if(!state.sessionId){rows=[];cursor=null;render();ui.overview();ui.status.textContent=viewText('no_session');return;}
    if(loading){if(!append)refreshAgain=true;return;}loading=true;const request=++epoch,saved=owner();detailEpoch++;const query=filter(),stamp=JSON.stringify(query),changed=stamp!==filterStamp,target=append?40:changed?40:Math.max(40,rows.length);filterStamp=stamp;
    const current=()=>request===epoch&&same(saved)&&toolViewIsOpen(page);
    ui.status.textContent=viewText('loading_operations');refreshViewContext();
    try{let value,next=append?cursor:null,collected=[];do{value=await rpc('operations.list',{reference:{project:state.workbench.project.root,session:saved[0],seq:null},filter:query,cursor:next,expected_selection_generation:saved[1]});if(!current())return;collected.push(...value.rows);next=value.next_cursor;}while(!append&&next&&collected.length<target);
      rows=append?[...rows,...collected.filter(row=>!rows.some(old=>old.id===row.id))]:collected;cursor=next;tools=value.tools;turns=value.turns;options();render();ui.status.textContent='';
      const prior=rows.find(row=>row.id===selected)||selectedRow;if(prior)await detail(prior,true);else ui.overview();
    }catch(error){if(current()){ui.status.textContent=error.message;if(!rows.length)ui.body.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void load()));}}
    finally{loading=false;refreshViewContext();if(refreshAgain){refreshAgain=false;if(toolViewIsOpen(page))void load();}}
  }
  async function detail(row,preserve=false){
    const request=++detailEpoch,saved=owner(),top=preserve?ui.body.scrollTop:0;if(!preserve||selected!==row.id)detailStamp='';selected=row.id;selectedRow=row;ui.show(()=>toolDisplayName(row.tool));render();if(!preserve)ui.body.replaceChildren(pageEmpty('loading'));
    const current=()=>request===detailEpoch&&same(saved)&&selected===row.id&&toolViewIsOpen(page);
    try{const value=await rpc('operations.detail',{reference:{project:state.workbench.project.root,session:saved[0],seq:null},id:row.id,expected_selection_generation:saved[1]});if(!current())return;const stamp=JSON.stringify(value);if(stamp===detailStamp){ui.body.querySelector('.operation-capture')?.dispatchEvent(new Event('clat:capture-refresh'));return;}detailStamp=stamp;const expanded=[...ui.body.querySelectorAll('details')].map(node=>node.open);
      ui.body.replaceChildren(el('p','operation-status',()=>viewText('status_'+value.row.status)));if(row.target)ui.body.append(el('p','',row.target));
      const technical=el('details');technical.append(el('summary','',()=>viewText('technical')),el('pre','review-diff',()=>[value.row.tool,value.row.call_id,'#'+value.row.seq].join('\n')));ui.body.append(technical);
      for(const [key,text]of [['arguments',value.arguments],['result',value.result]])ui.body.append(el('h3','',()=>viewText(key)),el('pre','review-diff',text??viewText(key==='arguments'?'missing_call':'missing_result')));
      if(value.truncated)ui.body.append(el('p','picker-hint',()=>viewText('detail_truncated')));
      installOperationCapture(ui.body,row.turn,{current:()=>same(saved)&&selected===row.id&&toolViewIsOpen(page)&&detailStamp===stamp,owner:saved});[...ui.body.querySelectorAll('details')].forEach((node,index)=>{node.open=Boolean(expanded[index]);});ui.body.scrollTop=top;
    }catch(error){if(current())ui.body.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void detail(row)));}
  }
  options();registerView(page,{sidebar:ui.sidebar,actions:()=>[viewAction('refresh','refresh',()=>pageText('refresh'),()=>load(),true,()=>!loading)]});
  trigger.onclick=()=>openWorkbenchTool(page);page.addEventListener('clat:tool-enter',()=>void load());page.addEventListener('clat:tool-refresh',()=>void load());
  page.addEventListener('clat:tool-leave',()=>{epoch++;detailEpoch++;});page.addEventListener('clat:tool-reset',()=>{epoch++;detailEpoch++;selected=null;selectedRow=null;detailStamp='';filterStamp='';rows=[];cursor=null;for(const input of Object.values(filters))input.value='';render();ui.overview();});
}
