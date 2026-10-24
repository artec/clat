import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import ts from '../desktop/node_modules/typescript/lib/typescript.js';

export function parseCatalog(source: string): Record<string, string | Record<string, string>> {
  const tree = ts.parseJsonText('catalog.json', source);
  function visit(node: ts.Node) {
    if (ts.isObjectLiteralExpression(node)) {
      const keys = new Set<string>();
      for (const property of node.properties) {
        if (!ts.isPropertyAssignment(property)) continue;
        const key = (property.name as ts.StringLiteral).text;
        if (keys.has(key)) throw new Error('Duplicate translation key: ' + key);
        keys.add(key);
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(tree);
  return JSON.parse(source);
}
const parameters = (text: string) => {
  const names = [...text.matchAll(/\{([A-Za-z][A-Za-z0-9_]*)\}/g)].map(m => m[1]).sort();
  if (text.replace(/\{[A-Za-z][A-Za-z0-9_]*\}/g, '').match(/[{}]/)) throw new Error('Invalid template: ' + text);
  if (/<\/(?:[a-z][^>]*)>|<(?:script|style|iframe|img|svg|a|div|span|button|input|p|br)\b[^>]*>/i.test(text)) throw new Error('HTML is not allowed in translations');
  return [...new Set(names)].join(',');
};
export function validateCatalogs(catalogs: Record<string, Record<string, string | Record<string, string>>>): void {
  const english = catalogs.en;
  if (!english) throw new Error('English catalog missing');
  for (const [locale, catalog] of Object.entries(catalogs)) {
    if (Object.keys(catalog).sort().join('\n') !== Object.keys(english).sort().join('\n')) throw new Error('Translation keys differ: ' + locale);
    for (const [key, source] of Object.entries(english)) {
      const entry = catalog[key];
      if (source && typeof source === 'object' && Object.keys(source).sort().join()!=='one,other') throw new Error('Unsupported quantity forms: '+key);
      const forms = typeof source === 'string' ? {text:source} : source;
      const translated = typeof entry === 'string' ? {text:entry} : entry;
      if (!translated || Object.keys(forms).sort().join() !== Object.keys(translated).sort().join()) throw new Error('Invalid forms: ' + key);
      if (typeof source !== 'string' && parameters(forms.one!) !== parameters(forms.other!)) throw new Error('Quantity parameters differ: '+key);
      for (const [form, text] of Object.entries(forms)) {
        if (typeof translated[form] !== 'string' || !translated[form]!.trim()) throw new Error('Empty translation: ' + key);
        if (parameters(text) !== parameters(translated[form]!)) throw new Error('Translation parameters differ: ' + key);
      }
    }
  }
}
export function validateManifest(value: unknown): asserts value is {default:string;languages:{id:string;name:string;aliases:string[]}[]} {
  const manifest=value as {default:string;languages:{id:string;name:string;aliases:string[]}[]};
  if(manifest?.default!=='en'||!Array.isArray(manifest.languages))throw new Error('Invalid language manifest');
  const ids=new Set<string>(),aliases=new Map<string,string>();
  for(const language of manifest.languages){
    if(!['en','zh-Hans','zh-Hant'].includes(language.id)||ids.has(language.id)||!language.name?.trim()||!Array.isArray(language.aliases))throw new Error('Invalid language declaration');
    ids.add(language.id);
    for(const alias of [language.id,...language.aliases]){
      if(typeof alias!=='string'||!/^([a-z]{2})(-[a-z]{2,4})?$/i.test(alias))throw new Error('Invalid language alias');
      const previous=aliases.get(alias.toLowerCase());
      if(previous&&previous!==language.id)throw new Error('Conflicting language alias: '+alias);
      aliases.set(alias.toLowerCase(),language.id);
    }
  }
  if(ids.size!==3)throw new Error('Missing supported language');
}
export function validateReferences(source:string,known:Set<string>):void {
  if (/\bnew\s+Error\(\s*CLATI18n\.(?:msg|t|present)\(/.test(source)) throw new Error('Translated errors freeze their language; use CLATI18n.error instead');
  if (/\bel\([^,\n]+,\s*CLATI18n\.(?:msg|t)\(/.test(source)) throw new Error('CSS classes must be language independent');
  const patterns=[/UiMessage::new\(\s*["']([^"']+)["']/g, /(?:\.text|CLATI18n\.(?:msg|t))\(\s*["']((?:common|cli|tui|web|desktop|update|command|installer|context)\.[^"']+)["']/g, /data-i18n(?:-(?:title|placeholder|aria-label|alt))?=["']([^"']+)["']/g];
  for(const pattern of patterns)for(const match of source.matchAll(pattern)){
    if(!known.has(match[1]!))throw new Error('Unknown translation reference: '+match[1]);
  }
}
if (import.meta.main) {
  const root = resolve(import.meta.dir, '..');
  const manifest = JSON.parse(readFileSync(resolve(root, 'locales/manifest.json'), 'utf8'));
  validateManifest(manifest);
  const catalogs = Object.fromEntries(manifest.languages.map((l: {id:string}) => [l.id, parseCatalog(readFileSync(resolve(root, `locales/${l.id}.json`), 'utf8'))]));
  validateCatalogs(catalogs);
  const known=new Set(Object.keys(catalogs.en));
  for(const pattern of ['src/**/*.rs','web/*.{js,html}','desktop/src/**/*.rs','desktop/ui/*.{js,html}']){
    for(const file of new Bun.Glob(pattern).scanSync({cwd:root}))validateReferences(readFileSync(resolve(root,file),'utf8'),known);
  }
  console.log(`Language catalogs: ${Object.keys(catalogs).length} languages, ${Object.keys(catalogs.en).length} keys`);
}
