/* Fetch/search capability state is configuration, not a connection probe. */
function installWebSettings() {
  const nav=document.querySelector('.settings-nav'),content=document.querySelector('.settings-content');
  const msg=key=>CLATI18n.msg('webtool.'+key);
  const tab=el('button','',()=>msg('title'));tab.type='button';tab.dataset.settingsCategory='web';
  const panel=el('section','settings-section');panel.dataset.settingsPanel='web';panel.hidden=true;
  const provider=el('select');
  for(const [value,key]of [['http','http'],['deepseek','deepseek']]){const option=el('option','',()=>msg(key));option.value=value;provider.append(option);}
  const source=el('select');for(const key of ['independent','model']){const option=el('option','',()=>pageText('key_'+key));option.value=key;source.append(option);}
  const key=el('input');key.type='password';key.autocomplete='new-password';key.maxLength=4096;
  const clear=el('input');clear.type='checkbox';
  const row=(label,input)=>{const node=el('label','form-row');node.append(el('span','',label),input);return node;};
  const status=el('p','settings-note');status.setAttribute('role','status');
  const fetchState=el('p','settings-note'),searchState=el('p','settings-note');
  const save=el('button','primary',()=>msg('save'));save.type='button';
  const fetch=el('section','settings-subsection'),search=el('section','settings-subsection');
  fetch.append(el('h4','',()=>pageText('fetch_heading')),row(()=>msg('fetch_provider'),provider),fetchState);
  search.append(el('h4','',()=>pageText('search_heading')),row(()=>pageText('key_source'),source),row(()=>msg('key'),key),row(()=>msg('clear_key'),clear),searchState);
  panel.append(el('h3','',()=>msg('title')),el('p','settings-note',()=>msg('description')),fetch,search,status,save);nav.append(tab);content.append(panel);
  let generation=0,dirty=false;
  const visibility=()=>{key.closest('label').hidden=source.value==='model';clear.closest('label').hidden=source.value==='model';};source.onchange=visibility;visibility();
  panel.addEventListener('input',()=>{dirty=true;generation++;});
  async function refresh(){const request=++generation;try{const view=await rpc('web.settings.get',{});if(request!==generation)return;
    if(!dirty){provider.value=view.fetch_provider;source.value=view.use_model_key?'model':'independent';visibility();}
    CLATI18n.bind(fetchState,'textContent',()=>pageText(view.fetch_available?'fetch_configured':'fetch_unconfigured'));
    CLATI18n.bind(searchState,'textContent',()=>msg(view.search_available?'available':'not_configured'));
  }catch(error){if(request===generation)CLATI18n.bind(status,'textContent',()=>error.message);}}
  dom['settings-dialog'].addEventListener('clat:settings-leave',()=>{generation++;key.value='';clear.checked=false;});
  dom['settings-dialog'].addEventListener('clat:settings-saved',event=>{if(event.detail===panel)dirty=false;});
  tab.onclick=()=>{selectSettingsCategory('web');void refresh();};
  save.onclick=async()=>{save.disabled=true;save.dataset.saving='true';const request=++generation;const payload={fetch_provider:provider.value,use_model_key:source.value==='model',api_key:source.value==='independent'?key.value||null:null,clear_key:source.value==='independent'&&clear.checked};key.value='';
    try{await rpc('web.settings.set',payload);if(request!==generation)return;clear.checked=false;dirty=false;acknowledgeSettingsSave(panel);CLATI18n.bind(status,'textContent',()=>msg('saved'));await refresh();}
    catch(error){if(request===generation)CLATI18n.bind(status,'textContent',()=>error.message);}finally{save.disabled=false;delete save.dataset.saving;}
  };
}
