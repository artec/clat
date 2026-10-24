/* Host-owned jobs; closing this dialog never stops a command. */
function installBackgroundJobs() {
  const msg = (key) => CLATI18n.msg(`job.${key}`);
  const trigger = el('button', 'sidebar-footer-button', () => msg('open')); trigger.id = 'jobs-open'; trigger.type = 'button';
  document.querySelector('.sidebar-footer').prepend(trigger);
  const dialog = el('dialog', 'history-dialog'); dialog.id = 'jobs-dialog';
  const head = el('div', 'history-dialog-head');
  const close = el('button', '', () => msg('close')); close.type = 'button'; close.onclick = () => dialog.close();
  const refresh = el('button', '', () => msg('refresh')); refresh.type = 'button';
  head.append(el('h2', '', () => msg('title')), refresh, close);
  const status = el('p', 'settings-note'); status.setAttribute('role', 'status');
  const list = el('div', 'jobs-list'); const detail = el('pre', 'history-json');
  const more = el('button', '', () => CLATI18n.msg('history.more')); more.type = 'button'; more.hidden = true;
  dialog.append(head, status, list, detail, more); document.body.append(dialog);
  let generation = 0, selection = 0, previousFocus = null, active = null;
  const error = (e) => CLATI18n.bind(status, 'textContent', () => e.message);
  async function output(job, append = false) {
    const request = ++selection; const view = generation;
    try {
      const page = await rpc('jobs.output', { job_id: job.id, offset: append ? active?.offset || 0 : 0 });
      if (request !== selection || view !== generation || !dialog.open) return;
      detail.textContent = (append ? detail.textContent : '') + page.text;
      active = { id: job.id, offset: page.next_offset }; more.hidden = !page.more;
      CLATI18n.bind(status, 'textContent', () => page.lost ? msg('lost') : '');
    } catch (e) { if (request === selection && view === generation) error(e); }
  }
  function card(job) {
    const row = el('article', 'history-hit'); const name = el('strong', '', job.label);
    const state = el('span', 'settings-note', () => msg(job.state));
    const meta = el('small', '', `${job.session} · ${job.id}`);
    const read = el('button', '', () => msg('output')); read.type = 'button'; read.onclick = () => void output(job);
    const stop = el('button', '', () => msg('stop')); stop.type = 'button'; stop.disabled = job.finished_at !== null;
    stop.onclick = async () => {
      if (!window.confirm(msg('stop_confirm'))) return;
      try { await rpc('jobs.kill', { job_id: job.id, confirm: job.id }); await load(); } catch (e) { error(e); }
    };
    const observation = el('details', ''); observation.append(el('summary', '', () => msg('output')),
      el('pre', 'history-json', JSON.stringify({ reference: job.reference, observation: job.observation, persistence_error: job.persistence_error, cleanup_error: job.cleanup_error }, null, 2)));
    row.append(name, state, meta, read, stop, observation);
    const follow = el('button', '', () => CLATI18n.msg('schedule.continuation')); follow.type = 'button';
    follow.onclick = () => { dialog.close(); window.dispatchEvent(new CustomEvent('clat:job-authorize', { detail: job })); };
    row.append(follow);
    for (const [field, key] of [['persistence_error', 'persistence_failed'], ['cleanup_error', 'cleanup_failed']]) {
      if (job[field]) row.append(el('p', 'settings-note', () => CLATI18n.msg(`job.${key}`, { detail: job[field] })));
    }
    if (job.persistence_error || job.cleanup_error) {
      const retry = el('button', '', () => msg('retry')); retry.type = 'button';
      retry.onclick = async () => {
        try { await rpc('jobs.retry', { job_id: job.id, confirm: job.id }); await load(); } catch (e) { error(e); }
      };
      row.append(retry);
    }
    return row;
  }
  async function load() {
    const request = ++generation;
    try {
      const page = await rpc('jobs.list', {}); if (request !== generation || !dialog.open) return;
      list.replaceChildren(...page.jobs.map(card));
      if (!page.jobs.length) list.append(el('p', '', () => msg('empty')));
    } catch (e) { if (request === generation) error(e); }
  }
  trigger.onclick = () => { previousFocus = document.activeElement; dialog.showModal(); void load(); };
  refresh.onclick = () => void load(); more.onclick = () => { if (active) void output(active, true); };
  dialog.addEventListener('close', () => { generation++; selection++; previousFocus?.focus(); });
  window.addEventListener('clat:job-change', () => { if (dialog.open) void load(); });
}
