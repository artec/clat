/* Local plugin control; catalog bytes never grant installation authority. */
function installPluginMarket() {
  const view=createExtensionNavigation(dom['market-dialog'],{cancel:cancelReview,refresh:async()=>{state.marketLoaded=false;await loadMarket();await refresh();}});
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
  return new Map(rows.map(plugin=>{
    const card=el('article','market-item');card.dataset.pluginId=plugin.id;
    card.append(el('h3','',()=>extensionLabel(plugin,'name')),el('p','market-summary',()=>extensionLabel(plugin,'summary')),
      el('p','settings-note',()=>CLATI18n.msg('extension.bundled')+' · '+plugin.version),
      el('p','',()=>CLATI18n.msg('extension.active_instances',{count:plugin.instances.filter(row=>row.phase==='active').length})),
      el('p','settings-note',()=>CLATI18n.msg('extension.static_configuration')));
    for(const instance of plugin.instances){
      const section=el('section'),details=el('details');
      section.append(el('h4','',()=>instance.project),el('p','settings-note',()=>CLATI18n.msg(instance.phase==='active'?'extension.active':'web.disabled')));
      section.append(el('p','settings-note',()=>CLATI18n.msg(instance.configurable?'extension.static_configuration':'extension.required_contract')));
      details.append(el('summary','',()=>CLATI18n.msg('extension.technical')),
        el('pre','',()=>JSON.stringify({id:plugin.id,version:plugin.version,runtime:plugin.runtime,scope:plugin.scope,...instance},null,2)));
      section.append(details);card.append(section);
    }
    return [plugin.id,card];
  }));
}
