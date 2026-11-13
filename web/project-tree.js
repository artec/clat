/* Project/session navigation only. Core remains the owner of selection and runs. */
(() => {
  'use strict';
  const text = (key, args) => CLATI18n.msg('workbench.' + key, args);
  const INITIAL_SESSION_COUNT = 5;
  const SESSION_BATCH_SIZE = 6;
  // WKWebView never synthesizes dblclick on project rows; pair clicks manually.
  const DOUBLE_CLICK_MS = 500;
  let lastProjectClick = { key: null, at: 0 };
  function node(tag, className = '', label) {
    const element = document.createElement(tag); element.className = className;
    if (label) CLATI18n.bind(element, 'textContent', label);
    return element;
  }
  function button(className, label, action) {
    const element = node('button', className); element.type = 'button';
    labelButton(element, label);
    element.addEventListener('click', action); return element;
  }
  function labelButton(element, label) {
    CLATI18n.attribute(element, 'aria-label', label);
    CLATI18n.attribute(element, 'title', label);
  }
  function glyph(path) {
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('aria-hidden', 'true');
    const shape = document.createElementNS(svg.namespaceURI, 'path'); shape.setAttribute('d', path);
    svg.append(shape); return svg;
  }
  class ProjectTree {
    constructor(host) {
      this.host = host; this.branches = new Map(); this.contexts = new WeakMap();
      this.rows = []; this.views = new Map();
    }
    key(row) { return row.root; }
    state(row) {
      let value = this.branches.get(this.key(row));
      if (!value || value.route !== row.route_id) {
        value = { route: row.route_id, expanded: Boolean(row.route_id), extraSessions: 0, items: null };
        this.branches.set(this.key(row), value);
      }
      return value;
    }
    seat(context) {
      if (this.contexts.has(context)) return this.contexts.get(context);
      const tree = node('nav', 'project-directory'); tree.id = 'project-directory';
      CLATI18n.attribute(tree, 'aria-label', () => text('projects'));
      const error = node('p', 'project-directory-error'); error.id = 'project-directory-error'; error.setAttribute('role', 'status');
      const panel = context.root.querySelector('#session-panel');
      panel.append(tree, error);
      const value = { tree, error, groups: new Map(), list: panel.querySelector('#session-list'), empty: panel.querySelector('#session-empty') };
      this.contexts.set(context, value); return value;
    }
    failure(error) {
      const target = this.seat(this.host.active).error;
      CLATI18n.bind(target, 'textContent', () => error.message);
    }
    async open(row, session, fresh = false) {
      this.seat(this.host.active).error.replaceChildren();
      try { await this.host.openConversation(row, session, fresh); } catch (error) { this.failure(error); }
    }
    createGroup(row, context) {
      const group = node('section', 'project-group'); group.dataset.projectKey = this.key(row);
      group.dataset.projectOwner = row.route_id || '';
      const header = node('div', 'project-group-head');
      const toggle = button('project-group-toggle', () => text(this.state(row).expanded ? 'collapse_project' : 'expand_project', { name: row.title }), () => {
        this.state(row).expanded = !this.state(row).expanded; this.host.render();
        if (this.state(row).expanded && row.route_id && !this.views.get(row.route_id)) void this.load(row, true);
      });
      toggle.dataset.action = 'toggle-project'; toggle.append(glyph('m9 5 7 7-7 7'));
      const title = button('project-directory-row', () => row.title || row.root, () => {
        void this.host.open(row).catch(error => this.failure(error));
      });
      title.dataset.projectRoute = row.route_id || '';
      title.dataset.action = 'open-project';
      // Double-click mirrors the work-area Project files entry. WKWebView does not
      // synthesize dblclick for this row, so consecutive clicks are paired manually
      // with the shared, render-stable project key. Pairing clicks the same hidden
      // trigger the work-area entry clicks; its disabled state carries the builtin
      // gate. No page globals: this file's scope cannot resolve all of them.
      title.addEventListener('click', () => {
        const key = this.key(row), now = Date.now();
        if (lastProjectClick.key === key && now - lastProjectClick.at <= DOUBLE_CLICK_MS) {
          lastProjectClick = { key: null, at: 0 };
          document.getElementById('files-open')?.click();
          return;
        }
        lastProjectClick = { key, at: now };
      });
      title.append(glyph('M3 7h7l2-3h9v16H3z'), node('span', 'project-directory-name', () => row.title || row.root));
      CLATI18n.attribute(title, 'title', () => row.root);
      const badge = node('small'); title.append(badge);
      const fresh = button('project-new-session', () => text('new_in_project', { name: row.title }), () => {
        this.state(row).expanded = true; this.host.render(); void this.open(row, null, true);
      });
      fresh.dataset.action = 'new-project-session'; fresh.append(glyph('M12 5v14M5 12h14'));
      const body = node('div', 'project-group-body');
      const list = node('ul', 'session-list');
      const status = node('p', 'project-branch-status'); status.setAttribute('role', 'status');
      const showMore = () => text('show_more');
      const more = button('project-sessions-more', showMore, () => {
        this.state(row).extraSessions += SESSION_BATCH_SIZE;
        this.host.render();
      });
      more.dataset.action = 'more-project-sessions'; CLATI18n.bind(more, 'textContent', showMore);
      const retry = button('project-sessions-retry', () => text('retry_project'), () => { void this.load(row, true); });
      CLATI18n.bind(retry, 'textContent', () => text('retry_project'));
      const organization=this.host.organization.menuButton(row);this.host.organization.bind(header,row,organization);
      const unpin = node('button', 'icon-button project-unpin'); unpin.type = 'button';
      unpin.dataset.action = 'unpin-project';
      for (const attr of ['aria-label', 'title']) CLATI18n.attribute(unpin, attr, () => CLATI18n.msg('project_org.unpin'));
      unpin.append(this.host.icon('pin'));
      unpin.hidden = true;
      unpin.addEventListener('click', () => { void this.host.organization.setPinned(row, false).catch(error => this.failure(error)); });
      header.append(toggle, title, unpin, organization, fresh); body.append(list, status, more, retry); group.append(header, body);
      return { group, body, toggle, title, badge, list, status, more, retry, fresh, unpin,
        identity: JSON.stringify([row.route_id, row.root]) };
    }
    renderSession(row, session) {
      const item = node('li'); item.dataset.sessionId = session.id;
      item.dataset.pinned = String(Boolean(session.pinned));
      const label = () => (session.title || CLATI18n.t('web.untitled_session'))
        + (session.archived ? CLATI18n.t('web.archived_suffix') : '');
      const link = button('session-item', label, () => { void this.open(row, session.id); });
      link.dataset.action = 'open-project-session';
      const copy = node('span', 'session-copy'); copy.append(node('strong', '', label));
      link.append(copy); item.append(link); return item;
    }
    renderHeader(group, row, context, snapshot) {
      const value = this.state(row), active = row.route_id === context.route;
      const summary = this.views.get(row.route_id)?.view?.summary();
      group.fresh.disabled = Boolean(this.views.get(row.route_id)?.view?.navigation().locked);
      const label = summary?.pending ? 'project_pending' : (summary ? summary.running : row.run_active || row.compacting) ? 'project_running' : null;
      group.group.dataset.active = String(active);
      group.toggle.setAttribute('aria-expanded', String(value.expanded));
      labelButton(group.toggle, () => text(value.expanded ? 'collapse_project' : 'expand_project', { name: row.title || row.root }));
      group.title.setAttribute('aria-current', String(active));
      group.body.hidden = !value.expanded;
      group.group.classList.toggle('pinned', !!row.pinned);
      group.unpin.hidden = !row.pinned;
      CLATI18n.bind(group.title.querySelector('.project-directory-name'),'textContent',()=>row.title||row.root);
      if (label) group.title.append(group.badge); else group.badge.remove();
      CLATI18n.bind(group.badge, 'textContent', () => label ? text(label) : '');
      if (active && group.list !== snapshot.list) {
        group.list.remove(); group.list = snapshot.list; group.body.prepend(snapshot.list, snapshot.empty);
      }
    }
    renderChildren(group, row, context, navigation) {
      const value = this.state(row), active = row.route_id === context.route;
      const summary = this.views.get(row.route_id)?.view?.navigation();
      const source = summary?.ready ? summary.items : value.items;
      let restoreFocus;
      group.status.hidden = true; group.retry.hidden = true;
      if (active && !group.list.children.length && navigation.ready && !navigation.query) {
        group.status.hidden = false;
        CLATI18n.bind(group.status, 'textContent', () => text('no_project_sessions'));
      }
      if (!active) {
        const query = navigation.query.trim().toLocaleLowerCase();
        const items = (source || []).filter(session => Boolean(session.archived) === Boolean(navigation.archived)
          && (!query || (session.title || '').toLocaleLowerCase().includes(query) || session.id.toLocaleLowerCase().includes(query)));
        const revision = JSON.stringify(items);
        if (group.list.dataset.revision !== revision) {
          const focused = group.list.contains(document.activeElement) ? document.activeElement : null;
          const focusedId = focused?.closest('li')?.dataset.sessionId;
          const focusSelector = focused?.classList.contains('session-pin') ? '.session-pin' : '.session-item';
          group.list.dataset.revision = revision;
          group.list.replaceChildren(...items.map(item => this.renderSession(row, item)));
          if (focusedId) restoreFocus = () => [...group.list.children].find(item => item.dataset.sessionId === focusedId)
            ?.querySelector(focusSelector)?.focus({ preventScroll: true });
        }
        if (!row.route_id || value.error || !source) {
          group.status.hidden = false;
          CLATI18n.bind(group.status, 'textContent', () => value.error?.message || CLATI18n.msg(row.route_id ? 'web.loading' : 'workbench.project_unopened'));
          group.retry.hidden = !value.error;
        } else if (!items.length) {
          group.status.hidden = false;
          CLATI18n.bind(group.status, 'textContent', () => text(navigation.query ? 'no_matching_project_sessions' : 'no_project_sessions'));
        }
        if (value.expanded && row.route_id && !source && !value.error) void this.load(row);
      }
      this.renderSessionIndicators(group, row);
      this.renderSessionLimit(group, value.extraSessions, navigation.query);
      restoreFocus?.();
    }
    renderSessionLimit(group, remaining, query) {
      let hasHiddenSessions = false;
      [...group.list.children].forEach((item, index) => {
        const alwaysVisible = query || index < INITIAL_SESSION_COUNT
          || item.classList.contains('active') || item.dataset.running === 'true';
        item.hidden = false;
        if (alwaysVisible) return;
        if (remaining > 0) {
          remaining -= 1;
        } else {
          item.hidden = true;
          hasHiddenSessions = true;
        }
      });
      group.more.hidden = !hasHiddenSessions;
    }
    createSessionPin(row, item) {
      const pinned = () => item.dataset.pinned === 'true';
      const pin = button('session-pin', () => CLATI18n.msg(pinned() ? 'web.unpin_session' : 'web.pin_session'), async () => {
        if ('pending' in pin.dataset) return;
        pin.dataset.pending = '';
        try { await this.host.setSessionPinned(row, item.dataset.sessionId, !pinned()); }
        catch (error) { this.failure(error); }
        finally { delete pin.dataset.pending; }
      });
      pin.append(this.host.icon('pin'));
      pin.setAttribute('aria-pressed', String(pinned()));
      item.append(pin);
      return pin;
    }
    renderSessionIndicators(group, row) {
      const view = this.views.get(row.route_id)?.view;
      const summary = view?.summary();
      const session = summary ? summary.runningSession : (row.run_active ? row.active_session_id : null);
      const busy = (summary?.running ?? (row.run_active || row.compacting))
        || view?.navigation().locked || this.state(row).loading;
      for (const item of group.list.children) {
        const running = Boolean(session && item.dataset.sessionId === session);
        item.dataset.running = String(running);
        const pin = item.querySelector('.session-pin') || this.createSessionPin(row, item);
        pin.disabled = Boolean(busy || running);
        let indicator=item.querySelector('.session-running');
        if(running&&!indicator){
          indicator=node('span','session-running');indicator.setAttribute('role','img');
          CLATI18n.attribute(indicator,'aria-label',()=>CLATI18n.msg('web.running_short'));
          CLATI18n.attribute(indicator,'title',()=>CLATI18n.msg('web.running_short'));item.append(indicator);
        }
        if(indicator)indicator.hidden=!running;
      }
    }
    async load(row, retry = false) {
      const value = this.state(row);
      if (value.loading || (!retry && value.items)) return;
      value.loading = true; value.error = null; this.host.render();
      try {
        const result = await this.host.readSessions(row);
        if (this.branches.get(this.key(row)) === value) value.items = result.sessions || [];
      } catch (error) { if (this.branches.get(this.key(row)) === value) value.error = error; }
      finally { value.loading = false; this.host.render(); }
    }
    render(context, directory, projectViews) {
      this.rows = directory; this.views = projectViews;
      if (!context.view) return;
      const snapshot = this.seat(context), navigation = context.view.navigation();
      const current = this.views.get(context.route)?.view?.summary();
      const entries = this.rows.some(row => row.route_id === context.route) ? this.rows : [...this.rows, {
        route_id: context.route, root: current.root, title: context.root.querySelector('#project-name').textContent,
      }];
      for (const [index, row] of entries.entries()) {
        let group = snapshot.groups.get(this.key(row));
        if (!group || group.identity !== JSON.stringify([row.route_id, row.root])) {
          group?.group.remove(); group = this.createGroup(row, context); snapshot.groups.set(this.key(row), group);
        }
        if (snapshot.tree.children[index] !== group.group) snapshot.tree.insertBefore(group.group, snapshot.tree.children[index] || null);
        this.renderHeader(group, row, context, snapshot); this.renderChildren(group, row, context, navigation);
      }
      const present = new Set(entries.map(row => this.key(row)));
      for (const [id, group] of snapshot.groups) if (!present.has(id)) { group.group.remove(); snapshot.groups.delete(id); }
    }
  }
  globalThis.CLATProjectTree = Object.freeze({ create: host => new ProjectTree(host) });
})();
