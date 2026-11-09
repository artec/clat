/* Pixel buckets bound drawing cost while each segment retains a record ID. */
function trajectoryLane(kind) {
  if (kind === 'tool' || kind === 'approval') return 2;
  return ['reasoning', 'message', 'request', 'retry', 'terminal'].includes(kind) ? 1 : 0;
}

function trajectoryHasDuration(row) {
  return Number.isFinite(row.started_at) && Number.isFinite(row.ended_at) && row.ended_at >= row.started_at;
}

function trajectoryDuration(row) {
  if (!trajectoryHasDuration(row)) return trajectoryText('unknown_time');
  const ms = row.ended_at - row.started_at;
  const duration = ms < 1000 ? trajectoryText('duration_millis', { ms }) : roundDuration(ms);
  return trajectoryText(row.estimated ? 'estimated_duration' : 'duration', { duration });
}

function trajectoryAxis(spans, mode) {
  const records = new Map(), turns = new Map();
  const full = { start: Infinity, end: -Infinity };
  spans.forEach((span, index) => {
    const start = mode === 'sequence' ? index : span.started_at ?? span.time;
    const end = mode === 'sequence' ? index + 1 : trajectoryHasDuration(span) ? span.ended_at : start;
    const record = { span, start, end };
    records.set(span.id, record);
    if (span.turn != null) {
      if (!turns.has(span.turn)) turns.set(span.turn, []);
      turns.get(span.turn).push(record);
    }
    if (Number.isFinite(start)) {
      full.start = Math.min(full.start, start);
      full.end = Math.max(full.end, end);
    }
  });
  return { records, turns, full };
}

function trajectoryViewport(axis, rows) {
  const visible = new Set();
  let start = Infinity, end = -Infinity;
  for (const row of rows) {
    const turn = axis.turns.get(row.turn);
    const records = turn ? (row.wholeTurn ? turn : turn.slice(0, 1)) : [axis.records.get(row.id)];
    for (const record of records) {
      if (!record) continue;
      visible.add(record.span.id);
      if (!Number.isFinite(record.start)) continue;
      start = Math.min(start, record.start);
      end = Math.max(end, record.end);
    }
  }
  return { visible, start, end };
}

function trajectoryWindow(full, viewport, zoomLevel) {
  const fullWidth = full.end - full.start;
  const minimum = Math.max(1, viewport.end - viewport.start);
  if (!Number.isFinite(viewport.start) || !Number.isFinite(fullWidth) || fullWidth <= minimum) {
    return { ...full, maxZoom: 0 };
  }
  const maxZoom = Math.ceil(Math.log2(fullWidth / minimum));
  const width = Math.max(minimum, fullWidth / 2 ** zoomLevel);
  const center = viewport.start + (viewport.end - viewport.start) / 2;
  const start = Math.max(full.start, Math.min(full.end - width, center - width / 2));
  return { start, end: start + width, maxZoom };
}

function trajectoryBuckets(axis, width, range, selected, visible, zoomed) {
  const dated = [], undated = [];
  for (const record of axis.records.values()) {
    if (Number.isFinite(record.start)) {
      const point = record.start === record.end;
      const intersects = point
        ? record.start >= range.start && record.start <= range.end
        : record.end > range.start && record.start < range.end;
      if (intersects) dated.push(record);
    } else if (!zoomed || visible.has(record.span.id)) {
      undated.push(record);
    }
  }

  // Untimed records get their own strip rather than a fabricated position on the clock.
  const strip = undated.length ? Math.min(80, Math.max(1, Math.floor(width / 6))) : 0;
  const gap = strip ? Math.min(6, Math.floor(width / 6)) : 0;
  const track = Math.max(0, width - strip - gap);
  const map = new Map();
  function add(record, left, right, limit) {
    if (limit <= 0) return;
    const x = Math.min(limit - 1, Math.floor(left));
    const lane = trajectoryLane(record.span.kind);
    const key = lane + ':' + x;
    const rank = record.span.id === selected ? 2 : Number(visible.has(record.span.id));
    if (!map.has(key) || map.get(key).rank < rank) {
      map.set(key, { x, lane, span: record.span, rank, width: Math.min(limit - x, Math.max(2, right - left)) });
    }
  }
  const length = Math.max(1, range.end - range.start);
  for (const record of dated) {
    const left = Math.max(0, (record.start - range.start) / length) * track;
    const right = Math.min(1, (record.end - range.start) / length) * track;
    add(record, left, right, track);
  }
  undated.forEach((record, index) => {
    const offset = track + gap;
    add(record, offset + strip * index / undated.length, offset + strip * (index + 1) / undated.length, width);
  });
  return { buckets: [...map.values()], navigable: [...dated, ...undated].map(record => record.span), track, strip };
}

function createTrajectoryOverview(parent, choose, zoomChanged) {
  const host = el('section', 'trajectory-overview');
  const labels = el('div', 'trajectory-lanes');
  const canvas = document.createElement('canvas');
  const hint = el('div', 'trajectory-hover');
  const unknown = el('small', 'trajectory-unknown-time');
  for (const key of ['lane_input', 'lane_model', 'lane_tool']) {
    labels.append(el('span', '', () => trajectoryText(key)));
  }
  canvas.tabIndex = 0;
  CLATI18n.attribute(canvas, 'aria-label', () => trajectoryText('overview_navigation'));
  hint.hidden = unknown.hidden = true;
  host.append(labels, canvas, hint, unknown);
  parent.append(host);
  let spans = [], selected = null, mode = 'sequence', buckets = [], frame;
  let axis = trajectoryAxis(spans, mode), viewport = [], zoomLevel = 0, focusId = null;
  let navigable = [];

  function viewWindow() {
    const coverage = trajectoryViewport(axis, viewport);
    return { coverage, range: trajectoryWindow(axis.full, coverage, zoomLevel) };
  }

  function zoom(direction) {
    const { range } = viewWindow();
    zoomLevel = direction > 0
      ? Math.min(range.maxZoom, zoomLevel + 1)
      : Math.max(0, Math.min(zoomLevel, range.maxZoom || 1) - 1);
    showHint();
    schedule();
  }

  function draw() {
    const width = Math.max(1, Math.floor(canvas.getBoundingClientRect().width));
    const ratio = window.devicePixelRatio || 1;
    canvas.width = width * ratio;
    canvas.height = 78 * ratio;
    const ctx = canvas.getContext('2d');
    ctx.scale(ratio, ratio);
    const style = getComputedStyle(host);
    const color = name => style.getPropertyValue(name).trim();
    const colors = [color('--accent'), color('--text-muted'), color('--warning')];
    const { coverage, range } = viewWindow();
    const layout = trajectoryBuckets(axis, width, range, selected, coverage.visible, zoomLevel > 0 && coverage.visible.size > 0);
    buckets = layout.buckets;
    navigable = layout.navigable;
    zoomChanged({ canZoomIn: zoomLevel < range.maxZoom, canZoomOut: zoomLevel > 0 });
    ctx.fillStyle = color('--line');
    for (let lane = 0; lane < 3; lane++) ctx.fillRect(0, lane * 24 + 20, width, 1);
    if (layout.strip) ctx.fillRect(layout.track + 2, 0, 1, 72);
    for (const bucket of buckets) {
      const active = bucket.span.id === selected;
      const y = bucket.lane * 24 + 5;
      ctx.fillStyle = colors[bucket.lane];
      ctx.globalAlpha = active ? 1 : .65;
      ctx.fillRect(bucket.x, y, bucket.width, 9);
      if (active) {
        ctx.strokeStyle = colors[0];
        ctx.strokeRect(bucket.x, y - 2, bucket.width, 13);
      }
    }
  }

  function schedule() {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(draw);
  }

  function pick(event) {
    const rect = canvas.getBoundingClientRect();
    const x = event.clientX - rect.left;
    const lane = Math.floor((event.clientY - rect.top) / 24);
    const hit = buckets.filter(bucket => bucket.lane === lane && x >= bucket.x - 2 && x <= bucket.x + bucket.width + 2);
    return hit.reduce((nearest, bucket) =>
      !nearest || Math.abs(bucket.x - x) < Math.abs(nearest.x - x) ? bucket : nearest, null);
  }

  function showHint(span) {
    hint.hidden = !span;
    if (span) hint.textContent = trajectoryText(span.kind) + ' · ' + trajectoryDuration(span);
  }

  canvas.onclick = event => {
    const bucket = pick(event);
    if (bucket) choose(bucket.span.id);
  };
  canvas.onpointermove = event => showHint(pick(event)?.span);
  canvas.onpointerleave = canvas.onblur = () => showHint();
  canvas.onkeydown = event => {
    if (!navigable.length || !['ArrowLeft', 'ArrowRight', 'Home', 'End', 'Enter', ' '].includes(event.key)) return;
    event.preventDefault();
    const current = navigable.findIndex(span => span.id === (focusId ?? selected));
    if (event.key === 'Enter' || event.key === ' ') {
      choose(navigable[Math.max(0, current)].id);
      return;
    }
    let index = 0;
    if (event.key === 'End') index = navigable.length - 1;
    else if (event.key === 'ArrowRight') index = (current + 1) % navigable.length;
    else if (event.key === 'ArrowLeft') index = (Math.max(0, current) + navigable.length - 1) % navigable.length;
    const span = navigable[index];
    focusId = span.id;
    showHint(span);
  };

  const observer = new ResizeObserver(schedule);
  observer.observe(host);
  const theme = new MutationObserver(schedule);
  theme.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'class', 'style'] });
  const unbind = CLATI18n.onChange(() => updateUnknown());
  function updateUnknown() {
    const missing = spans.filter(span => !trajectoryHasDuration(span)).length;
    unknown.hidden = mode !== 'duration' || !missing;
    unknown.textContent = missing ? trajectoryText('unknown_timing_count', { count: missing }) : '';
  }
  return {
    update(value, id, nextMode) {
      if (spans !== value || mode !== nextMode) axis = trajectoryAxis(value, nextMode);
      if (selected !== id) focusId = null;
      spans = value;
      selected = id;
      mode = nextMode;
      updateUnknown();
      schedule();
    },
    zoom,
    setViewport(rows) {
      viewport = rows;
      showHint();
      schedule();
    },
    dispose() {
      observer.disconnect();
      theme.disconnect();
      cancelAnimationFrame(frame);
      unbind();
    },
  };
}
