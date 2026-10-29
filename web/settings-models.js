/* Built-in routes come from the same Host catalog as TUI, never a second model table. */
const settingsPresetRows = new Map();
function renderSettingsPresets(view) {
  const host = modelField('presets');
  if (!host) return;
  const groups = new Map();
  for (const preset of view.presets || []) {
    if (!groups.has(preset.vendor)) groups.set(preset.vendor, []);
    groups.get(preset.vendor).push(preset);
  }
  for (const [vendor, presets] of groups) {
    let row = settingsPresetRows.get(vendor);
    if (!row) { row = settingsPresetRow(vendor, presets); settingsPresetRows.set(vendor, row); host.append(row.element); }
    const active = !view.active_profile && presets.some(preset => preset.id === view.current.preset);
    row.active.hidden = !active;
    if (!row.edited && active) row.select.value = view.current.preset;
  }
  for (const [vendor, row] of settingsPresetRows) if (!groups.has(vendor)) { row.element.remove(); settingsPresetRows.delete(vendor); }
}
function settingsPresetRow(vendor, presets) {
  const element = el('article', 'settings-card model-vendor'), heading = el('div', 'model-vendor-heading');
  const active = el('small', '', () => settingsUi('active_model'));
  heading.append(el('strong', '', vendor), active);
  const controls = el('div', 'model-vendor-controls'), select = el('select'), key = el('input');
  for (const preset of presets) { const option = el('option', '', preset.name); option.value = preset.id; select.append(option); }
  CLATI18n.attribute(select, 'aria-label', () => vendor + ' · ' + CLATI18n.msg('web.model_id'));
  key.type = 'password'; key.autocomplete = 'new-password'; key.maxLength = 4096;
  CLATI18n.attribute(key, 'aria-label', () => vendor + ' · ' + CLATI18n.msg('web.api_key'));
  CLATI18n.attribute(key, 'placeholder', () => settingsUi('preset_key_hint'));
  const row = { element, active, select, edited: false };
  select.onchange = () => { row.edited = true; };
  const apply = el('button', 'secondary', () => settingsUi('apply_preset')); apply.type = 'button';
  apply.onclick = () => modelAction(apply, async () => {
    const params = { id: select.value };
    if (key.value) params.api_key = key.value;
    key.value = ''; select.disabled = true;
    try { await rpc('model.preset.select', params); row.edited = false; }
    finally { select.disabled = false; }
  });
  controls.append(select, key, apply); element.append(heading, controls);
  return row;
}
