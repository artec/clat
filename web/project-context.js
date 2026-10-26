/* Project-local DOM and event ports. RPCs and async closures never change owner. */
(() => {
  'use strict';
  function createProjectContext(root, route, host) {
    const localEvents = new EventTarget();
    const context = { root, route, isActive: () => host.active === context };
    const documentPort = {
      body: root,
      documentElement: document.documentElement,
      getElementById: id => root.querySelector('#' + CSS.escape(id)),
      querySelector: selector => root.querySelector(selector),
      querySelectorAll: selector => root.querySelectorAll(selector),
      createElement: document.createElement.bind(document),
      createElementNS: document.createElementNS.bind(document),
      createTextNode: document.createTextNode.bind(document),
      createDocumentFragment: document.createDocumentFragment.bind(document),
      hasFocus: () => context.isActive() && document.hasFocus(),
      get activeElement() { return context.isActive() ? document.activeElement : null; },
      addEventListener(type, handler, options) {
        document.addEventListener(type, event => { if (context.isActive()) handler(event); }, options);
      },
    };
    const windowPort = {
      get innerWidth() { return window.innerWidth; },
      get __TAURI__() { return window.__TAURI__; },
      getComputedStyle: window.getComputedStyle.bind(window),
      getSelection: () => context.isActive() ? window.getSelection() : null,
      setTimeout: window.setTimeout.bind(window),
      confirm: message => context.isActive() && window.confirm(message),
      prompt: (...args) => context.isActive() ? window.prompt(...args) : null,
      focus: () => { void host.open({ route_id: route }).catch(() => {}); window.focus(); },
      addEventListener(type, handler, options) {
        if (type.startsWith('clat:')) localEvents.addEventListener(type, handler, options);
        else window.addEventListener(type, event => {
          if (type === 'popstate' && (new URLSearchParams(location.search).get('workspace') ?? 'default') !== route) return;
          if (type.startsWith('clat-desktop-install-') || context.isActive()) handler(event);
        }, options);
      },
      dispatchEvent(event) {
        if (!event.type.startsWith('clat:')) return window.dispatchEvent(event);
        const result = localEvents.dispatchEvent(event);
        if (event.type === 'clat:workbench-updated') host.changed();
        return result;
      },
    };
    Object.assign(context, {
      document: documentPort, window: windowPort,
      navigationToken: () => host.navigationToken(),
      history: host.history,
      navigatePage: route => host.navigatePage(route),
      openProject: (row, options) => host.open(row, options),
      refreshDirectory: () => host.refresh(),
      renderDirectory: () => host.render(),
      changed: () => host.changed(),
    });
    return context;
  }
  globalThis.CLATProjectContext = Object.freeze({ create: createProjectContext });
})();
