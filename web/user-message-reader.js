/* Bounded user-message presentation; original text remains the search/model source. */
function createUserMessageReader() {
  const entries = new Map();
  let dialog = null, active = null;
  const text = key => CLATI18n.msg('user_reader.' + key);
  function measure(content) {
    const entry = entries.get(content);
    if (!entry || !content.getClientRects().length) return;
    const height = parseFloat(getComputedStyle(content).lineHeight) * 6;
    const collapsed = content.scrollHeight > height + 1;
    entry.body.classList.toggle('user-message-collapsed', collapsed);
    content.inert = collapsed;
    if (collapsed) {
      entry.body.setAttribute('role', 'button'); entry.body.tabIndex = 0;
      entry.body.setAttribute('aria-haspopup', 'dialog');
      CLATI18n.attribute(entry.body, 'aria-label', () => text('open'));
    } else {
      for (const name of ['role', 'tabindex', 'aria-haspopup', 'aria-label']) entry.body.removeAttribute(name);
    }
  }
  const observer = new ResizeObserver(records => {
    for (const record of records) measure(record.target);
  });
  // Finish height-changing previews before history restores its reading anchor.
  function refresh() { for (const content of entries.keys()) measure(content); }
  function close() { if (dialog?.open) dialog.close(); }
  function open(entry, match) {
    dialog ||= createUserReaderDialog(() => active, () => {
      if (active?.body.isConnected && context.isActive()) active.body.focus({preventScroll:true});
      active = null;
    });
    active = entry;
    const content = dialog.querySelector('pre'); content.replaceChildren();
    dialog.querySelector('[role=status]').textContent = '';
    if (match) {
      content.append(document.createTextNode(entry.source.slice(0, match.offset)));
      const mark = el('mark', '', entry.source.slice(match.offset, match.offset + match.length));
      content.append(mark, document.createTextNode(entry.source.slice(match.offset + match.length)));
    } else content.textContent = entry.source;
    if (!dialog.open) dialog.showModal();
    content.scrollTop = 0;
    content.querySelector('mark')?.scrollIntoView({block:'center'});
  }
  function attach(node, body, source) {
    if (!source) return;
    const content = el('div', 'user-message-preview rich-text');
    content.append(...body.childNodes); body.classList.remove('rich-text'); body.append(content);
    const entry = { node, body, source:String(source) }; entries.set(content, entry);
    body.onclick = event => {
      if (!body.classList.contains('user-message-collapsed') || window.getSelection()?.toString()) return;
      event.preventDefault(); open(entry);
    };
    body.onkeydown = event => {
      if (body.classList.contains('user-message-collapsed') && ['Enter', ' '].includes(event.key)) { event.preventDefault(); open(entry); }
    };
    observer.observe(content);
  }
  function reveal(node, match) {
    const entry = [...entries.values()].find(entry => entry.node === node);
    if (entry?.body.classList.contains('user-message-collapsed')) open(entry, match);
  }
  function reset() { close(); observer.disconnect(); entries.clear(); }
  return { attach, reveal, reset, close, refresh };
}
function createUserReaderDialog(getActive, onClose) {
    const text = key => CLATI18n.msg('user_reader.' + key);
    const dialog = el('dialog', 'user-message-reader');
    const title = el('h2', '', () => text('title'));
    title.id = 'user-message-reader-title'; dialog.setAttribute('aria-labelledby', title.id);
    const dismiss = el('button', 'icon-button'); dismiss.type = 'button'; dismiss.autofocus = true;
    dismiss.append(svgIcon('close')); CLATI18n.attribute(dismiss, 'aria-label', () => CLATI18n.msg('common.close'));
    dismiss.onclick = () => dialog.close();
    const header = el('header', 'user-reader-header'); header.append(title, dismiss);
    const content = el('pre', 'user-reader-content'); content.tabIndex = 0;
    CLATI18n.attribute(content, 'aria-label', () => text('title'));
    const copy = el('button', 'secondary', () => text('copy')); copy.type = 'button';
    const status = el('span'); status.setAttribute('role', 'status');
    copy.onclick = async () => {
      try { await navigator.clipboard.writeText(getActive()?.source || ''); status.textContent = CLATI18n.t('web.copied'); }
      catch (_) { status.textContent = CLATI18n.t('web.copy_failed'); }
    };
    const footer = el('footer', 'user-reader-footer'); footer.append(copy, status);
    dialog.append(header, content, footer); context.root.append(dialog);
    dialog.addEventListener('click', event => { if (event.target === dialog) dialog.close(); });
    dialog.addEventListener('close', onClose);
    return dialog;
}

const userMessageReader = createUserMessageReader();
