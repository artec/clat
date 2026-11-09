/* Full content is requested only for the selected record. */
function trajectoryJSON(value) {
  const isContainer = value => value !== null && typeof value === 'object';
  const scalar = value => typeof value === 'string' ? value : JSON.stringify(value);
  if (!isContainer(value)) return el('pre', 'trajectory-json-value', scalar(value));

  function branch(key, value) {
    if (!isContainer(value)) {
      const row = el('div', 'trajectory-json-leaf');
      row.append(el('strong', '', key), el('span', '', scalar(value)));
      return row;
    }
    const node = el('details');
    const body = el('div');
    node.append(el('summary', '', key), body);
    node.addEventListener('toggle', () => {
      if (node.open && !body.childNodes.length) appendChildren(body, value);
    });
    return node;
  }

  function appendChildren(parent, value) {
    for (const [key, child] of Object.entries(value)) parent.append(branch(key, child));
  }
  const root = el('div', 'trajectory-json-tree');
  appendChildren(root, value);
  return root;
}

function createTrajectoryDetail(parent, { close, locate, reference, trusted }) {
  const pane = el('aside', 'trajectory-inspector');
  const head = el('header');
  const title = el('strong');
  const icon = el('span', 'trajectory-detail-icon');
  const body = el('section', 'trajectory-detail-body');
  const footer = el('footer');
  const jump = el('button', '', () => trajectoryText('locate'));
  const currentProject = () => reference.project === state.workbench?.project?.root;
  let value = null, tab = 'overview', loading = false;
  const tabs = navigationTabs(
    ['overview', 'preview', 'raw', 'source'].map(key => [key, () => trajectoryText('tab_' + key)]),
    key => { tab = key; paint(); },
  );

  pane.tabIndex = -1;
  pane.hidden = true;
  CLATI18n.attribute(pane, 'aria-label', () => trajectoryText('record_detail'));
  head.append(icon, title, navigationIcon(() => trajectoryText('close_details'), 'close', close));
  jump.type = 'button';
  jump.disabled = jump.hidden = true;
  jump.onclick = () => { if (value) locate(value.record); };
  footer.append(jump);
  pane.append(head, tabs.element, body, footer);
  parent.append(pane);

  function heading(key) {
    body.append(el('h3', '', () => trajectoryText(key)));
  }

  function field(key, text) {
    if (text == null || text === '') return;
    const row = el('div', 'trajectory-fact');
    row.append(el('span', '', () => trajectoryText(key)), el('span', '', text));
    body.append(row);
  }

  function markdown(label, text) {
    if (!text) return;
    heading(label);
    const node = el('div', 'rich-text');
    renderMarkdown(node, text);
    // The shared Markdown file actions resolve against the mounted project.
    if (!currentProject()) {
      for (const link of node.querySelectorAll('.markdown-file-link')) {
        link.replaceWith(el('span', '', link.textContent));
      }
    }
    body.append(node);
  }

  function overview() {
    const row = value.record;
    if (row.turn != null) field('turn', CLATI18n.msg('view.turn_number', { turn: row.turn }));
    if (row.status) field('status', trajectoryText('status_' + row.status));
    if (row.time != null) field('time', new Date(row.time).toLocaleString(CLATI18n.locale));
    field('elapsed', trajectoryDuration(row));
    field('model', row.model);
    if (row.tool) field('tool_name', toolDisplayName(row.tool));
    if (row.kind === 'compaction') {
      const disclosure = value.disclosure;
      field('log_range', disclosure?.range?.join(' – '));
      field('log_records', disclosure?.records);
      field('estimated_tokens', disclosure?.estimated_tokens);
    }
    if (row.kind === 'tool') field('content', row.preview);
    else markdown(row.kind === 'reasoning' ? 'reasoning' : 'content', value.text);
  }

  function preview() {
    if (value.arguments == null && value.output == null) markdown('content', value.text);
    for (const [key, data] of [['arguments', value.arguments], ['output', value.output]]) {
      if (data == null) continue;
      heading(key);
      body.append(trajectoryJSON(data));
    }
    for (const attachment of value.attachments || []) {
      const open = el('button', '', () => pageText('view_attachment'));
      open.type = 'button';
      open.onclick = () => {
        const seq = value.record.source_seqs.at(-1) ?? value.record.seq;
        showHistoricalAttachment({ ...reference, seq }, attachment, trusted);
      };
      body.append(open);
    }
  }

  function original() {
    body.append(el('p', 'trajectory-raw-note', () => trajectoryText('redacted_original')));
    for (const raw of value.raw) {
      body.append(copyTextButton(() => trajectoryText('copy_original'), () => raw, true));
      body.append(trajectoryJSON(JSON.parse(raw)));
    }
  }

  function source() {
    const row = value.record;
    field('project', reference.project);
    field('session', reference.session);
    field('event_sequences', row.source_seqs.join(', '));
    field('provider', row.provider);
    field('identifier', row.tool);
    const source = JSON.stringify({ ...reference, seq: row.seq }, null, 2);
    body.append(copyTextButton(() => trajectoryText('copy_reference'), () => source, true));
  }

  function paint() {
    if (!value || loading) return;
    tabs.select(tab);
    title.textContent = trajectoryLabel(value.record);
    icon.replaceChildren(svgIcon(trajectoryIcon(value.record)));
    body.replaceChildren();
    if (tab === 'overview') overview();
    else if (tab === 'preview') preview();
    else if (tab === 'raw') original();
    else source();
  }

  const unbind = CLATI18n.onChange(paint);
  return {
    loading() {
      loading = true;
      pane.hidden = false;
      body.replaceChildren(pageEmpty('loading'));
      jump.disabled = true;
    },
    show(next) {
      const changed = value?.record.id !== next.record.id;
      loading = false;
      value = next;
      pane.hidden = false;
      if (changed) {
        tab = 'overview';
        body.scrollTop = 0;
      }
      jump.disabled = false;
      jump.hidden = !currentProject() || ['request', 'approval'].includes(value.record.kind);
      paint();
    },
    hide() { pane.hidden = true; },
    error(error) {
      loading = true;
      pane.hidden = false;
      body.replaceChildren(el('p', 'error', error.message));
      jump.disabled = true;
    },
    dispose: unbind,
  };
}
