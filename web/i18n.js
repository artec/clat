/* Shared native/browser presentation runtime. No business requests or HTML evaluation. */
(() => {
  'use strict';
  if (globalThis.CLATI18n) return;
  const builtin=globalThis.__CLAT_BUILTIN_LOCALE__;
  const catalogs = builtin ? {en:builtin.english} : {}, bindings = new Set(), listeners = new Set();
  let locale = 'en', preference = 'system', tracking = false, tracked = false;
  let manifest=builtin?.manifest || {languages:[]};
  const normalize = value => {
    const parts=String(value || '').toLowerCase().replace(/_/g,'-').split(/[.@]/)[0].split('-');
    const alias=value=>manifest.languages.find(l=>[l.id,...l.aliases].some(a=>a.toLowerCase()===value))?.id || null;
    if(parts[0]==='zh'&&parts.includes('hant'))return alias('zh-hant');
    if(parts[0]==='zh'&&parts.includes('hans'))return alias('zh-hans');
    return alias(parts.join('-')) || parts.slice(1).map(p=>alias(parts[0]+'-'+p)).find(Boolean) || alias(parts[0]);
  };
  const resolve = value => value !== 'system' ? normalize(value) || 'en'
    : (navigator.languages || [navigator.language]).map(normalize).find(Boolean) || 'en';
  function format(text, args) {
    return String(text).replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (all, name) => Object.hasOwn(args, name) ? String(args[name]) : all);
  }
  function render(key, args = {}, fallback = key, argPresentations = {}, depth = 0) {
    if (tracking) tracked = true;
    if(!args || typeof args!=='object')args={};
    if(!argPresentations || typeof argPresentations!=='object')argPresentations={};
    const own=id=>catalogs[id]&&Object.hasOwn(catalogs[id],key)?catalogs[id][key]:undefined;
    let value = own(locale) ?? own('en') ?? fallback;
    if (value && typeof value === 'object') value = value[locale === 'en' && args.count === 1 ? 'one' : 'other'];
    if(typeof value!=='string')value=fallback;
    if(depth<8 && Object.keys(argPresentations).length){
      args={...args};
      for(const [name,message] of Object.entries(argPresentations)){
        Object.defineProperty(args,name,{value:render(message.key,message.args,message.fallback,message.arg_presentations,depth+1),enumerable:true,configurable:true});
      }
    }
    return format(value, args);
  }
  const t = (key, args = {}, fallback) => render(key, args, fallback);
  const msg = (key, args = {}, fallback, argPresentations = {}) => {
    const value = new String(fallback || key);
    Object.defineProperties(value, {
      key:{value:key},args:{value:args},fallback:{value:fallback},
      toString:{value:()=>render(key,args,fallback,argPresentations)},toJSON:{value:()=>render(key,args,fallback,argPresentations)},
      [Symbol.toPrimitive]:{value:()=>render(key,args,fallback,argPresentations)},
    });
    return Object.freeze(value);
  };
  function view(compute) {
    const value=new String('');
    Object.defineProperties(value,{key:{value:'client-view'},
      toString:{value:()=>String(compute())},toJSON:{value:()=>String(compute())},
      [Symbol.toPrimitive]:{value:()=>String(compute())},
    });return Object.freeze(value);
  }
  function present(value, fallback='') {
    const message=v=>v && typeof v.key==='string' && (typeof v.fallback==='string' || Object.hasOwn(catalogs.en || {},v.key));
    if(Array.isArray(value)) {
      if(!value.length || !value.every(message))return fallback;
      const result=new String(fallback);
      Object.defineProperties(result,{key:{value:'parts'},
        toString:{value:()=>value.map(v=>render(v.key,v.args,v.fallback,v.arg_presentations)).join('\n\n')},
        [Symbol.toPrimitive]:{value:()=>value.map(v=>render(v.key,v.args,v.fallback,v.arg_presentations)).join('\n\n')},
      });return Object.freeze(result);
    }
    return message(value) ? msg(value.key,value.args,value.fallback || fallback,value.arg_presentations) : fallback;
  }
  function textNode(value) {const node=document.createTextNode('');bind(node,'nodeValue',()=>value);return node;}
  function error(message) {const result=new Error(String(message));Object.defineProperty(result,'message',{get:()=>String(message)});return result;}
  const assign = (node, property, value) => property.startsWith('@') ? node.setAttribute(property.slice(1),value) : (node[property]=value);
  const attribute = (node,name,compute) => bind(node,'@'+name,compute);

  function bind(node, property, compute) {
    const previous = tracking, previousTracked = tracked; tracking = true; tracked = false;
    const value = compute(); const dynamic = tracked || (value && typeof value === 'object' && value.key);
    tracking = previous; tracked = previousTracked || !!dynamic;
    const existing = node.__clatBindings || (node.__clatBindings = new Map());
    if (dynamic) {
      if (!existing.has(property)) {
        bindings.add({ref:new WeakRef(node), property});
        existing.set(property,{compute});
      } else existing.get(property).compute = compute;
    } else if(existing.has(property)) existing.get(property).compute = null;
    assign(node,property,value);
  }

  function applyStatic(root = document) {
    for (const node of root.querySelectorAll('[data-i18n]')) {
      if(node.__clatBindings?.has('textContent'))continue;
      node.__clatEnglish ??= node.textContent;
      bind(node, 'textContent', () => t(node.dataset.i18n, {}, node.__clatEnglish));
    }
    for (const attr of ['title', 'placeholder', 'aria-label', 'alt']) {
      for (const node of root.querySelectorAll(`[data-i18n-${attr}]`)) {
        const key = node.getAttribute(`data-i18n-${attr}`);
        if(node.__clatBindings?.has('@'+attr))continue;
        node.__clatAttributes ??= {};
        node.__clatAttributes[attr] ??= node.getAttribute(attr);
        attribute(node,attr,()=>t(key, {}, node.__clatAttributes[attr]));
      }
    }
  }
  function refresh() {
    document.documentElement.lang = locale;
    for (const binding of bindings) {
      const node = binding.ref.deref();
      if (!node) { bindings.delete(binding); continue; }
      const compute = node.__clatBindings?.get(binding.property)?.compute;
      if (compute) assign(node,binding.property,compute());
    }
    applyStatic();
    for (const select of document.querySelectorAll('[data-language-select]')) select.value = preference;
    for (const fn of listeners) fn(locale);
  }
  async function setLanguage(value) {
    if (value !== 'system' && !normalize(value)) throw new Error('Unsupported interface language');
    if (globalThis.__TAURI__) {
      const info = await globalThis.__TAURI__.core.invoke('desktop_preferences', { language:value });
      install(info);
    } else {
      const selected = resolve(value);
      try {await load(selected);locale=selected;} catch (_) {locale='en';}
      preference = value;
      try {
        const saved = JSON.parse(localStorage.getItem('clat.presentation.v1') || '{}');
        saved.language = value; localStorage.setItem('clat.presentation.v1', JSON.stringify(saved));
      } catch (_) { /* Preference storage cannot block the client. */ }
    }
    refresh();
  }
  function install(info) {
    if (!info) return;
    if(info.manifest)manifest=info.manifest;
    Object.assign(catalogs, info.catalogs); preference = info.preference || 'system'; locale = info.locale || 'en';
  }
  async function load(selected) {
    for (const id of new Set(['en', selected])) {
      if (catalogs[id]) continue;
      const response = await fetch(`/locales/${id}.json`);
      if (!response.ok) throw new Error('Language resource unavailable');
      catalogs[id] = await response.json();
    }
  }
  const ready = (async () => {
    try {
      if (globalThis.__TAURI__) install(await globalThis.__TAURI__.core.invoke('desktop_locale'));
      else {
        try { preference = JSON.parse(localStorage.getItem('clat.presentation.v1') || '{}').language || 'system'; } catch (_) {}
        locale = resolve(preference); await load(locale);
      }
    } catch (_) { locale = 'en'; }
  })();
  function selector(container) {
    const label = document.createElement('label'); label.setAttribute('data-i18n', 'common.language');
    const select = document.createElement('select'); select.setAttribute('data-language-select', '');
    attribute(select,'aria-label',()=>t('common.language',{},'Language'));
    for (const [value, title] of [['system', t('common.system',{},'Follow system')], ...manifest.languages.map(l=>[l.id,l.name])]) {
      const option = document.createElement('option'); option.value = value; if(value==='system')bind(option,'textContent',()=>t('common.system',{},'Follow system'));else option.textContent=title; select.append(option);
    }
    select.value = preference;
    select.onchange = () => setLanguage(select.value).catch(error => { select.value = preference; console.error(error); });
    container.append(label, select);
  }
  function hydrate(root) {
    for (const node of root.querySelectorAll('[data-language-control]')) {
      if (!node.querySelector('[data-language-select]')) selector(node);
    }
    for (const select of root.querySelectorAll('[data-language-select]')) select.value = preference;
    applyStatic(root);
  }
  globalThis.CLATI18n = Object.freeze({ t, msg, view, present, textNode, error, bind, attribute, ready, normalize, render, setLanguage, selector, hydrate,
    onChange: fn => { listeners.add(fn); return () => listeners.delete(fn); },
    get locale() { return locale; }, get preference() { return preference; }, refresh });
  const boot = () => ready.then(() => {
    hydrate(document);
    refresh();
  });
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, {once:true}); else boot();
})();
