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
    // VP-2C：单选回显——服务端 view.qwen_entry 是单一事实源（激活时按
    // 配置端点，未激活按记忆 key 平台前缀）；2026-11-21 修复：此前
    // `entry.value = ...` 设在 radiogroup div 上是静默无效赋值，单选
    // 永远停在构造默认百炼。entryPending（用户进行中的点击）优先。
    if (row.entry) {
      if (!row.entryPending) {
        const currentEntry = view.qwen_entry === 'qianwen' ? 'qianwen' : 'aliyun';
        for (const radio of row.entry.querySelectorAll('input')) radio.checked = radio.value === currentEntry;
      }
    }
  }
  for (const [vendor, row] of settingsPresetRows) if (!groups.has(vendor)) { row.element.remove(); settingsPresetRows.delete(vendor); }
}
function settingsPresetRow(vendor, presets) {
  const element = el('article', 'settings-card model-vendor'), heading = el('div', 'model-vendor-heading');
  const active = el('small', '', () => settingsUi('active_model'));
  heading.append(el('strong', '', vendor));
  // VP-2C（2026-11-20 负责人裁定）：Qwen Token Plan 标题后加双入口
  // 单选（阿里云百炼 / 千问 AI 平台）——同一套预设与 Key，单选只切
  // 端点；2026-11-20 布局反馈：紧贴标题同一行横排（has-entry 类，
  // 见 settings.css），"当前模型"标记仍靠右。当前模型是该厂商预设
  // 时切换单选即时生效（model.preset.entry 只改端点、保留 override）；
  // 未激活时随 Apply 携带 entry。
  let entry = null, entryPending = null;
  if (vendor === 'Qwen Token Plan') {
    entry = el('div', 'model-vendor-entry');
    entry.setAttribute('role', 'radiogroup');
    CLATI18n.attribute(entry, 'aria-label', () => settingsUi('qwen_entry'));
    for (const value of ['aliyun', 'qianwen']) {
      const label = el('label'), radio = el('input');
      radio.type = 'radio'; radio.name = 'qwen-token-plan-entry'; radio.value = value;
      radio.checked = value === 'aliyun';
      label.append(radio, el('span', '', () => settingsUi('qwen_entry_' + value)));
      entry.append(label);
    }
    entry.addEventListener('change', event => {
      if (event.target.type !== 'radio') return;
      if (active.hidden) { entryPending = event.target.value; return; }
      entryPending = null;
      const radio = event.target;
      radio.disabled = true;
      void (async () => {
        try {
          // VP-2C 事故修复（2026-11-20 真机 401）：key 框已输入新平台
          // key 时随单选一并提交（一套 Key 二选一，旧 key 打新端点必
          // 401）；成功才清空，失败保留供重试。
          const params = { entry: radio.value };
          if (key.value) params.api_key = key.value;
          await rpc('model.preset.entry', params);
          key.value = '';
          await refreshModelSettings();
          await refreshWorkbench();
        } catch (error) {
          CLATI18n.bind(modelField('settings-error'), 'textContent', () => error.message);
        } finally { radio.disabled = false; }
      })();
    });
    heading.append(entry);
    heading.classList.add('has-entry');
  }
  heading.append(active);
  const controls = el('div', 'model-vendor-controls'), select = el('select'), key = el('input');
  for (const preset of presets) { const option = el('option', '', modelCapabilityName(preset.name, preset.image_input)); option.value = preset.id; select.append(option); }
  CLATI18n.attribute(select, 'aria-label', () => vendor + ' · ' + CLATI18n.msg('web.model_id'));
  key.type = 'password'; key.autocomplete = 'new-password'; key.maxLength = 4096;
  CLATI18n.attribute(key, 'aria-label', () => vendor + ' · ' + CLATI18n.msg('web.api_key'));
  CLATI18n.attribute(key, 'placeholder', () => settingsUi('preset_key_hint'));
  const row = { element, active, select, edited: false, get entry() { return entry; }, get entryPending() { return entryPending; } };
  select.onchange = () => { row.edited = true; };
  const apply = el('button', 'secondary', () => settingsUi('apply_preset')); apply.type = 'button';
  apply.onclick = () => modelAction(apply, async () => {
    const params = { id: select.value };
    if (key.value) params.api_key = key.value;
    if (entryPending) params.entry = entryPending;
    key.value = ''; select.disabled = true;
    try { await rpc('model.preset.select', params); row.edited = false; entryPending = null; }
    finally { select.disabled = false; }
  });
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
