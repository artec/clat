import {EditorView,ViewPlugin,Decoration} from '@codemirror/view';
import {HighlightStyle,syntaxHighlighting} from '@codemirror/language';
import {tags} from '@lezer/highlight';
export const highlighting=syntaxHighlighting(HighlightStyle.define([
  {tag:[tags.keyword,tags.modifier,tags.controlKeyword],color:'var(--dev-token-keyword)'},
  {tag:[tags.string,tags.special(tags.string),tags.regexp],color:'var(--dev-token-string)'},
  {tag:tags.comment,color:'var(--dev-token-comment)'},
  {tag:[tags.number,tags.bool,tags.null],color:'var(--dev-token-number)'},
  {tag:[tags.function(tags.variableName),tags.function(tags.propertyName)],color:'var(--dev-token-function)'},
  {tag:[tags.typeName,tags.className,tags.namespace],color:'var(--dev-token-type)'},
  {tag:[tags.propertyName,tags.attributeName],color:'var(--dev-token-property)'},
  {tag:[tags.operator,tags.punctuation],color:'var(--text-soft)'},
  {tag:tags.heading,fontWeight:'600',color:'var(--accent)'},
  {tag:tags.link,textDecoration:'underline',color:'var(--accent)'},
]));
export const theme=EditorView.theme({
  '&':{height:'100%',color:'var(--text)',backgroundColor:'var(--surface)',fontSize:'13px'},
  '.cm-scroller':{fontFamily:'var(--font-mono)',lineHeight:'1.6',overflow:'auto'},
  '.cm-content':{padding:'6px 0'},'.cm-line':{padding:'0 12px'},
  '.cm-gutters':{backgroundColor:'var(--surface)',color:'var(--text-muted)',border:'none',minWidth:'46px'},
  '.cm-lineNumbers .cm-gutterElement':{padding:'0 10px 0 8px'},
  '.cm-activeLine,.cm-activeLineGutter':{backgroundColor:'color-mix(in srgb,var(--text) 5%,transparent)'},
  '.cm-cursor':{borderLeftColor:'var(--text)'},
  '.cm-selectionBackground':{backgroundColor:'color-mix(in srgb,var(--accent) 28%,transparent) !important'},
  '.cm-panels,.cm-tooltip':{backgroundColor:'var(--surface)',color:'var(--text)',borderColor:'var(--line)'},
  '.cm-indent-guide':{backgroundImage:'repeating-linear-gradient(to right, var(--line) 0 1px, transparent 1px var(--dev-indent-step))',backgroundPosition:'12px 0',backgroundSize:'var(--dev-indent-end) 100%',backgroundRepeat:'no-repeat'},
});
export function inferIndent(text,path){
  if(/(?:^|\/)(?:GNUmakefile|[Mm]akefile)$|\.(?:mk|mak)$/.test(path))return '\t';
  const samples=text.split('\n').filter(line=>/^\s+\S/.test(line)).slice(0,300);
  if(samples.filter(line=>line.startsWith('\t')).length>samples.length/2)return '\t';
  const counts=samples.map(line=>line.match(/^ +/)?.[0].length||0).filter(n=>n>0);
  const width=counts.some(n=>n%2)?1:counts.some(n=>n%4)?2:4;return ' '.repeat(width);
}
export const indentGuides=ViewPlugin.fromClass(class{
  constructor(view){this.decorations=this.build(view);}
  update(update){if(update.docChanged||update.viewportChanged)this.decorations=this.build(update.view);}
  build(view){const lines=[];for(const {from,to}of view.visibleRanges){let line=view.state.doc.lineAt(from);while(line.from<=to){const whitespace=line.text.match(/^[ \t]+/)?.[0];if(whitespace){let columns=0;for(const char of whitespace)columns+=char==='\t'?view.state.tabSize-columns%view.state.tabSize:1;lines.push(Decoration.line({attributes:{class:'cm-indent-guide',style:`--dev-indent-end:${columns}ch;--dev-indent-step:${view.state.tabSize}ch`}}).range(line.from));}if(line.to>=view.state.doc.length)break;line=view.state.doc.line(line.number+1);}}return Decoration.set(lines,true);}
},{decorations:value=>value.decorations});
