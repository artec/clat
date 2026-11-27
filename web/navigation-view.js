/* Shared navigation presentation only; callers own requests, permissions and data. */
function navigationIcon(label,icon,action){
  const button=shellButton('',icon,label,action);button.classList.add('icon-button');button.querySelector('.shell-label').hidden=true;return button;
}
function navigationSearch(input,label){
  input ||= el('input');input.type='search';const search=el('label','session-search');
  CLATI18n.attribute(input,'placeholder',label);CLATI18n.attribute(input,'aria-label',label);search.append(svgIcon('search'),input);return {element:search,input};
}
function navigationProjectScope({id,onChange}={}){
  const TRUSTED='trusted';
  let value=TRUSTED;
  const label=()=>value===TRUSTED
    ?CLATI18n.msg('history.all_projects')
    :projectDisplayName(value,context.organization?.get(value)?.title);
  const title=el('span','',label);
  const select=el('button','resource-section-select');
  select.type='button';if(id)select.id=id;
  select.setAttribute('aria-haspopup','menu');
  CLATI18n.attribute(select,'aria-label',()=>CLATI18n.msg('history.all_projects'));
  select.append(title,svgIcon('chevron-down'));
  const actions=el('div','project-list-actions resource-section-actions');
  const row=el('div','section-row resource-section-row');
  row.append(select,actions);
  const element=el('div','session-nav navigation-project-nav');
  element.append(row);
  function sync(){CLATI18n.bind(title,'textContent',label);}
  function set(next,notify=true){
    const changed=value!==next;value=next;sync();
    if(changed&&notify)onChange?.(value);
  }
  select.onclick=()=>{
    const projects=context.organization?.rows()||[];
    editorPopup([
      {key:TRUSTED,icon:'folder',label:()=>CLATI18n.msg('history.all_projects'),run:()=>set(TRUSTED)},
      ...projects.map(project=>({
        key:project.root,
        icon:project.pinned?'pin':'folder',
        label:()=>projectDisplayName(project.root,project.title),
        run:()=>set(project.root),
      })),
    ],select);
  };
  return {
    element,select,actions,label,
    get value(){return value;},
    set value(next){set(next,false);},
    get trusted(){return value===TRUSTED;},
    set(next){set(next,true);},
  };
}
function navigationTabs(items,choose){
  const tabs=el('nav','session-tabs navigation-tabs');tabs.setAttribute('role','tablist');const buttons=new Map();
  for(const [id,label]of items){const button=el('button');button.type='button';button.setAttribute('role','tab');button.onclick=()=>choose(id);CLATI18n.bind(button,'textContent',label);tabs.append(button);buttons.set(id,button);}
  tabs.onkeydown=event=>{if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;event.preventDefault();const rows=[...buttons],index=rows.findIndex(([,button])=>button===document.activeElement),next=event.key==='Home'?0:event.key==='End'?rows.length-1:(index+(event.key==='ArrowRight'?1:rows.length-1))%rows.length;choose(rows[next][0]);rows[next][1].focus();};
  return {element:tabs,select(id){for(const [key,button]of buttons){button.setAttribute('aria-selected',String(id===key));button.tabIndex=key===id?0:-1;}}};
}
function navigationRow({title,meta,icon='info',selected=false,action,className='navigation-row'}){
  const button=el('button',className),copy=el('span','navigation-row-copy');button.type='button';button.setAttribute('aria-current',String(selected));
  copy.append(el('strong','',title));if(meta)copy.append(el('small','',meta));button.append(svgIcon(icon),copy);button.onclick=action;return button;
}
function sidebarNavigationGroup(title){
  const group=el('section','sidebar-nav-group');
  const heading=el('h3','section-row sidebar-section-heading');
  heading.append(el('span','',title));
  group.append(heading);
  return group;
}
function navigationOverview({title,description,icon,items=[]}){
  const overview=el('section','navigation-overview');overview.append(svgIcon(icon),el('h2','',title),el('p','',description));
  const options=el('div','navigation-options');
  for(const item of items){
    const button=el('button','navigation-option'),copy=el('span','navigation-option-copy');button.type='button';button.onclick=item.action;
    copy.append(el('strong','',item.title),el('small','',item.description));button.append(svgIcon(item.icon||icon),copy);options.append(button);
  }
  overview.append(options);return overview;
}
function createNavigationPage(page,{title,query,searchLabel,onOverview}={}){
  page.classList.add('navigation-layout');page.removeAttribute('aria-labelledby');
  const sidebar=el('aside','sidebar navigation-sidebar'),head=el('div','sidebar-head navigation-head'),heading=el('strong','',title);
  heading.id=page.id+'-navigation-title';page.setAttribute('aria-labelledby',heading.id);head.append(heading);const filters=el('div','navigation-filters'),list=el('nav','navigation-list');sidebar.append(head);
  let search;if(searchLabel){search=navigationSearch(query,searchLabel);sidebar.append(search.element);}sidebar.append(filters,list);
  const detail=el('section','navigation-detail'),bar=el('header','navigation-toolbar'),name=el('strong'),body=el('section','navigation-body');
  const back=navigationIcon(()=>pageText('back_list'),'previous',()=>{delete page.dataset.detail;});back.classList.add('navigation-back');bar.append(back);
  const overviewBack = onOverview && shellButton('', 'previous', () => CLATI18n.msg('experience.overview'), onOverview, false);
  if (overviewBack) {
    overviewBack.classList.add('navigation-overview-back'); overviewBack.dataset.action = 'back-overview';
    overviewBack.hidden = true; bar.append(overviewBack);
  }
  bar.append(name);detail.append(bar,body);page.append(sidebar,detail);
  function setOverviewReturn(visible) {
    if (!overviewBack) return;
    if (!visible && document.activeElement === overviewBack) {
      body.tabIndex = -1; body.focus({preventScroll:true});
    }
    overviewBack.hidden = !visible;
  }
  const chrome=navigationViewContext(page,sidebar,title);
  let scrolls=[];page.addEventListener('clat:page-capture',()=>{scrolls=[...page.querySelectorAll('*')].filter(n=>n.scrollHeight>n.clientHeight).map(n=>[n,n.scrollTop,n.scrollLeft]);});
  page.addEventListener('clat:page-enter',()=>requestAnimationFrame(()=>{for(const [node,top,left]of scrolls)if(page.contains(node)){node.scrollTop=top;node.scrollLeft=left;}}));
  return {page,sidebar,head,filters,list,detail,bar,name,body,overviewBack,query:search?.input,...chrome,setOverviewReturn,
    show(title){chrome.select(title);CLATI18n.bind(name,'textContent',typeof title==='function'?title:()=>title||'');page.dataset.detail='true';setOverviewReturn(true);},
    reset(){chrome.select(null);name.textContent='';body.replaceChildren();delete page.dataset.detail;setOverviewReturn(false);}};
}
