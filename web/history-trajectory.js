/* The history page owns this frozen snapshot independently of the composer. */
function createHistoryTrajectory(parent, reference, { trusted, current, locate }) {
  const host = el('section', 'history-trajectory');
  const toolbar = el('div', 'trajectory-toolbar');
  const find = el('input');
  const mode = el('select');
  const notice = el('p', 'trajectory-notice');
  const workspace = el('div', 'trajectory-workspace');
  const ledger = el('section', 'trajectory-list-pane');
  find.type = 'search';
  find.maxLength = 512;
  notice.setAttribute('role', 'status');
  CLATI18n.attribute(find, 'placeholder', () => trajectoryText('search_session'));
  CLATI18n.attribute(find, 'aria-label', () => trajectoryText('search_session'));
  for (const key of ['sequence', 'duration']) {
    const option = el('option', '', () => trajectoryText('mode_' + key));
    option.value = key;
    mode.append(option);
  }
  CLATI18n.attribute(mode, 'aria-label', () => trajectoryText('timeline_mode'));

  let epoch = 0, detailEpoch = 0, searchTimer, suspended = false;
  let snapshot = null, query = '', selected = null, before = null, after = null;
  let turns = [], overview = [], matches = [], records = new Map();
  let busy = false, needsLoad = false, invalid = false;
  const initialWorkspace = workspacePrefix;
  const valid = () => !suspended && current() && workspacePrefix === initialWorkspace;
  const scope = { reference: { ...reference, seq: null }, trusted_projects: trusted };
  const refresh = navigationIcon(() => trajectoryText('refresh'), 'refresh', refreshSnapshot);
  const collapse = navigationIcon(() => trajectoryText('collapse_all'), 'collapse-all', () => list.collapse());
  const previous = navigationIcon(() => trajectoryText('previous_match'), 'previous', () => void nextMatch(-1));
  const next = navigationIcon(() => trajectoryText('next_match'), 'next', () => void nextMatch(1));
  const count = el('small', 'trajectory-match-count');
  const zoomIn = navigationIcon(() => trajectoryText('zoom_in'), 'plus', () => chart.zoom(1));
  const zoomOut = navigationIcon(() => trajectoryText('zoom_out'), 'minus', () => chart.zoom(-1));
  const zoom = el('div', 'trajectory-zoom');
  zoom.append(zoomIn, zoomOut);
  toolbar.append(mode, collapse, zoom, find, count, previous, next, refresh);
  host.append(toolbar);
  const chart = createTrajectoryOverview(host, id => void choose(id), ({ canZoomIn, canZoomOut }) => {
    zoomIn.disabled = !canZoomIn;
    zoomOut.disabled = !canZoomOut;
  });
  workspace.append(ledger);
  host.append(notice, workspace);
  parent.append(host);

  const list = createTrajectoryList(ledger, {
    choose: id => void choose(id),
    viewportChanged: chart.setViewport,
    loadTurn(turn) {
      if (invalid || busy) return;
      const loaded = [...records.values()].filter(row => trajectoryGroup(row) === turn.id).length;
      if (loaded < turn.records) void load({ anchor: turn.first_seq, preserve: true });
    },
    loadPage(direction) {
      if (!invalid && !busy) void load({ cursor: direction === 'before' ? before : after });
    },
  });
  const inspector = createTrajectoryDetail(workspace, {
    reference, trusted, close: closeDetail,
    locate: row => locate({ ...reference, seq: row.source_seqs.at(-1) ?? row.seq }),
  });

  function closeDetail(focus = true) {
    detailEpoch++;
    inspector.hide();
    host.classList.remove('has-inspector');
    if (focus) list.focus();
  }

  function paint(reveal = false) {
    list.update(turns, records, selected, reveal);
    list.pages(before, after, busy || invalid);
    mode.options[1].disabled = !overview.some(span => trajectoryHasDuration(span));
    if (mode.options[1].disabled) mode.value = 'sequence';
    chart.update(overview, selected, mode.value);
    previous.disabled = next.disabled = invalid || needsLoad || !matches.length;
    const index = matches.indexOf(selected);
    CLATI18n.bind(count, 'textContent', () => query
      ? trajectoryText('match_count', { current: index + 1, total: matches.length })
      : '');
  }

  function reportError(error) {
    if (error.presentation?.key === 'history.source_changed') {
      invalid = true;
      CLATI18n.bind(notice, 'textContent', () => trajectoryText('snapshot_changed'));
    } else {
      CLATI18n.bind(notice, 'textContent', () => error.message);
    }
  }

  async function load(params = {}) {
    const ticket = ++epoch, ownedQuery = query;
    const stillCurrent = () => ticket === epoch && valid() && query === ownedQuery;
    busy = true;
    refresh.disabled = true;
    paint();
    CLATI18n.bind(notice, 'textContent', () => trajectoryText('loading'));
    try {
      const page = await rpc('history.trajectory', {
        ...scope, snapshot: params.refresh ? null : snapshot,
        query, cursor: params.cursor ?? null, anchor_seq: params.anchor ?? null,
      });
      if (!stillCurrent()) return false;
      const selectResult = !params.preserve && !params.cursor;
      if (params.refresh || !snapshot) records = new Map();
      snapshot = page.snapshot;
      turns = page.turns;
      overview = page.overview;
      matches = page.matches;
      before = page.before_cursor;
      after = page.after_cursor;
      invalid = needsLoad = false;
      for (const row of page.records) records.set(row.id, row);
      if (selectResult) {
        selected = page.selected_id ?? null;
        if (selected) {
          paint();
          list.select(selected, true, Boolean(query || (params.anchor != null && !params.refresh)));
          void detail(selected);
        } else {
          closeDetail(false);
        }
      }
      const noticeKey = !turns.length ? 'trajectory.empty'
        : query && !matches.length ? 'trajectory.no_matches'
        : selectResult && params.anchor != null && !selected ? 'history.not_found' : null;
      CLATI18n.bind(notice, 'textContent', () => noticeKey ? CLATI18n.msg(noticeKey) : '');
      return true;
    } catch (error) {
      if (stillCurrent()) reportError(error);
      return false;
    } finally {
      if (stillCurrent()) {
        busy = false;
        refresh.disabled = false;
        paint();
      }
    }
  }

  async function detail(id) {
    const ticket = ++detailEpoch;
    const stillCurrent = () => ticket === detailEpoch && valid() && selected === id;
    inspector.loading();
    host.classList.add('has-inspector');
    try {
      const value = await rpc('history.trajectory.detail', { ...scope, snapshot, id });
      if (stillCurrent()) inspector.show(value);
    } catch (error) {
      if (!stillCurrent()) return;
      reportError(error);
      inspector.error(error);
      paint();
    }
  }

  async function choose(id) {
    if (!valid() || invalid || needsLoad) return;
    selected = id;
    const span = overview.find(record => record.id === id);
    if (!records.has(id) && span) {
      closeDetail(false);
      if (!await load({ anchor: span.seq, preserve: true })) return;
    }
    if (!valid() || selected !== id || !records.has(id)) return;
    paint(true);
    list.select(id, true);
    await detail(id);
  }

  function nextMatch(direction) {
    if (!matches.length) return;
    const index = matches.indexOf(selected);
    const target = index < 0
      ? (direction > 0 ? 0 : matches.length - 1)
      : (index + direction + matches.length) % matches.length;
    void choose(matches[target]);
  }

  function refreshSnapshot() {
    clearTimeout(searchTimer);
    // Search still selects a match if the old selection disappeared.
    const anchor = query || invalid ? null : records.get(selected)?.seq;
    void load({ refresh: true, anchor });
  }

  find.oninput = () => {
    epoch++;
    detailEpoch++;
    busy = false;
    needsLoad = true;
    refresh.disabled = false;
    clearTimeout(searchTimer);
    query = find.value.trim();
    paint();
    if (!invalid) searchTimer = setTimeout(() => void load(), 250);
  };
  find.onkeydown = event => {
    if (event.key === 'Enter') {
      event.preventDefault();
      if (needsLoad && !invalid) {
        clearTimeout(searchTimer);
        void load();
      } else {
        nextMatch(event.shiftKey ? -1 : 1);
      }
    }
    if (event.key === 'Escape' && find.value) {
      find.value = '';
      find.oninput();
    }
  };
  mode.onchange = () => paint();
  host.onkeydown = event => {
    if (event.key === 'Escape' && host.classList.contains('has-inspector') && event.target !== find) {
      event.preventDefault();
      closeDetail();
    }
  };

  void load({ anchor: reference.seq });
  function suspend() {
    suspended = true;
    epoch++;
    detailEpoch++;
    clearTimeout(searchTimer);
    busy = false;
    refresh.disabled = false;
  }
  return {
    suspend,
    resume() {
      suspended = false;
      if (!invalid && (!snapshot || needsLoad)) {
        void load({ anchor: snapshot ? null : reference.seq });
      } else {
        paint();
        if (selected && !invalid && host.classList.contains('has-inspector')) void detail(selected);
      }
    },
    dispose() {
      suspend();
      list.dispose();
      chart.dispose();
      inspector.dispose();
      host.remove();
    },
  };
}
