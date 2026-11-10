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
  const form=el('form','navigation-filters');form.append(scope);const run=navigationIcon(()=>msg('search'),'search',()=>form.requestSubmit());run.type='button';run.hidden=true;form.append(run);submit.hidden=true;form.append(submit);
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const results = el('div', 'history-results'); const detail = el('section', 'history-detail');
  const more = el('button', '', () => msg('more')); more.type = 'button'; more.hidden = true;
  const view=createNavigationPage(dialog,{title:()=>shellText('history'),query,searchLabel:()=>msg('query')});
  view.filters.append(form);view.list.append(results,more);view.body.replaceWith(detail);detail.classList.add('navigation-body');view.body=detail;status.className='navigation-status';view.sidebar.insertBefore(status,view.list);document.body.append(dialog);
  let generation=0,detailGeneration=0,cursor=null,activeQuery={trusted_projects:false},reader=null,selected=null;
  view.actions([viewAction('search','search',()=>msg('search'),()=>form.requestSubmit(),true,()=>!submit.disabled),viewAction('recent','history',()=>viewText('recent_sessions'),()=>recent())]);
  view.context({crumbs:()=>[
    {label:()=>shellText('history'),run:()=>{delete dialog.dataset.detail;}},
    {label:()=>activeQuery.trusted_projects?msg('all_projects'):projectDisplayName(selected?.project||state.workbench?.project?.root,state.workbench?.project?.name)},
    ...(selected&&activeQuery.trusted_projects?[{label:()=>projectDisplayName(selected.project)}]:[]),
    ...(selected?[{label:()=>historyTitle||selected.session}]:[]),
  ]});let historyTitle='';
  function overview(hits=[]){
    if(selected)return;CLATI18n.bind(view.name,'textContent',()=>shellText('history'));
    detail.replaceChildren(navigationOverview({title:()=>pageText('history_intro_title'),description:()=>pageText('history_intro_description'),icon:'history',items:hits.slice(0,6).map(hit=>({title:hit.title||hit.reference.session,description:hit.excerpt||hit.reference.project,icon:'history',action:()=>void inspect(hit.reference,hit.title)}))}));
  }
  overview();
  dialog.addEventListener('clat:tool-reset',()=>{generation++;detailGeneration++;reader?.dispose();reader=null;selected=null;results.replaceChildren();cursor=null;more.hidden=true;overview();});
  dialog.addEventListener('clat:page-leave',()=>{generation++;submit.disabled=false;run.disabled=false;reader?.suspend();});
  dialog.addEventListener('clat:page-enter',()=>reader?.resume());
  trigger.onclick = () => {
    presentPage(dialog);
    if (!results.children.length) void recent();
    if (workbenchShell?.surfaces.get(dialog.id)?.type !== 'page') query.focus();
  };
  async function attempt(work) {
    const ticket = ++generation;submit.disabled=true;run.disabled=true;refreshViewContext();
    try { await work(ticket); }
    catch (error) { if (ticket === generation) CLATI18n.bind(status, 'textContent', () => error.message); }
    finally { if(ticket===generation){submit.disabled=false;run.disabled=false;refreshViewContext();} }
  }
  function hitNode(hit) {
    const identity = JSON.stringify(hit.reference);
    const item = navigationRow({
      title: hit.title || hit.reference.session,
      meta: () => [
        hit.time ? new Date(hit.time).toLocaleString(CLATI18n.locale) : '',
        projectDisplayName(hit.reference.project),
      ].filter(Boolean).join(' · '),
      icon: 'history',
      selected: JSON.stringify(selected) === identity,
      action: () => void inspect(hit.reference, hit.title),
    });
    if (hit.excerpt) {
      item.querySelector('.navigation-row-copy').append(el('small', 'history-hit-excerpt', hit.excerpt));
    }
    item.dataset.reference = identity;
    return item;
  }

  async function search(append) {
    if (!append) { clearReader();activeQuery = { query: query.value, trusted_projects: all.checked }; }
    await attempt(async (ticket) => {
      const page = await rpc('history.search', { ...activeQuery, cursor: append ? cursor : null });
      if (ticket !== generation) return;
      if (!append) { results.replaceChildren(); }
      results.append(...page.hits.map(hitNode)); cursor = page.next_cursor; more.hidden = !cursor;
      if(!append)overview(page.hits);
      CLATI18n.bind(status, 'textContent', () => page.truncated ? msg('bounded') : (!results.children.length ? msg('empty') : ''));
    });
  }
  function clearReader(){detailGeneration++;reader?.dispose();reader=null;selected=null;historyTitle='';overview();}
  async function recent() {
    query.value = '';
    await search(false);
  }
  async function inspect(reference,title) {
    historyTitle=title;selected=reference;const ticket=++detailGeneration,trusted=activeQuery.trusted_projects,current=()=>ticket===detailGeneration&&pageIsOpen(dialog)&&context.isActive();
    view.show(title||reference.session);for(const row of results.children)row.setAttribute('aria-current',String(row.dataset.reference===JSON.stringify(reference)));reader?.dispose();detail.replaceChildren();
    reader = createHistoryTrajectory(detail, reference, {trusted,current,locate:async target=>{
      try{if(await selectConversationTarget(target,current)){workbenchShell.chat();if(target.seq!=null)await jumpToMessage(target.seq);}}catch(error){if(current())CLATI18n.bind(status,'textContent',()=>error.message);}
    }});
  }

  all.onchange = () => void search(false);
  query.addEventListener('input', () => { if (!query.value.trim()) void search(false); });
  form.onsubmit = (event) => { event.preventDefault(); void search(false); };
  query.onkeydown=event=>{if(event.key==='Enter'){event.preventDefault();form.requestSubmit();}};
  more.onclick = () => void search(true);
}
