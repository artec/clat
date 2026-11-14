/* Device-local composer state. Host upload scopes and URLs are disposable. */
let composerDraftDatabase;
const composerDraftWrites = new Set();
const failedComposerDraftWrites = new Set();
let composerDraftKey = '';
let composerDraftRead;
const composerDraftKeys = new Map();
const composerImageDrafts = new Map();
const composerReceiptIds = new Set();
const composerSubmissions = new Map();

function openComposerDraftDatabase() {
  if (composerDraftDatabase) return composerDraftDatabase;
  composerDraftDatabase = new Promise((resolve, reject) => {
    const request = indexedDB.open('clat-composer', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('drafts');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  }).catch(error => { composerDraftDatabase = null; throw error; });
  return composerDraftDatabase;
}

async function composerDraftTransaction(mode, operation) {
  const database = await openComposerDraftDatabase();
  return new Promise((resolve, reject) => {
    const transaction = database.transaction('drafts', mode);
    transaction.oncomplete = resolve;
    transaction.onabort = () => reject(transaction.error);
    try { operation(transaction.objectStore('drafts')); }
    catch (error) { transaction.abort(); reject(error); }
  });
}

const COMPOSER_DRAFT_PARTS = ['text', 'images', 'submission'];

function writeComposerDraft(key, parts) {
  if (!key) return;
  const keys = Object.keys(parts).map(part => JSON.stringify([key, part]));
  return trackComposerDraftWrite(composerDraftTransaction('readwrite', store => {
    for (const [part, value] of Object.entries(parts)) {
      putComposerDraftPart(store, key, part, value);
    }
  }), keys);
}

function putComposerDraftPart(store, key, part, value) {
  if (value == null) store.delete([key, part]);
  else store.put(value, [key, part]);
}

function trackComposerDraftWrite(operation, keys) {
  const write = operation.then(() => {
    for (const key of keys) failedComposerDraftWrites.delete(key);
    return true;
  }, () => {
    for (const key of keys) failedComposerDraftWrites.add(key);
    updateRunState(CLATI18n.msg('web.composer_draft_save_failed'));
    return false;
  }).finally(() => composerDraftWrites.delete(write));
  composerDraftWrites.add(write);
  return write;
}

function saveComposerDraftText(key, draft) {
  const text = draft && (draft.text || draft.goal)
    ? {text: draft.text, goal: Boolean(draft.goal), revision: draft.revision}
    : null;
  return writeComposerDraft(key, {text});
}

function localComposerDraftKey(session) {
  const root = state.workbench?.project?.root;
  const storage = state.composerStorageRoot;
  return root && typeof storage === 'string' ? JSON.stringify([storage, root, session]) : '';
}

function snapshotComposerImages() {
  return state.draft.images.map(({id, file}) => ({id, file}));
}

function saveComposerImages() {
  if (!composerScope) return;
  const images = snapshotComposerImages();
  if (images.length) composerImageDrafts.set(composerScope, images);
  else composerImageDrafts.delete(composerScope);
  void writeComposerDraft(composerDraftKey, {images: images.length ? images : null});
}

function rememberComposerReceipt(id) {
  if (typeof id !== 'string' || !id) return;
  composerReceiptIds.add(id);
  if (composerReceiptIds.size > 256) composerReceiptIds.delete(composerReceiptIds.values().next().value);
}

function acceptComposerReceipt(id) {
  rememberComposerReceipt(id);
  const submission = composerSubmissions.get(id);
  if (!submission) return;
  const {owner, generation, revision, images, epoch} = submission;
  clearOwnedSubmittedText(owner, generation, revision);
  clearOwnedComposerImages(owner, images, epoch);
  finishComposerSubmission(owner, id);
}

function clearOwnedComposerImages(owner, images, epoch) {
  if (!images.length) return;
  if (composerOwns(owner) && epoch === state.draft.epoch) {
    clearDraft();
    return;
  }
  const canonical = composerOwnerAliases.get(owner) || owner;
  const existing = composerImageDrafts.get(canonical);
  if (!existing) return;
  const submitted = new Set(images.map(image => image.id));
  if (composerOwns(owner)) {
    for (const image of state.draft.images.filter(image => submitted.has(image.id))) removeDraftImage(image.id);
    return;
  }
  const remaining = existing.filter(image => !submitted.has(image.id));
  if (remaining.length) composerImageDrafts.set(canonical, remaining);
  else composerImageDrafts.delete(canonical);
  void writeComposerDraft(composerDraftKeys.get(canonical), {images: remaining.length ? remaining : null});
}

function discardReceivedComposerContent(draft, receipts) {
  const submission = draft.submission;
  if (!submission || !receipts.has(submission.id)) return false;
  if (submission.revision && draft.text?.revision === submission.revision) draft.text = null;
  draft.images = (draft.images || []).filter(image => !submission.images.includes(image.id));
  draft.submission = null;
  return true;
}

function storedComposerParts(draft) {
  return {text: draft.text, images: draft.images?.length ? draft.images : null, submission: draft.submission};
}

function migrateComposerDraft(oldOwner, owner, session) {
  const oldKey = composerDraftKeys.get(oldOwner);
  if (!oldKey) return;
  const [storage, root] = JSON.parse(oldKey);
  const key = JSON.stringify([storage, root, session]);
  composerDraftKey = key;
  composerDraftKeys.set(owner, key);
  composerDraftKeys.delete(oldOwner);
  // IndexedDB queues this move after edits already enqueued for the fresh key.
  const write = composerDraftTransaction('readwrite', store => {
    for (const part of COMPOSER_DRAFT_PARTS) {
      const request = store.get([oldKey, part]);
      request.onsuccess = () => {
        if (request.result !== undefined) store.put(request.result, [key, part]);
        store.delete([oldKey, part]);
      };
    }
  });
  trackComposerDraftWrite(write, [oldKey, key].flatMap(target => COMPOSER_DRAFT_PARTS.map(part => JSON.stringify([target, part]))));
}

async function readRecoverableComposerDraft(key, freshKey, receipts) {
  const draft = {};
  await composerDraftTransaction('readwrite', store => {
    // Resolve recovery in one transaction so a different window cannot edit
    // between the read and the cleanup of a received submission.
    const fresh = {};
    const sources = freshKey ? [[key, draft], [freshKey, fresh]] : [[key, draft]];
    let pending = sources.length * COMPOSER_DRAFT_PARTS.length;
    const reconcile = () => {
      const received = discardReceivedComposerContent(draft, receipts);
      const migrated = freshKey && discardReceivedComposerContent(fresh, receipts);
      if (migrated) {
        draft.text ??= fresh.text;
        draft.images ??= fresh.images;
      }
      if (!received && !migrated) return;
      for (const [part, value] of Object.entries(storedComposerParts(draft))) {
        putComposerDraftPart(store, key, part, value);
        if (migrated) store.delete([freshKey, part]);
      }
    };
    for (const [target, result] of sources) {
      for (const part of COMPOSER_DRAFT_PARTS) {
        const request = store.get([target, part]);
        request.onsuccess = () => {
          result[part] = request.result;
          if (--pending === 0) reconcile();
        };
      }
    }
  });
  return draft;
}

async function restoreComposerDraft(owner, key) {
  const read = composerDraftRead = {};
  const current = () => read === composerDraftRead && composerScope === owner && composerDraftKey === key;
  let draft;
  try {
    // The first send may have materialized the session before the client closed.
    const freshKey = composerSession !== null ? localComposerDraftKey(null) : '';
    draft = await readRecoverableComposerDraft(key, freshKey, new Set(composerReceiptIds));
  } catch (_) {
    if (current()) updateRunState(CLATI18n.msg('web.composer_draft_restore_failed'));
  }
  if (!current()) return;
  const local = composerDrafts.get(owner);
  const useLocal = part => !draft || failedComposerDraftWrites.has(JSON.stringify([key, part]));
  const text = useLocal('text') ? local : draft.text;
  dom.prompt.value = text?.text ?? '';
  composerGoalDraft = text?.goal ? (local?.goal || {}) : null;
  state.composerGeneration++;
  if (text) composerDrafts.set(owner, {...text, generation: state.composerGeneration, goal: composerGoalDraft});
  else composerDrafts.delete(owner);
  state.composerDraftLoading = false;
  const images = (useLocal('images') ? composerImageDrafts.get(owner) : draft.images) ?? [];
  if (images.length) composerImageDrafts.set(owner, images);
  else composerImageDrafts.delete(owner);
  restoreComposerImages(images);
  resizePrompt();
  syncInteractionControls();
}

function hydrateComposerDraft(info) {
  if (!info || !composerScope || state.composerStorageRoot === undefined || state.switching) return;
  const key = localComposerDraftKey(composerSession);
  if (!key) {
    if (!state.composerDraftLoading) return;
    state.composerDraftLoading = false;
    updateRunState(CLATI18n.msg('web.composer_draft_restore_failed'));
    restoreComposerImages(composerImageDrafts.get(composerScope) ?? []);
    syncInteractionControls();
    return;
  }
  if (composerDraftKey === key) {
    reconcileComposerQueue(info, acceptComposerReceipt);
    return;
  }
  composerDraftKey = key;
  composerDraftKeys.set(composerScope, key);
  state.composerDraftLoading = true;
  reconcileComposerQueue(info, rememberComposerReceipt);
  return restoreComposerDraft(composerScope, key);
}

function reconcileComposerQueue(info, receive) {
  if ((info.pending_inputs?.sessionId ?? null) === composerSession) {
    for (const item of info.pending_inputs?.items || []) receive(item.id);
  }
}

function persistComposerSubmission(id, images, owner = composerScope) {
  const canonical = composerOwnerAliases.get(owner) || owner;
  const key = composerDraftKeys.get(canonical);
  if (!key) return;
  const revision = inputHistory.draft === null ? composerDrafts.get(canonical)?.revision : undefined;
  if (composerSubmissions.size >= 256) composerSubmissions.delete(composerSubmissions.keys().next().value);
  composerSubmissions.set(id, {owner, generation: state.composerGeneration, revision, images, epoch: state.draft.epoch});
  return writeComposerDraft(key, {
    submission: {id, revision, images: images.map(image => image.id)},
  });
}

function finishComposerSubmission(owner, id) {
  if (!composerSubmissions.delete(id)) return;
  const key = composerDraftKeys.get(composerOwnerAliases.get(owner) || owner);
  if (!key) return;
  trackComposerDraftWrite(composerDraftTransaction('readwrite', store => {
    const request = store.get([key, 'submission']);
    request.onsuccess = () => {
      if (request.result?.id === id) store.delete([key, 'submission']);
    };
  }), [JSON.stringify([key, 'submission'])]);
}

function composerDraftNeedsProtection() {
  return composerDraftWrites.size > 0 || failedComposerDraftWrites.size > 0 || !composerDraftKey;
}
