/* Relocate existing interaction cards without duplicating request or answer state. */
function installInteractionDock() {
  const dock = el('section', 'interaction-dock'); dock.id = 'interaction-dock'; dock.hidden = true;
  CLATI18n.attribute(dock, 'aria-label', () => shellText('pending_interactions'));
  const header = el('div', 'interaction-dock-header');
  const count = el('span', 'interaction-count'); count.setAttribute('role', 'status');
  const content = el('div', 'interaction-dock-content');
  const model = { entries: [], current: null, collapsed: false, queued: false };
  const previous = shellButton('interaction-previous', 'previous', () => shellText('previous'), () => select(-1));
  const next = shellButton('interaction-next', 'next', () => shellText('next'), () => select(1));
  const origin = shellButton('interaction-origin', 'history', () => shellText('original_message'), () => {
    if (model.current?.anchor.closest('.schedule-interactions')) workbenchShell.open('schedules-dialog');
    model.current?.anchor.scrollIntoView({block:'center', behavior:'smooth'});
  });
  const collapse = shellButton('interaction-collapse', 'collapse', () => shellText('collapse_interaction'), () => {
    model.collapsed = !model.collapsed; render();
  });
  header.append(count, previous, next, origin, collapse); dock.append(header, content);
  dom['composer-shell'].before(dock);
  function select(delta) {
    const index = model.entries.indexOf(model.current);
    model.current = model.entries[(index + delta + model.entries.length) % model.entries.length];
    model.collapsed = false; render();
  }
  function render() {
    dock.hidden = model.entries.length === 0;
    if (!model.entries.includes(model.current)) model.current = model.entries[0];
    for (const entry of model.entries) entry.card.hidden = model.collapsed || entry !== model.current;
    CLATI18n.bind(count, 'textContent', () => CLATI18n.msg('workbench.interaction_count', {
      current: Math.max(0, model.entries.indexOf(model.current) + 1), total:model.entries.length,
    }));
    previous.disabled = next.disabled = model.entries.length < 2;
    collapse.setAttribute('aria-expanded', String(!model.collapsed));
    document.getElementById('nav-workbench').dataset.pending = String(model.entries.length > 0);
    document.getElementById('nav-tasks').dataset.pending = String(model.entries.some(entry => entry.anchor.closest('.schedule-interactions')));
    context.changed();
  }
  function sync() {
    model.queued = false;
    for (const entry of [...model.entries]) {
      if (context.root.contains(entry.anchor) && !entry.card.classList.contains('resolved')) continue;
      if (context.root.contains(entry.anchor)) { entry.card.hidden = false; entry.anchor.replaceWith(entry.card); }
      else entry.card.remove();
      model.entries.splice(model.entries.indexOf(entry), 1);
    }
    for (const card of document.querySelectorAll('#transcript .question-card:not(.resolved), #transcript .approval-card:not(.resolved), .schedule-interactions .question-card:not(.resolved), .schedule-interactions .approval-card:not(.resolved)')) {
      const anchor = shellButton('', 'info', () => shellText('respond_below'), () => {
        workbenchShell.chat();
        model.current = model.entries.find(entry => entry.card === card); model.collapsed = false; render();
        dock.scrollIntoView({block:'nearest'}); card.querySelector('button,textarea')?.focus();
      });
      anchor.classList.add('interaction-anchor'); card.before(anchor);
      model.entries.push({card, anchor}); content.append(card);
    }
    render();
  }
  const ownsCard = node => node.nodeType === 1 && (node.matches('.question-card,.approval-card,.interaction-anchor')
    || node.querySelector('.question-card,.approval-card,.interaction-anchor'));
  const observer = new MutationObserver(records => {
    if (model.queued || !records.some(record => record.type === 'attributes'
      ? record.target.matches('.question-card,.approval-card')
      : [...record.addedNodes, ...record.removedNodes].some(ownsCard))) return;
    model.queued = true; queueMicrotask(sync);
  });
  observer.observe(dom.transcript, {childList:true, subtree:true, attributes:true, attributeFilter:['class']});
  observer.observe(content, {childList:true, subtree:true, attributes:true, attributeFilter:['class']});
  const scheduled = document.querySelector('.schedule-interactions');
  if (scheduled) observer.observe(scheduled, {childList:true, subtree:true, attributes:true, attributeFilter:['class']});
}
