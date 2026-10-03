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
  status.textContent = 'Reading current working tree…';
  try {
    const changes = await rpc('workspace.changes', {});
    if (!reviewStillCurrent(request, owner)) return;
    status.textContent = changes.note + (changes.truncated ? ' · File limit reached; list is incomplete.' : '');
    if (changes.state !== 'available') { list.append(el('p', 'picker-hint', changes.state)); return; }
    if (!changes.files.length) list.append(el('p', 'picker-hint', 'No workspace changes at this snapshot.'));
    for (const file of changes.files) {
      const button = el('button', 'review-file'); button.type = 'button';
      button.append(el('code', '', file.status), el('span', '', file.path),
        el('small', '', [file.staged && 'staged', file.unstaged && 'unstaged', file.untracked && 'untracked'].filter(Boolean).join(' · ')));
      if (file.previous_path) button.title = file.previous_path + ' → ' + file.path;
      button.addEventListener('click', () => { void selectWorkspaceFile(file.path, request, owner); });
      list.appendChild(button);
    }
  } catch (error) { if (reviewStillCurrent(request, owner)) status.textContent = 'Workspace review unavailable: ' + error.message; }
}

async function selectWorkspaceFile(path, request, owner) {
  const fileRequest = ++reviewFileRequest;
  reviewSelected = path;
  const patch = document.getElementById('review-patch');
  patch.replaceChildren(el('h3', '', path), el('p', 'picker-hint', 'Reading bounded diff…'));
  const current = () => reviewStillCurrent(request, owner) && fileRequest === reviewFileRequest && reviewSelected === path;
  for (const button of document.querySelectorAll('.review-file')) button.classList.toggle('is-selected', button.querySelector('span').textContent === path);
  try {
    const diff = await rpc('workspace.diff', { path });
    if (!current()) return;
    patch.replaceChildren(el('h3', '', diff.path), el('p', 'picker-hint', diff.note));
    for (const [name, text] of [['Staged', diff.staged], ['Unstaged / untracked', diff.unstaged]]) {
      if (!text) continue;
      patch.appendChild(el('h4', '', name));
      const pre = el('pre', 'review-diff');
      for (const line of text.split('\n')) {
        const kind = line.startsWith('@@') ? 'hunk' : line.startsWith('+') ? 'added' : line.startsWith('-') ? 'removed' : '';
        pre.appendChild(el('span', kind, line + '\n'));
      }
      patch.appendChild(pre);
    }
  } catch (error) { if (current()) patch.replaceChildren(el('h3', '', path), el('p', 'error', 'No preview: ' + error.message)); }
}

function installWorkspaceReview() {
  reviewDialog = el('dialog', 'workspace-review'); reviewDialog.id = 'workspace-review';
  reviewDialog.setAttribute('aria-label', 'Review workspace changes');
  const header = el('div', 'review-header');
  const close = el('button', 'icon-button', '×'); close.type = 'button'; close.setAttribute('aria-label', 'Close workspace review');
  const refresh = el('button', 'ghost', 'Refresh working tree'); refresh.type = 'button';
  header.append(el('h2', '', 'Workspace changes'), refresh, close);
  const note = el('p', 'picker-hint', 'Read-only · current project · includes existing edits · not agent/run attribution');
  const status = el('p', 'picker-hint'); status.id = 'review-status'; status.setAttribute('role', 'status');
  const body = el('div', 'review-body');
  const files = el('nav', 'review-files'); files.id = 'review-files'; files.setAttribute('aria-label', 'Changed files');
  const patch = el('section', 'review-patch'); patch.id = 'review-patch'; patch.setAttribute('aria-label', 'File diff');
  body.append(files, patch); reviewDialog.append(header, note, status, body); document.body.appendChild(reviewDialog);
  close.addEventListener('click', () => reviewDialog.close());
  reviewDialog.addEventListener('close', () => { reviewRequest += 1; reviewFileRequest += 1; document.getElementById('review-open').focus(); });
  refresh.addEventListener('click', () => { void refreshWorkspaceReview(); });
  document.getElementById('review-open').addEventListener('click', () => { reviewDialog.showModal(); void refreshWorkspaceReview(); });
}
