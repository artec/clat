/* Native Web configuration; no credential is ever returned by the Host. */
function installWebSettings() {
  const nav = document.querySelector('.settings-nav');
  const content = document.querySelector('.settings-content');
  const msg = (key) => CLATI18n.msg(`webtool.${key}`);
  const tab = el('button', '', () => msg('title'));
  tab.type = 'button'; tab.dataset.settingsCategory = 'web';
  const panel = el('section', 'settings-section'); panel.dataset.settingsPanel = 'web'; panel.hidden = true;
  const provider = el('select', '');
  for (const [value, key] of [['http', 'http'], ['deepseek', 'deepseek']]) {
    const option = el('option', '', () => msg(key)); option.value = value; provider.append(option);
  }
  const useModel = el('input', ''); useModel.type = 'checkbox';
  const key = el('input', ''); key.type = 'password'; key.autocomplete = 'new-password'; key.maxLength = 4096;
  const clear = el('input', ''); clear.type = 'checkbox';
  const labeled = (name, input) => { const label = el('label', '', () => ''); label.append(el('span', '', () => msg(name)), input); return label; };
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const save = el('button', 'primary', () => msg('save')); save.type = 'button';
  panel.append(el('h3', '', () => msg('title')), el('p', 'settings-note', () => msg('description')),
    labeled('fetch_provider', provider), labeled('use_model_key', useModel), labeled('key', key), labeled('clear_key', clear), status, save);
  nav.append(tab); content.append(panel);
  let generation = 0; let dirty = false;
  panel.addEventListener('input', () => { dirty = true; generation++; });
  async function refresh() {
    const request = ++generation;
    try {
      const view = await rpc('web.settings.get', {});
      if (request !== generation) return;
      provider.value = view.fetch_provider; useModel.checked = view.use_model_key;
      CLATI18n.bind(status, 'textContent', () => msg(view.search_available ? 'available' : 'not_configured'));
    } catch (error) { if (request === generation) CLATI18n.bind(status, 'textContent', () => error.message); }
  }
  tab.addEventListener('click', () => { selectSettingsCategory('web'); if (!dirty) void refresh(); });
  save.addEventListener('click', async () => {
    save.disabled = true; const request = ++generation;
    try {
      await rpc('web.settings.set', { fetch_provider: provider.value, use_model_key: useModel.checked,
        api_key: key.value || null, clear_key: clear.checked });
      if (request !== generation) return;
      key.value = ''; clear.checked = false; dirty = false;
      CLATI18n.bind(status, 'textContent', () => msg('saved'));
    } catch (error) { if (request === generation) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { save.disabled = false; }
  });
}
