/* Settings map public state and explicit operations, never implement authority. */

function parseMcpArgs(value){
  const raw=value.trim();if(raw.startsWith('[')){
    const parsed=JSON.parse(raw);if(!Array.isArray(parsed)||!parsed.every(item=>typeof item==='string'))throw Error('invalid arguments');
    return parsed;
  }
  return raw.split('\n').map(value=>value.trim()).filter(Boolean);
}
function parseMcpPairs(value){
  const raw=value.trim();if(raw.startsWith('{')){
    const parsed=JSON.parse(raw);if(!parsed||Array.isArray(parsed)||typeof parsed!=='object'||!Object.values(parsed).every(item=>typeof item==='string'))throw Error('invalid map');
    return parsed;
  }
  const result={};for(const line of raw.split('\n').map(s=>s.trim()).filter(Boolean)){
    const i=line.indexOf('=');if(i<=0)throw Error('invalid map entry');
    const key=line.slice(0,i).trim();const val=line.slice(i+1);
    if(!key||key==='__proto__'||key==='constructor'||key==='prototype')throw Error('invalid map key');
    result[key]=val;
  }
  return result;
}
function installMcpSettings(){
  const page=dom['settings-dialog'],nav=page.querySelector('.settings-nav'),host=page.querySelector('.settings-content');
  const t=key=>CLATI18n.msg('mcp_settings.'+key);
  const navButton=el('button','',()=>t('title'));navButton.type='button';navButton.dataset.settingsCategory='mcp';navButton.onclick=()=>selectSettingsCategory('mcp');nav.append(navButton);
  const section=el('section','settings-section');section.dataset.settingsPanel='mcp';section.hidden=true;
  section.append(el('h2','',()=>t('title')),el('p','settings-note',()=>t('description')));
  const scope=el('select');CLATI18n.attribute(scope,'aria-label',()=>t('scope'));
  for(const value of ['user','project']){const option=el('option','',()=>t(value));option.value=value;scope.append(option);}
  const heading=el('div','settings-group-heading');heading.append(el('strong','',()=>t('scope')),scope);
  const list=el('div','settings-card'),message=el('p','settings-note'),form=el('div','settings-card settings-form');form.hidden=true;
  const add=el('button','secondary',()=>t('add'));add.type='button';
  section.append(heading,add,list,form,message);host.append(section);
  const name=el('input'),transport=el('select'),command=el('input'),args=el('textarea'),cwd=el('input'),url=el('input'),env=el('textarea'),headers=el('textarea'),enabled=el('input');
  name.maxLength=64;name.autocomplete='off';args.rows=2;env.rows=3;headers.rows=3;enabled.type='checkbox';enabled.checked=true;
  for(const mode of ['stdio','http']){const option=el('option','',()=>t(mode));option.value=mode;transport.append(option);}
  const field=(key,control)=>{const label=el('label','',()=>t(key));label.append(control);form.append(label);return label;};
  field('name',name);field('transport',transport);
  const processFields=[field('command',command),field('args',args),field('cwd',cwd),field('env',env)];
  const httpFields=[field('url',url),field('headers',headers)];
  field('enabled',enabled);form.append(el('p','settings-note',()=>t('credential_notice')));
  const updateTransport=()=>{const http=transport.value==='http';for(const item of processFields)item.hidden=http;for(const item of httpFields)item.hidden=!http;};
  transport.onchange=updateTransport;
  const actions=el('div','settings-actions'),save=el('button','primary',()=>t('save')),cancel=el('button','secondary',()=>t('cancel'));
  for(const b of [save,cancel])b.type='button';actions.append(save,cancel);form.append(actions);
  let loaded=null,revision='missing',serial=0,maskedUrl=false;
  const project=()=>state.workbench?.project?.root||'';
  const active=()=>currentSettingsCategory==='mcp'&&pageIsOpen(page);
  const showStatus=value=>CLATI18n.bind(message,'textContent',()=>value);
  const action=(title,handler,parent)=>{const node=el('button','secondary',()=>t(title));node.type='button';node.onclick=handler;parent.append(node);};
  function render(){
    list.replaceChildren();const rows=loaded?.[scope.value]||[];
    for(const item of rows){
      const row=el('div','settings-row'),copy=el('span','settings-row-copy'),buttons=el('span','settings-actions');
      copy.append(el('strong','',item.name),el('small','',()=>item.transport+' · '+t(item.enabled?'enabled':'disabled')));
      action('edit',()=>void openEdit(item.name),buttons);
      action(item.enabled?'disable':'enable',()=>void toggle(item),buttons);
      if(item.enabled)action('test',()=>void test(item),buttons);
      action('delete',()=>void remove(item),buttons);row.append(copy,buttons);list.append(row);
    }
    if(!rows.length)list.append(el('p','settings-note',()=>t('empty')));
    if(scope.value==='user'&&(loaded?.legacy||[]).length){list.append(el('p','settings-note',()=>t('legacy')));
      for(const item of loaded.legacy)list.append(el('div','settings-row',()=>item.name+' · '+item.transport));}
    if(scope.value==='project'&&(loaded?.user||[]).length){list.append(el('p','settings-note',()=>t('inherited')));
      for(const item of loaded.user)list.append(el('div','settings-row',()=>item.name+' · '+item.transport));}
    const servers=loaded?.runtime?.servers||[];
    if(servers.length)list.append(el('p','settings-note',()=>t('active')));
    for(const server of servers){
      const row=el('div','settings-row');
      row.append(el('strong','',server.name),el('small','',()=>server.transport+' · '+server.tools+' '+t('tools')));
      list.append(row);
    }
    const failures=loaded?.runtime?.failures||[];
    for(const failure of failures.slice(0,16))list.append(el('p','error',failure));
  }
  async function refresh(){
    const request=++serial;try{const value=await rpc('mcp.manage.list',{project_root:project()});
      if(request!==serial||!active())return;loaded=value;revision=value.revision;render();
      showStatus(value.runtime?value.runtime.connected+'/'+value.runtime.configured+' '+t('connected'):'');
    }catch(error){if(request===serial&&active())showStatus(error.message);}
  }
  function create(){
    maskedUrl=false;name.value='';name.disabled=false;transport.value='stdio';command.value='';args.value='';cwd.value='';url.value='';env.value='';headers.value='';enabled.checked=true;
    form.hidden=false;updateTransport();name.focus();
  }
  async function openEdit(id){
    const request=++serial;try{const value=await rpc('mcp.manage.get',{project_root:project(),scope:scope.value,name:id});
      if(request!==serial||!active())return;revision=value.revision;name.value=id;name.disabled=true;
      const entry=value.entry||{};maskedUrl=entry.url_has_query===true;transport.value=(entry.url||maskedUrl)?'http':'stdio';command.value=entry.command||'';url.value=entry.url||'';cwd.value=entry.cwd||'';enabled.checked=entry.enabled!==false;
      args.value='';env.value='';headers.value='';updateTransport();form.hidden=false;showStatus(t('credential_notice'));
    }catch(error){if(request===serial&&active())showStatus(error.message);}
  }
  async function toggle(item){
    const request=++serial;try{await rpc('mcp.manage.save',{project_root:project(),scope:scope.value,name:item.name,revision,enabled:!item.enabled});
      if(request===serial)await refresh();}catch(error){if(request===serial)showStatus(error.message);}
  }
  async function remove(item){
    if(!window.confirm(t('confirm_delete')))return;
    const request=++serial;try{await rpc('mcp.manage.remove',{project_root:project(),scope:scope.value,name:item.name,revision});
      if(request===serial)await refresh();}catch(error){if(request===serial)showStatus(error.message);}
  }
  async function test(item){
    if(!window.confirm(t('confirm_test')))return;
    const request=++serial;try{const value=await rpc('mcp.manage.test',{project_root:project(),scope:scope.value,name:item.name,confirm_execution:true});
      if(request===serial)showStatus(t('connected')+' · '+value.tools+' '+t('tools'));}catch(error){if(request===serial)showStatus(error.message);}
  }
  save.onclick=async()=>{
    const id=name.value.trim();if(!/^[A-Za-z0-9_-]{1,64}$/.test(id)){showStatus(t('invalid_name'));return;}
    const config=transport.value==='http'?{command:'',args:[],cwd:null,env:{}}:{command:command.value.trim(),url:null,headers:{},cwd:cwd.value.trim()||null};
    if(transport.value==='http'&&url.value.trim())config.url=url.value.trim();
    // The Host redacts credential-bearing URLs. Blank means unchanged, not removed.
    if(transport.value==='http'&&maskedUrl&&!url.value.trim())delete config.url;
    try{
      if(transport.value==='stdio'){
        if(args.value.trim())config.args=parseMcpArgs(args.value);
        if(env.value.trim())config.env=parseMcpPairs(env.value);
      }else if(headers.value.trim())config.headers=parseMcpPairs(headers.value);
    }catch{showStatus(t('invalid_json'));return;}
    const request=++serial;save.disabled=true;
    try{const result=await rpc('mcp.manage.save',{project_root:project(),scope:scope.value,name:id,revision,enabled:enabled.checked,config});
      if(request!==serial)return;form.hidden=true;env.value='';headers.value='';await refresh();acknowledgeSettingsSave(section);
      if((result.runtime_warnings||[]).length)showStatus(result.runtime_warnings.join('\n'));
    }catch(error){if(request===serial)showStatus(error.message);}
    finally{save.disabled=false;}
  };
  cancel.onclick=()=>{form.hidden=true;args.value='';env.value='';headers.value='';url.value='';maskedUrl=false;acknowledgeSettingsSave(section);};
  add.onclick=create;scope.onchange=()=>{form.hidden=true;render();};
  navButton.addEventListener('click',()=>void refresh());
  page.addEventListener('clat:page-enter',()=>{if(active())void refresh();});
  for(const event of ['clat:settings-leave','clat:page-leave'])page.addEventListener(event,()=>{
    serial++;form.hidden=true;args.value='';env.value='';headers.value='';url.value='';maskedUrl=false;
  });
  return {selectProject(){scope.value='project';selectSettingsCategory('mcp');void refresh();}};
}


/* Host-owned Skills. Project scope always follows the current mounted project route. */
function installSkillSettings(){
  const page=dom['settings-dialog'],nav=page.querySelector('.settings-nav'),content=page.querySelector('.settings-content');
  const t=key=>CLATI18n.msg('skill_settings.'+key);
  const button=el('button','',()=>t('title'));button.type='button';button.dataset.settingsCategory='skills';
  button.onclick=()=>selectSettingsCategory('skills');nav.append(button);
  const section=el('section','settings-section');section.dataset.settingsPanel='skills';section.hidden=true;
  section.append(el('h2','',()=>t('title')),el('p','settings-note',()=>t('description')));
  const scope=el('select');CLATI18n.attribute(scope,'aria-label',()=>t('scope'));
  for(const value of ['user','project']){const o=el('option','',()=>t(value));o.value=value;scope.append(o);}
  const bar=el('div','settings-group-heading');bar.append(el('strong','',()=>t('scope')),scope);
  const list=el('div','settings-card'),status=el('p','settings-note');
  const form=el('div','settings-card settings-form');form.hidden=true;
  const name=el('input'),description=el('input'),body=el('textarea'),execution=el('input');
  name.maxLength=64;name.autocomplete='off';description.maxLength=500;body.rows=9;
  execution.type='checkbox';
  const field=(caption,input)=>{const l=el('label','',()=>t(caption));l.append(input);form.append(l);};
  field('name',name);field('summary',description);field('instructions',body);field('requires_execution',execution);
  const actions=el('div','settings-actions'),save=el('button','primary',()=>t('save')),cancel=el('button','secondary',()=>t('cancel'));
  for(const b of [save,cancel])b.type='button';actions.append(save,cancel);form.append(actions);
  const add=el('button','secondary',()=>t('add'));add.type='button';
  section.append(bar,add,list,form,status);content.append(section);
  let epoch=0,revision='missing',editingName=null,loaded=null;
  const active=()=>currentSettingsCategory==='skills'&&pageIsOpen(page);
  function renderRows(){
    list.replaceChildren();const rows=loaded?.[scope.value]||[];
    for(const item of rows){
      const row=el('div','settings-row'),copy=el('span','settings-row-copy'),tools=el('span','settings-actions');
      copy.append(el('strong','',item.name),el('small','',item.description||''));
      const edit=el('button','secondary',()=>t('edit')),remove=el('button','secondary',()=>t('delete'));
      edit.type='button';remove.type='button';
      edit.onclick=()=>void openEdit(item.name);
      remove.onclick=()=>void removeSkill(item);
      tools.append(edit,remove);row.append(copy,tools);list.append(row);
    }
    if(!rows.length)list.append(el('p','settings-note',()=>t('empty')));
    const inherited=(loaded?.effective?.entries||[]).filter(item=>!rows.some(row=>row.name===item.name));
    if(inherited.length){const h=el('p','settings-note',()=>t('inherited'));list.append(h);
      for(const item of inherited.slice(0,64))list.append(el('div','settings-row',()=>item.name+' · '+item.source+' · '+item.description));}
  }
  async function refresh(){
    const request=++epoch;try{const data=await rpc('skill.manage.list',{});
      if(request!==epoch||!active())return;
      loaded=data;renderRows();CLATI18n.bind(status,'textContent',()=>data.project_root||'');
    }catch(error){if(request===epoch&&active())CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  function start(){revision='missing';editingName=null;name.value='';description.value='';body.value='';execution.checked=false;name.disabled=false;form.hidden=false;name.focus();}
  function parseText(value){
    const matched=value.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n([\s\S]*)$/);
    if(!matched)return {description:'',body:value,execution:false};
    const desc=matched[1].match(/^description:\s*(.+)$/m)?.[1]||'';
    let decoded=desc;try{decoded=JSON.parse(desc);}catch{}
    return {description:String(decoded),body:matched[2],execution:/^requires-execution:\s*true\s*$/m.test(matched[1])};
  }
  async function openEdit(value){
    const request=++epoch;try{const result=await rpc('skill.manage.get',{scope:scope.value,name:value});
      if(request!==epoch||!active())return;
      const parsed=parseText(result.text||'');editingName=value;revision=result.revision;
      name.value=value;name.disabled=true;description.value=parsed.description;body.value=parsed.body;execution.checked=parsed.execution;form.hidden=false;
      form.scrollIntoView({block:'nearest'});
    }catch(error){if(request===epoch&&active())CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  save.onclick=async()=>{
    const value=name.value.trim();
    if(!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(value)){CLATI18n.bind(status,'textContent',()=>t('invalid_name'));return;}
    const text='---\nname: '+value+'\ndescription: '+JSON.stringify(description.value.trim())+
      '\nrequires-execution: '+(execution.checked?'true':'false')+'\n---\n'+body.value;
    const request=++epoch;save.disabled=true;
    try{await rpc('skill.manage.save',{scope:scope.value,name:value,revision,text});
      if(request!==epoch)return;form.hidden=true;editingName=null;await refresh();void refreshCommandCatalog();
      acknowledgeSettingsSave(section);
    }catch(error){if(request===epoch)CLATI18n.bind(status,'textContent',()=>error.message);}
    finally{save.disabled=false;}
  };
  cancel.onclick=()=>{form.hidden=true;editingName=null;acknowledgeSettingsSave(section);};
  add.onclick=start;
  scope.onchange=()=>{form.hidden=true;editingName=null;renderRows();};
  async function removeSkill(item){
    if(!window.confirm(t('confirm_delete')))return;
    const request=++epoch;try{await rpc('skill.manage.remove',{scope:scope.value,name:item.name,revision:item.revision});
      if(request===epoch){await refresh();void refreshCommandCatalog();}}
    catch(error){if(request===epoch)CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  page.addEventListener('clat:page-enter',()=>{if(active())void refresh();});
  page.addEventListener('clat:settings-leave',()=>{epoch++;form.hidden=true;});
  page.addEventListener('clat:page-leave',()=>{epoch++;form.hidden=true;});
  button.addEventListener('click',()=>void refresh());
  return {refresh,selectProject(){scope.value='project';selectSettingsCategory('skills');void refresh();}};
}

function setupWechatUsers() {
  const panel=document.querySelector('[data-settings-panel=remote]');
  const section=el('section','settings-group-block');const list=el('div','settings-card');section.append(el('h3','',()=>pageText('authorized_users')),list);panel.append(section);
  let epoch=0;
  const active=()=>currentSettingsCategory==='remote'&&pageIsOpen(dom['settings-dialog']);
  async function refresh(){const request=++epoch;try{const data=await rpc('wechat.users.list',{});if(request!==epoch||!active())return;
    list.replaceChildren();for(const user of data.users){const row=el('article','project-setting-row'),copy=el('div');copy.append(el('strong','',user.user_id),el('small','',()=>[user.paired?pageText('paired_grant'):'',user.manual?pageText('manual_grant'):''].filter(Boolean).join(' · ')));
      if(user.sessions.length)copy.append(el('small','',user.sessions.join(' · ')));
      row.append(copy,pageAction('revoke',async()=>{if(!window.confirm(pageText('revoke_confirm')))return;const request=epoch;try{await rpc('wechat.user.revoke',{userId:user.user_id});if(request!==epoch||!active())return;const refreshed=request+1;await refresh();if(refreshed===epoch&&active())await refreshWechatStatus();}catch(error){if(request===epoch&&active())list.append(el('p','error',error.message));}}));list.append(row);}
    if(!data.users.length)list.append(pageEmpty('no_authorized_users'));
  }catch(error){if(request===epoch&&active())list.replaceChildren(el('p','error',error.message),pageAction('retry',()=>void refresh()));}}
  const heading=el('div','settings-group-heading');heading.append(section.querySelector('h3'),pageAction('refresh',()=>void refresh()));section.prepend(heading);
  dom['settings-dialog'].addEventListener('clat:page-enter',()=>{if(currentSettingsCategory==='remote')void refresh();});
  dom['settings-dialog'].addEventListener('clat:remote-enter',()=>void refresh());
  dom['settings-dialog'].addEventListener('clat:settings-leave',()=>epoch++);
  dom['settings-dialog'].addEventListener('clat:page-leave',()=>epoch++);
}
function setupWebAbout() {
  if(document.querySelector('[data-settings-category=desktop]'))return;
  const button=pageAction('updates_about',()=>selectSettingsCategory('desktop'));button.dataset.settingsCategory='desktop';document.querySelector('.settings-nav').append(button);
  const section=el('section','settings-section');section.dataset.settingsPanel='desktop';section.hidden=true;
  section.append(el('p','page-scope',()=>pageText('host_scope')),el('h3','','CLAT'),el('p','',()=>pageText('web_about')));
  const version=el('span'),status=el('p','settings-note');let epoch=0;
  const card=el('div','settings-card');card.append(settingsControlRow(()=>pageText('connected_host_version'),version));section.append(card,status);
  const link=el('a','settings-release-link',()=>pageText('releases'));link.href='https://github.com/artec/clat/releases';link.target='_blank';link.rel='noopener noreferrer';section.append(link);document.querySelector('.settings-content').append(section);
  async function refresh(){
    const request=++epoch;CLATI18n.bind(version,'textContent',()=>CLATI18n.msg('common.loading'));CLATI18n.bind(status,'textContent',()=>'');
    try{const value=await rpc('host.describe',{});if(request!==epoch)return;CLATI18n.bind(version,'textContent',()=>value.product_version||'—');}
    catch(error){if(request!==epoch)return;CLATI18n.bind(version,'textContent',()=>'—');CLATI18n.bind(status,'textContent',()=>CLATI18n.present(error));}
  }
  button.addEventListener('click',()=>void refresh());
  dom['settings-dialog'].addEventListener('clat:page-enter',()=>void refresh());
  dom['settings-dialog'].addEventListener('clat:page-leave',()=>epoch++);
}
function setupProjectPicker() {
  if(!window.__TAURI__)return;
  const button=pageAction('choose_directory',async()=>{
    try{const path=await window.__TAURI__.core.invoke('desktop_pick_directory');if(path){document.getElementById('workspace-root').value=path;document.getElementById('workspace-root').dispatchEvent(new Event('input',{bubbles:true}));}}
    catch(error){CLATI18n.bind(document.getElementById('workspace-error'),'textContent',()=>CLATI18n.present(error));}
  });document.getElementById('workspace-root').after(button);
}
