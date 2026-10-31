/* Core owns configuration and checks; this controller owns only the setup view. */
function createSetupPage(owner) {
  const page = el('section', 'model-setup-page'), card = el('div', 'model-setup-card'); page.hidden = true;
  CLATI18n.attribute(page, 'aria-label', () => setupText('title'));
  const brand = el('div', 'setup-brand'), logo = el('img'); logo.src = '/icons/clat.svg'; logo.alt = ''; logo.width = 34; logo.height = 34;
  const language = el('div', 'setup-language'); CLATI18n.selector(language); brand.append(logo, el('strong', '', 'CLAT'), language);
  const project = el('p', 'setup-project'), form = createModelSetupForm(), caption = el('p', 'setup-model');
  const status = el('p', 'setup-status'); status.setAttribute('role', 'status');
  const cost = el('p', 'setup-note', () => setupText('cost')), actions = el('div', 'setup-actions');
  const primary = setupButton('save_verify', () => owner.apply(true), 'primary');
  const save = setupButton('save_only', () => owner.apply(false)), retry = setupButton('verify', () => owner.verify());
  const cancel = setupButton('cancel', () => owner.cancel()), edit = setupButton('edit', () => owner.edit());
  const reload = setupButton('reload', () => owner.read(true)), enter = setupButton('start_chat', () => owner.close(false), 'primary');
  const skip = setupButton('later', () => owner.close(true), 'ghost');
  const help = setupButton('help', () => { owner.close(true); openHelp('models'); }, 'ghost');
  actions.append(primary, save, retry, cancel, edit, reload, enter, skip, help);
  card.append(brand, el('p', 'setup-steps', () => setupText('steps')), el('h1', '', () => setupText('title')),
    project, el('p', 'setup-description', () => setupText('intro')), caption, form.element, cost, status, actions);
  page.append(card); dom.app.append(page);
  const notice = el('div', 'setup-composer-notice'), configure = setupButton('configure', () => owner.open()); notice.hidden = true;
  notice.append(el('span', '', () => setupText('needs_model')), configure); dom['composer-shell'].before(notice);
  for (const type of ['input', 'change']) form.element.addEventListener(type, () => { owner.pending = null; owner.generation++; });
  page.addEventListener('keydown', event => { if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); owner.close(true); } });
  return { page, form, project, caption, status, cost, notice, configure, primary, save, retry, cancel, edit, reload, enter };
}
class ModelSetupController {
  constructor() {
    Object.assign(this, { view:null, operation:null, pending:null, generation:0, reading:null,
      initial:false, supported:false, autoAttempted:false, busy:false, saved:false, admitting:false });
    this.ui = createSetupPage(this);
    window.addEventListener('clat:workbench-updated', () => this.observe());
    window.addEventListener('clat:activated', () => { if (state.workbench) this.observe(); });
    const test = setupButton('verify_settings', async () => {
      await this.open();
      if (this.view?.state === 'configured') { this.saved = true; this.render(); await this.verify(); }
    });
    document.querySelector('[data-settings-panel="models"] .settings-copy').after(test); this.render();
  }
  message(key) { CLATI18n.bind(this.ui.status, 'textContent', () => setupText(key)); }
  render() {
    const u = this.ui, checking = this.operation?.state === 'checking';
    u.form.element.hidden = !this.view || this.saved; u.cost.hidden = !this.view || this.saved;
    for (const control of u.form.element.querySelectorAll('input,select')) control.disabled = this.busy;
    u.primary.hidden = u.save.hidden = !this.view || this.saved; u.primary.disabled = u.save.disabled = this.busy;
    u.retry.hidden = !this.saved; u.retry.disabled = this.busy; u.cancel.hidden = !checking;
    u.edit.hidden = !this.saved; u.edit.disabled = this.busy; u.reload.hidden = this.busy;
    u.enter.hidden = !this.saved; u.enter.disabled = this.busy;
    u.caption.hidden = !this.saved; u.caption.textContent = this.operation?.model || this.view?.settings.current.model || '';
    u.notice.hidden = !this.supported || this.view?.state === 'configured';
    CLATI18n.bind(u.notice.firstElementChild, 'textContent', () => setupText(this.view ? 'needs_model' : 'read_failed'));
    CLATI18n.bind(u.configure, 'textContent', () => setupText(this.view ? 'configure' : 'reload'));
    u.project.textContent = [state.workbench?.project?.name, state.workbench?.project?.root].filter(Boolean).join(' · ');
    u.project.title = state.workbench?.project?.root || '';
  }
  edit() { this.operation = this.pending = null; this.saved = false; this.message('intro'); this.render(); }
  dismissed(scope) { try { return localStorage.getItem('clat.setup.later.' + scope) === '1'; } catch (_) { return false; } }
  show() { this.ui.page.hidden = false; if (context.isActive()) this.ui.page.querySelector('select')?.focus({ preventScroll: true }); }
  async read(explicit = false) {
    if (!this.supported) { if (explicit) openSettings('models'); return; }
    if (this.busy) return;
    const current = ++this.generation;
    try {
      const next = await rpc('model.setup.get', {}); if (current !== this.generation) return;
      this.view = next;
      if (!this.initial) { this.ui.form.fill(next); this.initial = true; }
      if (!this.operation) this.message(next.state === 'configured' ? 'existing' : 'intro');
      else if (this.operation.saved && this.operation.revision !== next.revision) {
        this.operation = null; this.saved = false; this.message('changed');
      }
      this.render();
      if (explicit || (!this.autoAttempted && window.__TAURI__ && next.state !== 'configured' && !this.dismissed(next.scope))) this.show();
    } catch (_) {
      if (current !== this.generation) return;
      this.message('read_failed'); this.render(); if (explicit || (!this.autoAttempted && window.__TAURI__)) this.show();
    } finally { this.autoAttempted = true; }
  }
  async open() { if (!this.supported) { openSettings('models'); return; } this.show(); await this.read(true); }
  close(later) {
    if (this.view && later) try { localStorage.setItem('clat.setup.later.' + this.view.scope, '1'); } catch (_) {}
    if (this.operation?.state === 'checking') void this.cancel();
    this.ui.page.hidden = true;
    if (!later && this.saved) workbenchShell.chat();
    if (context.isActive()) dom.prompt.focus({ preventScroll: true });
    if (!later && this.saved && !state.sessionId) updateRunState(setupText('first_prompt'));
    void refreshWorkbench();
  }
  async apply(verify) {
    if (this.busy || !this.view) return;
    try {
      this.pending ||= { method:'model.setup.apply', params:{ operation_id:crypto.randomUUID(),
        expected_revision:this.view.revision, ...this.ui.form.payload(), verify } };
      await this.dispatch(this.pending);
    } catch (_) { this.message('invalid'); }
  }
  async verify() {
    if (this.busy || !this.view) return;
    if (this.operation?.state === 'checking') return this.dispatch({ method:'model.setup.status', params:{operation_id:this.operation.operation_id} });
    this.pending ||= { method:'model.setup.verify', params:{operation_id:crypto.randomUUID(), expected_revision:this.view.revision} };
    await this.dispatch(this.pending);
  }
  async dispatch(request) {
    this.busy = true; this.message('saving'); this.render(); const current = ++this.generation;
    try {
      const result = await rpc(request.method, request.params); if (current !== this.generation) return;
      this.operation = result; this.saved = result.saved; this.pending = null;
      if (this.saved) { this.ui.form.key.value = ''; this.view.revision = result.revision; }
      if (this.ui.page.hidden && result.state === 'checking') await this.cancel();
      await this.poll(current);
      const next = await rpc('model.setup.get', {}); if (current === this.generation) this.view = next;
    } catch (error) {
      if (current !== this.generation) return;
      const key = error.presentation?.key;
      if (key?.startsWith('setup.')) { this.pending = null; this.message(key.slice(6)); }
      else this.message('uncertain');
    } finally { if (current === this.generation) { this.busy = false; this.render(); } }
  }
  async poll(current) {
    while (current === this.generation) {
      const result = this.operation;
      this.message(result.failure?.startsWith('setup.') ? result.failure.slice(6) : result.state); this.render();
      if (result.state !== 'checking') return;
      await new Promise(resolve => setTimeout(resolve, 350)); if (current !== this.generation) return;
      this.operation = await rpc('model.setup.status', {operation_id:result.operation_id});
    }
  }
  async cancel() {
    if (!this.operation || this.operation.state !== 'checking') return;
    try { this.operation = await rpc('model.setup.cancel', {operation_id:this.operation.operation_id}); this.message(this.operation.state); this.render(); }
    catch (_) { this.message('cancel_failed'); }
  }
  async ensure() {
    if (!this.supported) return true;
    if (this.admitting) return false; this.admitting = true;
    try {
      if (this.reading) await this.reading;
      try { this.view = await rpc('model.setup.get', {}); this.render(); }
      catch (_) { this.message('read_failed'); this.show(); return false; }
      if (this.view.state === 'configured') return true;
      await this.open(); return false;
    } finally { this.admitting = false; }
  }
  observe() {
    this.supported = state.workbench?.methods?.includes('model.setup.get') || false;
    if (!this.supported || this.reading || this.busy) return;
    this.reading = this.read().finally(() => { this.reading = null; });
  }
}
function createModelSetup() { return new ModelSetupController(); }
