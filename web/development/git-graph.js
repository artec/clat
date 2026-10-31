/* Graph geometry is presentation; every edge comes from a core parent ID. */
function createGitGraph(host,{call,open,current,report,showCommit,state={}}){
  const list=el('section'),more=devButton('more_history',()=>void load(true));
  host.append(list,more);let rows=state.rows||[],cursor=state.cursor||null,epoch=0,selected=state.selected||null;render();host.scrollTop=state.scroll||0;list.onscroll=()=>{state.scroll=host.scrollTop;};
  async function load(append=false){const scope=readGitGraphScope();if(scope!==state.scope){append=false;selected=null;state.selected=null;}const version=++epoch;more.disabled=true;try{const value=await call('git.history',append?{cursor}:{scope});if(version!==epoch||!current())return;rows=append?[...rows,...value.commits]:value.commits;cursor=value.nextCursor;more.hidden=!cursor;state.rows=rows;state.cursor=cursor;state.scope=scope;const top=host.scrollTop;render();if(selected){const button=[...list.querySelectorAll('button[data-commit]')].find(b=>b.dataset.commit===selected),row=rows.find(r=>r.id===selected);if(button&&row)void inspect(row,button);}host.scrollTop=top;}catch(error){if(version===epoch&&current())report(error);}finally{more.disabled=false;}}
  function render(){
    list.replaceChildren();let lanes=[];
    for(const row of rows){const before=[...lanes];let lane=lanes.indexOf(row.id);if(lane<0){lane=lanes.length;lanes.push(row.id);before.push(row.id);}lanes.splice(lane,1,...row.parents.filter(id=>!lanes.includes(id)));lanes=[...new Set(lanes)];
      const button=el('button','git-graph-row'),copy=el('span','git-graph-copy');button.type='button';button.dataset.commit=row.id;button.setAttribute('aria-current',String(selected===row.id));CLATI18n.attribute(button,'title',()=>row.subject+' · '+row.id+' · '+row.author+' · '+row.date);
      const drawing=gitGraphDrawing(before,lanes,row,lane);copy.append(el('strong','',row.subject));if(row.refs.length)copy.append(el('small','git-refs',row.refs.join(' · ')));button.append(drawing,copy);button.onclick=()=>void inspect(row,button);list.append(button);
    }
    if(!rows.length)list.append(el('p','settings-note',()=>devText('no_history')));
  }
  async function inspect(row,button){
    selected=row.id;state.selected=row.id;for(const b of list.querySelectorAll('button'))b.setAttribute('aria-current',String(b===button));const version=++epoch;list.querySelector('.git-commit-files')?.remove();
    const files=el('section','git-commit-files'),parents=el('select');CLATI18n.attribute(parents,'aria-label',()=>devText('compare_parent'));
    for(const id of row.parents){const option=el('option','',id.slice(0,8));option.value=id;parents.append(option);}if(state.parent&&row.parents.includes(state.parent))parents.value=state.parent;if(row.parents.length>1)files.append(parents);button.after(files);
    const refresh=async()=>{const parent=parents.value||undefined;state.parent=parent;try{const data=await call('git.commit.files',{commit:row.id,parent});if(!current()||version!==epoch||parent!==(parents.value||undefined))return;for(const n of [...files.children])if(n!==parents)n.remove();files.append(devButton('commit_details',()=>void showCommit(row)));for(const file of data.files)files.append(gitFileRow(file,{open:()=>open({...file,commit:row.id,parent:data.parent},false),pin:()=>open({...file,commit:row.id,parent:data.parent},true)}));if(!data.files.length)files.append(el('p','settings-note',()=>devText('clean')));}catch(error){if(current()&&version===epoch)report(error);}};
    parents.onchange=()=>void refresh();await refresh();
  }
  return {load,invalidate(){epoch++;}};
}
function gitGraphDrawing(before,after,row,lane){
  const ns='http://www.w3.org/2000/svg',svg=document.createElementNS(ns,'svg'),width=Math.max(before.length,after.length,1)*12+8;
  svg.setAttribute('viewBox',`0 0 ${width} 30`);svg.style.width=width+'px';svg.setAttribute('aria-hidden','true');
  const x=i=>i*12+8,line=(a,b,c,d,color)=>{const p=document.createElementNS(ns,'path');p.setAttribute('d',`M${a} ${b}L${c} ${d}`);p.style.stroke=`var(--git-lane-${color%4})`;svg.append(p);};
  before.forEach((id,i)=>{if(id!==row.id&&after.includes(id))line(x(i),0,x(after.indexOf(id)),30,i);});
  if(before.includes(row.id))line(x(lane),0,x(lane),15,lane);
  for(const parent of row.parents)if(after.includes(parent))line(x(lane),15,x(after.indexOf(parent)),30,lane);
  const dot=document.createElementNS(ns,'circle');dot.setAttribute('cx',x(lane));dot.setAttribute('cy','15');dot.setAttribute('r','3');dot.style.stroke=`var(--git-lane-${lane%4})`;dot.style.fill='var(--surface)';svg.append(dot);return svg;
}

function readGitGraphScope(){
  try{return localStorage.getItem('clat.git.graph.scope')==='current'?'current':'all';}catch(_){return 'all';}
}
