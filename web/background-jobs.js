/* Host-owned background execution remains alive when the page is left. */
function installBackgroundJobs() { taskCenter = createTaskCenter(); }
function renderJobDetail(detail, job, current, fail) {
  const msg = key => CLATI18n.msg('job.' + key);
  const output = el('pre', 'history-json job-output'); const status = el('p', 'settings-note');
  const actions = el('div', 'page-toolbar'); let epoch = 0, offset = 0;
  const more = pageAction('more', () => void read(true)); more.hidden = true;
  async function read(append = false) {
    const request = ++epoch;
    try {
      const value = await rpc('jobs.output', {job_id:job.id, offset:append ? offset : 0});
      if (!current() || request !== epoch) return;
      output.textContent = (append ? output.textContent : '') + value.text;
      offset = value.next_offset; more.hidden = !value.more;
      CLATI18n.bind(status, 'textContent', () => value.lost ? msg('lost') : '');
    } catch (error) { if (current()) fail(error); }
  }
  actions.append(pageAction('refresh', () => void read()), pageAction('stop', async () => {
    if (!window.confirm(msg('stop_confirm'))) return;
    try { await rpc('jobs.kill', {job_id:job.id, confirm:job.id}); await taskCenter.refresh(); } catch (error) { if (current()) fail(error); }
  }));
  actions.lastChild.disabled = job.finished_at !== null;
  actions.append(pageAction('continuation', () => window.dispatchEvent(new CustomEvent('clat:job-authorize', {detail:job}))));
  if (job.persistence_error || job.cleanup_error) actions.append(pageAction('retry', async () => {
    try { await rpc('jobs.retry', {job_id:job.id, confirm:job.id}); await taskCenter.refresh(); } catch (error) { if (current()) fail(error); }
  }));
  const source = pageAction('source_conversation', () => {
    const host = el('section', ''); detail.replaceChildren(host);
    createConversationReader(host, {project:state.workbench.project.root,session:job.session,seq:null}, {editable:true,current});
  });
  detail.append(el('h3', '', job.label), el('p', 'page-scope', () => msg(job.state)), actions, status, output, more, source);
  if (job.persistence_error || job.cleanup_error) detail.append(el('p', 'error', job.persistence_error || job.cleanup_error));
  void read();
}
