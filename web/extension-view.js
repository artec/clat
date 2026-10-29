/* Extension list/detail presentation; plugin operations remain in their existing owner. */
function createExtensionNavigation(page,{cancel,refresh}){
  const panel=page.querySelector('.market-panel'),query=dom['market-search'];
  const footer=panel.querySelector('footer'),external=panel.querySelector('.market-toolbar a');
  const view=createNavigationPage(page,{title:()=>shellText('extensions'),query,searchLabel:()=>CLATI18n.msg('web.search_plugins')});
  const review=el('div'),content=el('section'),status=el('p','navigation-status'),catalogStatus=dom['market-status'];catalogStatus.className='navigation-status';catalogStatus.hidden=true;view.sidebar.insertBefore(catalogStatus,view.list);status.setAttribute('role','status');view.body.append(content,review);view.sidebar.insertBefore(status,view.list);
  const installed={rows:[],cards:new Map(),selected:null,query:'',scroll:0,read:0},discover={rows:[],cards:new Map(),selected:null,query:'',scroll:0,read:0};let current='installed',pending=false,epoch=0;
  const tabs=navigationTabs(['installed','discover'].map(key=>[key,()=>pageText(key)]),key=>void chooseTab(key));view.filters.append(tabs.element);tabs.select(current);
  view.actions([viewAction('refresh','refresh',()=>pageText('refresh'),()=>refresh(),true,()=>!pending)]);
  view.context({crumbs:()=>{const seat=current==='installed'?installed:discover;return [{label:()=>shellText('extensions'),run:()=>{delete page.dataset.detail;}},{label:()=>pageText(current),run:()=>void chooseTab(current)},...(seat.selected?[{label:()=>seat.rows.find(row=>row.id===seat.selected)?.name||seat.selected}]:[])];}});
  if(external){external.className='navigation-external';view.filters.append(external);}if(footer){footer.className='settings-note';view.body.append(footer);}
  panel.remove();showContent();
  function render(){const seat=current==='installed'?installed:discover,query=view.query.value.trim().toLocaleLowerCase();const top=view.list.scrollTop;view.list.replaceChildren();
    for(const row of seat.rows){if(query&&![row.id,row.name,row.summary,...(row.tags||[])].join(' ').toLocaleLowerCase().includes(query))continue;
      const button=navigationRow({title:row.name||row.id,meta:()=>current==='installed'?[row.version,CLATI18n.t(row.enabled?'web.enabled':'web.disabled')].filter(Boolean).join(' · '):row.summary,icon:row.runtime==='wasm-component'?'wasm':'mcp',selected:seat.selected===row.id,action:()=>void select(row.id)});button.disabled=pending;view.list.append(button);}
    if(!view.list.children.length)view.list.append(pageEmpty(current==='installed'?'empty_extensions':'empty_search'));view.list.scrollTop=top;
  }
  function showContent(open=false){refreshViewContext();
    const seat=current==='installed'?installed:discover,card=seat.cards.get(seat.selected);
    if(card){card.querySelector(':scope > h3')?.remove();const title=seat.rows.find(row=>row.id===seat.selected)?.name||seat.selected;CLATI18n.bind(view.name,'textContent',()=>title);if(open)page.dataset.detail='true';content.replaceChildren(card);return;}
    CLATI18n.bind(view.name,'textContent',()=>pageText(current));delete page.dataset.detail;
    const items=discover.rows.slice(0,6).map(row=>({title:row.name||row.id,description:()=>[CLATI18n.msg('web.'+(row.status==='available'?'available_short':row.status==='withdrawn'?'withdrawn_short':'preview_short')),row.summary].join(' · '),icon:row.runtime==='wasm-component'?'wasm':'mcp',action:()=>void openCatalog(row.id)}));
    if(!items.length)items.push({title:()=>pageText('discover'),description:()=>pageText('extension_intro_description'),icon:'wasm',action:()=>void chooseTab('discover')});
    content.replaceChildren(navigationOverview({title:()=>pageText('extension_intro_title'),description:()=>pageText('extension_intro_description'),icon:'wasm',items}));
    if(discover.rows.length)content.append(el('p','settings-note',()=>CLATI18n.msg(state.marketFallback?'web.built_in_preview_pi_at_cn_unavailable':'web.public_catalog')));
  }
  async function openCatalog(id){if(await chooseTab('discover'))await select(id);}
  async function select(id){
    if(pending)return;const request=++epoch;await cancel();if(request!==epoch)return;
    const seat=current==='installed'?installed:discover;seat.selected=id;showContent(true);view.body.scrollTop=0;render();
  }
  async function chooseTab(key){
    if(pending)return false;if(key===current)return true;
    const request=++epoch,old=current==='installed'?installed:discover;old.query=query.value;old.scroll=view.list.scrollTop;old.read=view.body.scrollTop;
    await cancel();if(request!==epoch)return false;current=key;catalogStatus.hidden=key!=='discover';const seat=current==='installed'?installed:discover;
    query.value=seat.query;tabs.select(key);showContent(Boolean(seat.selected));render();view.list.scrollTop=seat.scroll;view.body.scrollTop=seat.read;return true;
  }
  query.addEventListener('input',()=>{(current==='installed'?installed:discover).query=query.value;render();});
  function update(key,rows,cards){
    const seat=key==='installed'?installed:discover;seat.rows=rows;seat.cards=cards;
    if(seat.selected&&!rows.some(row=>row.id===seat.selected))seat.selected=null;
    if(key===current||!installed.selected&&current==='installed'){
      const top=view.body.scrollTop;showContent();view.body.scrollTop=top;render();
    }
  }
  return {review,status,update,render,setPending(value){pending=value;refreshViewContext();for(const button of view.head.querySelectorAll('button'))button.disabled=value;for(const button of tabs.element.querySelectorAll('button'))button.disabled=value;render();},leave(){epoch++;},get current(){return current;}};
}
