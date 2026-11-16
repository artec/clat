/* Read-only history UI. The composer and active session are never modified. */
function installHistoryQuery() {
  let source = 'local';
  const msg = (key) => CLATI18n.msg(`history.${key}`);
  const trigger = sidebarToolButton('history-open', 'history', () => msg('open'));
  document.querySelector('.sidebar-footer').prepend(trigger);
  const dialog = el('section', 'history-dialog'); dialog.id = 'history-dialog';
  const query = el('input', ''); query.type = 'search'; query.maxLength = 512;
  const {element: scope, input: all, text: trustedProjects} = navigationTrustedProjects('history-all-projects');
  const upload = el('input'); upload.type = 'file'; upload.accept = '.zip,application/zip'; upload.hidden = true;
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const results = el('div', 'history-results');
  const more = el('button', '', () => msg('more')); more.type = 'button'; more.hidden = true;
  const view=createNavigationPage(dialog,{title:()=>shellText('history'),query,searchLabel:()=>msg(source==='imported'?'archive_query':'query'),onOverview:clearReader});
  const detail=view.body;detail.classList.add('history-detail');
  view.filters.append(scope);view.list.append(results,more);status.className='navigation-status';view.sidebar.insertBefore(status,view.list);document.body.append(dialog);
  let generation=0,detailGeneration=0,cursor=null,activeQuery={trusted_projects:false},reader=null,selected=null,searching=false;
  let transferring=false, overviewHits=[];
  const tabs = navigationTabs([
    ['local', () => msg('archive_local')],
    ['imported', () => msg('archive_imported')],
  ], id => {
    if (source === id) return;
    selectSource(id); void search(false);
  });
  selectSource(source);
  view.filters.append(tabs.element, upload);
  view.actions([
    viewAction('search','search',()=>msg('search'),()=>search(false),true,()=>!searching),
    viewAction('archive-import','upload',()=>msg('archive_import'),()=>upload.click(),true,()=>!transferring),
    viewAction('archive-export','download',()=>msg('archive_export'),()=>download(),true,()=>Boolean(selected)&&!transferring),
  ]);
  view.context({crumbs:()=>[
    {label:()=>shellText('history'),run:clearReader},
    {label:()=>activeQuery.trusted_projects?trustedProjects():projectDisplayName(selected?.project||state.workbench?.project?.root,state.workbench?.project?.name)},
    ...(selected&&activeQuery.trusted_projects?[{label:()=>projectDisplayName(selected.project)}]:[]),
    ...(selected?[{label:()=>historyTitle||selected.session}]:[]),
  ]});let historyTitle='';
  function overview(){
    if(selected)return;CLATI18n.bind(view.name,'textContent',()=>shellText('history'));
    const imported=source==='imported';
    detail.replaceChildren(navigationOverview({
      title:()=>imported?msg('archive_imported'):pageText('history_intro_title'),
      description:()=>imported?msg('archive_intro'):pageText('history_intro_description'),
      icon:'history',
      items:overviewHits.map(hit=>({title:hit.title||hit.source||hit.reference.session,description:hit.excerpt||hit.reference.project,icon:'history',action:()=>void inspect(hit.reference,hit.title||hit.source,hit.source)})),
    }));
  }
  overview();
  dialog.addEventListener('clat:tool-reset',()=>{generation++;selectSource('local');results.replaceChildren();cursor=null;more.hidden=true;overviewHits=[];clearReader();});
  dialog.addEventListener('clat:page-leave',()=>{generation++;searching=false;reader?.suspend();});
  dialog.addEventListener('clat:page-enter',()=>reader?.resume());
  trigger.onclick = () => {
    presentPage(dialog);
    if (!results.children.length) void recent();
    if (workbenchShell?.surfaces.get(dialog.id)?.type !== 'page') query.focus();
  };
  async function attempt(work) {
    const ticket = ++generation;searching=true;refreshViewContext();
    try { await work(ticket); }
    catch (error) { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { if(ticket===generation){searching=false;refreshViewContext();} }
  }
  function hitNode(hit) {
    const identity = JSON.stringify(hit.reference);
    const item = navigationRow({
      title: hit.title || hit.source || hit.reference.session,
      meta: () => [
        hit.time ? new Date(hit.time).toLocaleString(CLATI18n.locale) : '',
        projectDisplayName(hit.reference.project),
        hit.main === false ? msg('archive_subagent') : '',
      ].filter(Boolean).join(' · '),
      icon: 'history',
      selected: JSON.stringify(selected) === identity,
      action: () => void inspect(hit.reference, hit.title||hit.source, hit.source),
    });
    if (hit.excerpt) {
      item.querySelector('.navigation-row-copy').append(el('small', 'history-hit-excerpt', hit.excerpt));
    }
    item.dataset.reference = identity;
    return item;
  }

  async function search(append) {
    if (!append) { activeQuery = { query: query.value, trusted_projects: all.checked }; overviewHits=[];clearReader(); }
    await attempt(async (ticket) => {
      const page = source === 'imported'
        ? {hits: await rpc('history.archive.list', activeQuery)}
        : await rpc('history.search', { ...activeQuery, cursor: append ? cursor : null });
      if (ticket !== generation) return;
      if (!append) { results.replaceChildren(); }
      results.append(...page.hits.map(hitNode)); cursor = page.next_cursor ?? null; more.hidden = !cursor;
      if(!append){overviewHits=page.hits.slice(0,6);overview();}
      CLATI18n.bind(status, 'textContent', () => page.truncated ? msg('bounded') : (!results.children.length ? msg(source==='imported'?'archive_empty':'empty') : ''));
    });
  }
  function clearReader(){
    detailGeneration++;reader?.dispose();reader=null;selected=null;historyTitle='';
    for(const row of results.children)row.setAttribute('aria-current','false');
    view.reset();overview();detail.scrollTop=0;
  }
  function selectSource(id) {
    source = id; tabs.select(id);
    for (const attribute of ['placeholder', 'aria-label']) {
      CLATI18n.attribute(query, attribute, () => msg(source === 'imported' ? 'archive_query' : 'query'));
    }
  }
  async function recent() {
    query.value = '';
    await search(false);
  }
  async function inspect(reference,title,sourceId) {
    historyTitle=title;selected=reference;const ticket=++detailGeneration,trusted=activeQuery.trusted_projects,current=()=>ticket===detailGeneration&&pageIsOpen(dialog)&&context.isActive();
    view.show(title||reference.session);for(const row of results.children)row.setAttribute('aria-current',String(row.dataset.reference===JSON.stringify(reference)));reader?.dispose();detail.replaceChildren();
    reader = createHistoryTrajectory(detail, reference, {trusted,current,sourceId,locate:sourceId?null:async target=>{
      try{if(await selectConversationTarget(target,current)){workbenchShell.chat();if(target.seq!=null)await jumpToMessage(target.seq);}}catch(error){if(current())CLATI18n.bind(status,'textContent',()=>error.message);}
    }});
    refreshViewContext();
  }

  async function transfer(action, work) {
    if (transferring) return;
    transferring = true; refreshViewContext();
    const ownership = generation;
    const current = () => ownership === generation && pageIsOpen(dialog) && context.isActive();
    CLATI18n.bind(status, 'textContent', () => msg(action));
    try { await work(current); }
    catch (error) { if (current()) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { transferring = false; refreshViewContext(); }
  }

  async function download() {
    if (!selected) return;
    const reference = selected;
    const options = { reference, trusted_projects: activeQuery.trusted_projects, imported: source==='imported' };
    await transfer('archive_exporting', async current => {
      const response = await requestHistoryArchive('export', options);
      const blob = await response.blob();
      if (!current() || selected !== reference) return;
      const filename = response.headers.get('Content-Disposition')?.match(/filename="(dsh-session-[A-Za-z0-9_-]+\.zip)"/)?.[1] || 'dsh-session.zip';
      const url = URL.createObjectURL(blob);
      const anchor = el('a'); anchor.href = url; anchor.download = filename;
      document.body.append(anchor); anchor.click(); anchor.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      CLATI18n.bind(status, 'textContent', () => '');
    });
  }

  upload.onchange = () => {
    const file = upload.files[0]; upload.value = '';
    if (file) void transfer('archive_importing', async current => {
      const response = await requestHistoryArchive('import', file);
      const {value: rows} = await response.json();
      if (!current()) return;
      selectSource('imported'); query.value = '';
      const expected = generation + 1;
      await search(false);
      if (generation !== expected || !pageIsOpen(dialog) || !context.isActive()) return;
      const main = rows.find(row => row.main);
      if (main) await inspect(main.reference, main.title||main.source, main.source);
    });
  };

  all.onchange = () => void search(false);
  query.addEventListener('input', () => { if (!query.value.trim()) void search(false); });
  query.onkeydown=event=>{if(event.key==='Enter'){event.preventDefault();void search(false);}};
  more.onclick = () => void search(true);
}

async function requestHistoryArchive(action, data) {
  const importing = action === 'import';
  const response = await fetch(workspacePrefix + '/api/history.archive.' + action, {
    method: 'POST',
    headers: { ...authHeaders(), 'Content-Type': importing ? 'application/zip' : 'application/json' },
    body: importing ? data : JSON.stringify(data),
  });
  if (!response.ok) {
    const body = await response.json().catch(() => null);
    const error = body?.error;
    throw CLATI18n.error(CLATI18n.view(() => CLATI18n.present(error?.presentation, error?.message || CLATI18n.msg('history.archive_failed', {detail:''}))));
  }
  return response;
}
