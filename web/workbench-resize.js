function installInspectorResize() {
  const handle = el('div', 'inspector-resize'); handle.id = 'inspector-resize';
  handle.tabIndex = 0; handle.setAttribute('role', 'separator');
  handle.setAttribute('aria-orientation', 'vertical');
  CLATI18n.attribute(handle, 'aria-label', () => shellText('resize_work_area'));
  dom.inspector.prepend(handle);
  function resize(width) {
    const viewportWidth = dom.app.clientWidth || window.innerWidth;
    const listWidth = !dom.app.dataset.page && state.sidebar === 'expanded' ? 248 : 0;
    const available = viewportWidth > INSPECTOR_DRAWER_BREAKPOINT ? viewportWidth - 58 - listWidth - 420 : 640;
    state.inspectorWidth = Math.round(Math.max(280, Math.min(width, 640, available)));
    dom.app.style.setProperty('--inspector-width', state.inspectorWidth + 'px');
    handle.setAttribute('aria-valuemin', '280'); handle.setAttribute('aria-valuemax', String(Math.max(280, Math.min(640,available))));
    handle.setAttribute('aria-valuenow', String(state.inspectorWidth));
  }
  let dragging = false;
  handle.addEventListener('pointerdown', event => {
    if (event.button !== 0) return;
    event.preventDefault(); dragging = true; handle.setPointerCapture(event.pointerId);
  });
  handle.addEventListener('pointermove', event => {
    if (dragging) resize(dom.app.getBoundingClientRect().right - event.clientX);
  });
  const finish = () => { if (dragging) { dragging = false; savePresentationPreference(); } };
  handle.addEventListener('pointerup', finish); handle.addEventListener('lostpointercapture', finish);
  handle.addEventListener('keydown', event => {
    if (!['ArrowLeft','ArrowRight','Home'].includes(event.key)) return;
    event.preventDefault(); resize(event.key === 'Home' ? 312 : state.inspectorWidth + (event.key === 'ArrowLeft' ? 24 : -24));
    savePresentationPreference();
  });
  resize(state.inspectorWidth);
}
