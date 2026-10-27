/* One application document, lazy project views, and shared authenticated directory. */
function installTransientScrollbars() {
  const timers = new Map();
  let pressed = null, movement = null;
  const settle = node => {
    clearTimeout(timers.get(node));
    timers.delete(node);
    if (pressed === node) return;
    timers.set(node, setTimeout(() => {
      node.removeAttribute('data-scrollbar-active'); timers.delete(node);
    }, 1200));
  };
  const reveal = node => {
    if (!(node instanceof Element)) return;
    node.setAttribute('data-scrollbar-active', ''); settle(node);
  };
  const scrollOwner = target => {
    for (let node = target instanceof Element ? target : target?.parentElement; node; node = node.parentElement) {
      const vertical = node.scrollHeight > node.clientHeight + 1;
      const horizontal = node.scrollWidth > node.clientWidth + 1;
      if (!vertical && !horizontal) continue;
      const style = getComputedStyle(node);
      if ((vertical && /auto|scroll|overlay/.test(style.overflowY))
          || (horizontal && /auto|scroll|overlay/.test(style.overflowX))) return node;
    }
    return null;
  };
  const release = () => {
    const node = pressed; pressed = null;
    if (node) settle(node);
  };
  document.documentElement.classList.add('auto-hide-scrollbars');
  document.addEventListener('scroll', event => reveal(event.target), { capture: true, passive: true });
  document.addEventListener('wheel', event => reveal(scrollOwner(event.target)), { passive: true });
  document.addEventListener('pointermove', event => {
    if (movement !== null) return;
    const { target, clientX, clientY } = event;
    movement = requestAnimationFrame(() => {
      movement = null;
      const node = scrollOwner(target);
      if (!node) return;
      const bounds = node.getBoundingClientRect();
      if (clientX >= bounds.right - 14 || clientY >= bounds.bottom - 14) reveal(node);
    });
  }, { passive: true });
  document.addEventListener('pointerdown', event => {
    release(); pressed = scrollOwner(event.target); reveal(pressed);
  }, { passive: true });
  window.addEventListener('pointerup', release, { passive: true });
  window.addEventListener('mouseup', release, { passive: true });
  document.addEventListener('pointercancel', release, { passive: true });
  window.addEventListener('blur', release);
  document.addEventListener('focusin', event => reveal(scrollOwner(event.target)));
}

(() => {
  'use strict';
  installTransientScrollbars();
  const views = new Map();
  const viewport = document.getElementById('project-view');
  const template = document.getElementById('project-template');
  let directory = [], refreshPending = null, navigation = 0, nativeLocked = false;
  const appHistory = createApplicationHistory();
  const host = { active: null, open, openConversation, readSessions, refresh, render, changed, history: appHistory,
    navigatePage: route => appHistory.push('#/' + route), navigationToken: () => navigation };
  const validRoute = route => /^(default|[a-f0-9-]{36})$/.test(route);
  const currentRoute = () => new URLSearchParams(location.search).get('workspace') ?? 'default';
  const message = key => CLATI18n.msg('workbench.' + key);
  const blocked = () => nativeLocked || CLATWorkbenchState.snapshot().composing;
  const tree = CLATProjectTree.create(host);

  function createApplicationHistory() {
    const id = globalThis.crypto?.randomUUID?.() || Date.now() + '-' + Math.random(), listeners = new Set();
    let index = 0, length = 1;
    const publish = () => { for (const listener of listeners) listener(); };
    const stamp = () => ({ ...history.state, clatNavigation: { id, index } });
    history.replaceState(stamp(), '', location.href);
    window.addEventListener('popstate', () => {
      const seat = history.state?.clatNavigation;
      if (seat?.id === id) index = seat.index;
      else { index = 0; length = 1; history.replaceState(stamp(), '', location.href); }
      publish();
    });
    return {
      canGo: delta => index + delta >= 0 && index + delta < length,
      go(delta) { if (!blocked() && this.canGo(delta)) history.go(delta); },
      push(url) {
        if (new URL(url, location.href).href === location.href) return;
        index += 1; length = index + 1;
        history.pushState(stamp(), '', url); publish();
      },
      subscribe: listener => listeners.add(listener),
    };
  }

  function projectUrl(route, hash) {
    const target = new URL(location.href);
    if (route === 'default') target.searchParams.delete('workspace');
    else target.searchParams.set('workspace', route);
    target.hash = hash;
    return target.pathname + target.search + target.hash;
  }

  function retainActiveRoute() {
    const active = host.active;
    history.replaceState(history.state, '', projectUrl(active.route,
      '#/' + (active.root.querySelector('#app').dataset.page || 'workbench')));
  }

  async function hostRpc(method, params = {}, prefix = '') {
    const response = await fetch(prefix + '/api/' + method, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(params),
    });
    const reply = await response.json().catch(() => null);
    if (!reply || typeof reply.ok !== 'boolean') throw CLATI18n.error(CLATI18n.msg('web.malformed_rpc_reply'));
    if (!reply.ok) throw CLATI18n.error(CLATI18n.view(() => CLATI18n.present(reply.error?.presentation,
      reply.error?.message || CLATI18n.msg('web.request_failed'))));
    return reply.value;
  }

  function create(route) {
    const root = document.createElement('div'); root.className = 'project-context';
    root.append(template.content.cloneNode(true));
    const context = CLATProjectContext.create(root, route, host);
    views.set(route, context);
    host.active = context;
    viewport.replaceChildren(root);
    CLATI18n.hydrate(root);
    context.view = CLATProjectWorkbench(context);
    return context;
  }

  function activate(route, hash, push) {
    const previous = host.active;
    if (previous?.route === route) return;
    if (previous) {
      previous.hash = '#/' + (previous.root.querySelector('#app').dataset.page || 'workbench');
      previous.view.deactivate();
    }
    let next = views.get(route);
    if (push) appHistory.push(projectUrl(route, hash || next?.hash || '#/workbench'));
    if (!next) next = create(route);
    else { host.active = next; viewport.replaceChildren(next.root); }
    next.view.activate();
    exposeTestPorts(next);
    window.dispatchEvent(new Event('clat:project-activated'));
    render();
  }

  // These internals exist only in Rust's test build; product ports stay scoped to the view.
  function exposeTestPorts(context) {
    for (const key of Object.keys(context.view.test || {})) {
      if (Object.getOwnPropertyDescriptor(window, key)?.configurable === false) continue;
      Object.defineProperty(window, key, { configurable: true,
        get: () => host.active.view.test[key],
        set: value => { host.active.view.test[key] = value; },
      });
    }
  }

  async function open(row, { push = true, hash = null, expectedNavigation } = {}) {
    if (expectedNavigation !== undefined && expectedNavigation !== navigation) return;
    if (blocked()) return;
    const request = ++navigation;
    let route = row.route_id;
    if (!route) route = (await hostRpc('workspace.open', { root: row.root, trust: false })).id;
    if (!validRoute(route)) throw CLATI18n.error(CLATI18n.msg('web.request_failed'));
    if (request !== navigation || nativeLocked) return;
    activate(route, hash, push);
    const selected = host.active;
    await refresh();
    if (request === navigation && !nativeLocked) return selected;
  }

  function readSessions(row) {
    if (!validRoute(row.route_id)) return Promise.reject(CLATI18n.error(message('project_unopened')));
    return hostRpc('session.list', {}, '/workspace/' + row.route_id);
  }

  async function openConversation(row, session, fresh) {
    let target;
    if (row.route_id === host.active?.route) {
      if (blocked()) return;
      navigation++; target = host.active;
    } else target = await open(row);
    if (!target) return;
    const request = navigation;
    await target.view.navigateConversation({ session, fresh },
      () => target === host.active && request === navigation && !blocked());
  }

  async function refresh() {
    if (refreshPending) return refreshPending;
    refreshPending = (async () => {
      directory = (await hostRpc('workspace.directory')).projects;
      render();
    })();
    try { await refreshPending; } finally { refreshPending = null; }
  }

  function changed() {
    render();
    if (!directory.length) void refresh().catch(() => {});
  }

  function render() {
    if (host.active) tree.render(host.active, directory, views);
  }

  window.addEventListener('popstate', () => {
    const route = currentRoute();
    if (route === host.active?.route) return;
    if (blocked()) { retainActiveRoute(); return; }
    void open({ route_id: route }, { push: false, hash: location.hash }).catch(error => {
      retainActiveRoute();
      CLATI18n.bind(host.active.root.querySelector('#project-directory-error'), 'textContent', () => error.message);
    });
  });
  window.addEventListener('clat-desktop-install-lock', event => {
    if (!window.__TAURI__) return;
    queueMicrotask(() => { nativeLocked = !event.defaultPrevented; });
  });
  window.addEventListener('clat-desktop-install-unlock', () => { nativeLocked = false; });
  const first = create(validRoute(currentRoute()) ? currentRoute() : 'invalid');
  exposeTestPorts(first);
  CLATI18n.onChange(render);
})();
