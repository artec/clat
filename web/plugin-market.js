/* Local plugin control; catalog bytes never grant installation authority. */
function installPluginMarket() {
  const panel = dom['market-dialog'].querySelector('.market-panel');
  const installed = el('section', 'plugin-installed');
  installed.append(el('h3', '', () => (CLATI18n.msg("web.installed_on_this_host", {}, "Installed on this host"))));
  const installedList = el('div', 'market-list'); installed.append(installedList);
  const review = el('section', 'plugin-review hidden'); CLATI18n.attribute(review,'aria-label',()=>CLATI18n.msg("web.review_plugin_permissions", {}, "Review plugin permissions"));
  panel.querySelector('.market-toolbar').before(installed);
  installed.before(review);
  let packages = [], ticket = null, generation = 0, pending = false;
  const status = el('p', 'market-status'); status.setAttribute('role', 'status'); installed.before(status);
  const message = (text) => { CLATI18n.bind(status, "textContent", () => text); };
  function button(label, action, parent, danger = false) {
    const node = el('button', danger ? 'danger-button' : 'secondary', () => (label)); node.type = 'button';
    node.disabled = pending; node.addEventListener('click', action); parent.append(node); return node;
  }
  function renderInstalled() {
    installedList.replaceChildren();
    if (!packages.length) installedList.append(el('p', 'market-summary', () => (CLATI18n.msg("web.no_plugins_installed", {}, "No plugins installed."))));
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
      card.append(actions); installedList.append(card);
    }
  }
  async function refresh() {
    const captured = generation;
    try {
      const result = await rpc('plugin.list', {});
      if (captured !== generation || !pageIsOpen(dom['market-dialog'])) return;
      packages = result.installed; renderInstalled(); renderMarket();
    } catch (error) { if (captured === generation) installedList.replaceChildren(el('p', 'error', () => (error.message))); }
  }
  function setPending(value) {
    pending = value; renderInstalled(); renderMarket();
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
      ticket = result.ticket; renderReview(result); review.scrollIntoView({ block: 'start' }); message(CLATI18n.msg("web.review_permissions_and_configuration_before_activating", {}, "Review permissions and configuration before activating."));
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
    fields.append(el('legend', '', () => (CLATI18n.msg("web.dynamic_value_configuration", {value1:(plugin.name)}, "{value1} configuration"))));
    if (schema.type !== 'object' || !schema.properties) {
      const input = el('textarea'); input.dataset.configKey = '__json'; input.dataset.json = 'true';
      CLATI18n.attribute(input,'aria-label',()=>CLATI18n.msg("web.plugin_configuration_json", {}, "Plugin configuration JSON")); input.spellcheck = false;
      fields.append(el('p', '', () => (CLATI18n.msg("web.enter_the_configuration_as_json_it_stays_on_this_local_host", {}, "Enter the configuration as JSON. It stays on this local host."))), input);
      return { fields, read: () => input.value ? JSON.parse(input.value) : null };
    }
    for (const [key, property] of Object.entries(schema.properties)) {
      const label = el('label', 'plugin-config-field'); label.append(el('span', '', () => (property.title || key)));
      let input;
      if (Array.isArray(property.enum) || property.type === 'boolean') {
        input = el('select'); const empty = el('option', '', () => (CLATI18n.msg("web.select", {}, "Select…"))); empty.value = ''; input.append(empty);
        for (const item of property.enum || [true, false]) { const option = el('option', '', () => (String(item))); option.value = JSON.stringify(item); input.append(option); }
        input.dataset.json = 'true';
      } else {
        input = el(property.type === 'object' || property.type === 'array' ? 'textarea' : 'input');
        if (input.tagName === 'INPUT') input.type = ['integer', 'number'].includes(property.type) ? 'number' : property.writeOnly || property.format === 'password' ? 'password' : 'text';
        else input.dataset.json = 'true';
      }
      input.dataset.configKey = key; input.autocomplete = 'off'; input.spellcheck = false;
      input.required = (schema.required || []).includes(key); CLATI18n.attribute(input,'aria-label',()=>property.title || key);
      label.append(input); if (property.description) label.append(el('small', '', () => (property.description))); fields.append(label);
    }
    return { fields, read };
  }
  function renderReview(result) {
    review.replaceChildren(); review.classList.remove('hidden');
    review.append(el('h3', '', () => (CLATI18n.msg("web.review_and_activate", {}, "Review and activate"))), el('p', '', () => (CLATI18n.msg("web.these_permissions_come_from_verified_packages_configuration_is_sent_only_to_your_l", {}, "These permissions come from verified packages. Configuration is sent only to your local CLAT host."))));
    if (result.action === 'update') review.append(el('p', '', () => (CLATI18n.msg("web.leave_configuration_blank_to_retain_the_existing_local_values_entered_configuratio", {}, "Leave configuration blank to retain the existing local values. Entered configuration replaces them."))));
    if (result.action === 'configure') review.append(el('p', '', () => (CLATI18n.msg("web.existing_values_are_hidden_enter_all_settings_to_replace_the_configuration_an_empt", {}, "Existing values are hidden. Enter all settings to replace the configuration; an empty form clears it."))));
    const forms = [];
    for (const plugin of result.review.packages) {
      review.append(el('h4', '', () => (`${plugin.name} · ${plugin.version}`)));
      review.append(el('p', 'market-id', () => (`${plugin.id} · ${plugin.publisher || CLATI18n.msg("web.local_package", {}, "Local package")}`)));
      const caps = plugin.capabilities; const permissions = [];
      for (const [key, label] of Object.entries({ tools: CLATI18n.msg("web.provide_tools", {}, "Provide tools"), prompts: CLATI18n.msg("web.provide_system_instructions", {}, "Provide system instructions"), sampling: CLATI18n.msg("web.request_model_calls", {}, "Request model calls"), elicitation: CLATI18n.msg("web.ask_for_information", {}, "Ask for information"), hostContext: CLATI18n.msg("web.read_host_context", {}, "Read host context") })) {
        if (caps[key]) permissions.push(label);
      }
      if (caps.network) permissions.push(CLATI18n.msg("web.line_network_restricted_to_value",{value1:(caps.network.origins.map(origin => `${origin.scheme}://${origin.host}:${origin.port} (${origin.methods.join(', ')})`).join('; '))},"Network restricted to: {value1}"));
      if (caps.clock) permissions.push(CLATI18n.msg("web.clock_access", {}, "Clock access"));
      if (caps.hostTools?.length) permissions.push(CLATI18n.msg("web.line_call_host_tools_value",{value1:(caps.hostTools.join(', '))},"Call host tools: {value1}"));
      review.append(el('p', 'plugin-permissions', () => (permissions.join(' · ') || CLATI18n.msg("web.no_declared_host_capabilities", {}, "No declared host capabilities"))));
      if (plugin.runtime === 'mcp-stdio') review.append(el('p', 'plugin-native-risk', () => (CLATI18n.msg("web.native_executable_runs_with_your_user_account_s_file_network_and_process_access_in", {}, "Native executable: runs with your user account’s file, network and process access. Install only from a publisher you trust."))));
      const form = configFields(plugin); review.append(form.fields); forms.push({ id: plugin.id, ...form });
    }
    const consentLabel = el('label', 'plugin-consent'); const consent = el('input'); consent.type = 'checkbox';
    consentLabel.append(consent, CLATI18n.textNode(CLATI18n.msg("web.i_approve_the_listed_permissions_and_runtime_access", {}, "I approve the listed permissions and runtime access."))); review.append(consentLabel);
    const actions = el('div', 'plugin-actions'); review.append(actions);
    const activate = button(result.action === 'install' ? CLATI18n.msg("web.install", {}, "Install") : CLATI18n.msg("web.apply", {}, "Apply"), async () => {
      if (!consent.checked) { message(CLATI18n.msg("web.confirm_the_listed_permissions_first", {}, "Confirm the listed permissions first.")); return; }
      let configs;
      try {
        configs = Object.create(null);
        for (const form of forms) {
          for (const input of form.fields.querySelectorAll('input,select,textarea')) if (!input.reportValidity()) return;
          const value = form.read(); if (value !== null) configs[form.id] = value; else if (result.action === 'configure') configs[form.id] = {};
        }
      } catch { message(CLATI18n.msg("web.configuration_must_contain_valid_json", {}, "Configuration must contain valid JSON.")); return; }
      const captured = generation, submitted = ticket; ticket = null;
      // Clear every credential before the request settles; never retain it in UI state.
      review.replaceChildren(); review.classList.add('hidden'); setPending(true); message(CLATI18n.msg("web.activating_plugin", {}, "Activating plugin…"));
      try {
        const outcome = await rpc('plugin.commit', { ticket: submitted, accept_capabilities: true, configs });
        if (captured === generation) message(outcome.runtime_warnings?.length ? CLATI18n.msg("web.line_change_saved_runtime_needs_attention_value",{value1:(outcome.runtime_warnings.join('; '))},"Change saved; runtime needs attention: {value1}") : CLATI18n.msg("web.plugin_change_applied", {}, "Plugin change applied."));
        await refreshWorkbench();
      } catch (error) { if (captured === generation) message(CLATI18n.msg("web.line_value_refresh_installed_plugins_before_retrying",{value1:(error.message)},"{value1} Refresh installed plugins before retrying.")); }
      finally { configs = null; if (captured === generation) { setPending(false); await refresh(); } }
    }, actions);
    activate.dataset.requiresConsent = 'true'; activate.disabled = true; consent.addEventListener('change', () => { activate.disabled = !consent.checked || pending; });
    button(CLATI18n.msg("web.cancel", {}, "Cancel"), () => { void cancelReview(); }, actions);
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
  dom['market-dialog'].addEventListener('clat:page-leave', () => { generation += 1; pending = false; void cancelReview(); });
  return {
    refresh,
    decorate(card, plugin) {
      const current = packages.find(p => p.id === plugin.id);
      if (!current && plugin.status === 'available') {
        const actions = el('div', 'plugin-actions'); button(CLATI18n.msg("web.install_667", {}, "Install…"), () => prepare(plugin, 'install'), actions); card.append(actions);
      }
    },
  };
}
