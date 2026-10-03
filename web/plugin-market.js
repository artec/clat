/* Local plugin control; catalog bytes never grant installation authority. */
function installPluginMarket() {
  const panel = dom['market-dialog'].querySelector('.market-panel');
  const installed = el('section', 'plugin-installed');
  installed.append(el('h3', '', 'Installed on this host'));
  const installedList = el('div', 'market-list'); installed.append(installedList);
  const review = el('section', 'plugin-review hidden'); review.setAttribute('aria-label', 'Review plugin permissions');
  panel.querySelector('.market-toolbar').before(installed);
  installed.before(review);
  let packages = [], ticket = null, generation = 0, pending = false;
  const status = el('p', 'market-status'); status.setAttribute('role', 'status'); installed.before(status);
  const message = (text) => { status.textContent = text; };
  function button(label, action, parent, danger = false) {
    const node = el('button', danger ? 'danger-button' : 'secondary', label); node.type = 'button';
    node.disabled = pending; node.addEventListener('click', action); parent.append(node); return node;
  }
  function renderInstalled() {
    installedList.replaceChildren();
    if (!packages.length) installedList.append(el('p', 'market-summary', 'No plugins installed.'));
    for (const plugin of packages) {
      const card = el('article', 'market-item'); card.dataset.pluginId = plugin.id;
      card.append(el('h3', '', plugin.name), el('p', 'market-id', `${plugin.id} · ${plugin.version}`),
        el('p', 'market-summary', `${plugin.enabled ? 'Enabled' : 'Disabled'} · ${plugin.trust}${plugin.publisher ? ' · ' + plugin.publisher : ''}`));
      if (plugin.health) card.append(el('p', 'error', plugin.health));
      const actions = el('div', 'plugin-actions');
      button(plugin.enabled ? 'Disable' : 'Enable', () => plugin.enabled ? remove(plugin, 'disable') : prepare(plugin, 'enable'), actions);
      button('Configure', () => prepare(plugin, 'configure'), actions);
      button('Update', () => prepare(plugin, 'update'), actions);
      if (plugin.rollback_version) button(`Roll back to ${plugin.rollback_version}`, () => prepare(plugin, 'rollback'), actions);
      button('Uninstall', () => remove(plugin, 'uninstall'), actions, true);
      card.append(actions); installedList.append(card);
    }
  }
  async function refresh() {
    const captured = generation;
    try {
      const result = await rpc('plugin.list', {});
      if (captured !== generation || !dom['market-dialog'].open) return;
      packages = result.installed; renderInstalled(); renderMarket();
    } catch (error) { if (captured === generation) installedList.replaceChildren(el('p', 'error', error.message)); }
  }
  function setPending(value) {
    pending = value; renderInstalled(); renderMarket();
    for (const node of review.querySelectorAll('button,input,select,textarea')) node.disabled = value || (node.dataset.requiresConsent === 'true' && !review.querySelector('.plugin-consent input')?.checked);
  }
  async function prepare(plugin, action) {
    if (pending) return;
    const captured = ++generation; await cancelReview(); setPending(true);
    message(action === 'install' || action === 'update' ? 'Downloading and verifying the signed package…' : 'Reading installed package…');
    try {
      const result = await rpc('plugin.prepare', { id: plugin.id, action });
      if (captured !== generation || !dom['market-dialog'].open) {
        await rpc('plugin.cancel', { ticket: result.ticket }); return;
      }
      ticket = result.ticket; renderReview(result); review.scrollIntoView({ block: 'start' }); message('Review permissions and configuration before activating.');
    } catch (error) { if (captured === generation) message(error.message); }
    finally { if (captured === generation) setPending(false); }
  }
  function configFields(plugin) {
    const fields = el('fieldset', 'plugin-config');
    const schema = plugin.config_schema;
    const read = () => {
      const value = Object.create(null);
      for (const input of fields.querySelectorAll('[data-config-key]')) {
        if (input.type === 'checkbox') value[input.dataset.configKey] = input.checked;
        else if (input.value !== '') {
          value[input.dataset.configKey] = input.dataset.json === 'true' ? JSON.parse(input.value)
            : input.type === 'number' ? Number(input.value) : input.value;
        }
      }
      return Object.keys(value).length ? value : null;
    };
    if (!schema) return { fields, read: () => null };
    fields.append(el('legend', '', `${plugin.name} configuration`));
    if (schema.type !== 'object' || !schema.properties) {
      const input = el('textarea'); input.dataset.configKey = '__json'; input.dataset.json = 'true';
      input.setAttribute('aria-label', 'Plugin configuration JSON'); input.spellcheck = false;
      fields.append(el('p', '', 'Enter the configuration as JSON. It stays on this local host.'), input);
      return { fields, read: () => input.value ? JSON.parse(input.value) : null };
    }
    for (const [key, property] of Object.entries(schema.properties)) {
      const label = el('label', 'plugin-config-field'); label.append(el('span', '', property.title || key));
      let input;
      if (Array.isArray(property.enum) || property.type === 'boolean') {
        input = el('select'); const empty = el('option', '', 'Select…'); empty.value = ''; input.append(empty);
        for (const item of property.enum || [true, false]) { const option = el('option', '', String(item)); option.value = JSON.stringify(item); input.append(option); }
        input.dataset.json = 'true';
      } else {
        input = el(property.type === 'object' || property.type === 'array' ? 'textarea' : 'input');
        if (input.tagName === 'INPUT') input.type = ['integer', 'number'].includes(property.type) ? 'number' : property.writeOnly || property.format === 'password' ? 'password' : 'text';
        else input.dataset.json = 'true';
      }
      input.dataset.configKey = key; input.autocomplete = 'off'; input.spellcheck = false;
      input.required = (schema.required || []).includes(key); input.setAttribute('aria-label', property.title || key);
      label.append(input); if (property.description) label.append(el('small', '', property.description)); fields.append(label);
    }
    return { fields, read };
  }
  function renderReview(result) {
    review.replaceChildren(); review.classList.remove('hidden');
    review.append(el('h3', '', 'Review and activate'), el('p', '', 'These permissions come from verified packages. Configuration is sent only to your local CLAT host.'));
    if (result.action === 'update') review.append(el('p', '', 'Leave configuration blank to retain the existing local values. Entered configuration replaces them.'));
    if (result.action === 'configure') review.append(el('p', '', 'Existing values are hidden. Enter all settings to replace the configuration; an empty form clears it.'));
    const forms = [];
    for (const plugin of result.review.packages) {
      review.append(el('h4', '', `${plugin.name} · ${plugin.version}`));
      review.append(el('p', 'market-id', `${plugin.id} · ${plugin.publisher || 'Local package'}`));
      const caps = plugin.capabilities; const permissions = [];
      for (const [key, label] of Object.entries({ tools: 'Provide tools', prompts: 'Provide system instructions', sampling: 'Request model calls', elicitation: 'Ask for information', hostContext: 'Read host context' })) {
        if (caps[key]) permissions.push(label);
      }
      if (caps.hostTools?.length) permissions.push('Call host tools: ' + caps.hostTools.join(', '));
      review.append(el('p', 'plugin-permissions', permissions.join(' · ') || 'No declared host capabilities'));
      if (plugin.runtime === 'mcp-stdio') review.append(el('p', 'plugin-native-risk', 'Native executable: runs with your user account’s file, network and process access. Install only from a publisher you trust.'));
      const form = configFields(plugin); review.append(form.fields); forms.push({ id: plugin.id, ...form });
    }
    const consentLabel = el('label', 'plugin-consent'); const consent = el('input'); consent.type = 'checkbox';
    consentLabel.append(consent, document.createTextNode('I approve the listed permissions and runtime access.')); review.append(consentLabel);
    const actions = el('div', 'plugin-actions'); review.append(actions);
    const activate = button(result.action === 'install' ? 'Install' : 'Apply', async () => {
      if (!consent.checked) { message('Confirm the listed permissions first.'); return; }
      let configs;
      try {
        configs = Object.create(null);
        for (const form of forms) {
          for (const input of form.fields.querySelectorAll('input,select,textarea')) if (!input.reportValidity()) return;
          const value = form.read(); if (value !== null) configs[form.id] = value; else if (result.action === 'configure') configs[form.id] = {};
        }
      } catch { message('Configuration must contain valid JSON.'); return; }
      const captured = generation, submitted = ticket; ticket = null;
      // Clear every credential before the request settles; never retain it in UI state.
      review.replaceChildren(); review.classList.add('hidden'); setPending(true); message('Activating plugin…');
      try {
        const outcome = await rpc('plugin.commit', { ticket: submitted, accept_capabilities: true, configs });
        if (captured === generation) message(outcome.runtime_warnings?.length ? 'Change saved; runtime needs attention: ' + outcome.runtime_warnings.join('; ') : 'Plugin change applied.');
        await refreshWorkbench();
      } catch (error) { if (captured === generation) message(error.message + ' Refresh installed plugins before retrying.'); }
      finally { configs = null; if (captured === generation) { setPending(false); await refresh(); } }
    }, actions);
    activate.dataset.requiresConsent = 'true'; activate.disabled = true; consent.addEventListener('change', () => { activate.disabled = !consent.checked || pending; });
    button('Cancel', () => { void cancelReview(); }, actions);
  }
  async function cancelReview() {
    const cancelled = ticket; ticket = null; review.replaceChildren(); review.classList.add('hidden');
    if (cancelled) await rpc('plugin.cancel', { ticket: cancelled }).catch(() => {});
  }
  async function remove(plugin, action) {
    if (pending) return; const captured = ++generation; await cancelReview(); setPending(true);
    try {
      const result = await rpc('plugin.remove', { id: plugin.id, action });
      if (captured === generation) message(result.note); await refreshWorkbench();
    } catch (error) { if (captured === generation) message(error.message); }
    finally { if (captured === generation) { setPending(false); await refresh(); } }
  }
  dom['market-dialog'].addEventListener('close', () => { generation += 1; pending = false; void cancelReview(); });
  return {
    refresh,
    decorate(card, plugin) {
      const current = packages.find(p => p.id === plugin.id);
      if (!current && plugin.status === 'available') {
        const actions = el('div', 'plugin-actions'); button('Install…', () => prepare(plugin, 'install'), actions); card.append(actions);
      }
    },
  };
}
