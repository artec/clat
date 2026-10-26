/* Page ownership is explicit. Only temporary overlays use native dialog APIs. */
const navigationPages = new Map();
let permissionEditTarget=null;
const pageText = (key, args = {}) => CLATI18n.msg('pages.' + key, args);
function pageIsOpen(node) { return Boolean(node && navigationPages.get(node.id)?.active); }
function presentPage(node) { workbenchShell?.show(node.id); }
function leavePage(node) {
  const seat = navigationPages.get(node.id);
  if (!seat?.active) return;
  seat.scrollTop = workbenchShell?.pageHost?.scrollTop || 0;
  seat.active = false; node.classList.add('hidden'); node.hidden = true;
  node.dispatchEvent(new Event('clat:page-leave'));
}
function pageAction(key, action, primary = false) {
  const button = el('button', primary ? 'primary' : 'secondary', () => pageText(key));
  button.type = 'button'; button.dataset.action = key; button.onclick = action; return button;
}
function pageEmpty(key, action) {
  const empty = el('section', 'page-empty'); empty.append(svgIcon('info'), el('p', '', () => pageText(key)));
  if (action) empty.append(action); return empty;
}
function pageHeader(node, title, scope) {
  const head = el('header', 'page-header'); const copy = el('div', '');
  copy.append(el('small', 'page-scope', scope), el('h2', '', title)); head.append(copy); node.prepend(head); return head;
}
function settingsSaveToken(section) {
  return JSON.stringify([...section.querySelectorAll('input,select,textarea')].map(input=>[input.value,input.checked]));
}
function pageSettingsLeave() {
  const settings = dom['settings-dialog'];
  for (const input of settings.querySelectorAll('input[type=password]')) {
    if(input.value){const section=input.closest('[data-settings-panel]');if(section&&!section.querySelector('.secret-cleared-note'))section.append(el('p','settings-note secret-cleared-note',()=>pageText('secrets_cleared')));}
    input.value = '';
  }
  state.wechatBindingPoll++; wechatSensitiveEpoch++; clearWechatQr();
  dom['wechat-pairing-code'].textContent = ''; hide(dom['wechat-pairing']);
  settings.dispatchEvent(new Event('clat:settings-leave'));
}
function setupSettingsSections() {
  const nav = document.querySelector('.settings-nav');
  for (const [key, categories] of [['workbench', ['appearance', 'projects']], ['capabilities', ['models', 'utility', 'permissions']], ['integrations', ['wechat', 'web']], ['application', ['desktop']]]) {
    const label = el('small', 'settings-group', () => pageText(key));
    const first = nav.querySelector(`[data-settings-category="${categories[0]}"]`);
    if (first) first.before(label);
  }
  const general = document.querySelector('[data-settings-panel=appearance]');
  general.append(document.getElementById('page-notifications')?.closest('.notification-settings') || el('span'));
  for (const section of document.querySelectorAll('[data-settings-panel]')) {
    const scope = el('p', 'page-scope', () => settingsScope(section.dataset.settingsPanel));
    section.prepend(scope);
  }
  const advanced = el('details', 'settings-advanced'); advanced.append(el('summary', '', () => pageText('advanced')));
  const grid = document.querySelector('.model-form-grid');
  for (const id of ['model-profile-protocol', 'model-profile-path']) advanced.append(document.getElementById(id).closest('label'));
  grid.after(advanced);
  dom['settings-dialog'].addEventListener('clat:page-leave', pageSettingsLeave);
}
function settingsScope(category) {
  if (category === 'appearance') return pageText('client_scope');
  if (category === 'permissions') return pageText('session_scope', {project: permissionEditTarget?.project||state.workbench?.project?.root||'', session: permissionEditTarget?.session||state.sessionId||CLATI18n.msg('web.fresh')});
  if (category === 'projects') return pageText('project_scope');
  if (category === 'desktop') return pageText('app_scope');
  return pageText('host_scope');
}
function setupSettingsDirtyProtection() {
  const settings=dom['settings-dialog'], initial=new WeakMap();
  const fields=section=>[...section.querySelectorAll('input,select,textarea')].filter(input=>input.type!=='password');
  const capture=section=>{if(!initial.has(section))initial.set(section,fields(section).map(input=>[input,input.value,input.checked]));};
  settings.addEventListener('focusin',event=>{const section=event.target.closest('[data-settings-panel]');if(section&&!section.dataset.unsaved)capture(section);});
  settings.addEventListener('input',event=>{
    const section=event.target.closest('[data-settings-panel]');
    if(!section||section.dataset.settingsPanel==='appearance')return;
    if(section.dataset.settingsPanel==='permissions'&&!permissionEditTarget)permissionEditTarget={project:state.workbench?.project?.root,session:state.sessionId,generation:state.selectionGeneration};
    section.dataset.unsaved='true';
    let bar=section.querySelector('.settings-draft-bar');
    if(!bar){bar=el('div','settings-draft-bar');bar.append(el('span','',()=>pageText('unsaved')),pageAction('discard',()=>{
      for(const [input,value,checked]of initial.get(section)||[]){input.value=value;input.checked=checked;}
      for(const input of section.querySelectorAll('input[type=password]'))input.value='';
      acknowledgeSettingsSave(section);if(section.dataset.settingsPanel==='permissions')selectPermissionMode(state.workbench?.permission?.mode||'workspace-write');updateFullAccessConfirmation();CLATI18n.render();
    }));section.append(bar);}bar.hidden=false;
  });
  settings.addEventListener('clat:settings-saved',event=>{const section=event.detail;initial.delete(section);if(section.dataset.settingsPanel==='permissions')permissionEditTarget=null;delete section.dataset.unsaved;section.querySelector('.settings-draft-bar')?.setAttribute('hidden','');});
  return (section,token)=>{if(token!==undefined && token!==settingsSaveToken(section))return false;settings.dispatchEvent(new CustomEvent('clat:settings-saved',{detail:section}));return true;};
}

let acknowledgeSettingsSave = () => {};
function installNavigationPages() {
  setupWebAbout(); setupProjectPicker(); setupWechatUsers(); setupPluginTabs(); installTaskNotifications();
  setupSettingsSections(); acknowledgeSettingsSave = setupSettingsDirtyProtection();
  const scope = el('p', 'page-scope current-permission');
  CLATI18n.bind(scope, 'textContent', () => pageText('effective_permission', {mode: state.workbench?.permission?.mode || 'workspace-write'}));
  dom['permission-options'].before(scope);

}
