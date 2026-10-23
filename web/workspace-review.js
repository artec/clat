let reviewDialog = null;
let reviewRequest = 0;
let reviewFileRequest = 0;
let reviewSelected = null;

function reviewOwner() { return [state.stream, state.selectionGeneration, workspacePrefix]; }
function reviewStillCurrent(request, owner) {
  return reviewDialog.open && request === reviewRequest && owner.every((v, i) => v === reviewOwner()[i]);
}

async function refreshWorkspaceReview() {
  const request = ++reviewRequest;
  const owner = reviewOwner();
  const list = document.getElementById('review-files');
  const status = document.getElementById('review-status');
  list.replaceChildren(); reviewSelected = null; reviewFileRequest += 1;
  document.getElementById('review-patch').replaceChildren();
  CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.reading_current_working_tree", {}, "Reading current working tree…"));
  try {
    const changes = await rpc('workspace.changes', {});
    if (!reviewStillCurrent(request, owner)) return;
    CLATI18n.bind(status, "textContent", () => CLATI18n.present(changes.presentation,changes.note) + (changes.truncated ? CLATI18n.msg("web.file_limit",{}," · File limit reached; list is incomplete.") : ''));
    if (changes.state !== 'available') { list.append(el('p', 'picker-hint', () => (changes.state))); return; }
    if (!changes.files.length) list.append(el('p', 'picker-hint', () => (CLATI18n.msg("web.no_workspace_changes_at_this_snapshot", {}, "No workspace changes at this snapshot."))));
    for (const file of changes.files) {
      const button = el('button', 'review-file'); button.type = 'button';
      button.append(el('code', '', () => (file.status)), el('span', '', () => (file.path)),
        el('small', '', () => ([file.staged && CLATI18n.msg("web.file_staged"), file.unstaged && CLATI18n.msg("web.file_unstaged"), file.untracked && CLATI18n.msg("web.file_untracked")].filter(Boolean).join(' · '))));
      if (file.previous_path) CLATI18n.bind(button, "title", () => file.previous_path + ' → ' + file.path);
      button.addEventListener('click', () => { void selectWorkspaceFile(file.path, request, owner); });
      list.appendChild(button);
    }
  } catch (error) { if (reviewStillCurrent(request, owner)) CLATI18n.bind(status, "textContent", () => CLATI18n.msg("web.line_workspace_review_unavailable_value",{value1:(error.message)},"Workspace review unavailable: {value1}")); }
}

async function selectWorkspaceFile(path, request, owner) {
  const fileRequest = ++reviewFileRequest;
  reviewSelected = path;
  const patch = document.getElementById('review-patch');
  patch.replaceChildren(el('h3', '', () => (path)), el('p', 'picker-hint', () => (CLATI18n.msg("web.reading_bounded_diff", {}, "Reading bounded diff…"))));
  const current = () => reviewStillCurrent(request, owner) && fileRequest === reviewFileRequest && reviewSelected === path;
  for (const button of document.querySelectorAll('.review-file')) button.classList.toggle('is-selected', button.querySelector('span').textContent === path);
  try {
    const diff = await rpc('workspace.diff', { path });
    if (!current()) return;
    patch.replaceChildren(el('h3', '', () => (diff.path)), el('p', 'picker-hint', () => (CLATI18n.present(diff.presentation,diff.note))));
    for (const [name, text] of [[CLATI18n.msg("web.staged", {}, "Staged"), diff.staged], [CLATI18n.msg("web.unstaged_untracked", {}, "Unstaged / untracked"), diff.unstaged]]) {
      if (!text) continue;
      patch.appendChild(el('h4', '', () => (name)));
      const pre = el('pre', 'review-diff');
      for (const line of text.split('\n')) {
        const kind = line.startsWith('@@') ? 'hunk' : line.startsWith('+') ? 'added' : line.startsWith('-') ? 'removed' : '';
        pre.appendChild(el('span', kind, () => (line + '\n')));
      }
      patch.appendChild(pre);
    }
  } catch (error) { if (current()) patch.replaceChildren(el('h3', '', () => (path)), el('p', 'error', () => (CLATI18n.msg("web.line_no_preview_value",{value1:(error.message)},"No preview: {value1}")))); }
}

function installWorkspaceReview() {
  reviewDialog = el('dialog', 'workspace-review'); reviewDialog.id = 'workspace-review';
  CLATI18n.attribute(reviewDialog,'aria-label',()=>CLATI18n.msg("web.review_workspace_changes", {}, "Review workspace changes"));
  const header = el('div', 'review-header');
  const close = el('button', 'icon-button', () => ('×')); close.type = 'button'; CLATI18n.attribute(close,'aria-label',()=>CLATI18n.msg("web.close_workspace_review", {}, "Close workspace review"));
  const refresh = el('button', 'ghost', () => (CLATI18n.msg("web.refresh_working_tree", {}, "Refresh working tree"))); refresh.type = 'button';
  header.append(el('h2', '', () => (CLATI18n.msg("web.workspace_changes", {}, "Workspace changes"))), refresh, close);
  const note = el('p', 'picker-hint', () => (CLATI18n.msg("web.read_only_current_project_includes_existing_edits_not_agent_run_attribution", {}, "Read-only · current project · includes existing edits · not agent/run attribution")));
  const status = el('p', 'picker-hint'); status.id = 'review-status'; status.setAttribute('role', 'status');
  const body = el('div', 'review-body');
  const files = el('nav', 'review-files'); files.id = 'review-files'; CLATI18n.attribute(files,'aria-label',()=>CLATI18n.msg("web.changed_files", {}, "Changed files"));
  const patch = el('section', 'review-patch'); patch.id = 'review-patch'; CLATI18n.attribute(patch,'aria-label',()=>CLATI18n.msg("web.file_diff", {}, "File diff"));
  body.append(files, patch); reviewDialog.append(header, note, status, body); document.body.appendChild(reviewDialog);
  close.addEventListener('click', () => reviewDialog.close());
  reviewDialog.addEventListener('close', () => { reviewRequest += 1; reviewFileRequest += 1; focusWorkbenchTool(document.getElementById('review-open')); });
  refresh.addEventListener('click', () => { void refreshWorkspaceReview(); });
  document.getElementById('review-open').addEventListener('click', () => { reviewDialog.showModal(); void refreshWorkspaceReview(); });
}
