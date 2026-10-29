/* Read-only history UI. The composer and active session are never modified. */
function installHistoryQuery() {
  const msg = (key) => CLATI18n.msg(`history.${key}`);
  const trigger = sidebarToolButton('history-open', 'history', () => msg('open'));
  document.querySelector('.sidebar-footer').prepend(trigger);
  const dialog = el('section', 'history-dialog'); dialog.id = 'history-dialog';
  const query = el('input', ''); query.type = 'search'; query.maxLength = 512;
  CLATI18n.bind(query, 'placeholder', () => msg('query')); CLATI18n.bind(query, 'ariaLabel', () => msg('query'));
  const all = el('input', ''); all.type = 'checkbox';
  const scope = el('label', ''); scope.append(all, el('span', '', () => msg('all_projects')));
  const submit = el('button', 'primary', () => msg('search')); submit.type = 'submit';
  const form=el('form','navigation-filters');form.append(scope);const run=navigationIcon(()=>msg('search'),'search',()=>form.requestSubmit());run.type='button';form.append(run);submit.hidden=true;form.append(submit);
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const results = el('div', 'history-results'); const detail = el('section', 'history-detail');
  const more = el('button', '', () => msg('more')); more.type = 'button'; more.hidden = true;
  const view=createNavigationPage(dialog,{title:()=>shellText('history'),query,searchLabel:()=>msg('query')});
  view.filters.append(form);view.list.append(results,more);view.body.replaceWith(detail);detail.classList.add('navigation-body');view.body=detail;status.className='navigation-status';view.sidebar.insertBefore(status,view.list);document.body.append(dialog);
  let generation=0,detailGeneration=0,cursor=null,activeQuery={trusted_projects:false},reader=null,selected=null;
  function overview(hits=[]){
    if(selected)return;CLATI18n.bind(view.name,'textContent',()=>shellText('history'));
    detail.replaceChildren(navigationOverview({title:()=>pageText('history_intro_title'),description:()=>pageText('history_intro_description'),icon:'history',items:hits.slice(0,6).map(hit=>({title:hit.title||hit.reference.session,description:hit.excerpt||hit.reference.project,icon:'history',action:()=>void inspect(hit.reference,hit.title)}))}));
  }
  overview();
  dialog.addEventListener('clat:page-leave',()=>{generation++;submit.disabled=false;run.disabled=false;});
  dialog.addEventListener('clat:page-enter',()=>{if(selected&&!detail.querySelector('.reader-messages')?.children.length)void reader?.refresh();});
  trigger.onclick = () => {
    presentPage(dialog);
    if (!results.children.length) void recent();
    if (workbenchShell?.surfaces.get(dialog.id)?.type !== 'page') query.focus();
  };
  async function attempt(work) {
    const ticket = ++generation;submit.disabled=true;run.disabled=true;
    try { await work(ticket); }
    catch (error) { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { if(ticket===generation){submit.disabled=false;run.disabled=false;} }
  }
  function hitNode(hit) {
    const item=navigationRow({title:hit.title||hit.reference.session,meta:()=>[hit.excerpt,hit.time?new Date(hit.time).toLocaleString(CLATI18n.locale):'',hit.reference.project||''].filter(Boolean).join(' · '),icon:'history',selected:JSON.stringify(selected)===JSON.stringify(hit.reference),action:()=>void inspect(hit.reference,hit.title)});
    item.dataset.reference=JSON.stringify(hit.reference);return item;
  }

  async function search(append) {
    if (!append) activeQuery = { query: query.value, trusted_projects: all.checked };
    await attempt(async (ticket) => {
      const page = await rpc('history.search', { ...activeQuery, cursor: append ? cursor : null });
      if (ticket !== generation) return;
      if (!append) { results.replaceChildren(); }
      results.append(...page.hits.map(hitNode)); cursor = page.next_cursor; more.hidden = !cursor;
      if(!append)overview(page.hits);
      CLATI18n.bind(status, 'textContent', () => page.truncated ? msg('bounded') : (!results.children.length ? msg('empty') : ''));
    });
  }
  async function recent() {
    await attempt(async ticket => {
      const value = await rpc('session.list', {}); if (ticket !== generation) return;
      results.replaceChildren(...value.sessions.map(session => hitNode({title:session.title || session.id, reference:{project:state.workbench.project.root,session:session.id,seq:null},excerpt:'',time:session.last_activity_ms})));
      overview(value.sessions.map(session=>({title:session.title||session.id,reference:{project:state.workbench.project.root,session:session.id,seq:null}})));
      if (!value.sessions.length) results.append(pageEmpty('search_intro'));
    });
  }
  async function inspect(reference,title) {
    selected=reference;const ticket=++detailGeneration,trusted=activeQuery.trusted_projects,current=()=>ticket===detailGeneration&&pageIsOpen(dialog)&&context.isActive();
    view.show(title||reference.session);for(const row of results.children)row.setAttribute('aria-current',String(row.dataset.reference===JSON.stringify(reference)));reader?.dispose();detail.replaceChildren();
    reader = createConversationReader(detail, reference, {trusted,current});
    if (reference.project === state.workbench.project.root) detail.append(pageAction('continue_conversation', async()=>{
      try{if(await selectConversationTarget(reference,current))workbenchShell.chat();}catch(error){CLATI18n.bind(status,'textContent',()=>error.message);}
    }));
    const advanced=el('details','history-advanced'); advanced.append(el('summary','',()=>pageText('raw_events')));
    const rawHost=el('section'); advanced.append(rawHost); detail.append(advanced);
    advanced.addEventListener('toggle',()=>{if(advanced.open&&!rawHost.children.length)void inspectHistoryEvents(rawHost,reference,{trusted,current,status,msg});});
  }

  form.onsubmit = (event) => { event.preventDefault(); void search(false); };
  query.onkeydown=event=>{if(event.key==='Enter'){event.preventDefault();form.requestSubmit();}};
  more.onclick = () => void search(true);
}

function inspectHistoryEvents(detail,reference,{trusted,current,status,msg}) {
    detail.replaceChildren();
    const filter = el('input', ''); CLATI18n.bind(filter, 'placeholder', () => msg('filter'));
    const events = el('div', 'history-events');
    const next = el('button', '', () => msg('more')); next.type = 'button';
    const raw = el('pre', 'history-json'); const continuation = el('button', '', () => msg('more')); continuation.type = 'button'; continuation.hidden = true;
    let after = null; let readRequest = null; let readEpoch = 0; let listEpoch = 0;
    async function loadRead(ref, append) {
      const epoch = ++readEpoch;
      const data = await rpc('history.event.read', append ? readRequest : { reference: ref, trusted_projects:trusted });
      if (!current() || epoch !== readEpoch) return;
      raw.textContent = (append ? raw.textContent : '') + data.json;
      readRequest = { reference: ref, trusted_projects:trusted, offset: data.next_offset, version: data.version };
      continuation.hidden = data.next_offset === null; continuation.onclick = () => void attemptRead(ref);
    }
    async function attemptRead(ref) { try { await loadRead(ref, true); } catch (error) { if (current()) CLATI18n.bind(status, 'textContent', () => error.message); } }
    async function loadEvents(append) {
      const epoch = ++listEpoch;
      const data = await rpc('history.events', { reference, trusted_projects:trusted, after_seq: append ? after : null, event_type: filter.value || null });
      if (!current() || epoch !== listEpoch) return;
      if (!append) events.replaceChildren();
      for (const event of data.events) {
        const button = el('button', 'history-event', `${event.reference.seq} · ${event.event_type} · ${event.preview}`); button.type = 'button';
        button.onclick = () => void loadRead(event.reference, false).catch((error) => { if (current()) CLATI18n.bind(status, 'textContent', () => error.message); }); events.append(button);
      }
      after = data.next_seq; next.hidden = after === null;
    }
    const refresh = el('button', '', () => msg('events')); refresh.type = 'button';
    refresh.onclick = () => void loadEvents(false).catch((error) => { if (current()) CLATI18n.bind(status, 'textContent', () => error.message); });
    next.onclick = () => void loadEvents(true).catch((error) => { if (current()) CLATI18n.bind(status, 'textContent', () => error.message); });
    detail.append(el('h3', '', reference.session), filter, refresh, events, next, raw, continuation);
    return loadEvents(false).then(()=>reference.seq!==null?loadRead(reference,false):undefined).catch(error=>{if(current())CLATI18n.bind(status,'textContent',()=>error.message);});
  }
