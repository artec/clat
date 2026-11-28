/* Compact model picker: presentation only; Host catalog and selection RPC remain authoritative. */
let compactPickerView = null;
let compactPickerMode = null;
let compactPickerSource = null;

function compactPresetName(preset) {
  return preset.name.replace(/\s+\((?:OpenCode Go|Hy Common)\)$/, '');
}

function compactPickerCurrent(view) {
  const current = view.current;
  const preset = (view.presets || []).find(item => item.id === current.preset);
  const modelName = preset ? compactPresetName(preset) : current.model;
  const display = () => current.vendor && modelName
    ? CLATI18n.msg('web.model_route_label', { provider: current.vendor, model: modelName })
    : modelName || CLATI18n.msg('web.not_configured');
  const title = dom['model-picker-current'];
  title.replaceChildren(el('span', 'model-picker-current-name', display));
  if (current.image_input) {
    const badge = el('span', 'vision-capability');
    CLATI18n.attribute(badge, 'title', () => CLATI18n.msg('web.accepts_images'));
    CLATI18n.attribute(badge, 'aria-label', () => CLATI18n.msg('web.accepts_images'));
    badge.append(svgIcon('vision'));
    title.append(badge);
  }
  if (modelName) {
    CLATI18n.attribute(title, 'title', display);
  } else {
    title.removeAttribute('title');
  }
}

function compactModelChoice(choice) {
  const button = el('button', 'model-choice');
  button.type = 'button';
  button.setAttribute('role', 'radio');
  button.setAttribute('aria-checked', String(choice.active));
  CLATI18n.attribute(button, 'aria-label', () =>
    CLATI18n.msg('web.dynamic_use_value', { value1: choice.label }) +
    (choice.imageInput ? ' · ' + CLATI18n.msg('web.accepts_images') : ''));
  const label = el('span', 'model-choice-content');
  label.append(el('strong', '', choice.label));
  button.append(label);
  // Selection goes before capability so the eye stays at the far right.
  if (choice.active) button.append(el('span', 'model-choice-mark', '✓'));
  if (choice.imageInput) {
    const badge = el('span', 'model-choice-vision');
    CLATI18n.attribute(badge, 'title', () => CLATI18n.msg('web.accepts_images'));
    badge.append(svgIcon('vision'));
    button.append(badge);
  }
  button.addEventListener('click', () => selectModelChoice(choice));
  return button;
}

function compactPickerRows(choices) {
  const list = dom['model-picker-list'];
  list.replaceChildren();
  for (const choice of choices) list.append(compactModelChoice(choice));
}

function compactPickerThinking(current) {
  const slider = document.getElementById('model-thinking');
  const panel = document.querySelector('.model-thinking-panel');
  // Host notifications can arrive while a level is still being dragged or saved.
  // Keep the local preview steady; reconcile against the latest Host view on settle.
  if (slider.hasAttribute('data-saving') || panel.dataset.preview === 'true') return;
  renderThinkingSlider(current);
  const ticks = document.getElementById('model-thinking-ticks');
  const levels = current.thinking_levels || [];
  const endpoints = document.getElementById('model-thinking-ends');
  endpoints.hidden = levels.length < 2;
  if (levels.length >= 2) {
    CLATI18n.bind(document.getElementById('model-thinking-min'), 'textContent',
      () => thinkingLevelLabel(levels[0]));
    CLATI18n.bind(document.getElementById('model-thinking-max'), 'textContent',
      () => thinkingLevelLabel(levels[levels.length - 1]));
  }
  const paint = () => {
    const index = Number(slider.value);
    for (const [i, tick] of [...ticks.children].entries()) {
      tick.dataset.current = String(i === index);
    }
    panel.dataset.adjustable = String(levels.length > 1);
  };
  const originalInput = slider.oninput;
  slider.oninput = event => {
    originalInput?.(event);
    paint();
    panel.dataset.preview = 'true';
  };
  slider.onchange = () => { delete panel.dataset.preview; };
  paint();
}

function compactPickerRender(view) {
  if (!modelPickerOpen()) {
    compactPickerMode = null;
    compactPickerSource = null;
  }
  compactPickerView = view;
  compactPickerCurrent(view);
  compactPickerThinking(view.current);
  const presets = view.presets || [];
  const groups = new Map();
  for (const preset of presets) {
    if (!groups.has(preset.vendor)) groups.set(preset.vendor, []);
    groups.get(preset.vendor).push(preset);
  }
  if (compactPickerMode === null) compactPickerMode = view.active_profile ? 'custom' : 'builtin';
  if (!compactPickerSource || !groups.has(compactPickerSource)) {
    compactPickerSource = presets.find(item => item.id === view.current.preset)?.vendor
      || groups.keys().next().value || null;
  }
  const builtins = document.getElementById('model-picker-builtins');
  const custom = document.getElementById('model-picker-custom');
  builtins.setAttribute('aria-pressed', String(compactPickerMode === 'builtin'));
  custom.setAttribute('aria-pressed', String(compactPickerMode === 'custom'));
  const browser = document.getElementById('model-picker-browser');
  browser.dataset.mode = compactPickerMode;
  const sources = document.getElementById('model-picker-sources');
  const heading = document.getElementById('model-picker-section-title');
  const sourceScrollTop = sources.scrollTop;
  sources.replaceChildren();
  if (compactPickerMode === 'custom') {
    heading.textContent = '';
    const profiles = (view.profiles || []).map(name => ({
      label: name, active: view.active_profile === name,
      method: 'model.profile.activate', params: { name },
      preset: false, imageInput: view.profile_image_input?.[name],
    }));
    compactPickerRows(profiles);
    if (!profiles.length) {
      dom['model-picker-list'].append(el('p', 'model-picker-empty',
        () => CLATI18n.msg('tui.no_custom_models_yet_create_the_first_one')));
    }
    return;
  }
  for (const [vendor, group] of groups) {
    const button = el('button', 'model-picker-source');
    button.type = 'button';
    button.dataset.modelSource = vendor;
    button.setAttribute('aria-pressed', String(vendor === compactPickerSource));
    button.append(el('span', 'model-picker-source-name', vendor),
      el('span', 'model-picker-source-count', String(group.length)));
    button.addEventListener('click', () => {
      compactPickerSource = vendor;
      compactPickerRender(compactPickerView);
    });
    sources.append(button);
  }
  sources.scrollTop = sourceScrollTop;
  heading.textContent = compactPickerSource || '';
  compactPickerRows((groups.get(compactPickerSource) || []).map(preset => ({
    label: compactPresetName(preset),
    active: !view.active_profile && view.current.preset === preset.id,
    method: 'model.preset.select', params: { id: preset.id },
    preset: true, imageInput: preset.image_input,
  })));
}

// The original popover, RPC and slider save handlers remain authoritative.
renderModelPicker = compactPickerRender;
// The save handler removes data-saving only after its final Host refresh.
// Reconcile once then, including rollback after a rejected override.
const compactThinkingInput = document.getElementById('model-thinking');
new MutationObserver(() => {
  if (!compactThinkingInput.hasAttribute('data-saving') && compactPickerView) {
    compactPickerThinking(compactPickerView.current);
  }
}).observe(compactThinkingInput, { attributes: true, attributeFilter: ['data-saving'] });
dom['model-picker-manage'].replaceChildren(svgIcon('settings'));
const compactPickerTrigger = dom['model-picker-trigger'];
new MutationObserver(() => {
  if (compactPickerTrigger.getAttribute('aria-expanded') === 'false') {
    compactPickerMode = null;
    compactPickerSource = null;
  }
}).observe(compactPickerTrigger, { attributes: true, attributeFilter: ['aria-expanded'] });
document.getElementById('model-picker-builtins').addEventListener('click', () => {
  compactPickerMode = 'builtin';
  if (compactPickerView) compactPickerRender(compactPickerView);
});
document.getElementById('model-picker-custom').addEventListener('click', () => {
  compactPickerMode = 'custom';
  if (compactPickerView) compactPickerRender(compactPickerView);
});
dom['model-picker-manage'].addEventListener('click', () => closeModelPicker());
CLATI18n.onChange(() => {
  if (compactPickerView && modelPickerOpen()) compactPickerRender(compactPickerView);
});
