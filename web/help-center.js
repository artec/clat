/* Local, context-aware guidance. Navigation delegates to existing page owners. */
const helpText = key => CLATI18n.msg('help.' + key);
let helpCenter = null;

function currentHelpTopic(view) {
  if (view.id === 'settings-dialog') return ({terminal:'terminal_font',web:'network',desktop:'about',git:'git_settings'})[currentSettingsCategory] || currentSettingsCategory;
  return ({conversation:'chat','file-browser':'files','workspace-review':'git','turn-review':'operations','task-review':'processes','workflow-details':'workflow','terminal-page':'terminal','resources-dialog':'resources','jobs-dialog':'tasks','history-dialog':'history','market-dialog':'extensions'})[view.id] || 'chat';
}

function captureHelpSource() {
  const shell = workbenchShell;
  if (!shell || shell.page === 'help-dialog') return null;
  const page = shell.page, category = currentSettingsCategory, tool = shell.tools.current()?.id;
  const project = state.workbench?.project?.root, session = state.sessionId;
  return {
    topic: currentHelpTopic(activeViewContext(shell)),
    valid: () => context.isActive() && project === state.workbench?.project?.root && session === state.sessionId,
    restore() {
      if (!this.valid()) return;
      if (page === 'settings-dialog') openSettings(category);
      else if (page) shell.open(page);
      else if (tool) shell.tools.open(tool);
      else shell.chat();
    },
  };
}

function openHelp(topic) { helpCenter?.open(topic); }

function helpDestination(topic) {
  if (topic.target.startsWith('settings:')) return () => openSettings(topic.target.slice(9));
  if (topic.target === 'conversation') return () => workbenchShell.chat();
  if (WORKSPACE_TOOLS.some(tool => tool.id === topic.target)) return () => workbenchShell.tools.open(topic.target);
  return () => workbenchShell.open(topic.target);
}

function helpSearchText(topic) {
  return [topic.id, ...['title','summary','step_1','step_2','step_3','question','answer'].map(key => helpText(topic.id+'_'+key))].join(' ').toLocaleLowerCase();
}

function createHelpArticle(topic, select) {
  const article = el('article','help-article navigation-content');
  const heading = el('header','help-intro');
  heading.append(svgIcon(topic.icon), el('h2','',()=>helpText(topic.id+'_title')), el('p','',()=>helpText(topic.id+'_summary')));
  const open = el('button','secondary',()=>helpText('open'));
  open.type='button';open.onclick=helpDestination(topic);heading.append(open);
  const steps = el('section','help-steps'), list = el('ol');
  steps.append(el('h3','',()=>helpText('steps')));
  for (let i=1;i<=3;i++) list.append(el('li','',()=>helpText(topic.id+'_step_'+i)));
  steps.append(list);
  const questions = el('section','help-questions');
  questions.append(el('h3','',()=>helpText('questions')), el('h4','',()=>helpText(topic.id+'_question')), el('p','',()=>helpText(topic.id+'_answer')));
  const related = el('section','help-related'); related.append(el('h3','',()=>helpText('related')));
  for (const id of topic.related) {
    const item=HELP_TOPICS.find(row=>row.id===id);
    related.append(navigationRow({title:()=>helpText(id+'_title'),icon:item.icon,action:()=>select(id)}));
  }
  article.append(heading,steps,questions,related); return article;
}

function createHelpCenter() {
  const page=el('section','help-page');page.id='help-dialog';
  const view=createNavigationPage(page,{title:()=>shellText('help'),searchLabel:()=>helpText('search')});
  let selected='chat',source=null,requested=null,selection=0;const positions=new Map();
  function select(id) {
    const topic=HELP_TOPICS.find(row=>row.id===id);if(!topic)return;
    positions.set(selected,view.body.scrollTop);selected=id;
    view.show(()=>helpText(topic.id+'_title'));view.body.replaceChildren(createHelpArticle(topic,select));
    const top=positions.get(id)||0,version=++selection;view.body.scrollTop=top;renderList();
    // Apply after the shared page-enter scroll restoration, using this guide's position.
    requestAnimationFrame(()=>{
      if(version!==selection||!context.isActive()||!pageIsOpen(page))return;
      view.body.scrollTop=top;view.list.querySelector('[aria-current=true]')?.scrollIntoView({block:'nearest'});
    });
    if(pageIsOpen(page)&&context.isActive()&&!workbenchShell.routing)context.navigatePage('help?topic='+encodeURIComponent(id));
  }
  function renderList() {
    const words=view.query.value.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
    const matches=HELP_TOPICS.filter(topic=>words.every(word=>helpSearchText(topic).includes(word)));
    view.list.replaceChildren();
    for(const group of ['conversation','tools','pages','settings']){
      const rows=matches.filter(topic=>topic.group===group);if(!rows.length)continue;
      const section=el('section','help-topic-group');section.append(el('h3','',()=>helpText(group)));
      for(const topic of rows)section.append(navigationRow({title:()=>helpText(topic.id+'_title'),icon:topic.icon,selected:topic.id===selected,action:()=>select(topic.id)}));
      view.list.append(section);
    }
    if(!matches.length){
      const empty=el('section','help-no-results');empty.setAttribute('role','status');empty.append(el('p','',()=>helpText('empty')));
      const clear=el('button','secondary',()=>helpText('clear'));clear.type='button';clear.onclick=()=>{view.query.value='';renderList();view.query.focus();};empty.append(clear);view.list.append(empty);
    }
    return matches;
  }
  const back=viewAction('help-source','previous',()=>helpText('back'),()=>source?.restore());
  back.hidden=()=>!source?.valid();view.actions([back]);
  view.query.oninput=renderList;
  view.query.onkeydown=event=>{if(event.key==='Enter'&&!event.isComposing){const first=renderList()[0];if(first){event.preventDefault();select(first.id);view.body.querySelector('button')?.focus();}}};
  page.addEventListener('clat:page-enter',()=>{
    const params=new URLSearchParams(location.hash.split('?')[1]||'');
    select(requested||params.get('topic')||selected);requested=null;
  });
  CLATI18n.onChange(()=>{renderList();refreshViewContext();});
  document.body.append(page);select(selected);
  return {open(topic){
    const origin=captureHelpSource();if(origin){source=origin;view.query.value='';}
    const route=workbenchShell.routing?new URLSearchParams(location.hash.split('?')[1]||'').get('topic'):null;
    const id=topic||route||origin?.topic||selected;
    requested=HELP_TOPICS.some(row=>row.id===id)?id:'chat';
    if(pageIsOpen(page)){select(requested);requested=null;}
    // Publish the topic route on entry, without an extra bare help history entry.
    else workbenchShell.show(page.id,false);
  }};
}
