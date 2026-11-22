let authorizationSettings = null;
const authorizationText = (key, args) => CLATI18n.msg('authorization.' + key, args);

function authorizationButton(key, action, icon, primary = false) {
  const button = el('button', primary ? 'primary' : 'secondary');
  button.type = 'button';
  button.dataset.action = key;
  if (icon) button.append(svgIcon(icon));
  button.append(el('span', '', () => authorizationText(key)));
  if (action) button.onclick = action;
  return button;
}

function authorizationCard(provider, icon) {
  const card = el('section', 'settings-card authorization-card');
  card.dataset.provider = provider;
  const title = el('h4', '', () => authorizationText(provider));
  title.id = 'authorization-title-' + provider;
  card.setAttribute('aria-labelledby', title.id);
  const head = el('header', 'authorization-card-head');
  const identity = el('div', 'authorization-identity'), copy = el('div');
  const mark = el('span', 'authorization-mark');
  mark.append(svgIcon(icon));
  copy.append(title, el('p', '', () => authorizationText(provider + '_method')));
  identity.append(mark, copy);
  const state = el('span', 'authorization-state');
  state.setAttribute('role', 'status');
  head.append(identity, state);
  const details = el('div', 'authorization-details');
  const quota = el('div', 'authorization-quota'), actions = el('div', 'settings-row-actions');
  details.append(quota, actions);
  const message = el('p', 'authorization-message');
  message.setAttribute('role', 'status');
  card.append(head, details, message);
  return { card, state, quota, actions, message };
}

function authorizationTimestamp(instant) {
  const date = new Date(instant);
  return Number.isFinite(date.getTime()) ? date.toLocaleString(CLATI18n.locale) : '';
}

function authorizationQuota(row, view) {
  row.replaceChildren();
  if (!view?.configured) return;
  const { allowance, refreshing } = view;
  const windows = (allowance?.items || []).filter(item => item.kind === 'quota' && Number.isFinite(Number(item.value)));
  if (!windows.length) {
    const key = refreshing ? 'checking_quota' : 'quota_unavailable';
    row.append(el('p', '', () => authorizationText(key)));
    return;
  }
  for (const item of windows) {
    const line = el('div', 'authorization-quota-window');
    const meter = el('progress');
    meter.max = 100;
    meter.value = Number(item.value);
    const name = () => railWindowLabel(Number(item.window_hours), railText);
    const amount = () => (meter.value / 100).toLocaleString(CLATI18n.locale, {
      style: 'percent', maximumFractionDigits: 1,
    });
    CLATI18n.attribute(meter, 'aria-label', name);
    line.append(el('span', '', name), meter, el('strong', '', amount));
    if (item.reset_at && authorizationTimestamp(item.reset_at)) {
      line.append(el('small', '', () => authorizationText('reset_at', { time: authorizationTimestamp(item.reset_at) })));
    }
    row.append(line);
  }
  if (allowance.updated_at && authorizationTimestamp(allowance.updated_at)) {
    row.append(el('small', '', () => authorizationText(refreshing ? 'checking_quota' : 'quota_updated', {
      time: authorizationTimestamp(allowance.updated_at),
    })));
  }
}

function tencentCredentialForm(save, close) {
  const form = el('form', 'authorization-credentials');
  form.hidden = true;
  form.autocomplete = 'off';
  function field(key) {
    const input = el('input');
    input.type = 'password';
    input.required = true;
    input.maxLength = 256;
    input.autocomplete = 'off';
    input.spellcheck = false;
    const label = el('label');
    label.append(el('span', '', () => railText(key)), input);
    form.append(label);
    return input;
  }
  const id = field('tencent_secret_id'), key = field('tencent_secret_key');
  const submit = authorizationButton('save_key', null, null, true);
  submit.type = 'submit';
  const cancel = authorizationButton('cancel', close);
  const actions = el('div', 'settings-row-actions');
  actions.append(cancel, submit);
  form.append(actions);
  form.onsubmit = event => {
    event.preventDefault();
    void save({ secret_id: id.value, secret_key: key.value });
  };
  return { form, id, submit, cancel };
}

function installAuthorizationSettings() {
  const page = dom['settings-dialog'];
  const panel = page.querySelector('[data-settings-panel=authorizations]');
  const qianwen = authorizationCard('qianwen', 'sparkles');
  const bailian = authorizationCard('bailian', 'alibaba-cloud');
  const tencent = authorizationCard('tencent', 'cloud');
  const cards = { qianwen, bailian, tencent };
  let snapshot = null, timer, epoch = 0, request = 0, mutating = false, refreshing = false;
  const loginUrls = { qianwen: null, bailian: null };
  const errors = {};
  const mobile = matchMedia('(max-width: 760px)');
  const active = () => context.isActive() && pageIsOpen(page) && !panel.hidden
    && (!mobile.matches || page.dataset.detail === 'true');

  function closeTencentForm(restoreFocus = false) {
    credential.form.reset();
    credential.form.hidden = true;
    paint();
    if (restoreFocus) tencent.primary.focus();
  }

  const credential = tencentCredentialForm(
    saveTencentKey, () => closeTencentForm(true),
  );
  const refreshAll = authorizationButton('refresh_all', () => void refresh(true), 'refresh');
  const toolbar = el('div', 'authorization-toolbar');
  const feedback = el('p', 'authorization-message');
  feedback.setAttribute('role', 'status');
  toolbar.append(feedback, refreshAll);

  for (const provider of ['qianwen', 'bailian']) {
    const row = cards[provider];
    row.primary = authorizationButton('sign_in', () => void startLogin(provider), null, true);
    row.open = authorizationButton('open_login', () => void openLogin(provider), 'open', true);
    row.cancel = authorizationButton('cancel_login', () => void cancelLogin(provider));
  }
  tencent.primary = authorizationButton('add_key', showTencentForm, null, true);
  for (const [provider, row] of Object.entries(cards)) {
    row.remove = authorizationButton('remove', () => void remove(provider), 'trash');
    row.actions.append(row.remove, row.primary);
  }
  qianwen.actions.append(qianwen.cancel, qianwen.open);
  bailian.actions.append(bailian.cancel, bailian.open);
  tencent.card.append(credential.form);
  panel.append(toolbar, ...Object.values(cards).map(row => row.card));

  function showTencentForm() {
    if (!active() || mutating) return;
    delete errors.tencent;
    panel.querySelector('.secret-cleared-note')?.remove();
    credential.form.hidden = false;
    paint();
    credential.id.focus();
  }

  function label(button, key) {
    CLATI18n.bind(button.querySelector('span'), 'textContent', () => authorizationText(key));
  }

  function paintCard(provider, row) {
    const view = snapshot?.providers.find(item => item.provider === provider);
    const phase = provider === 'tencent' ? null : provider === 'qianwen' ? snapshot?.qianwen_login : snapshot?.bailian_login;
    const waiting = phase === 'starting' || phase === 'pending';
    const configured = Boolean(view?.configured);
    const key = !view ? 'loading' : waiting ? 'awaiting_approval'
      : !configured ? 'disconnected' : view.allowance?.status === 'needs_auth' ? 'reauthorize'
        : provider === 'tencent' ? 'key_saved' : 'connected';
    row.state.dataset.state = key;
    CLATI18n.bind(row.state, 'textContent', () => view || !errors.refresh ? authorizationText(key) : '');
    authorizationQuota(row.quota, view);
    row.remove.hidden = !configured || waiting || (provider === 'tencent' && !credential.form.hidden);
    row.primary.hidden = provider === 'tencent' ? !credential.form.hidden : Boolean(waiting && loginUrls[provider]);
    row.primary.disabled = mutating || !view || phase === 'starting';
    row.remove.disabled = mutating;
    if (provider === 'tencent') {
      label(row.primary, configured ? 'replace_key' : 'add_key');
    } else {
      label(row.primary, waiting ? 'continue_login' : configured ? 'sign_in_again' : 'sign_in');
      row.open.hidden = !waiting || !loginUrls[provider];
      row.open.disabled = mutating;
      row.cancel.hidden = !waiting;
      row.cancel.disabled = mutating;
    }
    const outcome = ['expired', 'denied', 'failed'].includes(phase) ? 'login_' + phase : null;
    const message = errors[provider] || outcome;
    row.message.dataset.error = String(Boolean(message));
    CLATI18n.bind(row.message, 'textContent', () => message ? authorizationText(message)
      : waiting ? authorizationText('approve_in_browser')
      : view?.configured_at && authorizationTimestamp(view.configured_at)
        ? authorizationText('configured_at', { time: authorizationTimestamp(view.configured_at) }) : '');
  }

  function paint() {
    refreshAll.disabled = refreshing || mutating || Boolean(snapshot?.providers.some(row => row.refreshing));
    credential.submit.disabled = credential.cancel.disabled = mutating;
    for (const [provider, row] of Object.entries(cards)) paintCard(provider, row);
    feedback.dataset.error = String(Boolean(errors.refresh));
    CLATI18n.bind(feedback, 'textContent', () => errors.refresh ? authorizationText(errors.refresh) : '');
  }

  async function refresh(force = false) {
    if (!active()) return;
    clearTimeout(timer);
    const issued = ++request;
    const current = () => active() && issued === request;
    refreshing = force;
    delete errors.refresh;
    if (force) paint();
    try {
      const value = await rpc('quota.authorizations.get', force ? { refresh: true } : {});
      if (!current()) return;
      snapshot = value;
      if (value.qianwen_login !== 'pending') loginUrls.qianwen = null;
      if (value.bailian_login !== 'pending') loginUrls.bailian = null;
      if (['starting', 'pending'].includes(value.qianwen_login)
        || ['starting', 'pending'].includes(value.bailian_login)
        || value.providers.some(row => row.refreshing)) {
        timer = setTimeout(() => void refresh(), 1000);
      }
    } catch {
      if (current()) errors.refresh = 'load_failed';
    } finally {
      if (current()) {
        refreshing = false;
        paint();
      }
    }
  }

  async function mutate(provider, failure, action) {
    if (!active() || mutating) return;
    clearTimeout(timer);
    request++;
    refreshing = false;
    mutating = true;
    const started = epoch;
    const current = () => active() && epoch === started;
    delete errors[provider];
    paint();
    try {
      await action(current);
    } catch {
      if (current()) errors[provider] = failure;
    } finally {
      mutating = false;
      if (active()) {
        paint();
        await refresh();
      }
    }
  }

  async function startLogin(provider = 'qianwen') {
    await mutate(provider, 'login_failed', async current => {
      const view = await rpc('quota.console.login', { provider });
      if (!current()) return;
      loginUrls[provider] = view.url;
      if (globalThis.CLATDesktop?.openInBrowser) await openLogin(provider);
    });
    if (active() && loginUrls[provider]) cards[provider].open.focus();
  }

  async function openLogin(provider = 'qianwen') {
    const url = loginUrls[provider];
    if (!url) return;
    const started = epoch;
    try { await openMarkdownUrl(url); }
    catch { if (active() && epoch === started) { errors[provider] = 'open_failed'; paint(); } }
  }

  async function cancelLogin(provider = 'qianwen') {
    const started = epoch;
    await mutate(provider, 'action_failed', async current => {
      await rpc('quota.console.cancel', { provider });
      if (current()) loginUrls[provider] = null;
    });
    if (active() && epoch === started) cards[provider].primary.focus();
  }

  async function saveTencentKey(params) {
    const started = epoch;
    await mutate('tencent', 'save_failed', async current => {
      await rpc('quota.tencent.key', params);
      if (current()) closeTencentForm();
    });
    if (active() && epoch === started && credential.form.hidden) tencent.primary.focus();
  }

  async function remove(provider) {
    const started = epoch;
    const name = authorizationText(provider);
    if (!await CLATDialog.confirm(authorizationText('remove_confirm', { provider: name }))) return;
    if (!active() || epoch !== started) return;
    await mutate(provider, 'action_failed', async () => { await rpc('quota.authorizations.remove', { provider }); });
    if (active() && epoch === started) cards[provider].primary.focus();
  }

  function leave() {
    epoch++;
    request++;
    clearTimeout(timer);
    loginUrls.qianwen = null;
    loginUrls.bailian = null;
    refreshing = false;
    credential.form.reset();
    credential.form.hidden = true;
    for (const key of Object.keys(errors)) delete errors[key];
  }

  page.addEventListener('clat:settings-leave', leave);
  new MutationObserver(() => {
    if (mobile.matches && !page.dataset.detail && !panel.hidden) leave();
  }).observe(page, { attributes: true, attributeFilter: ['data-detail'] });
  for (const event of ['clat:page-enter', 'clat:authorizations-enter']) {
    page.addEventListener(event, () => { if (active() && !mutating) void refresh(); });
  }
  window.addEventListener('clat:project-activated', () => {
    if (active()) { if (!mutating) void refresh(); }
    else leave();
  });
  paint();
  return {
    open(provider) {
      openSettings('authorizations');
      if (!active()) return;
      if (provider === 'tencent') showTencentForm();
      else if (provider === 'qianwen' || provider === 'bailian') void startLogin(provider);
    },
  };
}
