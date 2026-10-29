function createApplicationBreadcrumbs(host,shell){
  const overflow=el('details','breadcrumb-overflow'),summary=el('summary'),menu=el('div','breadcrumb-menu');summary.append(svgIcon('more'));CLATI18n.attribute(summary,'aria-label',()=>viewText('parent_levels'));overflow.append(summary);menu.hidden=true;host.after(menu);overflow.ontoggle=()=>{menu.hidden=!overflow.open;};
  let crumbs=[],signature='',nodes=[];
  function render(view){
    crumbs=(view.crumbs?.()||[]).filter(crumb=>String((typeof crumb.label==='function'?crumb.label():crumb.label)||'').trim());const next=JSON.stringify(crumbs.map(c=>[typeof c.label==='function'?c.label():c.label,Boolean(c.run),Boolean(c.close),Boolean(c.session)]));
    if(next!==signature){signature=next;overflow.open=false;menu.hidden=true;host.replaceChildren();nodes=[];menu.replaceChildren();
      crumbs.forEach((crumb,index)=>{const row=el('span','breadcrumb-level');if(index)row.append(el('span','application-separator','/'));const name=el(crumb.run?'button':'span','breadcrumb-name',crumb.label);CLATI18n.attribute(name,'title',crumb.label);if(crumb.run){name.type='button';name.onclick=crumb.run;}row.append(name);
        if(crumb.session){const badge=el('small','conversation-indicator');badge.setAttribute('role','status');row.append(badge);row.dataset.session='true';}
        if(crumb.close){const close=navigationIcon(()=>viewText('close_tool'),'close',crumb.close);close.id='workspace-tool-close';row.append(close);}host.append(row);nodes.push(row);
      });
    }
    updateStatus();requestAnimationFrame(fit);
  }
  function updateStatus(){const badge=host.querySelector('.conversation-indicator');if(!badge)return;const status=shell.tools?.status();badge.textContent=status?.pending?CLATI18n.msg('workflow.pending_count',{count:status.pending}):status?.unread?workflowText('updated'):status?.running?CLATI18n.msg('web.running_short'):'';}
  function fit(){
    for(const node of nodes)node.hidden=false;overflow.remove();overflow.open=false;menu.hidden=true;menu.replaceChildren();if(nodes.length<3)return;
    const width=()=>nodes.filter(node=>!node.hidden).reduce((sum,node)=>{const name=node.querySelector('.breadcrumb-name');return sum+node.getBoundingClientRect().width+Math.max(0,name.scrollWidth-name.clientWidth);},overflow.isConnected?overflow.offsetWidth:0);const candidates=nodes.slice(1,-1).filter(node=>!node.querySelector('.conversation-indicator')?.textContent);for(const node of candidates){if(width()<=host.clientWidth+1)break;if(!overflow.isConnected)host.insertBefore(overflow,nodes[1]);node.hidden=true;const crumb=crumbs[nodes.indexOf(node)],button=el('button','',crumb.label);button.type='button';button.disabled=!crumb.run;button.onclick=()=>{overflow.open=false;crumb.run?.();};menu.append(button);}
  }
  new ResizeObserver(()=>requestAnimationFrame(fit)).observe(host);
  document.addEventListener('pointerdown',event=>{if(!overflow.contains(event.target)&&!menu.contains(event.target))overflow.open=false;});
  menu.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();overflow.open=false;summary.focus();}});
  overflow.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();overflow.open=false;summary.focus();}});
  return {render,close:()=>{overflow.open=false;}};
}
