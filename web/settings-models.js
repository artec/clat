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
  for (const preset of presets) { const option = el('option', '', modelCapabilityName(preset.name, preset.image_input)); option.value = preset.id; select.append(option); }
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

function thinkingLevelLabel(level) {
  return ['minimal','low','medium','high','xhigh','max'].includes(level)
    ? CLATI18n.msg('thinking_level.'+level) : level;
}
function renderThinkingSlider(current) {
  const slider=document.getElementById('model-thinking'),output=document.getElementById('model-thinking-value');
  const ticks=document.getElementById('model-thinking-ticks'),levels=current.thinking_levels||[];
  slider.dataset.levels=JSON.stringify(levels);slider.dataset.adjustable=String(levels.length>1);
  slider.max=String(Math.max(0,levels.length-1));
  slider.value=String(Math.max(0,levels.indexOf(current.thinking_level)));
  slider.disabled=levels.length<2||state.modelSelectionPending||state.runActive||state.switching||state.compactionActive;
  ticks.replaceChildren(...levels.map((level,index)=>{
    const tick=el('span','',()=>thinkingLevelLabel(level));
    tick.style.left=((index/(levels.length-1)||0)*100)+'%';return tick;
  }));
  function paint(preview) {
    const level=levels[Number(slider.value)];
    CLATI18n.bind(output,'textContent',()=>!levels.length?CLATI18n.msg('web.not_adjustable')
      :!preview&&!current.thinking_level?CLATI18n.msg('web.choose_intensity'):thinkingLevelLabel(level));
    CLATI18n.attribute(slider,'aria-valuetext',()=>!levels.length?CLATI18n.msg('web.not_adjustable'):thinkingLevelLabel(level));
    slider.parentElement.style.setProperty('--thinking-ratio',String(Number(slider.value)/(levels.length-1)||0));
    for(const [index,tick] of [...ticks.children].entries())tick.dataset.filled=String(index<=Number(slider.value));
  }
  slider.oninput=()=>paint(true);paint(false);
}
