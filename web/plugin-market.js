/* Local plugin control; catalog bytes never grant installation authority. */
function installPluginMarket() {
  const view=createExtensionNavigation(dom['market-dialog'],{cancel:cancelReview,refresh:async()=>{state.marketLoaded=false;await loadMarket();await refresh();}});
  const builtinEditor=installBuiltinConfiguration(view.builtinConfig,dom['market-dialog']);
  view.guardBuiltins(run=>builtinEditor.beforeTab(run));
  const review=el('section','plugin-review hidden');view.review.append(review);
  CLATI18n.attribute(review,'aria-label',()=>CLATI18n.msg('web.review_plugin_permissions'));
  let packages = [], ticket = null, generation = 0, pending = false;
  const status=view.status;
  const message = (text) => { CLATI18n.bind(status, "textContent", () => text); };
  function button(label, action, parent, danger = false) {
    const node = el('button', danger ? 'danger-button' : 'secondary', () => (label)); node.type = 'button';
    node.disabled = pending; node.addEventListener('click', action); parent.append(node); return node;
  }
  function renderInstalled(){view.update('installed',packages,installedPluginCards(packages,{button,prepare,remove}));}
  async function refresh() {
    void builtinEditor.refresh();
    const captured = generation;
    try {
      const result = await rpc('plugin.list', {});
      if (captured !== generation || !pageIsOpen(dom['market-dialog'])) return;
      packages = result.installed; view.update('builtins',result.builtins||[],builtinPluginCards(result.builtins||[])); renderInstalled(); renderMarket();
    } catch (error) { if (captured === generation) message(error.message); }
  }
  function setPending(value) {
    pending = value;view.setPending(value);renderInstalled();renderMarket();
    for (const node of review.querySelectorAll('button,input,select,textarea')) node.disabled = value || (node.dataset.requiresConsent === 'true' && !review.querySelector('.plugin-consent input')?.checked);
  }
  async function prepare(plugin, action) {
    if (pending) return;
    const captured = ++generation; await cancelReview(); setPending(true);
    message(action === 'install' || action === 'update' ? CLATI18n.msg("web.downloading_and_verifying_the_signed_package", {}, "Downloading and verifying the signed package…") : CLATI18n.msg("web.reading_installed_package", {}, "Reading installed package…"));
    try {
      const result = await rpc('plugin.prepare', { id: plugin.id, action, market_generation: plugin.manifestVersion || plugin.manifest_version || 1 });
      if (captured !== generation || !pageIsOpen(dom['market-dialog'])) {
        await rpc('plugin.cancel', { ticket: result.ticket }); return;
      }
      ticket = result.ticket; renderPluginReview(result,{review,button,message,setPending,refresh,cancelReview,isPending:()=>pending,capture:()=>generation,isCurrent:id=>id===generation,takeTicket:()=>{const value=ticket;ticket=null;return value;}}); review.scrollIntoView({ block: 'start' }); message(CLATI18n.msg("web.review_permissions_and_configuration_before_activating", {}, "Review permissions and configuration before activating."));
    } catch (error) { if (captured === generation) message(error.message); }
    finally { if (captured === generation) setPending(false); }
  }
  async function cancelReview() {
    const cancelled = ticket; ticket = null; review.replaceChildren(); review.classList.add('hidden');
    if (cancelled) await rpc('plugin.cancel', { ticket: cancelled }).catch(() => {});
  }
  async function remove(plugin, action) {
    if (pending) return; const captured = ++generation; await cancelReview(); setPending(true);
    try {
      const result = await rpc('plugin.remove', { id: plugin.id, action, market_generation: plugin.manifestVersion || plugin.manifest_version || 1 });
      if (captured === generation) message(result.note); await refreshWorkbench();
    } catch (error) { if (captured === generation) message(error.message); }
    finally { if (captured === generation) { setPending(false); await refresh(); } }
  }
  CLATI18n.onChange(()=>{
    const consent=review.querySelector('.plugin-consent input');
    if(consent){consent.checked=false;for(const button of review.querySelectorAll('[data-requires-consent]'))button.disabled=true;}
  });
  dom['market-dialog'].addEventListener('clat:page-leave', () => {generation++;pending=false;view.setPending(false);view.leave();void cancelReview();});
  return {
    refresh,
    presentCatalog(cards,packages){view.update('discover',packages,new Map(packages.map((row,index)=>[row.id,cards[index]])));},
    decorate(card, plugin) {
      const current = packages.find(p => p.id === plugin.id);
      if (!current && plugin.status === 'available') {
        const actions = el('div', 'plugin-actions'); button(CLATI18n.msg("web.install_667", {}, "Install…"), () => prepare(plugin, 'install'), actions); card.append(actions);
      }
    },
  };
}

function installedPluginCards(packages,{button,prepare,remove}) {
    const cards=new Map();
    for (const plugin of packages) {
      const card = el('article', 'market-item'); card.dataset.pluginId = plugin.id;
      card.append(el('h3', '', () => (plugin.name)), el('p', 'market-id', () => (`${plugin.id} · ${plugin.version}`)),
        el('p', 'market-summary', () => (`${plugin.enabled ? CLATI18n.msg("web.enabled", {}, "Enabled") : CLATI18n.msg("web.disabled", {}, "Disabled")} · ${plugin.trust}${plugin.publisher ? ' · ' + plugin.publisher : ''}`)));
      if (plugin.health) card.append(el('p', 'error', () => (plugin.health)));
      const actions = el('div', 'plugin-actions');
      button(plugin.enabled ? CLATI18n.msg("web.disable", {}, "Disable") : CLATI18n.msg("web.enable", {}, "Enable"), () => plugin.enabled ? remove(plugin, 'disable') : prepare(plugin, 'enable'), actions);
      button(CLATI18n.msg("web.configure", {}, "Configure"), () => prepare(plugin, 'configure'), actions);
      button(CLATI18n.msg("web.update", {}, "Update"), () => prepare(plugin, 'update'), actions);
      if (plugin.rollback_version) button(CLATI18n.msg("web.dynamic_roll_back_to_value", {value1:(plugin.rollback_version)}, "Roll back to {value1}"), () => prepare(plugin, 'rollback'), actions);
      button(CLATI18n.msg("web.uninstall", {}, "Uninstall"), () => remove(plugin, 'uninstall'), actions, true);
      card.append(actions);cards.set(plugin.id,card);
    }
    return cards;
  }

function extensionLabel(row,field){return row?CLATI18n.present(row[field+'_presentation'],row[field]||''):'';}
function builtinPluginCards(rows){
  const labels={plugins:new Map(rows.map(row=>[row.id,row])),services:new Map()};
  for(const row of rows)for(const instance of row.instances)for(const service of instance.provides||[])labels.services.set(service,row);
  return new Map(rows.map(plugin=>{
    const card=el('article','market-item');card.dataset.pluginId=plugin.id;
    card.append(el('h3','',()=>extensionLabel(plugin,'name')),el('p','market-summary',()=>extensionLabel(plugin,'summary')),
      el('p','settings-note',()=>CLATI18n.msg('extension.bundled')+' · '+plugin.version),
      el('p','',()=>CLATI18n.msg('extension.active_instances',{count:plugin.instances.filter(row=>row.phase==='active').length})));
    for(const instance of plugin.instances){
      const section=el('section');
      section.append(el('h4','',()=>instance.project),el('p','settings-note',()=>CLATI18n.msg(instance.phase==='active'?'extension.active':'web.disabled')));
      section.append(el('p','settings-note',()=>CLATI18n.msg(instance.configurable?'extension.static_configuration':'extension.required_contract')));
      if(instance.configurable===false)for(const root of instance.required_by||[])section.append(el('p','settings-note',()=>CLATI18n.present(root.reason_presentation)));
      section.append(builtinInstanceDetails(plugin,instance,labels));card.append(section);
    }
    return [plugin.id,card];
  }));
}

function builtinReferenceNames(ids,labels,unknownKey){
  const names=(ids||[]).map(id=>String(labels.has(id)?extensionLabel(labels.get(id),'name'):CLATI18n.msg(unknownKey)));
  return [...new Set(names)].join(' · ')||CLATI18n.msg('web.none_short');
}
function builtinInstanceDetails(plugin,instance,labels){
  const details=el('details','builtin-details'),table=el('table','builtin-info-table'),body=el('tbody');
  details.append(el('summary','',()=>CLATI18n.msg('extension.technical')));
  table.append(el('caption','visually-hidden',()=>CLATI18n.msg('extension.technical')),body);
  const rows=[
    ['extension.detail_status',()=>CLATI18n.msg(instance.phase==='active'?'web.enabled':instance.phase==='disabled'?'web.disabled':'extension.detail_unknown')],
    ['extension.detail_version',()=>plugin.version],
    ['extension.detail_runtime',()=>CLATI18n.msg(plugin.runtime==='rust-builtin'?'extension.bundled':'extension.detail_unknown')],
    ['extension.detail_scope',()=>CLATI18n.msg(['trusted-project','bootstrap','run'].includes(plugin.scope)?'extension.scope.'+plugin.scope:'extension.detail_unknown')],
    ['extension.detail_configurable',()=>CLATI18n.msg(instance.configurable?'extension.detail_can_disable':'extension.required_contract')],
    ['extension.provides',()=>builtinReferenceNames(instance.provides,labels.services,'extension.detail_other_service')],
    ['extension.requires',()=>builtinReferenceNames(instance.requires,labels.services,'extension.detail_other_service')],
    ['extension.optional',()=>builtinReferenceNames(instance.optional,labels.services,'extension.detail_other_service')],
    ['extension.detail_disabled_by',()=>builtinReferenceNames(instance.disabled_by,labels.plugins,'extension.detail_other_plugin')],
    ['extension.tools',()=>[...new Set((instance.tools||[]).map(tool=>String(toolDisplayName(tool))))].join(' · ')||CLATI18n.msg('web.none_short')],
    ['extension.commands',()=>instance.commands?.length?CLATI18n.msg('extension.detail_command_count',{count:instance.commands.length}):CLATI18n.msg('web.none_short')],
  ];
  for(const [key,value] of rows){
    const row=el('tr'),heading=el('th','',()=>CLATI18n.msg(key));heading.setAttribute('scope','row');
    row.append(heading,el('td','',value));body.append(row);
  }
  const source=()=>JSON.stringify({id:plugin.id,version:plugin.version,runtime:plugin.runtime,scope:plugin.scope,...instance},null,2);
  const copy=copyTextButton(()=>CLATI18n.msg('extension.copy_json'),source);
  copy.dataset.action='copy-builtin-json';copy.onclick=event=>{event.preventDefault();event.stopPropagation();};
  const original=el('details','builtin-json'),heading=el('summary');
  heading.append(el('span','',()=>CLATI18n.msg('extension.raw_json')),copy);original.append(heading,el('pre','',source));
  details.append(table,el('p','settings-note',()=>CLATI18n.msg('extension.json_info_note')),original);
  return details;
}

/* Client draft only. Selection, validation and persistence belong to the Host. */
function createBuiltinConfigDraft(){
  let base=null,draft=new Set(),preview=null,review=false;
  const sorted=value=>JSON.stringify([...value].sort());
  return {
    get base(){return base;},get draft(){return draft;},get preview(){return preview;},get review(){return review;},
    get dirty(){return Boolean(base)&&sorted(draft)!==sorted(new Set(base.disabled));},
    receive(value){
      const keep=base&&sorted(draft)!==sorted(new Set(base.disabled))&&base.storage_root===value.storage_root;
      const changed=keep&&(base.instance_id!==value.instance_id||base.revision!==value.revision);
      base=value;if(!keep)draft=new Set(value.disabled);preview=null;review=Boolean(keep&&(review||changed));
    },
    toggle(id,enabled){if(enabled)draft.delete(id);else draft.add(id);preview=null;},
    discard(){draft=new Set(base?.disabled||[]);preview=null;review=false;},
    invalidate(){preview=null;review=true;},acknowledge(){review=false;},
    payload(){return {instance_id:base.instance_id,revision:base.revision,disabled:[...draft].sort()};},
    accept(value){if(value.instance_id===base?.instance_id&&value.revision===base?.revision&&sorted(new Set(value.disabled))===sorted(draft)){preview=value;return true;}return false;},
  };
}
function installBuiltinConfiguration(host,page){
  const draft=createBuiltinConfigDraft();let serial=0,busy=false,available=false,note=null;
  const text=key=>CLATI18n.msg(`extension.config_${key}`);
  const action=(key,run)=>{const button=el('button','secondary',()=>text(key));button.type='button';button.dataset.action='builtin-'+key;button.onclick=run;return button;};
  function render(){
    host.replaceChildren();host.append(el('h3','',()=>text('title')));
    if(!available||!draft.base){host.append(el('p','settings-note',()=>note||CLATI18n.msg('extension.static_configuration')));return;}
    host.append(el('p','settings-note',()=>text('scope')));
    const status=el('p','settings-note',()=>note||text(draft.dirty?'unsaved':draft.base.pending_restart?'pending':'unchanged'));status.setAttribute('role','status');host.append(status);
    if(draft.review)host.append(action('recheck',()=>void refresh().then(()=>{if(available){draft.acknowledge();note=null;render();}})));
    const list=el('details','builtin-config-list');list.append(el('summary','',()=>text('edit')));
    const table=el('table','builtin-info-table'),body=el('tbody');
    for(const row of draft.preview?.next||draft.base.next){
      const current=draft.base.current.find(item=>item.id===row.id),tr=el('tr'),label=el('label'),check=el('input');check.type='checkbox';check.checked=!draft.draft.has(row.id);check.disabled=busy||!row.configurable;check.dataset.pluginId=row.id;
      check.onchange=()=>{draft.toggle(row.id,check.checked);note=null;render();};label.append(check,el('span','',()=>CLATI18n.present(row.name_presentation)));
      const cell=el('td');cell.append(label);tr.append(cell,el('td','',()=>text(current?.enabled?'current_on':'current_off')),el('td','',()=>text(row.enabled?'next_on':'next_off')));
      const reasons=el('td','settings-note',()=>configRowReason(row,draft.base.next));tr.append(reasons);body.append(tr);
    }
    const head=el('thead'),headRow=el('tr');for(const key of ['feature','current','next','reason'])headRow.append(el('th','',()=>text(key)));head.append(headRow);table.append(head,body);list.append(table);list.open=draft.dirty;host.append(list);
    const bar=el('div','plugin-actions'),preview=action('preview',()=>void request('preview')),save=action('save',()=>void request('set')),discard=action('discard',()=>{draft.discard();note=null;render();});
    preview.disabled=busy||draft.review||!draft.dirty;save.disabled=busy||draft.review||!draft.preview;discard.disabled=busy||!draft.dirty;bar.append(preview,save,discard);host.append(bar);
    if(draft.preview){host.append(el('p','settings-note',()=>text('confirm')));const changes=el('ul');for(const row of draft.preview.changes)changes.append(el('li','',()=>CLATI18n.present(row.name_presentation)+' · '+text(row.direct?'direct':'dependent')+' · '+text(row.enabled?'next_on':'next_off')));host.append(changes);if(draft.preview.model_execution_available===false)host.append(el('p','settings-note',()=>text('no_model')));}
  }
  async function refresh(){
    const request=++serial;busy=true;draft.invalidate();
    try{
      const description=await rpc('host.describe',{});if(request!==serial)return;
      available=['plugin.builtin.get','plugin.builtin.preview','plugin.builtin.set'].every(method=>description.methods?.includes(method));
      if(!available){note=CLATI18n.msg('extension.static_configuration');return;}
      const value=await rpc('plugin.builtin.get',{});if(request!==serial)return;draft.receive(value);note=draft.review?text('conflict'):null;
    }catch(error){if(request===serial){available=false;note=error.message;}}
    finally{if(request===serial){busy=false;render();}}
  }
  async function request(kind){
    if(busy||draft.review||!available||!draft.base||kind==='set'&&!draft.preview)return;
    const request=++serial,payload=draft.payload();if(kind==='set')payload.preview_id=draft.preview.preview_id;busy=true;render();
    try{
      const value=await rpc('plugin.builtin.'+kind,payload);if(request!==serial)return;
      if(kind==='set'){draft.discard();draft.receive(value);note=text(value.pending_restart?'pending':'unchanged');}else draft.accept(value);
    }catch(error){if(request===serial){draft.invalidate();note=kind==='set'?text('verify_save')+' '+error.message:error.message;}}
    finally{if(request===serial){busy=false;render();}}
  }
  page.addEventListener('clat:page-before-leave',event=>{if(!draft.dirty)return;event.preventDefault();void chooseLeave(event.detail.retry);});
  page.addEventListener('clat:page-leave',()=>{serial++;busy=false;draft.invalidate();});
  window.addEventListener('clat:builtin-availability',()=>{serial++;busy=false;draft.invalidate();if(pageIsOpen(page))void refresh();});
  async function chooseLeave(run){
    const dialog=el('dialog','builtin-leave-dialog');dialog.append(el('p','',()=>text('leave')));
    const finish=choice=>{dialog.close();dialog.remove();if(choice==='discard')draft.discard();if(choice!=='back'){page.builtinLeaveApproved=true;run();page.builtinLeaveApproved=false;}render();};
    for(const choice of ['keep','discard','back'])dialog.append(action(choice,()=>finish(choice)));document.body.append(dialog);dialog.showModal();dialog.addEventListener('cancel',()=>dialog.remove(),{once:true});
  }
  CLATI18n.onChange(render);render();return {refresh,beforeTab(run){if(draft.dirty){void chooseLeave(run);return false;}return true;}};
}
function configRowReason(row,rows){
  if(!row.configurable)return row.required_by.map(root=>String(CLATI18n.present(root.reason_presentation))).join(' ');
  return row.disabled_by.map(id=>String(CLATI18n.present(rows.find(row=>row.id===id)?.name_presentation)||CLATI18n.msg('extension.detail_other_plugin'))).join(' · ');
}
