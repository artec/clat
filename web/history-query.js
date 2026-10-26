/* Read-only history UI. The composer and active session are never modified. */
function installHistoryQuery() {
  const msg = (key) => CLATI18n.msg(`history.${key}`);
  const trigger = sidebarToolButton('history-open', 'history', () => msg('open'));
  document.querySelector('.sidebar-footer').prepend(trigger);
  const dialog = el('section', 'history-dialog'); dialog.id = 'history-dialog';
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
  const head = el('div', 'history-dialog-head'); head.append(heading);
  const body = el('div', 'page-list-detail'); const listing = el('div', 'page-object-list'); listing.append(results, more); body.append(listing, detail);
  dialog.append(head, form, status, body); document.body.append(dialog);
  let generation = 0; let cursor = null; let activeQuery = { trusted_projects: false }; let reader = null; let previousFocus = null;
  close.onclick = () => leavePage(dialog);
  dialog.addEventListener('clat:page-leave', () => {
    generation++; reader?.dispose(); submit.disabled = false;
    if (workbenchShell?.surfaces.get(dialog.id)?.type !== 'page') previousFocus?.focus();
  });
  trigger.onclick = () => {
    previousFocus = document.activeElement; presentPage(dialog);
    if (!results.children.length) void recent();
    if (workbenchShell?.surfaces.get(dialog.id)?.type !== 'page') query.focus();
  };
  async function attempt(work) {
    const ticket = ++generation; submit.disabled = true;
    try { await work(ticket); }
    catch (error) { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { if (ticket === generation) submit.disabled = false; }
  }
  function hitNode(hit) {
    const item = el('article', 'history-hit');
    const open = el('button', 'history-hit-open', hit.title || hit.reference.session); open.type = 'button';
    const meta = el('small', '', ()=>`${hit.reference.project || ''} · ${hit.time ? new Date(hit.time).toLocaleString(CLATI18n.locale) : ''}`);
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
  async function recent() {
    await attempt(async ticket => {
      const value = await rpc('session.list', {}); if (ticket !== generation) return;
      results.replaceChildren(...value.sessions.map(session => hitNode({title:session.title || session.id, reference:{project:state.workbench.project.root,session:session.id,seq:null},excerpt:'',time:session.last_activity_ms})));
      if (!value.sessions.length) results.append(pageEmpty('search_intro'));
    });
  }
  async function inspect(reference, ticket) {
    reader?.dispose(); detail.replaceChildren();
    reader = createConversationReader(detail, reference, {trusted:activeQuery.trusted_projects,current:()=>ticket===generation && pageIsOpen(dialog)});
    if (reference.project === state.workbench.project.root) detail.append(pageAction('continue_conversation', async()=>{
      try{if(await selectConversationTarget(reference,()=>ticket===generation))workbenchShell.chat();}catch(error){CLATI18n.bind(status,'textContent',()=>error.message);}
    }));
    const advanced=el('details','history-advanced'); advanced.append(el('summary','',()=>pageText('raw_events')));
    const rawHost=el('section'); advanced.append(rawHost); detail.append(advanced);
    advanced.addEventListener('toggle',()=>{if(advanced.open&&!rawHost.children.length)void inspectEvents(reference,ticket,rawHost);});
  }
  async function inspectEvents(reference, ticket, detail) {
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
