const editorMarkdownTags = new Set(`
  a b blockquote br caption center code col colgroup dd del details div dl dt em
  figcaption figure h1 h2 h3 h4 h5 h6 hr i img input kbd li mark ol p pre s samp
  span strong sub summary sup table tbody td tfoot th thead tr ul
`.trim().split(/\s+/));

function editorMarkdownPath(documentPath, reference) {
  const decoded = decodeEditorMarkdownReference(reference.split(/[?#]/, 1)[0]);
  if (!decoded || decoded.startsWith('/') || /[\\:\x00-\x1f\x7f]/.test(decoded)) return null;
  const parts = documentPath.split('/').slice(0, -1);
  for (const part of decoded.split('/')) {
    if (!part || part === '.') continue;
    if (part.toLowerCase() === '.git') return null;
    if (part === '..') {
      if (!parts.length) return null;
      parts.pop();
    } else {
      parts.push(part);
    }
  }
  return parts.join('/') || null;
}

function decodeEditorMarkdownReference(value) {
  try { return decodeURIComponent(value); }
  catch (_) { return null; }
}

function editorMarkdownLink(link, reference, context) {
  const external = markdownWebAddress(reference);
  const fragment = reference.startsWith('#');
  const path = external || fragment ? null : editorMarkdownPath(context.path, reference);
  if (!external && !fragment && !path) return;
  link.href = external || (fragment ? reference : '#');
  if (external) {
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
  }
  link.addEventListener('click', event => {
    event.preventDefault();
    if (external) {
      void openMarkdownUrl(external).catch(context.reportError);
    } else if (path) {
      void context.openFile(path);
    } else {
      const target = context.anchors.get(decodeEditorMarkdownReference(reference.slice(1)));
      target?.focus({preventScroll:true});
      target?.scrollIntoView({block:'start'});
    }
  });
}

function editorMarkdownAttributes(source, node) {
  const tag = node.localName;
  const alignment = source.getAttribute('align')?.toLowerCase();
  if (['left', 'center', 'right', 'justify'].includes(alignment)) node.style.textAlign = alignment;
  if (['td', 'th'].includes(tag)) {
    for (const attr of ['colspan', 'rowspan']) {
      const value = source.getAttribute(attr);
      if (/^[1-9]\d{0,2}$/.test(value)) node.setAttribute(attr, value);
    }
  }
  if (tag === 'col') {
    const width = source.getAttribute('width') || /^\s*width\s*:\s*(\d+(?:\.\d+)?%)\s*;?\s*$/i.exec(source.getAttribute('style'))?.[1];
    if (/^(?:100|[1-9]?\d(?:\.\d+)?)%$/.test(width)) node.style.width = width;
  }
  if (tag === 'details') node.open = source.hasAttribute('open');
  if (tag === 'ol' && /^-?\d{1,9}$/.test(source.getAttribute('start'))) node.start = Number(source.getAttribute('start'));
  const title = source.getAttribute('title');
  if (title) node.title = title;
}

function editorMarkdownAnchor(source, node, context) {
  const explicit = source.id || (node.localName === 'a' && source.getAttribute('name'));
  const heading = /^h[1-6]$/.test(node.localName);
  if (!explicit && !heading) return;
  node.tabIndex = -1;
  if (explicit && !context.anchors.has(explicit)) context.anchors.set(explicit, node);
  if (!heading) return;
  const base = node.textContent.trim().toLowerCase().replace(/[^\p{L}\p{M}\p{N}_\- ]/gu, '').replace(/ /g, '-');
  if (!base || base === explicit) return;
  let slug = base;
  for (let suffix = 1; context.anchors.has(slug); suffix++) slug = base + '-' + suffix;
  context.anchors.set(slug, node);
}

function editorMarkdownImage(source, context) {
  const fallback = el('span', 'editor-preview-image-alt', source.alt);
  const path = editorMarkdownPath(context.path, source.getAttribute('src') || '');
  if (path) context.images.push({source, fallback, path});
  return fallback;
}

function copyEditorMarkdown(source, context) {
  if (source.nodeType === Node.TEXT_NODE) return document.createTextNode(source.data);
  if (source.namespaceURI !== 'http://www.w3.org/1999/xhtml' || !editorMarkdownTags.has(source.localName)) return null;
  const tag = source.localName;
  if (tag === 'img') return editorMarkdownImage(source, context);
  if (tag === 'input' && source.type !== 'checkbox') return null;
  const node = document.createElement(tag);
  editorMarkdownAttributes(source, node);
  if (tag === 'input') {
    node.type = 'checkbox';
    node.disabled = true;
    node.checked = source.hasAttribute('checked');
  }
  for (const child of source.childNodes) {
    const safe = copyEditorMarkdown(child, context);
    if (safe) node.append(safe);
  }
  if (tag === 'a') editorMarkdownLink(node, source.getAttribute('href')?.trim() || '', context);
  editorMarkdownAnchor(source, node, context);
  if (tag !== 'table') return node;
  const scroller = el('div', 'table-scroll');
  scroller.append(node);
  return scroller;
}

async function loadEditorMarkdownImages(images, signal) {
  for (const {source, fallback, path} of images) {
    if (signal.aborted) break;
    try {
      const {base64, media_type} = await rpc('editor.previewImage', {path}, {signal});
      if (signal.aborted) break;
      const bytes = Uint8Array.from(atob(base64), char => char.charCodeAt(0));
      const image = new Image();
      image.alt = source.alt;
      if (source.title) image.title = source.title;
      for (const attr of ['width', 'height']) {
        const value = source.getAttribute(attr);
        if (/^[1-9]\d{0,3}$/.test(value)) image.setAttribute(attr, value);
      }
      // Decode as an image and revoke before exposing the element. The SVG
      // bytes are available only to the browser's inert image renderer.
      const url = URL.createObjectURL(new Blob([bytes], {type:media_type}));
      try {
        image.src = url;
        await image.decode();
      } finally {
        URL.revokeObjectURL(url);
      }
      if (!signal.aborted) fallback.replaceWith(image);
    } catch (_) {
      // Keep the image's alternative text when the project asset is unavailable.
    }
  }
}

function renderEditorMarkdown(preview, doc, {openFile, reportError}) {
  // Template content is inert. Only freshly created, explicitly allowed
  // nodes enter the live document, so the source can't trigger navigation,
  // requests, scripts or style changes as a side effect of parsing.
  const template = document.createElement('template');
  const context = {path:doc.path, openFile, reportError, anchors:new Map(), images:[]};
  const controller = new AbortController();
  try {
    template.innerHTML = globalThis.CLATDevelopmentWidgets.parseMarkdown(doc.text);
    const fragment = document.createDocumentFragment();
    for (const child of template.content.childNodes) {
      const safe = copyEditorMarkdown(child, context);
      if (safe) fragment.append(safe);
    }
    preview.append(fragment);
    void loadEditorMarkdownImages(context.images, controller.signal);
  } catch (_) {
    preview.append(el('pre', '', doc.text));
  }
  return () => controller.abort();
}
