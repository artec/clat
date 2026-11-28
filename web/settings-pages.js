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

function managedSettingsEditor(section, t) {
  const group = el('section', 'settings-group-block');
  const heading = el('h4', '', () => t('add'));
  const card = el('div', 'settings-card settings-form managed-settings-editor');
  const add = el('button', 'primary', () => t('add'));
  const form = el('div', 'managed-settings-fields');
  const message = el('p', 'settings-note');
  message.setAttribute('role', 'status');
  add.type = 'button';
  form.hidden = true;
  card.append(add, form, message);
  group.append(heading, card);
  section.append(group);
  return {
    add, form, message,
    field(key, control) {
      const label = el('label', 'managed-settings-field');
      label.append(el('span', '', () => t(key)), control);
      form.append(label);
      return label;
    },
    open(field, editing = false) {
      CLATI18n.bind(heading, 'textContent', () => t(editing ? 'edit' : 'add'));
      form.hidden = false;
      add.hidden = true;
      group.scrollIntoView({block:'nearest'});
      field.focus({preventScroll:true});
    },
    close() {
      form.hidden = true;
      add.hidden = false;
      CLATI18n.bind(heading, 'textContent', () => t('add'));
    },
  };
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
  const list=el('div','settings-card managed-settings-list'),summary=el('p','settings-note');
  section.append(heading,list,summary);host.append(section);
  const editor=managedSettingsEditor(section,t);
  const {add,form,message,field}=editor;
  const name=el('input'),transport=el('select'),command=el('input'),args=el('textarea'),cwd=el('input'),url=el('input'),env=el('textarea'),headers=el('textarea'),enabled=el('input');
  name.maxLength=64;name.autocomplete='off';args.rows=2;env.rows=3;headers.rows=3;enabled.type='checkbox';enabled.checked=true;
  for(const mode of ['stdio','http']){const option=el('option','',()=>t(mode));option.value=mode;transport.append(option);}
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
      CLATI18n.bind(summary,'textContent',()=>value.runtime?value.runtime.connected+'/'+value.runtime.configured+' '+t('connected'):'');
      showStatus('');
    }catch(error){if(request===serial&&active())showStatus(error.message);}
  }
  function create(){
    maskedUrl=false;name.value='';name.disabled=false;transport.value='stdio';command.value='';args.value='';cwd.value='';url.value='';env.value='';headers.value='';enabled.checked=true;
    showStatus('');updateTransport();editor.open(name);
  }
  async function openEdit(id){
    const request=++serial;try{const value=await rpc('mcp.manage.get',{project_root:project(),scope:scope.value,name:id});
      if(request!==serial||!active())return;revision=value.revision;name.value=id;name.disabled=true;
      const entry=value.entry||{};maskedUrl=entry.url_has_query===true;transport.value=(entry.url||maskedUrl)?'http':'stdio';command.value=entry.command||'';url.value=entry.url||'';cwd.value=entry.cwd||'';enabled.checked=entry.enabled!==false;
      args.value='';env.value='';headers.value='';updateTransport();showStatus('');editor.open(transport,true);
    }catch(error){if(request===serial&&active())showStatus(error.message);}
  }
  async function toggle(item){
    const request=++serial;try{await rpc('mcp.manage.save',{project_root:project(),scope:scope.value,name:item.name,revision,enabled:!item.enabled});
      if(request===serial)await refresh();}catch(error){if(request===serial)showStatus(error.message);}
  }
  async function remove(item){
    if(!await CLATDialog.confirm(t('confirm_delete')))return;
    const request=++serial;try{await rpc('mcp.manage.remove',{project_root:project(),scope:scope.value,name:item.name,revision});
      if(request===serial)await refresh();}catch(error){if(request===serial)showStatus(error.message);}
  }
  async function test(item){
    if(!await CLATDialog.confirm(t('confirm_test')))return;
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
      if(request!==serial)return;editor.close();env.value='';headers.value='';await refresh();acknowledgeSettingsSave(section);
      if((result.runtime_warnings||[]).length)showStatus(result.runtime_warnings.join('\n'));
    }catch(error){if(request===serial)showStatus(error.message);}
    finally{save.disabled=false;}
  };
  cancel.onclick=()=>{editor.close();args.value='';env.value='';headers.value='';url.value='';maskedUrl=false;showStatus('');acknowledgeSettingsSave(section);add.focus();};
  add.onclick=create;scope.onchange=()=>{editor.close();showStatus('');render();};
  navButton.addEventListener('click',()=>void refresh());
  page.addEventListener('clat:page-enter',()=>{if(active())void refresh();});
  for(const event of ['clat:settings-leave','clat:page-leave'])page.addEventListener(event,()=>{
    serial++;editor.close();args.value='';env.value='';headers.value='';url.value='';maskedUrl=false;
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
  const list=el('div','settings-card managed-settings-list'),summary=el('p','settings-note');
  section.append(bar,list,summary);content.append(section);
  const editor=managedSettingsEditor(section,t);
  const {add,form,message:status,field}=editor;
  const name=el('input'),description=el('input'),body=el('textarea'),execution=el('input');
  name.maxLength=64;name.autocomplete='off';description.maxLength=500;body.rows=9;
  execution.type='checkbox';
  field('name',name);field('summary',description);field('instructions',body);field('requires_execution',execution);
  const actions=el('div','settings-actions'),save=el('button','primary',()=>t('save')),cancel=el('button','secondary',()=>t('cancel'));
  for(const b of [save,cancel])b.type='button';actions.append(save,cancel);form.append(actions);
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
      loaded=data;renderRows();CLATI18n.bind(summary,'textContent',()=>data.project_root||'');
      CLATI18n.bind(status,'textContent',()=>'');
    }catch(error){if(request===epoch&&active())CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  function start(){revision='missing';editingName=null;name.value='';description.value='';body.value='';execution.checked=false;name.disabled=false;CLATI18n.bind(status,'textContent',()=>'');editor.open(name);}
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
      name.value=value;name.disabled=true;description.value=parsed.description;body.value=parsed.body;execution.checked=parsed.execution;CLATI18n.bind(status,'textContent',()=>'');editor.open(description,true);
    }catch(error){if(request===epoch&&active())CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  save.onclick=async()=>{
    const value=name.value.trim();
    if(!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(value)){CLATI18n.bind(status,'textContent',()=>t('invalid_name'));return;}
    const text='---\nname: '+value+'\ndescription: '+JSON.stringify(description.value.trim())+
      '\nrequires-execution: '+(execution.checked?'true':'false')+'\n---\n'+body.value;
    const request=++epoch;save.disabled=true;
    try{await rpc('skill.manage.save',{scope:scope.value,name:value,revision,text});
      if(request!==epoch)return;editor.close();editingName=null;await refresh();void refreshCommandCatalog();
      acknowledgeSettingsSave(section);
    }catch(error){if(request===epoch)CLATI18n.bind(status,'textContent',()=>error.message);}
    finally{save.disabled=false;}
  };
  cancel.onclick=()=>{editor.close();editingName=null;CLATI18n.bind(status,'textContent',()=>'');acknowledgeSettingsSave(section);add.focus();};
  add.onclick=start;
  scope.onchange=()=>{editor.close();editingName=null;CLATI18n.bind(status,'textContent',()=>'');renderRows();};
  async function removeSkill(item){
    if(!await CLATDialog.confirm(t('confirm_delete')))return;
    const request=++epoch;try{await rpc('skill.manage.remove',{scope:scope.value,name:item.name,revision:item.revision});
      if(request===epoch){await refresh();void refreshCommandCatalog();}}
    catch(error){if(request===epoch)CLATI18n.bind(status,'textContent',()=>error.message);}
  }
  page.addEventListener('clat:page-enter',()=>{if(active())void refresh();});
  page.addEventListener('clat:settings-leave',()=>{epoch++;editor.close();});
  button.addEventListener('click',()=>void refresh());
  return {refresh,selectProject(){scope.value='project';selectSettingsCategory('skills');void refresh();}};
}

const computerUseText = (key, args) => CLATI18n.msg('cua_settings.' + key, args);
function computerUseSettingsPanel(page, category) {
  const t = computerUseText;
  const nav = el('button', '', () => t('title'));
  nav.type = 'button';
  nav.dataset.settingsCategory = category;
  page.querySelector('.settings-nav').append(nav);
  const section = el('section', 'settings-section');
  section.dataset.settingsPanel = category;
  section.hidden = true;
  const description = el('p', 'settings-note', () => t('description'));
  section.append(el('h2', '', () => t('title')), description);
  page.querySelector('.settings-content').append(section);

  const system = settingsGroup(section, () => t('system_access'));
  const row = el('div', 'settings-row');
  const copy = el('span', 'settings-row-copy');
  const access = el('strong');
  const hint = el('small', '', () => t('system_hint'));
  const reload = el('button', 'secondary', () => t('refresh'));
  reload.dataset.action = 'refresh-accessibility';
  const requestAccess = el('button', 'primary', () => t('request_accessibility'));
  const actions = el('div', 'settings-row-actions');
  reload.type = 'button';
  requestAccess.type = 'button';
  requestAccess.dataset.action = 'request-accessibility';
  requestAccess.hidden = true;
  actions.append(reload, requestAccess);
  copy.append(access, hint);
  row.append(copy, actions);
  system.append(row);
  const list = settingsGroup(section, () => t('application_consents'));
  const consentsSection = list.parentElement;
  const status = el('p', 'settings-note');
  status.setAttribute('role', 'status');
  section.append(status);
  return { nav, description, systemHeading: system.parentElement.querySelector('h4'),
    access, hint, reload, requestAccess, list, consentsSection, status };
}

function computerUseConsentRow(consent, onRevoke) {
  const t = computerUseText;
  const item = el('div', 'settings-row');
  const text = el('div', 'settings-row-copy');
  const name = () => consent.name || t('unnamed_app');
  text.append(el('strong', '', name), el('small', '', () => {
    const scope = CLATI18n.msg(consent.scope === 'session' ? 'cua.scope_session' : 'cua.scope_always');
    const decision = CLATI18n.msg(consent.allowed ? 'cua.decision_allow' : 'cua.decision_deny');
    return String(scope) + ' · ' + decision;
  }));
  const revoke = el('button', 'secondary', () => t('revoke'));
  revoke.type = 'button';
  CLATI18n.attribute(revoke, 'aria-label', () => t('revoke_app', { app: name() }));
  revoke.onclick = async () => {
    revoke.disabled = true;
    try { await onRevoke(t('revoked', { app: name() })); }
    finally { revoke.disabled = false; }
  };
  item.append(text, revoke);
  return item;
}

function renderComputerUseConsents(list, view, onRevoke) {
  const entries = (Array.isArray(view.consents) ? view.consents : []).filter(entry =>
    /^[a-f0-9]{64}$/.test(entry?.app_id) && ['session','always'].includes(entry.scope));
  list.replaceChildren(...entries.map(entry => computerUseConsentRow(entry, success => onRevoke(entry, success))));
  if (!entries.length) {
    const empty = el('div', 'settings-row');
    empty.append(el('span', 'settings-note', () => CLATI18n.msg('cua.consent_empty')));
    list.append(empty);
  }
}

function installComputerUseSettings() {
  const category = 'computer-use';
  const page = dom['settings-dialog'];
  const t = computerUseText;
  const {nav, description, systemHeading, access, hint, reload, requestAccess, list, consentsSection, status} = computerUseSettingsPanel(page, category);
  const active = () => currentSettingsCategory === category && pageIsOpen(page);
  const selection = () => JSON.stringify([workspacePrefix, state.sessionId, state.selectionGeneration]);
  let generation = 0;
  let shownFor = null;
  let requestedAccess = false;
  let windowsHost = false;
  let restarting = false;
  let checkingManually = false;
  const command = (args = '') => rpc('command.run', {
    command: '/ui-consents' + args, expected_selection_generation: state.selectionGeneration,
  });
  const showStatus = value => CLATI18n.bind(status, 'textContent', () => typeof value === 'function' ? value() : value);
  const setAccess = key => {
    access.dataset.systemStatus = key;
    requestAccess.hidden = windowsHost || key !== 'not_granted';
    CLATI18n.bind(access, 'textContent', () => t(key));
    CLATI18n.bind(hint, 'textContent', () => t(windowsHost ? 'windows_hint'
      : key === 'not_granted' ? 'access_recovery' : 'system_hint'));
    CLATI18n.bind(description, 'textContent', () => t(windowsHost ? 'windows_description' : 'description'));
    CLATI18n.bind(systemHeading, 'textContent', () => t(windowsHost ? 'windows_system_access' : 'system_access'));
  };
  async function change(args, target, success) {
    const current = () => active() && target === selection();
    if (!current()) return;
    try {
      const reply = await command(args);
      if (current()) await refresh(() => success(reply));
    } catch (error) {
      if (current()) showStatus(() => CLATI18n.present(error.presentation, error.message));
    }
  }
  async function restartAfterGrant(restart) {
    restarting = true;
    requestAccess.disabled = true;
    showStatus(() => t('restarting'));
    try {
      await restart();
      requestedAccess = false;
    } finally {
      restarting = false;
      requestAccess.disabled = false;
    }
  }
  async function refresh(success = '', manual = false) {
    if (!active() || restarting || (!manual && checkingManually)) return;
    const request = ++generation, target = selection();
    const current = () => request === generation && active() && target === selection();
    if (shownFor !== target) {
      shownFor = target;
      list.replaceChildren();
      consentsSection.hidden = true;
    }
    const available = state.workbench?.builtin_availability?.['builtin.native_ui'];
    hint.hidden = available?.enabled !== true;
    reload.hidden = hint.hidden;
    if (hint.hidden) {
      consentsSection.hidden = true;
      setAccess(state.workbench ? 'unavailable' : 'loading');
      showStatus(available?.reason ? CLATI18n.present(available.reason) : '');
      return;
    }
    if (consentsSection.hidden) setAccess('loading');
    reload.disabled = true;
    showStatus('');
    let restartNeeded = false;
    try {
      const view = await command();
      if (!current()) return;
      windowsHost = view.system?.platform === 'windows';
      if (windowsHost) {
        requestedAccess = false;
        setAccess(view.system.desktop === 'ready' ? 'windows_ready'
          : view.system.desktop === 'locked' ? 'windows_locked' : 'windows_unknown');
      } else {
        setAccess(view.accessibility_granted === true ? 'granted'
          : view.accessibility_granted === false ? 'not_granted' : 'unknown');
      }
      renderComputerUseConsents(list, view, (entry, success) =>
        change(` revoke ${entry.scope} ${entry.app_id}`, target, () => success));
      consentsSection.hidden = !Array.isArray(view.consents);
      if (view.accessibility_granted === true) requestedAccess = false;
      restartNeeded = !windowsHost && manual && requestedAccess && view.accessibility_granted === false
        && Boolean(window.__TAURI__);
      if (restartNeeded) {
        const restart = window.refreshDesktopAccessibility;
        if (!restart) throw CLATI18n.msg('cua_settings.restart_unavailable');
        await restartAfterGrant(restart);
        return;
      }
      showStatus(success || (manual && view.accessibility_granted === false ? () => t('access_still_missing') : ''));
    } catch (error) {
      if (current()) {
        if (!restartNeeded) setAccess(windowsHost ? 'windows_unknown' : 'unknown');
        showStatus(() => CLATI18n.present(error, error.message || String(error)));
      }
    } finally {
      if (current()) reload.disabled = false;
    }
  }
  nav.onclick = () => { selectSettingsCategory(category); void refresh(); };
  reload.onclick = async () => {
    checkingManually = true;
    try { await refresh('', true); }
    finally { checkingManually = false; }
  };
  requestAccess.onclick = async () => {
    requestAccess.disabled = true;
    try {
      const target = selection();
      const reply = await command(' request-accessibility');
      if (active() && target === selection()) {
        requestedAccess = true;
        await refresh(() => CLATI18n.present(reply.presentation, reply.message));
      }
    } catch (error) {
      if (active()) showStatus(() => CLATI18n.present(error.presentation, error.message));
    } finally { requestAccess.disabled = false; }
  };
  page.addEventListener('clat:page-enter', () => void refresh());
  page.addEventListener('clat:settings-leave', () => { generation++; shownFor = null; });
  window.addEventListener('clat:workbench-updated', () => { if (active()) void refresh(); });
}

const REMOTE_CHANNELS = {
  wechat: {
    name: 'web.wechat', icon: 'wechat', bind: 'web.bind_wechat',
    description: 'web.remote_wechat_binding_hint', pairing: 'web.wechat_pairing_instruction',
    replace: 'web.replace_the_current_binding_existing_user_grants_are_kept_until_the_new_qr_is_conf',
    unbind: 'web.remove_the_wechat_credential_paired_users_and_chat_mappings',
  },
  feishu: {
    name: 'web.feishu', icon: 'feishu', bind: 'web.bind_feishu',
    description: 'web.remote_feishu_binding_hint', pairing: 'web.feishu_pairing_instruction',
    replace: 'web.feishu_replace_confirm',
    unbind: 'web.remove_the_feishu_app_paired_users_and_chat_mappings',
  },
  telegram: {
    name: 'web.telegram', icon: 'telegram', bind: 'web.bind_telegram',
    description: 'web.remote_telegram_binding_hint', pairing: 'web.telegram_pairing_instruction',
    replace: 'web.telegram_replace_confirm',
    unbind: 'web.remove_the_telegram_bot_paired_users_and_chat_mappings',
  },
};

function installRemoteChannelTabs(node, select) {
  const channels = Object.keys(REMOTE_CHANNELS);
  return Object.fromEntries(channels.map((channel, index) => {
    const spec = REMOTE_CHANNELS[channel];
    const tab = el('button', 'remote-channel-tab');
    const status = el('span', 'remote-channel-state');
    const counts = el('small');
    tab.type = 'button';
    tab.id = 'remote-tab-' + channel;
    tab.dataset.channel = channel;
    tab.setAttribute('role', 'tab');
    tab.setAttribute('aria-controls', 'remote-channel-panel');
    const name = el('strong', '', () => CLATI18n.msg(spec.name));
    name.id = tab.id + '-name';
    status.id = tab.id + '-state';
    counts.id = tab.id + '-counts';
    tab.setAttribute('aria-labelledby', name.id);
    tab.setAttribute('aria-describedby', status.id + ' ' + counts.id);
    const mark = el('span', 'authorization-mark');
    mark.append(svgIcon(spec.icon));
    tab.append(mark, name, status, counts);
    tab.onclick = () => select(channel);
    tab.onkeydown = event => {
      const next = { ArrowLeft: index - 1, ArrowRight: index + 1, Home: 0, End: channels.length - 1 }[event.key];
      if (next === undefined) return;
      event.preventDefault();
      const target = channels[(next + channels.length) % channels.length];
      select(target);
      node.querySelector('#remote-tab-' + target).focus();
    };
    node.append(tab);
    return [channel, { tab, status, counts }];
  }));
}

function renderRemoteChannelTab({ tab, status, counts }, selected, snapshot) {
  tab.setAttribute('aria-selected', String(selected));
  tab.tabIndex = selected ? 0 : -1;
  status.dataset.bound = String(Boolean(snapshot?.bound));
  CLATI18n.bind(status, 'textContent', () => CLATI18n.msg(snapshot?.error
    ? 'web.unavailable_short' : snapshot ? (snapshot.bound ? 'web.bound_short' : 'web.not_bound') : 'web.checking'));
  CLATI18n.bind(counts, 'textContent', () => snapshot && !snapshot.error
    ? CLATI18n.msg('web.dynamic_value_paired_value_chats', { value1: snapshot.paired_users || 0, value2: snapshot.mapped_chats || 0 })
    : '');
}

function renderRemoteAuthorizedUsers(list, users, revoke) {
  list.replaceChildren();
  for (const user of users) {
    const row = el('article', 'project-setting-row');
    const copy = el('div');
    const grants = () => [user.paired ? pageText('paired_grant') : '', user.manual ? pageText('manual_grant') : '']
      .filter(Boolean).join(' · ');
    copy.append(el('strong', '', user.user_id), el('small', '', grants));
    row.append(svgIcon('user'), copy, pageAction('revoke', () => revoke(user.user_id)));
    list.append(row);
  }
}

function remoteFeishuCredentialValid(value) {
  return value.length > 0 && new TextEncoder().encode(value).length <= 8192 && ![...value].some(character => {
    const code = character.codePointAt(0);
    return code <= 31 || (code >= 127 && code <= 159);
  });
}

function installRemoteWechatBinding(get, message, refresh, render) {
  const qr = get('wechat-qr'), image = get('wechat-qr-image');
  const state = get('wechat-qr-state'), form = get('wechat-verify-row'), code = get('wechat-verify-code');
  const label = key => CLATI18n.bind(state, 'textContent', () => CLATI18n.msg(key));
  const phases = {
    waiting: 'web.waiting_for_scan',
    scanned: 'web.scanned_confirm_on_the_phone',
    need_verify_code: 'web.enter_the_verification_code_shown_by_wechat',
    verify_code_blocked: 'web.verification_was_blocked_wait_before_trying_again',
    expired: 'web.qr_code_expired_start_a_new_binding',
    already_bound: 'web.the_account_is_already_bound_and_returned_no_new_credential',
  };
  let generation = 0, url, timer, scanning = false;
  function clear() {
    generation++;
    clearTimeout(timer);
    if (url) URL.revokeObjectURL(url);
    url = undefined;
    image.removeAttribute('src');
    form.hidden = qr.hidden = true;
    code.value = '';
    scanning = false;
  }
  async function poll(request, verifyCode) {
    if (request !== generation) return;
    try {
      const value = await rpc('wechat.binding.poll', verifyCode ? { verifyCode } : {});
      if (request !== generation) return;
      if (value.state === 'confirmed') {
        clear();
        message(CLATI18n.msg('web.binding_confirmed_create_a_one_time_user_pairing_code_next'));
        refresh();
        return;
      }
      label(phases[value.state] || 'web.unavailable_short');
      form.hidden = value.state !== 'need_verify_code';
      if (!form.hidden) code.focus();
      else if (value.state === 'waiting' || value.state === 'scanned') timer = setTimeout(() => poll(request), 500);
      else { scanning = false; render(); }
    } catch (error) {
      if (request !== generation) return;
      clear();
      message(error.message, true);
      render();
    }
  }
  form.onsubmit = event => {
    event.preventDefault();
    const value = code.value.trim();
    if (!/^[A-Za-z0-9]{1,32}$/.test(value)) {
      message(CLATI18n.msg('web.verification_code_must_be_1_32_letters_or_digits'), true);
      return;
    }
    form.hidden = true;
    code.value = '';
    message('');
    void poll(generation, value);
  };
  get('wechat-qr-close').onclick = () => { clear(); message(''); render(); get('remote-bind').focus(); };
  return {
    clear,
    scanning: () => scanning,
    async start(replace, current) {
      clear();
      const value = await rpc('wechat.binding.start', { replace });
      if (!current()) return;
      url = URL.createObjectURL(new Blob([value.qr_svg], { type: 'image/svg+xml' }));
      image.src = url;
      label('web.waiting_for_scan');
      scanning = true;
      qr.hidden = false;
      get('wechat-qr-close').focus();
      void poll(generation);
    },
  };
}

function remoteSettingsView(get) {
  return {
    panel: get('remote-channel-panel'), bind: get('remote-bind'), unbind: get('remote-unbind'),
    receive: get('remote-feishu-receive'), connection: get('remote-feishu-connection'),
    pair: get('remote-pair'), form: get('feishu-binding-form'), save: get('feishu-save'), cancel: get('feishu-binding-cancel'),
    telegramForm: get('telegram-binding-form'), telegramSave: get('telegram-save'), telegramCancel: get('telegram-binding-cancel'),
    users: get('remote-users'), accessDetail: get('remote-access-detail'), accessTitle: get('remote-access-title'),
    bindingDetail: get('remote-binding-detail'), pairing: get('remote-pairing'), refresh: get('remote-refresh'),
  };
}

function remoteChannelReceives(channel) {
  return channel === 'feishu' || channel === 'telegram';
}

function credentialDraftOpen(view) {
  return !view.form.hidden || !view.telegramForm.hidden;
}

function closeRemoteForms(view) {
  view.form.hidden = true;
  view.form.reset();
  view.telegramForm.hidden = true;
  view.telegramForm.reset();
}

function channelConnected(status) {
  return Boolean(status?.receiving && status.online);
}

function remoteBindingDetail(channel, status, spec) {
  if (status?.error) return status.error;
  if (channel === 'feishu' && status?.app_id && channelConnected(status)) {
    return CLATI18n.msg('web.feishu_receive_on', { app_id: status.app_id });
  }
  if (channel === 'feishu' && status?.app_id) return CLATI18n.msg('web.remote_feishu_bound_hint', { app_id: status.app_id });
  if (channel === 'telegram' && status?.bound && channelConnected(status)) return CLATI18n.msg('web.telegram_receive_on');
  if (channel === 'telegram' && status?.bound) return CLATI18n.msg('web.telegram_bound_hint');
  return CLATI18n.msg(spec.description);
}

function renderRemoteConnection(view, channel, status, busy) {
  const visible = remoteChannelReceives(channel) && Boolean(status?.bound) && !status.error;
  const connected = channelConnected(status);
  view.receive.hidden = !visible;
  view.connection.hidden = !visible || connected;
  if (!visible) return;
  if (!connected) {
    const key = status.receiving
      ? (channel === 'telegram' ? 'web.telegram_receive_connecting' : 'web.feishu_receive_connecting')
      : (channel === 'telegram' ? 'web.telegram_receive_off' : 'web.feishu_receive_off');
    CLATI18n.bind(view.connection, 'textContent', () => CLATI18n.msg(key));
  }
  const receiving = Boolean(status.receiving);
  const action = receiving ? 'web.feishu_receive_stop' : 'web.feishu_receive_start';
  CLATI18n.bind(view.receive, 'textContent', () => CLATI18n.msg(action));
  view.receive.classList.toggle('primary', !receiving);
  view.receive.classList.toggle('secondary', receiving);
  view.receive.disabled = busy || credentialDraftOpen(view);
}

function installRemoteReceive(view, snapshots, selectedChannel, perform, bindingMessage) {
  view.receive.onclick = () => {
    const receiving = !snapshots.get(selectedChannel())?.receiving;
    void perform(bindingMessage, async (selected, current) => {
      if (!remoteChannelReceives(selected)) return false;
      await rpc(selected + '.connection.set', { receiving });
      if (!current()) return false;
      bindingMessage('');
      return true;
    }, true);
  };
}

function setRemoteText(node, value) {
  CLATI18n.bind(node, 'textContent', () => value);
}

function renderRemoteSettings(view, channel, tabs, snapshots, busy) {
  for (const [id, tab] of Object.entries(tabs)) renderRemoteChannelTab(tab, id === channel, snapshots.get(id));
  const spec = REMOTE_CHANNELS[channel], status = snapshots.get(channel);
  const bound = Boolean(status?.bound);
  const drafting = credentialDraftOpen(view);
  view.panel.setAttribute('aria-labelledby', tabs[channel].tab.id + '-name');
  view.bind.disabled = !status || Boolean(status.error) || busy || drafting;
  view.unbind.hidden = view.pair.hidden = !bound;
  view.unbind.disabled = view.pair.disabled = !bound || busy || drafting;
  view.save.disabled = busy || !status || Boolean(status.error);
  view.telegramSave.disabled = view.save.disabled;
  view.cancel.disabled = busy;
  view.telegramCancel.disabled = busy;
  view.bind.classList.toggle('primary', !bound);
  view.bind.classList.toggle('secondary', bound);
  setRemoteText(view.bind, CLATI18n.msg(bound ? 'web.replace_binding' : spec.bind));
  setRemoteText(view.bindingDetail, remoteBindingDetail(channel, status, spec));
  view.bindingDetail.classList.toggle('remote-binding-line', remoteChannelReceives(channel) && channelConnected(status));
}

function createRemoteSettingsRefresh(view, selectedChannel, snapshots, active, render, revoke) {
  let read = 0;
  return function refresh() {
    const request = ++read, selected = selectedChannel();
    const current = () => request === read && active();
    view.refresh.disabled = true;
    const statuses = Object.keys(REMOTE_CHANNELS).map(async id => {
      try {
        const value = await rpc(id + '.binding.status', {});
        if (current()) snapshots.set(id, value);
      } catch (error) {
        if (current()) snapshots.set(id, { error: error.message });
      }
      if (current()) render();
    });
    const users = (async () => {
      try {
        const data = await rpc(selected + '.users.list', {});
        if (!current()) return;
        renderRemoteAuthorizedUsers(view.users, data.users, revoke);
        setRemoteText(view.accessDetail, CLATI18n.msg(data.users.length ? 'web.remote_access_hint' : 'web.remote_access_empty'));
      } catch (error) {
        if (!current()) return;
        view.users.replaceChildren();
        setRemoteText(view.accessDetail, error.message);
      }
    })();
    return Promise.all([...statuses, users]).then(() => { if (current()) view.refresh.disabled = false; });
  };
}

function revokeRemoteUser(userId, perform, accessMessage) {
  void perform(accessMessage, async (selected, current) => {
    const channel = CLATI18n.msg(REMOTE_CHANNELS[selected].name);
    const confirm = CLATI18n.msg('web.remote_revoke_confirm', { channel, user: userId });
    if (!await CLATDialog.confirm(confirm) || !current()) return false;
    await rpc(selected + '.user.revoke', { userId });
    return true;
  }, true);
}

function installRemoteBindingActions({
  view, get, selectedChannel, bindingIsBound, perform, bindingMessage, closeForm, render, clearPairing, wechat,
}) {
  view.bind.onclick = () => {
    if (openRemoteCredentialForm(view, get, selectedChannel())) {
      bindingMessage('');
      render();
      return;
    }
    const replace = Boolean(bindingIsBound());
    void perform(bindingMessage, async (selected, current) => {
      if (replace && !await CLATDialog.confirm(CLATI18n.msg(REMOTE_CHANNELS[selected].replace))) return;
      if (!current()) return;
      clearPairing();
      await wechat.start(replace, current);
    });
  };
  view.cancel.onclick = () => { closeForm(); bindingMessage(''); render(); view.bind.focus(); };
  view.telegramCancel.onclick = view.cancel.onclick;
  view.form.onsubmit = event => {
    event.preventDefault();
    if (selectedChannel() !== 'feishu' || view.save.disabled) return;
    const appId = get('feishu-app-id').value.trim(), appSecret = get('feishu-app-secret').value.trim();
    if (!remoteFeishuCredentialValid(appId) || !remoteFeishuCredentialValid(appSecret)) {
      bindingMessage(CLATI18n.msg('web.feishu_credential_invalid'), true);
      return;
    }
    const replace = Boolean(bindingIsBound());
    void perform(bindingMessage, async (selected, current) => {
      if (replace && !await CLATDialog.confirm(CLATI18n.msg(REMOTE_CHANNELS[selected].replace))) return false;
      if (!current() || view.form.hidden) return false;
      await rpc('feishu.binding.save', { appId, appSecret, replace });
      if (current()) {
        closeForm();
        clearPairing();
        bindingMessage(CLATI18n.msg('web.feishu_saved'));
      }
      return true;
    }, true);
  };
  view.unbind.onclick = () => void perform(bindingMessage, async (selected, current) => {
    if (!await CLATDialog.confirm(CLATI18n.msg(REMOTE_CHANNELS[selected].unbind)) || !current()) return false;
    await rpc(selected + '.binding.unbind', { confirm: 'unbind-' + selected });
    if (current()) {
      clearPairing();
      wechat.clear();
    }
    return true;
  }, true);
}

function installRemotePairingAction(view, get, perform, accessMessage, clearPairing) {
  view.pair.onclick = () => void perform(accessMessage, async (selected, current) => {
    clearPairing();
    const value = await rpc(selected + '.pairing.create', {});
    if (!current()) return;
    get('remote-pairing-code').textContent = value.code;
    setRemoteText(get('remote-pairing-instruction'), CLATI18n.msg(REMOTE_CHANNELS[selected].pairing, { command: '/pair <code>', minutes: 60 }));
    view.pairing.hidden = false;
  });
}

function openRemoteCredentialForm(view, get, channel) {
  if (channel === 'feishu') {
    view.telegramForm.hidden = true;
    view.form.hidden = false;
    get('feishu-app-id').focus();
    return true;
  }
  if (channel === 'telegram') {
    view.form.hidden = true;
    view.telegramForm.hidden = false;
    get('telegram-token').focus();
    return true;
  }
  return false;
}

function installTelegramBinding(view, get, selectedChannel, bindingIsBound, perform, bindingMessage, closeForm, clearPairing) {
  view.telegramForm.onsubmit = event => {
    event.preventDefault();
    if (selectedChannel() !== 'telegram' || view.telegramSave.disabled) return;
    const token = get('telegram-token').value.trim();
    if (!remoteFeishuCredentialValid(token)) {
      bindingMessage(CLATI18n.msg('web.telegram_credential_invalid'), true);
      return;
    }
    const replace = Boolean(bindingIsBound());
    void perform(bindingMessage, async (selected, current) => {
      if (replace && !await CLATDialog.confirm(CLATI18n.msg(REMOTE_CHANNELS[selected].replace))) return false;
      if (!current() || view.telegramForm.hidden) return false;
      await rpc('telegram.binding.save', { token, replace });
      if (current()) {
        closeForm();
        clearPairing();
        bindingMessage(CLATI18n.msg('web.telegram_saved'));
      }
      return true;
    }, true);
  };
}

function remoteLinkChannel(detail) {
  return REMOTE_CHANNELS[detail] ? detail : 'wechat';
}

function clearRemotePairing(view, get) {
  view.pairing.hidden = true;
  get('remote-pairing-code').textContent = '';
  setRemoteText(get('remote-pairing-instruction'), '');
}

function scheduleRemoteConnectionPoll(ctx) {
  clearTimeout(ctx.connectionPoll);
  const status = ctx.snapshots.get(ctx.channel);
  if (!ctx.active() || !remoteChannelReceives(ctx.channel) || !status?.receiving || status.online) return;
  ctx.connectionPoll = setTimeout(() => { if (ctx.active()) void ctx.refresh(); }, 1000);
}

function renderRemotePage(ctx) {
  const busy = ctx.pending.has(ctx.channel) || ctx.wechat.scanning();
  renderRemoteSettings(ctx.view, ctx.channel, ctx.tabs, ctx.snapshots, busy);
  renderRemoteConnection(ctx.view, ctx.channel, ctx.snapshots.get(ctx.channel), busy);
  scheduleRemoteConnectionPoll(ctx);
}

function clearRemoteTransient(ctx) {
  ctx.generation += 1;
  clearTimeout(ctx.connectionPoll);
  ctx.wechat.clear();
  ctx.closeForm();
  ctx.clearPairing();
  ctx.bindingMessage('');
  ctx.accessMessage('');
  ctx.view.users.replaceChildren();
  setRemoteText(ctx.view.accessDetail, CLATI18n.msg('web.checking'));
}

function selectRemoteChannel(ctx, id, reload = true) {
  if (id === ctx.channel) return;
  clearRemoteTransient(ctx);
  ctx.channel = id;
  ctx.render();
  if (reload && ctx.active()) ctx.refresh();
}

async function performRemoteOperation(ctx, sectionMessage, operation, refreshAfter = false) {
  const selected = ctx.channel, owner = ctx.generation;
  if (!ctx.active() || ctx.pending.has(selected)) return;
  const current = () => ctx.active() && owner === ctx.generation;
  ctx.pending.add(selected);
  sectionMessage('');
  ctx.render();
  try {
    if (await operation(selected, current) && refreshAfter && ctx.active()) await ctx.refresh();
  } catch (error) {
    if (current()) sectionMessage(error.message, true);
  } finally {
    ctx.pending.delete(selected);
    if (ctx.active()) ctx.render();
  }
}

function installRemoteSettings() {
  const settings = dom['settings-dialog'];
  const get = id => document.getElementById(id);
  const view = remoteSettingsView(get);
  const setMessage = (id, value, error = false) => {
    const node = get(id);
    node.dataset.error = String(error);
    setRemoteText(node, value);
  };
  const ctx = {
    view, snapshots: new Map(), pending: new Set(), channel: 'wechat', generation: 0, connectionPoll: 0,
    active: () => currentSettingsCategory === 'remote' && pageIsOpen(settings),
    bindingMessage: (value, error) => setMessage('remote-binding-message', value, error),
    accessMessage: (value, error) => setMessage('remote-access-message', value, error),
    closeForm: () => closeRemoteForms(view),
    clearPairing: () => clearRemotePairing(view, get),
  };
  ctx.render = () => renderRemotePage(ctx);
  ctx.perform = (message, operation, refreshAfter) => performRemoteOperation(ctx, message, operation, refreshAfter);
  ctx.refresh = createRemoteSettingsRefresh(view, () => ctx.channel, ctx.snapshots, ctx.active, ctx.render,
    userId => revokeRemoteUser(userId, ctx.perform, ctx.accessMessage));
  ctx.wechat = installRemoteWechatBinding(get, ctx.bindingMessage, ctx.refresh, ctx.render);
  ctx.tabs = installRemoteChannelTabs(get('remote-channels'), (id, reload) => selectRemoteChannel(ctx, id, reload));
  const bound = () => Boolean(ctx.snapshots.get(ctx.channel)?.bound);
  installRemoteReceive(view, ctx.snapshots, () => ctx.channel, ctx.perform, ctx.bindingMessage);
  installRemoteBindingActions({
    view, get, selectedChannel: () => ctx.channel, bindingIsBound: bound,
    perform: ctx.perform, bindingMessage: ctx.bindingMessage, closeForm: ctx.closeForm,
    render: ctx.render, clearPairing: ctx.clearPairing, wechat: ctx.wechat,
  });
  installTelegramBinding(view, get, () => ctx.channel, bound, ctx.perform, ctx.bindingMessage, ctx.closeForm, ctx.clearPairing);
  installRemotePairingAction(view, get, ctx.perform, ctx.accessMessage, ctx.clearPairing);
  const enter = () => {
    if (!ctx.active()) return;
    setRemoteText(view.accessDetail, CLATI18n.msg('web.checking'));
    ctx.snapshots.clear();
    ctx.render();
    ctx.refresh();
  };
  view.refresh.append(svgIcon('refresh'), el('span', '', () => pageText('refresh')));
  view.refresh.onclick = ctx.refresh;
  setRemoteText(view.accessTitle, pageText('authorized_users'));
  settings.addEventListener('clat:page-enter', enter);
  settings.addEventListener('clat:remote-enter', enter);
  settings.addEventListener('clat:settings-leave', () => clearRemoteTransient(ctx));
  settings.addEventListener('clat:remote-channel-link', event => selectRemoteChannel(ctx, remoteLinkChannel(event.detail), false));
  ctx.render();
}
function setupWebAbout() {
  if(document.querySelector('[data-settings-category=desktop]'))return;
  const button=el('button','',()=>pageText('updates_about'));button.type='button';button.dataset.settingsCategory='desktop';button.onclick=()=>selectSettingsCategory('desktop');document.querySelector('.settings-nav').append(button);
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
