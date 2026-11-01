/* Extension list/detail presentation; plugin operations remain in their existing owner. */
function createExtensionNavigation(page,{cancel,refresh}){
  const panel=page.querySelector('.market-panel'),query=dom['market-search'];
  const footer=panel.querySelector('footer');
  const view=createNavigationPage(page,{title:()=>shellText('extensions'),query,searchLabel:()=>CLATI18n.msg('web.search_plugins')});
  const builtinConfig=el('section','builtin-config-editor');
  const review=el('div'),content=el('section'),frame=el('section','navigation-content'),status=el('p','navigation-status'),catalogStatus=dom['market-status'];catalogStatus.className='navigation-status';catalogStatus.hidden=true;view.sidebar.insertBefore(catalogStatus,view.list);status.setAttribute('role','status');frame.append(builtinConfig,content,review);view.body.append(frame);view.sidebar.insertBefore(status,view.list);
  const seats=Object.fromEntries(['installed','builtins','discover'].map(key=>[key,{rows:[],cards:new Map(),selected:null,query:'',scroll:0,read:0,configuring:false}])),{installed,discover}=seats;let current='builtins',pending=false,epoch=0,beforeLeaveBuiltins=()=>true;
  const tabs=navigationTabs(['builtins','installed','discover'].map(key=>[key,()=>tabTitle(key)]),key=>void chooseTab(key));view.filters.append(tabs.element);tabs.select(current);
  const website=viewAction('extension-website','open',()=>CLATI18n.msg('web.open_pi_at_cn'),()=>{
    // Desktop's navigation handler opens external HTTP(S) URLs in the system
    // browser. Use it directly rather than depending on WebView popup support.
    if(window.__TAURI__)window.location.assign('https://pi.at.cn');
    else window.open('https://pi.at.cn','_blank','noopener,noreferrer');
  },false);website.hidden=()=>current!=='discover';
  const configure=viewAction('builtin-config','settings',()=>CLATI18n.msg('extension.config_title'),()=>openBuiltinConfiguration(),true,()=>!pending);
  view.actions([viewAction('refresh','refresh',()=>pageText('refresh'),()=>refresh(),true,()=>!pending),website,configure]);
  view.context({crumbs:()=>{const seat=seats[current];return [{label:()=>shellText('extensions'),run:()=>{delete page.dataset.detail;}},{label:()=>tabTitle(current),run:()=>void chooseTab(current)},...(seat.selected?[{label:()=>extensionLabel(seat.rows.find(row=>row.id===seat.selected),'name')||seat.selected}]:[])];}});
  if(footer){footer.className='settings-note navigation-note';frame.append(footer);}
  panel.remove();showContent();
  function render(){const seat=seats[current],query=view.query.value.trim().toLocaleLowerCase();const top=view.list.scrollTop;view.list.replaceChildren();
    for(const row of seat.rows){if(query&&![row.id,extensionLabel(row,'name'),extensionLabel(row,'summary'),...(row.tags||[])].join(' ').toLocaleLowerCase().includes(query))continue;
      const button=navigationRow({title:()=>extensionLabel(row,'name')||row.id,meta:()=>current==='builtins'?CLATI18n.msg('extension.active_instances',{count:row.instances.filter(instance=>instance.phase==='active').length}):current==='installed'?[row.version,CLATI18n.t(row.enabled?'web.enabled':'web.disabled')].filter(Boolean).join(' · '):row.summary,icon:current==='builtins'?'extensions':row.runtime==='wasm-component'?'wasm':'mcp',selected:seat.selected===row.id,action:()=>void select(row.id)});button.dataset.pluginId=row.id;button.disabled=pending;view.list.append(button);}
    if(!view.list.children.length)view.list.append(pageEmpty(current==='installed'?'empty_extensions':'empty_search'));view.list.scrollTop=top;
  }
  function showContent(open=false){builtinConfig.hidden=current!=='builtins'||!seats[current].configuring;refreshViewContext();
    const seat=seats[current],card=seat.cards.get(seat.selected);
    if(current==='builtins'&&seat.configuring){CLATI18n.bind(view.name,'textContent',()=>CLATI18n.msg('extension.config_title'));if(open)page.dataset.detail='true';content.replaceChildren();return;}
    if(card){card.querySelector(':scope > h3')?.remove();const title=seat.rows.find(row=>row.id===seat.selected)?extensionLabel(seat.rows.find(row=>row.id===seat.selected),'name'):seat.selected;CLATI18n.bind(view.name,'textContent',()=>title);if(open)page.dataset.detail='true';content.replaceChildren(card);return;}
    CLATI18n.bind(view.name,'textContent',()=>tabTitle(current));delete page.dataset.detail;
    if(current==='builtins'){content.replaceChildren(navigationOverview({title:()=>CLATI18n.msg('extension.bundled'),description:()=>CLATI18n.msg('extension.static_configuration'),icon:'extensions',items:seat.rows.slice(0,6).map(row=>({title:()=>extensionLabel(row,'name'),description:()=>extensionLabel(row,'summary'),icon:'extensions',action:()=>void select(row.id)}))}));return;}
    const items=discover.rows.slice(0,6).map(row=>({title:extensionLabel(row,'name')||row.id,description:()=>[CLATI18n.msg('web.'+(row.status==='available'?'available_short':row.status==='withdrawn'?'withdrawn_short':'preview_short')),row.summary].join(' · '),icon:current==='builtins'?'extensions':row.runtime==='wasm-component'?'wasm':'mcp',action:()=>void openCatalog(row.id)}));
    if(!items.length)items.push({title:()=>pageText('discover'),description:()=>pageText('extension_intro_description'),icon:'extensions',action:()=>void chooseTab('discover')});
    content.replaceChildren(navigationOverview({title:()=>pageText('extension_intro_title'),description:()=>pageText('extension_intro_description'),icon:'extensions',items}));
    if(discover.rows.length)content.append(el('p','settings-note',()=>CLATI18n.msg(state.marketFallback?'web.built_in_preview_pi_at_cn_unavailable':'web.public_catalog')));
  }
  async function openBuiltinConfiguration(){
    if(pending)return;
    if(current!=='builtins'&&!await chooseTab('builtins'))return;
    const seat=seats.builtins;seat.selected=null;seat.configuring=true;showContent(true);view.body.scrollTop=0;render();
  }
  async function openCatalog(id){if(await chooseTab('discover'))await select(id);}
  async function select(id,approved=false){
    if(pending)return;
    if(!approved&&current==='builtins'&&seats[current].configuring&&!beforeLeaveBuiltins(()=>void select(id,true)))return;
    const request=++epoch;await cancel();if(request!==epoch)return;
    const seat=seats[current];seat.configuring=false;seat.selected=id;showContent(true);view.body.scrollTop=0;render();
  }
  async function chooseTab(key,approved=false){
    if(!approved&&current==='builtins'&&key!==current&&!beforeLeaveBuiltins(()=>void chooseTab(key,true)))return false;
    if(pending)return false;if(key===current)return true;
    const request=++epoch,old=seats[current];old.query=query.value;old.scroll=view.list.scrollTop;old.read=view.body.scrollTop;
    await cancel();if(request!==epoch)return false;current=key;catalogStatus.hidden=key!=='discover';const seat=seats[current];
    query.value=seat.query;tabs.select(key);showContent(Boolean(seat.selected||seat.configuring));render();view.list.scrollTop=seat.scroll;view.body.scrollTop=seat.read;return true;
  }
  query.addEventListener('input',()=>{(seats[current]).query=query.value;render();});
  function update(key,rows,cards){
    const seat=seats[key];seat.rows=rows;seat.cards=cards;
    if(seat.selected&&!rows.some(row=>row.id===seat.selected))seat.selected=null;
    if(key===current||!installed.selected&&current==='installed'){
      const top=view.body.scrollTop;showContent();view.body.scrollTop=top;render();
    }
  }
  function tabTitle(key){return key==='builtins'?CLATI18n.msg('extension.builtins'):pageText(key);}
  CLATI18n.onChange(render);
  return {guardBuiltins(run){beforeLeaveBuiltins=run;},builtinConfig,review,status,update,render,setPending(value){pending=value;refreshViewContext();for(const button of view.head.querySelectorAll('button'))button.disabled=value;for(const button of tabs.element.querySelectorAll('button'))button.disabled=value;render();},leave(){epoch++;},get current(){return current;}};
}
