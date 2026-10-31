/* One compact form over the Host catalog; it never persists credentials. */
const setupText = key => CLATI18n.msg('setup.' + key);
function setupButton(key, run, cls = 'secondary') {
  const button = el('button', cls, () => setupText(key)); button.type = 'button'; button.onclick = run; return button;
}
function setupField(parent, key, type = 'text') {
  const label = el('label', 'setup-field'), input = el(type === 'select' ? 'select' : 'input');
  if (type !== 'select') { input.type = type; input.autocomplete = type === 'password' ? 'new-password' : 'off'; }
  label.append(el('span', '', () => setupText(key)), input); parent.append(label); return input;
}
function createModelSetupForm() {
  const element = el('form', 'setup-form'), route = setupField(element, 'provider', 'select');
  const preset = setupField(element, 'model', 'select');
  const custom = el('div', 'setup-custom'), name = setupField(custom, 'profile_name'); name.maxLength = 128;
  const protocol = setupField(custom, 'protocol', 'select');
  for (const [id, label] of [['open_ai_compatible','OpenAI Compatible'],['open_ai_responses','OpenAI Responses']]) {
    const option = el('option', '', label); option.value = id; protocol.append(option);
  }
  const endpoint = setupField(custom, 'endpoint', 'url'), model = setupField(custom, 'model'); model.maxLength = 256;
  const advanced = el('details', 'setup-advanced'); advanced.append(el('summary', '', () => setupText('advanced')));
  const path = setupField(advanced, 'request_path'); path.value = '/chat/completions';
  const headers = setupField(advanced, 'extra_headers'), body = setupField(advanced, 'extra_body');
  protocol.onchange = () => { path.value = protocol.value === 'open_ai_responses' ? '/responses' : '/chat/completions'; };
  custom.append(advanced); element.append(custom);
  const key = setupField(element, 'api_key', 'password'); key.maxLength = 8192;
  const links = el('div', 'setup-links'); element.append(links);
  let view = null;
  function showRoute() {
    custom.hidden = route.value !== 'custom'; preset.parentElement.hidden = route.value === 'custom';
    CLATI18n.bind(key.previousElementSibling, 'textContent', () => setupText(route.value === 'custom' ? 'api_key' : 'provider_key'));
    const presets = view?.settings.presets.filter(item => item.vendor === route.value) || [];
    preset.replaceChildren();
    for (const item of presets) { const option = el('option', '', item.name); option.value = item.id; preset.append(option); }
    links.replaceChildren();
    const help = view?.vendors.find(item => item.vendor === route.value);
    if (help) for (const [label, url] of [['get_key', help.keys_url], ['provider_help', help.help_url]]) {
      const link = el('a', '', () => setupText(label)); link.href = url; link.rel = 'noopener noreferrer';
      // Native navigation opens the system browser; browser clients use a tab.
      if (!window.__TAURI__) link.target = '_blank'; links.append(link);
    }
  }
  route.onchange = showRoute; element.onsubmit = event => event.preventDefault();
  return { element, key, fill(next) {
    view = next; route.replaceChildren();
    const empty = el('option', '', () => setupText('choose_provider')); empty.value = ''; route.append(empty);
    for (const vendor of new Set(next.settings.presets.map(item => item.vendor))) {
      const option = el('option', '', vendor); option.value = vendor; route.append(option);
    }
    const option = el('option', '', () => setupText('custom')); option.value = 'custom'; route.append(option); showRoute();
  }, payload() {
    if (!route.value) throw new Error(setupText('choose_provider'));
    if (route.value !== 'custom') return { preset: preset.value, ...(key.value ? { api_key: key.value } : {}) };
    const profile = { name: name.value.trim(), protocol: protocol.value, endpoint: endpoint.value.trim(),
      model: model.value.trim(), request_path: path.value.trim() };
    if (!profile.name || !profile.model || !profile.endpoint) throw new Error(setupText('invalid'));
    if (key.value) profile.api_key = key.value;
    if (headers.value.trim()) profile.extra_headers = JSON.parse(headers.value);
    if (body.value.trim()) profile.extra_body = JSON.parse(body.value);
    return { profile };
  } };
}
