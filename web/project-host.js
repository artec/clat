/* One application document, lazy project views, and shared authenticated directory. */
function configureScrollbarRendering() {
  const root = document.documentElement;
  const webview = window.__CLAT_WEBVIEW__ || 'browser';
  const custom = webview === 'wkwebview' || webview === 'webkitgtk'
    || (webview === 'webview2' ? !CSS.supports('scrollbar-color', 'gray transparent')
      : CSS.supports('selector(::-webkit-scrollbar)'));
  root.dataset.webview = webview;
  root.dataset.scrollbars = custom ? 'custom' : 'standard';
  root.classList.add('auto-hide-scrollbars');
}

function scrollbarOwner(target) {
  if (target === document) return document.scrollingElement;
  for (let node = target instanceof Element ? target : target?.parentElement; node; node = node.parentElement) {
    const style = getComputedStyle(node);
    if ((node.scrollHeight > node.clientHeight + 1 && /auto|scroll|overlay/.test(style.overflowY))
        || (node.scrollWidth > node.clientWidth + 1 && /auto|scroll|overlay/.test(style.overflowX))) return node;
  }
  return null;
}

function nearScrollbarEdge(node, x, y) {
  const bounds = node.getBoundingClientRect();
  if (x < bounds.left || x > bounds.right || y < bounds.top || y > bounds.bottom) return false;
  return (node.scrollHeight > node.clientHeight + 1 && x >= bounds.right - 14)
    || (node.scrollWidth > node.clientWidth + 1 && y >= bounds.bottom - 14);
}

function installTransientScrollbars() {
  configureScrollbarRendering();
  const timers = new Map();
  let movement = null;
  const hide = node => {
    clearTimeout(timers.get(node)); timers.delete(node);
    node.removeAttribute('data-scrollbar-active');
    node.style.removeProperty('--clat-scrollbar-thumb');
  };
  const reveal = node => {
    if (!(node instanceof Element)) return;
    if (!node.hasAttribute('data-scrollbar-active')) {
      // Change the scroll owner's inherited paint value directly; every native
      // renderer sees the same state, without relying on pseudo-selector invalidation.
      node.style.setProperty('--clat-scrollbar-thumb', 'var(--line-strong)');
      node.setAttribute('data-scrollbar-active', '');
    }
    clearTimeout(timers.get(node));
    timers.set(node, setTimeout(() => hide(node), 1200));
  };
  const revealOwner = event => reveal(scrollbarOwner(event.target));
  const captured = { capture: true, passive: true };
  document.addEventListener('scroll', revealOwner, captured);
  document.addEventListener('wheel', revealOwner, captured);
  document.addEventListener('pointerover', event => {
    const node = scrollbarOwner(event.target);
    if (node && (!(event.relatedTarget instanceof Node) || !node.contains(event.relatedTarget))) reveal(node);
  }, captured);
  document.addEventListener('pointermove', event => {
    if (movement !== null) return;
    const { target, clientX, clientY } = event;
    movement = requestAnimationFrame(() => {
      movement = null;
      const node = scrollbarOwner(target);
      if (node && nearScrollbarEdge(node, clientX, clientY)) reveal(node);
    });
  }, captured);
  document.addEventListener('pointerdown', revealOwner, captured);
  document.addEventListener('focusin', revealOwner, captured);
  window.addEventListener('blur', () => { for (const node of timers.keys()) hide(node); });
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) for (const node of timers.keys()) hide(node);
  });
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
