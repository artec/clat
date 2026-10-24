/* Read-only history UI. The composer and active session are never modified. */
function installHistoryQuery() {
  const msg = (key) => CLATI18n.msg(`history.${key}`);
  const trigger = el('button', 'sidebar-footer-button', () => msg('open'));
  trigger.id = 'history-open'; trigger.type = 'button';
  document.querySelector('.sidebar-footer').prepend(trigger);
  const dialog = el('dialog', 'history-dialog'); dialog.id = 'history-dialog';
  const heading = el('h2', '', () => msg('title'));
  const close = el('button', '', () => msg('close')); close.type = 'button';
  const query = el('input', ''); query.type = 'search'; query.maxLength = 512;
  CLATI18n.bind(query, 'placeholder', () => msg('query')); CLATI18n.bind(query, 'ariaLabel', () => msg('query'));
  const all = el('input', ''); all.type = 'checkbox';
  const scope = el('label', ''); scope.append(all, el('span', '', () => msg('all_projects')));
  const submit = el('button', 'primary', () => msg('search')); submit.type = 'submit';
  const form = el('form', 'history-query-form'); form.append(query, scope, submit);
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const results = el('div', 'history-results'); const detail = el('section', 'history-detail');
  const more = el('button', '', () => msg('more')); more.type = 'button'; more.hidden = true;
  const head = el('div', 'history-dialog-head'); head.append(heading, close);
  dialog.append(head, form, status, results, more, detail); document.body.append(dialog);
  let generation = 0; let cursor = null; let activeQuery = null; let previousFocus = null;
  close.onclick = () => dialog.close();
  dialog.addEventListener('close', () => { generation++; submit.disabled = false; previousFocus?.focus(); });
  trigger.onclick = () => { previousFocus = document.activeElement; dialog.showModal(); query.focus(); };
  async function attempt(work) {
    const ticket = ++generation; submit.disabled = true;
    try { await work(ticket); }
    catch (error) { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { if (ticket === generation) submit.disabled = false; }
  }
  function hitNode(hit) {
    const item = el('article', 'history-hit');
    const open = el('button', 'history-hit-open', hit.title || hit.reference.session); open.type = 'button';
    const meta = el('small', '', `${hit.reference.project || ''} · ${hit.reference.session} · ${hit.reference.seq ?? ''}`);
    const snippet = el('p', '', hit.excerpt); // Untrusted content is text, never HTML.
    open.onclick = () => void attempt((ticket) => inspect(hit.reference, ticket));
    item.append(open, meta, snippet); return item;
  }
  async function search(append) {
    if (!append) activeQuery = { query: query.value, trusted_projects: all.checked };
    await attempt(async (ticket) => {
      const page = await rpc('history.search', { ...activeQuery, cursor: append ? cursor : null });
      if (ticket !== generation) return;
      if (!append) { results.replaceChildren(); detail.replaceChildren(); }
      results.append(...page.hits.map(hitNode)); cursor = page.next_cursor; more.hidden = !cursor;
      CLATI18n.bind(status, 'textContent', () => page.truncated ? msg('bounded') : (!results.children.length ? msg('empty') : ''));
    });
  }
  async function inspect(reference, ticket) {
    detail.replaceChildren();
    const filter = el('input', ''); CLATI18n.bind(filter, 'placeholder', () => msg('filter'));
    const events = el('div', 'history-events');
    const next = el('button', '', () => msg('more')); next.type = 'button';
    const raw = el('pre', 'history-json'); const continuation = el('button', '', () => msg('more')); continuation.type = 'button'; continuation.hidden = true;
    let after = null; let readRequest = null; let readEpoch = 0; let listEpoch = 0;
    async function loadRead(ref, append) {
      const epoch = ++readEpoch;
      const data = await rpc('history.event.read', append ? readRequest : { reference: ref, trusted_projects: activeQuery.trusted_projects });
      if (ticket !== generation || epoch !== readEpoch) return;
      raw.textContent = (append ? raw.textContent : '') + data.json;
      readRequest = { reference: ref, trusted_projects: activeQuery.trusted_projects, offset: data.next_offset, version: data.version };
      continuation.hidden = data.next_offset === null; continuation.onclick = () => void attemptRead(ref);
    }
    async function attemptRead(ref) { try { await loadRead(ref, true); } catch (error) { if (ticket === generation) if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); } }
    async function loadEvents(append) {
      const epoch = ++listEpoch;
      const data = await rpc('history.events', { reference, trusted_projects: activeQuery.trusted_projects, after_seq: append ? after : null, event_type: filter.value || null });
      if (ticket !== generation || epoch !== listEpoch) return;
      if (!append) events.replaceChildren();
      for (const event of data.events) {
        const button = el('button', 'history-event', `${event.reference.seq} · ${event.event_type} · ${event.preview}`); button.type = 'button';
        button.onclick = () => void loadRead(event.reference, false).catch((error) => { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); }); events.append(button);
      }
      after = data.next_seq; next.hidden = after === null;
    }
    const refresh = el('button', '', () => msg('events')); refresh.type = 'button';
    refresh.onclick = () => void loadEvents(false).catch((error) => { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); });
    next.onclick = () => void loadEvents(true).catch((error) => { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); });
    detail.append(el('h3', '', reference.session), filter, refresh, events, next, raw, continuation);
    await loadEvents(false); if (reference.seq !== null) await loadRead(reference, false);
    detail.scrollIntoView({ block: 'nearest' });
  }
  form.onsubmit = (event) => { event.preventDefault(); void search(false); };
  more.onclick = () => void search(true);
}
