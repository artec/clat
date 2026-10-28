/* Permission presentation only; the Host owns admission and persistence. */
const COMPOSER_PERMISSIONS = [
  { mode: 'read-only', icon: 'vision', label: 'read_only', detail: 'permission_read_only_short' },
  { mode: 'workspace-write', icon: 'permission-write', label: 'project_write', detail: 'permission_project_write_short' },
  { mode: 'danger-full-access', icon: 'permission-full', label: 'full_access', detail: 'permission_full_access_short' },
];
let composerPermissions = null;
function renderComposerPermission(mode) {
  const button = dom['composer-permission'];
  if (button.dataset.mode !== (mode || '')) {
    button.querySelector('svg').replaceWith(svgIcon(COMPOSER_PERMISSIONS.find(item => item.mode === mode)?.icon || 'settings'));
  }
}
function installComposerPermissions() {
  const trigger = dom['composer-permission'], seat = el('div', 'usage-seat permission-seat');
  trigger.before(seat); seat.append(trigger); trigger.removeAttribute('title');
  const popup = el('section', 'usage-popover permission-popover hidden'); popup.id = 'composer-permission-popup';
  popup.setAttribute('role', 'dialog'); CLATI18n.attribute(popup, 'aria-label', () => CLATI18n.msg('web.permission_mode'));
  trigger.setAttribute('aria-haspopup', 'dialog'); trigger.setAttribute('aria-expanded', 'false'); trigger.setAttribute('aria-controls', popup.id);
  const heading = el('div', 'usage-row');
  const currentLabel = el('span');
  heading.append(el('span', '', () => CLATI18n.msg('web.permission_mode')), currentLabel);
  const options = el('fieldset', 'composer-permission-options');
  CLATI18n.attribute(options, 'aria-label', () => CLATI18n.msg('web.permission_mode'));
  const confirmation = el('label', 'composer-permission-confirm'); confirmation.hidden = true;
  const checkbox = el('input'); checkbox.type = 'checkbox';
  confirmation.append(checkbox, el('span', '', () => CLATI18n.msg('web.i_understand_that_full_access_removes_tool_approval_prompts')));
  const apply = el('button', 'primary permission-apply', () => CLATI18n.msg('web.apply_permission_mode')); apply.type = 'button'; apply.hidden = true;
  const error = el('p', 'error'); error.setAttribute('role', 'alert'); error.hidden = true;
  const status = el('p', 'permission-popup-status'); status.setAttribute('role', 'status'); status.hidden = true;
  popup.append(heading, el('small', '', () => CLATI18n.msg('web.this_is_a_session_property_changes_apply_at_the_next_tool_permission_check')), options, confirmation, apply, error, status);
  seat.append(popup);
  const view = composerPermissions = { trigger, popup, currentLabel, options, confirmation, checkbox, apply, error, status, rows: [], target: null, selected: null, saving: false };
  for (const item of COMPOSER_PERMISSIONS) addComposerPermissionOption(view, item);
  view.popover = attachComposerPopup(seat, trigger, popup, prepareComposerPermissions, 'left', { onClose: () => { view.target = null; checkbox.checked = false; } });
  trigger.onclick = view.popover.toggle;
  checkbox.onchange = syncComposerPermissionControls;
  apply.onclick = () => void applyComposerPermission(view.selected);
  window.addEventListener('clat:workbench-updated', syncComposerPermissionControls);
}
function addComposerPermissionOption(view, item) {
  const label = el('label', 'composer-permission-option'); label.dataset.mode = item.mode;
  const radio = el('input'); radio.type = 'radio'; radio.name = 'composer-permission-mode'; radio.value = item.mode;
  const copy = el('span', 'permission-option-copy');
  copy.append(el('strong', '', () => CLATI18n.msg('web.' + item.label)), el('small', '', () => CLATI18n.msg('web.' + item.detail)));
  const check = svgIcon('check'); check.classList.add('permission-option-check');
  label.append(radio, svgIcon(item.icon), copy, check); view.options.append(label); view.rows.push({ radio, label });
  radio.onchange = () => {
    view.selected = item.mode; view.checkbox.checked = false;
    view.error.hidden = true; view.status.hidden = true; syncComposerPermissionControls();
    if (item.mode !== 'danger-full-access') void applyComposerPermission(item.mode);
  };
}
function prepareComposerPermissions() {
  const view = composerPermissions;
  if (!view.target) {
    view.target = { session: state.sessionId, generation: state.selectionGeneration };
    view.selected = state.workbench?.permission?.mode;
    view.error.hidden = true; view.status.hidden = true; view.checkbox.checked = false;
  }
  syncComposerPermissionControls();
}
function syncComposerPermissionControls() {
  const view = composerPermissions; if (!view) return;
  if (view.target && (view.target.session !== state.sessionId || view.target.generation !== state.selectionGeneration)) { view.popover.close(); return; }
  const current = state.workbench?.permission?.mode;
  const item = COMPOSER_PERMISSIONS.find(option => option.mode === current);
  CLATI18n.bind(view.currentLabel, 'textContent', () => CLATI18n.msg('web.' + (item?.label || 'permission_mode')));
  const blocked = view.saving || state.switching || state.desktopUpdateLocked || !current;
  view.options.disabled = Boolean(blocked);
  for (const { radio, label } of view.rows) {
    radio.checked = view.selected === radio.value;
    label.classList.toggle('is-current', current === radio.value);
  }
  const needsConfirmation = view.selected === 'danger-full-access' && current !== view.selected;
  view.confirmation.hidden = !needsConfirmation; view.apply.hidden = !needsConfirmation;
  view.checkbox.disabled = Boolean(blocked); view.apply.disabled = Boolean(blocked || !view.checkbox.checked);
}
async function applyComposerPermission(mode) {
  const view = composerPermissions, target = view.target;
  if (!target || view.saving || state.switching || state.desktopUpdateLocked) return;
  if (target.session !== state.sessionId || target.generation !== state.selectionGeneration) { view.popover.close(); return; }
  if (mode === 'danger-full-access' && state.workbench?.permission?.mode !== mode && !view.checkbox.checked) return;
  if (mode === state.workbench?.permission?.mode) return;
  view.saving = true; view.error.hidden = true; view.status.hidden = true; syncComposerPermissionControls();
  try {
    await rpc('permission.set', { mode, ...(mode === 'danger-full-access' ? { confirm: mode } : {}), expected_selection_generation: target.generation });
    await refreshWorkbench();
    if (view.target === target) {
      view.selected = state.workbench?.permission?.mode;
      CLATI18n.bind(view.status, 'textContent', () => CLATI18n.msg('web.permission_mode_updated')); view.status.hidden = false;
    }
  } catch (error) {
    if (view.target === target) {
      CLATI18n.bind(view.error, 'textContent', () => error.message); view.error.hidden = false;
      view.selected = state.workbench?.permission?.mode;
    }
    // Even a persistence error may have changed Host memory; read back, never retry a write.
    await refreshWorkbench();
  } finally { view.saving = false; syncComposerPermissionControls(); }
}
