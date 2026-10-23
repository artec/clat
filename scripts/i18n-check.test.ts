import {expect,test} from 'bun:test';
import {readFileSync} from 'node:fs';
import {parseCatalog,validateCatalogs,validateManifest,validateReferences} from './i18n-check';
test('catalog check rejects duplicates, missing keys, empty text and parameter drift',()=>{
 expect(()=>parseCatalog('{"a":"one","a":"two"}')).toThrow('Duplicate');
 for(const zh of ([{},{a:''},{a:'{wrong}'},{a:'<script>bad</script>'},{a:'broken {'}] as Record<string,string>[])) expect(()=>validateCatalogs({en:{a:'{path}'},zh})).toThrow();
 validateCatalogs({en:{a:'File {path}'},zh:{a:'文件 {path}'}});
});
test('quantity forms and reference checks reject incomplete catalogs',()=>{
 const valid={one:'{count} image',other:'{count} images'};
 validateCatalogs({en:{a:valid},zh:{a:{one:'{count} 张图片',other:'{count} 张图片'}}});
 const invalidForms:Record<string,string>[]=[{other:'{count} 张图片'},{one:'一张图片',other:'{count} 张图片'},{one:'{count} 张图片',few:'{count} 张图片'}];
 for(const invalid of invalidForms){
   expect(()=>validateCatalogs({en:{a:valid},zh:{a:invalid}})).toThrow();
 }
 const known=new Set(['common.language']);
 validateReferences('UiMessage::new("common.language")',known);
 expect(()=>validateReferences(`el('div', CLATI18n.msg('common.language'), () => 'body')`,known)).toThrow('CSS classes');
 validateReferences(`el('div', 'stable-class', () => CLATI18n.msg('common.language'))`,known);
 for(const source of ['UiMessage::new("common.missing")','CLATI18n.msg("web.missing")','data-i18n-title="tui.missing"',`CLATI18n.msg('web.missing')`,`data-i18n-alt="web.missing"`])expect(()=>validateReferences(source,known)).toThrow('Unknown translation');
});
test('manifest cannot redirect catalog paths or assign one alias to two languages',()=>{
 const valid=JSON.parse(readFileSync(new URL('../locales/manifest.json',import.meta.url),'utf8'));
 validateManifest(valid);
 const conflict=structuredClone(valid);conflict.languages[1].aliases.push('en');
 const path=structuredClone(valid);path.languages[1].id='../private';
 const missing=structuredClone(valid);missing.languages.pop();
 for(const invalid of [conflict,path,missing])expect(()=>validateManifest(invalid)).toThrow();
});
