/* Shared in-app confirm/prompt dialogs.
   Native window.confirm/prompt are silent no-ops in webviews without JS
   panel delegates (the desktop wry WKWebView implements none), so product
   flows must not depend on them. One dialog at a time: a new call closes
   the previous one as cancelled. */
(() => {
  'use strict';
  let active = null;

  function seat(kind) {
    if (active) active.close();
    const dialog = document.createElement('dialog');
    dialog.className = 'workbench-dialog clat-dialog';
    dialog.dataset.kind = kind;
    const actions = document.createElement('div');
    actions.className = 'workbench-dialog-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'secondary';
    cancel.dataset.role = 'cancel';
    CLATI18n.bind(cancel, 'textContent', () => CLATI18n.msg('common.cancel'));
    const accept = document.createElement('button');
    accept.type = 'button';
    accept.className = 'primary';
    accept.dataset.role = 'accept';
    actions.append(cancel, accept);
    dialog.onkeydown = (event) => { event.stopPropagation(); };
    document.body.append(dialog);
    active = dialog;
    return { dialog, actions, cancel, accept };
  }

  function confirm(message) {
    return new Promise((resolve) => {
      const { dialog, actions, cancel, accept } = seat('confirm');
      let settled = false;
      const done = (value) => { if (!settled) { settled = true; resolve(value); } };
      const close = () => dialog.close();
      const events = new AbortController();
      CLATI18n.bind(accept, 'textContent', () => CLATI18n.msg('common.confirm'));
      const body = document.createElement('p');
      body.className = 'clat-dialog-message';
      body.textContent = String(message ?? '');
      dialog.append(body, actions);
      accept.addEventListener('click', () => { done(true); dialog.close(); });
      cancel.addEventListener('click', close);
      dialog.onclose = () => {
        events.abort();
        if (active === dialog) active = null;
        dialog.remove();
        done(false);
      };
      window.addEventListener('popstate', close, { once: true, signal: events.signal });
      window.addEventListener('clat:project-activated', close, { once: true, signal: events.signal });
      dialog.showModal();
      accept.focus();
    });
  }

  function prompt(message, value = '') {
    return new Promise((resolve) => {
      const { dialog, actions, cancel, accept } = seat('prompt');
      let settled = false;
      const done = (result) => { if (!settled) { settled = true; resolve(result); } };
      const close = () => dialog.close();
      const events = new AbortController();
      CLATI18n.bind(accept, 'textContent', () => CLATI18n.msg('common.apply'));
      const form = document.createElement('form');
      const head = document.createElement('header');
      head.className = 'workbench-dialog-header';
      const title = document.createElement('h2');
      title.textContent = String(message ?? '');
      head.append(title);
      const label = document.createElement('label');
      const input = document.createElement('input');
      input.type = 'text';
      input.autocomplete = 'off';
      input.value = value ?? '';
      CLATI18n.attribute(input, 'aria-label', () => String(message ?? ''));
      label.append(input);
      form.append(head, label, actions);
      dialog.append(form);
      accept.addEventListener('click', () => { done(input.value); dialog.close(); });
      cancel.addEventListener('click', close);
      form.addEventListener('submit', (event) => { event.preventDefault(); accept.click(); });
      dialog.onclose = () => {
        events.abort();
        if (active === dialog) active = null;
        dialog.remove();
        done(null);
      };
      window.addEventListener('popstate', close, { once: true, signal: events.signal });
      window.addEventListener('clat:project-activated', close, { once: true, signal: events.signal });
      dialog.showModal();
      input.focus();
      input.select();
    });
  }

  globalThis.CLATDialog = Object.freeze({ confirm, prompt });
})();
