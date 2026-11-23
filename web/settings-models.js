/* Built-in routes come from the same Host catalog as TUI, never a second model table. */
const settingsPresetRows = new Map();
const settingsPresetEntries = new Map([
  ['Qwen Token Plan', { field: 'qwen_entry', values: ['aliyun', 'qianwen'] }],
  ['GLM Coding Plan', { field: 'glm_entry', values: ['bigmodel', 'zai'] }],
]);

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
    row.entry?.sync(view);
    if (row.tier && !row.tierPending) {
      const currentTier = view.kimi_tier === 'pro' ? 'pro' : 'plus';
      for (const radio of row.tier.querySelectorAll('input')) radio.checked = radio.value === currentTier;
    }
  }
  for (const [vendor, row] of settingsPresetRows) if (!groups.has(vendor)) { row.element.remove(); settingsPresetRows.delete(vendor); }
}

function settingsPresetEntry(choice, active, key, apply) {
  const group = el('div', 'model-vendor-entry');
  group.setAttribute('role', 'radiogroup');
  CLATI18n.attribute(group, 'aria-label', () => settingsUi(choice.field));
  const radios = choice.values.map(value => {
    const label = el('label'), radio = el('input');
    radio.type = 'radio'; radio.name = choice.field; radio.value = value;
    label.append(radio, el('span', '', () => settingsUi(choice.field + '_' + value)));
    group.append(label);
    return radio;
  });
  let current = choice.values[0], pending = null;
  const paint = () => { for (const radio of radios) radio.checked = radio.value === (pending ?? current); };
  const disable = disabled => { for (const radio of radios) radio.disabled = disabled; };
  const clear = () => { pending = null; paint(); };
  paint();
  group.addEventListener('change', event => {
    if (!radios.includes(event.target) || event.target.disabled) return;
    const requested = event.target.value;
    pending = requested;
    if (active.hidden) return;
    disable(true);
    void modelAction(apply, async () => {
      const submittedKey = key.value;
      const params = { entry: requested };
      if (submittedKey) params.api_key = submittedKey;
      await rpc('model.preset.entry', params);
      current = requested;
      if (key.value === submittedKey) key.value = '';
    }).finally(() => { clear(); disable(false); });
  });
  return {
    group, disable, clear,
    get pending() { return pending; },
    sync(view) {
      const value = view[choice.field];
      current = choice.values.includes(value) ? value : choice.values[0];
      paint();
    },
  };
}

function settingsPresetRow(vendor, presets) {
  const element = el('article', 'settings-card model-vendor'), heading = el('div', 'model-vendor-heading');
  const active = el('small', '', () => settingsUi('active_model'));
  const controls = el('div', 'model-vendor-controls'), select = el('select'), key = el('input');
  const apply = el('button', 'secondary', () => settingsUi('apply_preset')); apply.type = 'button';
  heading.append(el('strong', '', vendor));
  const entryChoice = settingsPresetEntries.get(vendor);
  const entry = entryChoice ? settingsPresetEntry(entryChoice, active, key, apply) : null;
  let tier = null, tierPending = null;
  if (entry) {
    heading.append(entry.group);
    heading.classList.add('has-entry');
  }
  if (vendor === 'Kimi Coding Plan') {
    tier = el('div', 'model-vendor-entry');
    tier.setAttribute('role', 'radiogroup');
    CLATI18n.attribute(tier, 'aria-label', () => settingsUi('kimi_tier'));
    for (const value of ['plus', 'pro']) {
      const label = el('label'), radio = el('input');
      radio.type = 'radio'; radio.name = 'kimi-coding-plan-tier'; radio.value = value;
      radio.checked = value === 'plus';
      label.append(radio, el('span', '', () => settingsUi('kimi_tier_' + value)));
      tier.append(label);
    }
    tier.addEventListener('change', event => {
      if (event.target.type !== 'radio' || tierPending) return;
      tierPending = event.target.value;
      for (const radio of tier.querySelectorAll('input')) radio.disabled = true;
      apply.disabled = true;
      void (async () => {
        try {
          await rpc('model.preset.tier', { tier: tierPending });
          await refreshWorkbench();
        } catch (error) {
          CLATI18n.bind(modelField('settings-error'), 'textContent', () => error.message);
        } finally {
          tierPending = null;
          for (const radio of tier.querySelectorAll('input')) radio.disabled = false;
          apply.disabled = false;
          await refreshModelSettings();
        }
      })();
    });
    heading.append(tier);
    heading.classList.add('has-entry');
  }
  heading.append(active);
  for (const preset of presets) { const option = el('option', '', modelCapabilityName(preset.name, preset.image_input)); option.value = preset.id; select.append(option); }
  CLATI18n.attribute(select, 'aria-label', () => vendor + ' · ' + CLATI18n.msg('web.model_id'));
  key.type = 'password'; key.autocomplete = 'new-password'; key.maxLength = 4096;
  CLATI18n.attribute(key, 'aria-label', () => vendor + ' · ' + CLATI18n.msg('web.api_key'));
  CLATI18n.attribute(key, 'placeholder', () => settingsUi('preset_key_hint'));
  const row = {
    element, active, select, entry, edited: false,
    get tier() { return tier; }, get tierPending() { return tierPending; },
  };
  select.onchange = () => { row.edited = true; };
  apply.onclick = async () => {
    const params = { id: select.value };
    const submittedKey = key.value;
    if (submittedKey) params.api_key = submittedKey;
    if (entry?.pending) params.entry = entry.pending;
    select.disabled = true;
    entry?.disable(true);
    try {
      await modelAction(apply, async () => {
        await rpc('model.preset.select', params);
        row.edited = false;
        entry?.clear();
        if (key.value === submittedKey) key.value = '';
      });
    } finally {
      select.disabled = false;
      entry?.disable(false);
    }
  };
  controls.append(select, key, apply); element.append(heading, controls);
  return row;
}

function thinkingLevelLabel(level) {
  return ['minimal','low','medium','high','xhigh','max'].includes(level)
    ? CLATI18n.msg('thinking_level.'+level) : level;
}

function bindThinkingTickHover(slider, ticks) {
  const control = slider.parentElement;
  function hover(tick) {
    const previous = ticks.querySelector('[data-hovered]');
    if (previous === tick) return;
    previous?.removeAttribute('data-hovered');
    if (tick) tick.dataset.hovered = '';
  }
  control.onpointermove = (event) => {
    if (event.pointerType === 'touch' || slider.disabled && !('saving' in slider.dataset)) return hover(null);
    const { left, top, width, height } = ticks.getBoundingClientRect();
    const intervals = ticks.childElementCount - 1;
    if (intervals < 1 || width <= 0) return hover(null);
    const offset = event.clientX - left;
    const step = width / intervals;
    const index = Math.round(offset / step);
    const distance = Math.hypot(offset - index * step, event.clientY - (top + height / 2));
    hover(distance <= 9 ? ticks.children[index] : null);
  };
  control.onpointerleave = control.onpointercancel = () => hover(null);
}

function renderThinkingSlider(current) {
  const slider=document.getElementById('model-thinking'),output=document.getElementById('model-thinking-value');
  const ticks=document.getElementById('model-thinking-ticks'),levels=current.thinking_levels||[];
  const serialized = JSON.stringify(levels);
  if (slider.dataset.levels !== serialized) {
    ticks.replaceChildren(...levels.map((level,index)=>{
      const tick=el('span');
      tick.style.left=((index/(levels.length-1)||0)*100)+'%';return tick;
    }));
    slider.dataset.levels = serialized;
    bindThinkingTickHover(slider, ticks);
  }
  slider.dataset.adjustable=String(levels.length>1);
  slider.max=String(Math.max(0,levels.length-1));
  slider.value=String(Math.max(0,levels.indexOf(current.thinking_level)));
  slider.disabled=levels.length<2||state.modelSelectionPending||state.runActive||state.switching||state.compactionActive;
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
