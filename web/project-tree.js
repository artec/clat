/* Project/session navigation only. Core remains the owner of selection and runs. */
(() => {
  'use strict';
  const text = (key, args) => CLATI18n.msg('workbench.' + key, args);
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
    key(row) { return row.project_id || row.root; }
    state(row) {
      let value = this.branches.get(this.key(row));
      if (!value || value.route !== row.route_id) {
        value = { route: row.route_id, expanded: Boolean(row.route_id), all: false, items: null };
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
      const more = button('project-sessions-more', () => text(this.state(row).all ? 'show_less' : 'show_more'), () => {
        this.state(row).all = !this.state(row).all; this.host.render();
      });
      more.dataset.action = 'more-project-sessions'; CLATI18n.bind(more, 'textContent', () => text(this.state(row).all ? 'show_less' : 'show_more'));
      const retry = button('project-sessions-retry', () => text('retry_project'), () => { void this.load(row, true); });
      CLATI18n.bind(retry, 'textContent', () => text('retry_project'));
      header.append(toggle, title, fresh); body.append(list, status, more, retry); group.append(header, body);
      return { group, body, toggle, title, badge, list, status, more, retry, fresh,
        identity: JSON.stringify([row.route_id, row.root, row.title]) };
    }
    renderSession(row, session) {
      const item = node('li'); item.dataset.sessionId = session.id;
      const label = () => (session.pinned ? '★ ' : '') + (session.title || CLATI18n.t('web.untitled_session'))
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
          const focused = group.list.contains(document.activeElement)
            ? document.activeElement.closest('li')?.dataset.sessionId : null;
          group.list.dataset.revision = revision;
          group.list.replaceChildren(...items.map(item => this.renderSession(row, item)));
          if (focused) [...group.list.children].find(item => item.dataset.sessionId === focused)
            ?.querySelector('button')?.focus({ preventScroll: true });
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
      const children = [...group.list.children];
      children.forEach((item, index) => { item.hidden = !value.all && !navigation.query && index >= 5 && !item.classList.contains('active'); });
      group.more.hidden = children.length <= 5 || Boolean(navigation.query);
      group.more.setAttribute('aria-expanded', String(value.all));
      labelButton(group.more, () => text(value.all ? 'show_less' : 'show_more'));
      CLATI18n.bind(group.more, 'textContent', () => text(value.all ? 'show_less' : 'show_more'));
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
        if (!group || group.identity !== JSON.stringify([row.route_id, row.root, row.title])) {
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
