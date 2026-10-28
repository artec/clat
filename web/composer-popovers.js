/* Composer action popups open explicitly; context usage opts into hover preview. */
let activeComposerPopup = null, composerPopupDismissalInstalled = false;
function installComposerPopupSurface(popup, trigger, alignment) {
  dom.app.append(popup);
  const position = () => { if (!popup.hidden) positionUsagePopover(popup, trigger, alignment); };
  const observer = new ResizeObserver(position); observer.observe(popup); observer.observe(dom['composer-shell']);
  window.addEventListener('resize', position);
  return position;
}
function composerPopupFocusables(root) {
  return [...root.querySelectorAll('button, input, select, textarea, a[href], [tabindex]')]
    .filter(node => !node.disabled && node.tabIndex >= 0 && node.getClientRects().length);
}
function composerPopupKeyboard(event, controller, trigger, popup) {
  if (event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
  const items = composerPopupFocusables(popup);
  if (event.target === trigger && (event.key === 'ArrowDown' || (event.key === 'Tab' && !event.shiftKey && controller.isOpen() && items.length))) {
    event.preventDefault(); controller.open({ pin: true }); composerPopupFocusables(popup)[0]?.focus(); return;
  }
  if (event.key !== 'Tab' || !popup.contains(event.target)) return;
  if (event.shiftKey && event.target === items[0]) { event.preventDefault(); trigger.focus(); return; }
  if (!event.shiftKey && event.target === items.at(-1)) {
    event.preventDefault(); controller.close();
    const controls = composerPopupFocusables(dom['composer-shell'].querySelector('.composer-row'));
    (controls[controls.indexOf(trigger) + 1] || dom.prompt).focus();
  }
}
function installComposerPopupDismissal() {
  if (composerPopupDismissalInstalled) return;
  composerPopupDismissalInstalled = true;
  document.addEventListener('keydown', event => {
    if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || event.keyCode === 229 || !activeComposerPopup) return;
    event.preventDefault(); activeComposerPopup.close({ focus: true });
  }, true);
}
function attachComposerPopup(seat, trigger, popup, render, alignment = 'center', { hover = false, onClose = () => {} } = {}) {
  let pinned = false, timer;
  const position = installComposerPopupSurface(popup, trigger, alignment);
  const contains = target => target instanceof Node && (seat.contains(target) || popup.contains(target));
  const isOpen = () => !popup.hidden && !popup.classList.contains('hidden');
  const close = ({ focus = false } = {}) => {
    clearTimeout(timer); if (!isOpen()) return;
    if (focus) trigger.focus({ preventScroll: true });
    hide(popup); popup.hidden = true; pinned = false; trigger.setAttribute('aria-expanded', 'false');
    if (activeComposerPopup === controller) activeComposerPopup = null;
    onClose();
  };
  const open = ({ pin = false } = {}) => {
    clearTimeout(timer); if (trigger.disabled) return false;
    if (activeComposerPopup && activeComposerPopup !== controller) {
      if (activeComposerPopup.isPinned() && !pin) return false;
      activeComposerPopup.close();
    }
    const fresh = !isOpen(); pinned ||= pin; activeComposerPopup = controller;
    popup.hidden = false; show(popup); trigger.setAttribute('aria-expanded', 'true');
    if (fresh) render(); position(); return true;
  };
  const controller = { open, close, position, isOpen, isPinned: () => pinned, toggle: () => pinned && isOpen() ? close() : open({ pin: true }) };
  if (hover) seat.addEventListener('pointerenter', event => { if (event.pointerType !== 'touch') open(); });
  popup.addEventListener('pointerenter', () => clearTimeout(timer));
  popup.addEventListener('pointerdown', () => { pinned = true; });
  for (const region of [seat, popup]) {
    region.addEventListener('pointerleave', () => {
      if (hover && !pinned) timer = setTimeout(() => { if (!popup.contains(document.activeElement)) close(); }, 220);
    });
    region.addEventListener('focusin', () => { clearTimeout(timer); if (hover && !isOpen()) open(); });
    region.addEventListener('focusout', event => { if (event.relatedTarget && !contains(event.relatedTarget)) close(); });
    region.addEventListener('keydown', event => composerPopupKeyboard(event, controller, trigger, popup));
  }
  document.addEventListener('pointerdown', event => { if (!contains(event.target)) close(); });
  installComposerPopupDismissal();
  return controller;
}
