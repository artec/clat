/* Project-owned PTYs; page lifecycle only attaches/detaches presentation. */
function installDevelopmentTerminal(){
  let projects=[],selected=null,active=false,epoch=0,refreshEpoch=0,preferredShell='',menuClose=null,creating=false,renderStamp='';
  const terminals=new Map(),expanded=new Map();
  const actions={render,create:project=>void createTerminal('',project),select:(project,row)=>void choose(project,row),claim:()=>void claim(true),find:()=>{const text=window.prompt(devText('find_terminal'));if(text)selected?.widget.search.findNext(text);},copy:()=>void navigator.clipboard.writeText(selected?.widget.term.getSelection()||'').catch(report),clear:()=>selected?.widget.term.clear(),menu:rowMenu,cleanup:project=>void cleanup(project),shellMenu:source=>void shellMenu(source),more:source=>{menuClose=terminalMenu(source,[['refresh','refresh',refresh],['close_ended','trash',()=>cleanup()]]);}};
  const ui=createTerminalPage(actions),{node,mount,status}=ui;document.body.append(node);
  const trigger=devButton('terminal',()=>presentPage(node));trigger.id='terminal-open';trigger.hidden=true;document.body.append(trigger);
  const visible=()=>active&&context.isActive(),report=error=>devReport(status,error);
  const currentProject=()=>projects.find(project=>project.route_id===context.route)||{root:state.workbench?.project?.root,title:state.workbench?.project?.root,route_id:context.route,terminals:[]};
  function render(){const stamp=JSON.stringify([projects,selected?.id,ui.query.value,[...expanded],creating]);if(stamp!==renderStamp){const top=ui.list.scrollTop;renderTerminalDirectory(ui,projects,selected,actions,expanded);ui.list.scrollTop=top;renderStamp=stamp;}ui.empty.hidden=Boolean(selected)||creating;ui.name.textContent=terminalHeading(selected?.row);}
  async function refresh(){
    const request=++refreshEpoch,previous=currentProject().terminals,previousIndex=previous.findIndex(row=>row.id===selected?.id);
    try{const data=await rpc('terminal.directory',{});if(request!==refreshEpoch)return;projects=data.projects;
      const local=currentProject();for(const [id,seat]of terminals){const row=local.terminals.find(row=>row.id===id);if(!row){dispose(seat);if(selected===seat)selected=null;terminals.delete(id);}else seat.row=row;}
      render();if(!selected&&visible()&&!creating){const row=local.terminals[Math.min(Math.max(previousIndex,0),local.terminals.length-1)];if(row)await select(row);else{const other=projects.find(p=>p.terminals.length);if(other)await choose(other,other.terminals.at(-1));}}
    }catch(error){report(error);}
  }
  function dispose(seat){seat.stream?.abort();seat.unsubscribe?.();seat.widget.destroy();seat.element.remove();}
  function makeSeat(id,row){const element=el('div','dev-terminal-screen');mount.append(element);const widget=globalThis.CLATDevelopmentWidgets.terminal(element,{...devTerminalOptions(),disableStdin:true});const seat={id,root:currentProject().root,row,element,widget,offset:0,sequence:0,lease:0,controlled:false,inputLane:Promise.resolve(),sizeLane:Promise.resolve(),sizeVersion:0};wireTerminalSeat(seat,{report,fit,status,isVisible:()=>selected===seat&&visible()});return seat;}
  async function choose(project,row){
    if(project.root!==currentProject().root){const target=await context.openProject(project,{hash:'#/terminal'});if(target)await target.view.openTerminal(row.id);return;}
    await select(row,true);
  }
  async function select(row,focus=false){
    const request=++epoch;await devLoadTerminalFonts();if(request!==epoch)return;
    selected?.stream?.abort();if(selected)selected.element.hidden=true;
    let seat=terminals.get(row.id);if(!seat){seat=makeSeat(row.id,row);terminals.set(row.id,seat);}seat.row=row;selected=seat;seat.element.hidden=false;node.dataset.detail='true';render();
    if(row.phase==='running'){await claim(false);if(request!==epoch)return;await fit();if(visible())void attach(seat);if(focus&&seat.controlled)seat.widget.term.focus();}
    else{seat.controlled=false;seat.widget.term.options.disableStdin=true;devStatus(status,'ended');}
  }
  async function claim(takeover){
    if(!selected||selected.row.phase!=='running')return;const seat=selected;
    try{Object.assign(seat,await rpc('terminal.claim',{id:seat.id,clientId:developmentClient,takeover}));seat.widget.term.options.disableStdin=!seat.controlled;devStatus(status,seat.controlled?'controlling':'observer');if(takeover&&seat.controlled){await fit();seat.widget.term.focus();}}catch(error){seat.controlled=false;seat.widget.term.options.disableStdin=true;report(error);}
  }
  async function fit(){
    const seat=selected;if(!seat||!visible()||!mount.clientWidth||!mount.clientHeight)return;
    seat.widget.fit.fit();const size={rows:seat.widget.term.rows,cols:seat.widget.term.cols},version=++seat.sizeVersion;
    if(!seat.controlled||seat.row.phase!=='running')return;
    seat.sizeLane=seat.sizeLane.catch(()=>{}).then(async()=>{if(version!==seat.sizeVersion)return;await rpc('terminal.resize',{id:seat.id,clientId:developmentClient,lease:seat.lease,...size});}).catch(report);await seat.sizeLane;
  }
  async function createTerminal(directory='',project=currentProject()){
    if(project.root!==currentProject().root){const target=await context.openProject(project,{hash:'#/terminal'});if(target)await target.view.createTerminal(directory,preferredShell);return;}
    if(creating)return;creating=true;let seat;
    try{await devLoadTerminalFonts();if(!visible())throw new Error(devText('terminal_measure_failed'));
      selected?.stream?.abort();if(selected)selected.element.hidden=true;seat=makeSeat(devId(),{phase:'starting'});selected=seat;node.dataset.detail='true';render();await new Promise(resolve=>requestAnimationFrame(resolve));
      const dimensions=seat.widget.fit.proposeDimensions();if(!mount.clientWidth||!mount.clientHeight||!dimensions||dimensions.rows<1||dimensions.cols<1)throw new Error(devText('terminal_measure_failed'));seat.widget.fit.fit();
      const row=await rpc('terminal.create',{operationId:seat.id,clientId:developmentClient,directory,shell:preferredShell||undefined,...dimensions});seat.row=row;terminals.set(row.id,seat);await claim(false);await fit();if(visible()){void attach(seat);seat.widget.term.focus();}await refresh();
    }catch(error){if(seat&&!terminals.has(seat.id)){dispose(seat);if(selected===seat)selected=null;}report(error);}finally{creating=false;render();}
  }
  const projectRpc=(project,method,params)=>terminalProjectRpc(project,method,params,currentProject().root);
  async function remove(project,row){if(row.phase==='running'&&!window.confirm(devText('close_terminal_confirm')))return;
    try{await projectRpc(project,'terminal.remove',{operationId:devId(),id:row.id,confirmed:true});await refresh();}catch(error){report(error);}}
  async function cleanup(project){const targets=project?[project]:projects,count=targets.reduce((n,p)=>n+p.terminals.filter(row=>row.phase!=='running').length,0);if(!count||!window.confirm(devText('close_ended_confirm',{count})))return;
    const failures=[];for(const row of targets){try{await projectRpc(row,'terminal.removeEnded',{operationId:devId()});}catch(error){failures.push(error.message);}}await refresh();if(failures.length)report(new Error(failures.join('\n')));}
  function rowMenu(project,row,source){menuClose=terminalMenu(source,[['rename','edit',async()=>{const name=window.prompt(devText('terminal_name'),row.name);if(name)try{await projectRpc(project,'terminal.rename',{id:row.id,name});await refresh();}catch(error){report(error);}}],['copy_id','copy',()=>navigator.clipboard.writeText(row.id).catch(report)],['close_terminal','close',()=>remove(project,row)]]);}
  async function shellMenu(source){try{const data=await rpc('terminal.shells',{});menuClose=terminalMenu(source,data.shells.map(path=>[()=>devText('shell_for_project',{project:currentProject().title,shell:path.split(/[\\/]/).pop()}),'terminal',()=>{preferredShell=path;void createTerminal();}]));}catch(error){report(error);}}
  const attach=seat=>startTerminalStream(seat,{visible,isSelected:()=>selected===seat,refresh,has:()=>terminals.has(seat.id),attach,status});
  new ResizeObserver(()=>{if(visible())requestAnimationFrame(()=>void fit());}).observe(mount);
  node.addEventListener('clat:page-enter',async()=>{active=true;await refresh();if(selected?.row.phase==='running'){await claim(false);await fit();void attach(selected);}});
  const leave=()=>{active=false;epoch++;menuClose?.();globalThis.CLATTerminalMenuClose?.();selected?.stream?.abort();};node.addEventListener('clat:page-leave',leave);window.addEventListener('clat:development-deactivated',leave);
  window.addEventListener('clat:activated',()=>{if(pageIsOpen(node)){active=true;void refresh();}});
  node.addEventListener('keydown',event=>{if(event.key==='Escape'||mount.contains(event.target))event.stopPropagation();});
  setInterval(()=>{if(visible())void refresh();},3000);
  return {node,open:()=>presentPage(node),openDirectory:async directory=>{presentPage(node);await createTerminal(directory);},selectById:async id=>{presentPage(node);await refresh();const row=currentProject().terminals.find(row=>row.id===id);if(row)await select(row,true);},create:async(directory,shell)=>{preferredShell=shell||preferredShell;presentPage(node);await createTerminal(directory);},fit};
}
function terminalKey(event,seat){
  if(event.type!=='keydown')return true;const mod=/Mac|iPhone|iPad/.test(navigator.platform)?event.metaKey:event.ctrlKey&&event.shiftKey;if(!mod)return true;
  if(['+','=','-','0'].includes(event.key)){event.preventDefault();const settings=readDevTerminalSettings();settings.fontSize=event.key==='0'?14:Math.max(8,Math.min(40,settings.fontSize+(event.key==='-'?-1:1)));void saveDevTerminalSettings(settings).catch(()=>{});return false;}
  if(event.key.toLowerCase()==='c'&&seat.widget.term.hasSelection()){event.preventDefault();void navigator.clipboard.writeText(seat.widget.term.getSelection());return false;}return true;
}

function wireTerminalSeat(seat,{report,fit,status,isVisible}){
    seat.widget.term.onData(data=>{
      if(data.length>2*1024*1024){devStatus(status,'input_large');return;}
      seat.inputLane=seat.inputLane.then(async()=>{if(!seat.controlled||seat.row.phase!=='running')return;
        for(let start=0;start<data.length;){let end=Math.min(start+16000,data.length);if(end<data.length&&/[\uD800-\uDBFF]/.test(data[end-1]))end--;const chunk=data.slice(start,end);start=end;const result=await rpc('terminal.input',{id:seat.id,clientId:developmentClient,lease:seat.lease,sequence:seat.sequence,data:chunk});seat.sequence=result.sequence;}
      }).catch(error=>{seat.controlled=false;seat.widget.term.options.disableStdin=true;report(error);});
    });
    seat.widget.term.attachCustomKeyEventHandler(event=>terminalKey(event,seat));
    seat.unsubscribe=terminalAppearance.subscribe(async options=>{Object.assign(seat.widget.term.options,options);if(isVisible()){await fit();seat.widget.term.refresh(0,seat.widget.term.rows-1);}});
  }

async function terminalProjectRpc(project,method,params,localRoot){
    if(project.root===localRoot)return rpc(method,params);
    let route=project.route_id;if(!route)route=(await rpc('workspace.open',{root:project.root,trust:false})).id;
    const response=await fetch('/workspace/'+encodeURIComponent(route)+'/api/'+method,{method:'POST',headers:{...authHeaders(),'Content-Type':'application/json'},body:JSON.stringify(params)}),reply=await response.json();if(!reply.ok)throw CLATI18n.error(CLATI18n.view(()=>CLATI18n.present(reply.error?.presentation,reply.error?.message)));return reply.value;
  }

async function readTerminalOutput(seat,{visible,isSelected,refresh,has,attach,status}){
    seat.stream?.abort();const controller=new AbortController();seat.stream=controller;
    try{const response=await fetch(workspacePrefix+'/api/terminal/'+seat.id+'/'+seat.offset,{headers:authHeaders(),signal:controller.signal});if(!response.ok||!response.body){await refresh();return;}const reader=response.body.getReader(),decoder=new TextDecoder();let buffer='';
      while(!controller.signal.aborted){const {done,value}=await reader.read();if(done)break;buffer+=decoder.decode(value,{stream:true});let end;
        while((end=buffer.indexOf('\n\n'))!==-1){const frame=parseSseBlock(buffer.slice(0,end));buffer=buffer.slice(end+2);if(!frame)continue;const chunk=JSON.parse(frame.data);if(controller.signal.aborted)return;
          if(chunk.reset){seat.widget.term.reset();devStatus(status,'history_truncated');}await new Promise(resolve=>seat.widget.term.write(Uint8Array.from(atob(chunk.data),c=>c.charCodeAt(0)),resolve));seat.offset=chunk.offset;
          if(chunk.lease!==seat.lease){seat.controlled=false;seat.widget.term.options.disableStdin=true;devStatus(status,'observer');}if(chunk.ended){seat.row.phase='ended';devStatus(status,'ended');await refresh();return;}
        }
      }
      if(!controller.signal.aborted&&visible()&&isSelected()){await refresh();if(has()&&seat.row.phase==='running')setTimeout(()=>{if(visible()&&isSelected())void attach(seat);},1000);}
    }catch(error){if(!controller.signal.aborted){devStatus(status,'disconnected');if(visible()&&isSelected())setTimeout(()=>{if(visible()&&isSelected()&&has())void attach(seat);},1500);}}finally{if(seat.stream===controller)seat.stream=null;}
  }

function startTerminalStream(seat,ports){
  if(seat.reading&&seat.stream&&!seat.stream.signal.aborted)return seat.reading;
  const generation=(seat.attachVersion||0)+1;seat.attachVersion=generation;
  const previous=seat.reading||Promise.resolve();
  const reading=previous.catch(()=>{}).then(()=>{if(generation!==seat.attachVersion||!ports.visible()||!ports.isSelected()||seat.row.phase!=='running')return;return readTerminalOutput(seat,ports);});
  seat.reading=reading;void reading.finally(()=>{if(seat.reading===reading)seat.reading=null;}).catch(()=>{});return reading;
}
