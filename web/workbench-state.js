/* Client-only ownership and native navigation guards; no business persistence. */
(() => {
  'use strict';
  const owners = new Map();
  function register(key, read) {
    const token = {};
    owners.set(key, { token, read });
    return () => { if (owners.get(key)?.token === token) owners.delete(key); };
  }
  function snapshot(action) {
    const result = { text: false, images: false, submitting: false, composing: false, busy: false, unavailable: false };
    for (const owner of owners.values()) {
      try {
        const value = owner.read(action);
        for (const key of Object.keys(result)) result[key] ||= value?.[key] === true;
      } catch (_) { result.unavailable = true; }
    }
    return result;
  }
  function hasUnsavedWork(action) { return Object.values(snapshot(action)).some(Boolean); }
  // Native Host activation must not infer an empty draft from a workbench
  // that has not yet registered its composer owners.
  const hasOwners = () => owners.size > 0;
  const api = Object.freeze({ version: 1, register, snapshot, hasUnsavedWork, hasOwners });
  Object.defineProperty(globalThis, 'CLATWorkbenchState', { value: api });
  if (typeof window !== 'undefined') {
    window.addEventListener('clat-desktop-action-request', event => {
      if (hasUnsavedWork(event.detail?.action)) event.preventDefault();
    });
    window.addEventListener('clat-desktop-install-lock', event => {
      if (hasUnsavedWork()) event.preventDefault();
    });
  }
})();
