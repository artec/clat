/* Settings map public state and explicit operations, never implement authority. */
function setupWechatUsers() {
  const panel=document.querySelector('[data-settings-panel=wechat]');
  const section=el('section','settings-subsection');const list=el('div','');section.append(el('h3','',()=>pageText('authorized_users')),list);panel.append(section);
  let epoch=0;
  async function refresh(){const request=++epoch;try{const data=await rpc('wechat.users.list',{});if(request!==epoch||!pageIsOpen(dom['settings-dialog']))return;
    list.replaceChildren();for(const user of data.users){const row=el('article','project-setting-row'),copy=el('div');copy.append(el('strong','',user.user_id),el('small','',()=>[user.paired?pageText('paired_grant'):'',user.manual?pageText('manual_grant'):''].filter(Boolean).join(' · ')));
      if(user.sessions.length)copy.append(el('small','',user.sessions.join(' · ')));
      row.append(copy,pageAction('revoke',async()=>{if(!window.confirm(pageText('revoke_confirm')))return;try{await rpc('wechat.user.revoke',{userId:user.user_id});await refresh();await refreshWechatStatus();}catch(error){list.append(el('p','error',error.message));}}));list.append(row);}
    if(!data.users.length)list.append(pageEmpty('no_authorized_users'));
  }catch(error){if(request===epoch)list.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void refresh()));}}
  section.prepend(pageAction('refresh',()=>void refresh()));
  dom['settings-dialog'].addEventListener('clat:page-enter',()=>void refresh());
  dom['settings-dialog'].addEventListener('clat:page-leave',()=>epoch++);
}
function setupWebAbout() {
  if(document.querySelector('[data-settings-category=desktop]'))return;
  const button=pageAction('updates_about',()=>selectSettingsCategory('desktop'));button.dataset.settingsCategory='desktop';document.querySelector('.settings-nav').append(button);
  const section=el('section','settings-section');section.dataset.settingsPanel='desktop';section.hidden=true;
  section.append(el('p','page-scope',()=>pageText('host_scope')),el('h3','','CLAT'),el('p','',()=>pageText('web_about')));
  const link=el('a','',()=>pageText('releases'));link.href='https://github.com/artec/clat/releases';link.target='_blank';link.rel='noopener noreferrer';section.append(link);const version=el('p','settings-note');section.append(version);document.querySelector('.settings-content').append(section);dom['settings-dialog'].addEventListener('clat:page-enter',()=>{void rpc('host.describe',{}).then(value=>{version.textContent='Host · '+value.product_version;}).catch(error=>{version.textContent=error.message;});});
}
function setupProjectPicker() {
  if(!window.__TAURI__)return;
  const button=pageAction('choose_directory',async()=>{
    try{const path=await window.__TAURI__.core.invoke('desktop_pick_directory');if(path){document.getElementById('workspace-root').value=path;document.getElementById('workspace-root').dispatchEvent(new Event('input',{bubbles:true}));}}
    catch(error){CLATI18n.bind(document.getElementById('workspace-error'),'textContent',()=>CLATI18n.present(error));}
  });document.getElementById('workspace-root').after(button);
}
function setupPluginTabs() {
  const panel=dom['market-dialog'].querySelector('.market-panel'),tabs=el('nav','page-tabs');let selected='installed';
  const controls=[];const discover=['.market-toolbar','#market-status','#market-list'];
  const choose=key=>{selected=key;panel.querySelector('.plugin-installed').hidden=key!=='installed';for(const selector of discover)panel.querySelector(selector).hidden=key!=='discover';controls.forEach(b=>b.setAttribute('aria-selected',String(b.dataset.tab===key)));};
  for(const key of ['installed','discover']){const button=pageAction(key,()=>choose(key));button.dataset.tab=key;tabs.append(button);controls.push(button);}panel.querySelector('header').after(tabs);choose(selected);
}
