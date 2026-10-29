/* Application chrome remains available independently of the active content page. */
function createApplicationToolbar(shell) {
  const bar = el('header', 'application-toolbar'); bar.id = 'application-toolbar';
  const navigation = el('nav', 'application-navigation');
  const back = shellButton('application-back', 'previous', () => shellText('back'), () => context.history.go(-1));
  const forward = shellButton('application-forward', 'next', () => shellText('forward'), () => context.history.go(1));
  const list = shellButton('project-list-toggle', 'panel', () => CLATI18n.msg('web.sessions'), () => {
    if (isMobile()) dom['mobile-sidebar-open'].click(); else toggleProjectList();
  });
  list.setAttribute('aria-controls', 'sidebar');
  list.classList.add('desktop-only');
  navigation.append(back, forward, list, dom['mobile-sidebar-open']);
  const location = el('div', 'application-location');
  const project = el('span', 'application-project'); project.id = 'application-project';
  const page = el('strong', 'application-page'); page.id = 'application-page';
  location.append(svgIcon('folder'), project, el('span', 'application-separator', '/'), dom['session-title'], page);
  const actions = document.querySelector('.header-actions');
  const more = shellButton('application-session-menu', 'more', () => CLATI18n.msg('web.session_actions'), () => {
    const session = state.sessions.find(item => item.id === state.sessionId);
    if (!session) return;
    const rect = more.getBoundingClientRect();
    toggleSessionMenu(session, rect.right, rect.bottom + 4, more);
  });
  actions.prepend(more);
  const technical=document.querySelector('.session-technical');
  technical.querySelector('summary').after(document.querySelector('.conversation-meta'));
  document.querySelector('.conversation-header').remove();
  bar.append(navigation, location, actions); dom.app.prepend(bar);
  shell.toolbar = { back, forward, list, page, project, more };
  context.history.subscribe(() => refreshApplicationToolbar(shell));
  refreshApplicationToolbar(shell);
}

function refreshApplicationToolbar(shell = workbenchShell) {
  if (!shell?.toolbar) return;
  const { back, forward, list, page, project, more } = shell.toolbar;
  const route = (dom.app.dataset.page || 'workbench').split('?')[0];
  const labels = { workbench: 'workbench', terminal:'terminal', resources: 'resources', tasks: 'tasks', history: 'history', extensions: 'extensions', settings: 'settings', help: 'help' };
  CLATI18n.bind(page, 'textContent', () => shellText(labels[route.split('/')[0]] || 'workbench'));
  CLATI18n.bind(project, 'textContent', () => dom['project-name'].textContent);
  CLATI18n.attribute(project, 'title', () => dom['project-root'].textContent);
  const chat = route === 'workbench'; const readingConversation = chat || Boolean(borrowedConversation);
  list.hidden = !chat || Boolean(dom.app.dataset.tool); dom['session-title'].hidden = !chat; page.hidden = chat;
  dom['mobile-sidebar-open'].hidden = !chat || Boolean(dom.app.dataset.tool);
  more.hidden = !readingConversation; more.disabled = !state.sessionId || state.switching;
  document.getElementById('find-open').hidden = !readingConversation || Boolean(dom.app.dataset.tool);
  dom['inspector-toggle'].hidden = Boolean(dom.app.dataset.tool);
  if(dom.app.dataset.tool)more.hidden=true;
  if (!chat) closeSessionMenu();
  back.disabled = !context.history.canGo(-1); forward.disabled = !context.history.canGo(1);
  syncPanelAccessibility();
}
