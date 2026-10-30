/* Shared manual project intents and presentation; Host owns metadata and ordering. */
(() => {
  const text=(key,args={})=>CLATI18n.msg('project_org.'+key,args);
  function node(tag,cls='',label){const item=document.createElement(tag);item.className=cls;if(label)CLATI18n.bind(item,'textContent',label);return item;}
  function button(cls,label,run){const item=node('button',cls,label);item.type='button';item.onclick=run;return item;}
  function create({request,refresh,icon}){
    const records=new Map(),busy=new Set();let ordered=[],activeAnchor=null,closeMenu=()=>{},closeDialog=()=>{};
    const current=row=>records.get(row.root)||row;
    function accept(rows){
      const present=new Set(rows.map(row=>row.root));for(const root of records.keys())if(!present.has(root))records.delete(root);
      ordered=rows.map(row=>{const value=records.get(row.root);if(value){Object.assign(value,row);return value;}records.set(row.root,row);return row;});return ordered;
    }
    async function update(row,edit){
      row=current(row);const key=row.root;if(busy.has(key))return false;busy.add(key);
      try{const target=row.project_id?{projectId:row.project_id}:{routeId:row.route_id};await request('workspace.update',{...target,edit});await refresh(true);return true;}
      finally{busy.delete(key);}
    }
    function rename(row){
      closeMenu();closeDialog();row=current(row);
      const dialog=node('dialog','task-editor project-rename-dialog'),form=node('form'),head=node('header','page-header'),label=node('label'),input=node('input'),error=node('p','error'),actions=node('div','project-rename-actions');
      CLATI18n.attribute(dialog,'aria-label',()=>text('rename'));head.append(node('h2','',()=>text('rename')));input.type='text';input.value=row.title||'';input.autocomplete='off';label.append(node('span','',()=>text('name')),input);error.setAttribute('role','alert');
      const save=button('primary',()=>text('save'),()=>{}),cancel=button('secondary',()=>text('cancel'),()=>dialog.close());save.type='submit';actions.append(cancel,save);
      form.append(head,label,node('p','settings-note',()=>text('name_hint')),error,actions);dialog.append(form);document.body.append(dialog);
      const close=()=>{dialog.close();dialog.remove();if(closeDialog===close)closeDialog=()=>{};};closeDialog=close;dialog.onclose=()=>{dialog.remove();if(closeDialog===close)closeDialog=()=>{};};
      dialog.onkeydown=event=>{event.stopPropagation();if(event.key==='Escape'&&!event.isComposing){event.preventDefault();close();}};
      form.onsubmit=async event=>{event.preventDefault();if(save.disabled)return;save.disabled=true;CLATI18n.bind(error,'textContent',()=>'');try{if(await update(row,{title:input.value}))close();}catch(failure){if(dialog.isConnected)CLATI18n.bind(error,'textContent',()=>failure.message);}finally{save.disabled=false;}};
      dialog.showModal();input.focus();input.select();
    }
    function menu(row,anchor,event,extra=[]){
      if(!event&&activeAnchor===anchor){closeMenu();return;}
      closeMenu();activeAnchor=anchor;row=current(row);const wasPinned=!!row.pinned,popup=node('div','session-context-menu');popup.setAttribute('role','menu');
      const items=[['rename','edit',()=>rename(row)], [wasPinned?'unpin':'pin',wasPinned?'unpin':'pin',()=>update(row,{pinned:!wasPinned})],['reset','restore',()=>update(row,{reset_title:true})],...extra];
      const error=node('p','error');error.setAttribute('role','alert');
      function close(){popup.remove();document.removeEventListener('pointerdown',outside,true);document.removeEventListener('keydown',key,true);if(closeMenu===close){closeMenu=()=>{};activeAnchor=null;}}
      function outside(e){if(!popup.contains(e.target)&&!anchor.contains(e.target))close();}
      function key(e){if(e.key==='Escape'){e.preventDefault();e.stopPropagation();close();if(anchor.isConnected)anchor.focus();}else if(['ArrowDown','ArrowUp'].includes(e.key)){e.preventDefault();const rows=[...popup.querySelectorAll('button:not(:disabled)')],i=rows.indexOf(document.activeElement);rows[(i+(e.key==='ArrowDown'?1:rows.length-1))%rows.length]?.focus();}}
      for(const [name,glyph,run]of items){const action=button('',null,async()=>{action.disabled=true;try{const done=await run();if(done!==false)close();}catch(failure){if(popup.isConnected)CLATI18n.bind(error,'textContent',()=>failure.message);}finally{action.disabled=false;}});action.setAttribute('role','menuitem');action.append(icon(glyph),node('span','',typeof name==='function'?name:()=>text(name)));popup.append(action);}
      popup.append(error);document.body.append(popup);const rect=anchor.getBoundingClientRect();
      popup.style.left=Math.max(12,Math.min(event?.clientX??rect.right-popup.offsetWidth,innerWidth-popup.offsetWidth-12))+'px';popup.style.top=Math.max(12,Math.min(event?.clientY??rect.bottom+6,innerHeight-popup.offsetHeight-12))+'px';
      closeMenu=close;document.addEventListener('pointerdown',outside,true);document.addEventListener('keydown',key,true);popup.querySelector('button')?.focus();
    }
    function menuButton(row,extra=[]){const item=button('icon-button project-more',null,()=>menu(row,item,null,extra));item.append(icon('more'));for(const attr of ['aria-label','title'])CLATI18n.attribute(item,attr,()=>text('more'));item.setAttribute('aria-haspopup','menu');return item;}
    function bind(target,row,anchor,extra=[]){target.classList.add('project-organizable');target.oncontextmenu=event=>{event.preventDefault();menu(row,anchor,event,extra);};}
    function pin(row){const mark=icon('pin');mark.classList.add('project-pinned-mark');mark.classList.toggle('hidden',!current(row).pinned);CLATI18n.attribute(mark,'aria-label',()=>text('pin'));return mark;}
    function close(){closeMenu();closeDialog();}
    window.addEventListener('clat:project-activated',close);
    window.addEventListener('popstate',close);
    return {accept,get:root=>records.get(root),rows:()=>ordered,menuButton,bind,pin,close};
  }
  globalThis.CLATProjectOrganization=Object.freeze({create});
})();
