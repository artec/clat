import {EditorView,basicSetup} from 'codemirror';
import {EditorState,Compartment} from '@codemirror/state';
import {keymap} from '@codemirror/view';
import {indentWithTab} from '@codemirror/commands';
import {openSearchPanel} from '@codemirror/search';
import {javascript} from '@codemirror/lang-javascript';
import {rust} from '@codemirror/lang-rust';
import {json} from '@codemirror/lang-json';
import {html} from '@codemirror/lang-html';
import {css} from '@codemirror/lang-css';
import {markdown} from '@codemirror/lang-markdown';
import {python} from '@codemirror/lang-python';
import {yaml} from '@codemirror/lang-yaml';
import {StreamLanguage,HighlightStyle,syntaxHighlighting} from '@codemirror/language';
import {tags} from '@lezer/highlight';
import {shell} from '@codemirror/legacy-modes/mode/shell';
import {toml} from '@codemirror/legacy-modes/mode/toml';
import {MergeView,unifiedMergeView} from '@codemirror/merge';
import {Terminal} from '@xterm/xterm';
import {FitAddon} from '@xterm/addon-fit';
import {SearchAddon} from '@xterm/addon-search';
import {Unicode11Addon} from '@xterm/addon-unicode11';
import '@xterm/xterm/css/xterm.css';
const phraseCompartment=new Compartment();
const accessCompartment=new Compartment(),languageCompartment=new Compartment();
const highlighting=syntaxHighlighting(HighlightStyle.define([
  {tag:tags.keyword,color:'var(--dev-token-keyword)'},{tag:[tags.string,tags.special(tags.string)],color:'var(--dev-token-string)'},
  {tag:tags.comment,color:'var(--dev-token-comment)'},{tag:[tags.number,tags.bool],color:'var(--dev-token-number)'},
  {tag:tags.function(tags.variableName),color:'var(--dev-token-function)'},{tag:tags.typeName,color:'var(--dev-token-type)'},
]));
const theme=EditorView.theme({
  '&':{height:'100%',color:'var(--text)',backgroundColor:'var(--surface)',fontSize:'13px'},
  '.cm-scroller':{fontFamily:'var(--font-mono)',lineHeight:'1.55',overflow:'auto'},
  '.cm-gutters':{backgroundColor:'var(--surface)',color:'var(--text-muted)',border:'none'},
  '.cm-activeLine,.cm-activeLineGutter':{backgroundColor:'color-mix(in srgb,var(--text) 5%,transparent)'},
  '.cm-cursor':{borderLeftColor:'var(--text)'},
  '.cm-selectionBackground':{backgroundColor:'color-mix(in srgb,var(--accent) 35%,transparent) !important'},
  '.cm-panels,.cm-tooltip':{backgroundColor:'var(--surface)',color:'var(--text)',borderColor:'var(--line)'},
});
function language(path){
  const ext=path.split('.').pop().toLowerCase();
  if(['js','jsx','mjs','cjs','ts','tsx'].includes(ext))return javascript({typescript:['ts','tsx'].includes(ext),jsx:['tsx','jsx'].includes(ext)});
  const modes={rs:rust,json,html,htm:html,css,md:markdown,py:python,yaml,yml:yaml,sh:()=>StreamLanguage.define(shell),zsh:()=>StreamLanguage.define(shell),toml:()=>StreamLanguage.define(toml)};
  return modes[ext]?.()||[];
}
function editor(parent,{text='',path='',readonly=false,state,phrases={},onChange=()=>{},onSave=()=>{}}={}){
  const access=value=>[EditorState.readOnly.of(value),EditorView.editable.of(!value)];
  const extensions=[basicSetup,theme,highlighting,languageCompartment.of(language(path)),phraseCompartment.of(EditorState.phrases.of(phrases)),EditorView.contentAttributes.of({'aria-label':path}),keymap.of([{key:'Mod-s',run:()=>{onSave();return true;}},indentWithTab]),accessCompartment.of(access(readonly)),EditorView.updateListener.of(update=>{if(update.docChanged)onChange(update.state.doc.toString());})];
  const view=new EditorView({state:state||EditorState.create({doc:text,extensions}),parent});
  view.dispatch({effects:[phraseCompartment.reconfigure(EditorState.phrases.of(phrases)),accessCompartment.reconfigure(access(readonly)),languageCompartment.reconfigure(language(path))]});
  return {view,search:()=>openSearchPanel(view),setPhrases:phrases=>view.dispatch({effects:phraseCompartment.reconfigure(EditorState.phrases.of(phrases))}),text:()=>view.state.doc.toString(),state:()=>view.state,focus:()=>view.focus(),destroy:()=>view.destroy(),setReadonly:value=>view.dispatch({effects:accessCompartment.reconfigure(access(value))}),replace:value=>view.dispatch({changes:{from:0,to:view.state.doc.length,insert:value}})};
}
function diff(parent,{left,right,path='',phrases={}}){
  let current;const compact=matchMedia('(max-width: 900px)');
  const render=()=>{current?.destroy();parent.replaceChildren();
    const extensions=[basicSetup,theme,highlighting,language(path),phraseCompartment.of(EditorState.phrases.of(phrases)),EditorState.readOnly.of(true),EditorView.editable.of(false)];
    if(compact.matches){current=new EditorView({doc:right,extensions:[...extensions,unifiedMergeView({original:left,mergeControls:false})],parent});}
    else{current=new MergeView({a:{doc:left,extensions},b:{doc:right,extensions},parent,collapseUnchanged:{margin:4,minSize:12},diffConfig:{scanLimit:2000}});}
  };render();compact.addEventListener('change',render);
  return {setPhrases(value){phrases=value;for(const view of current.a?[current.a,current.b]:[current])view.dispatch({effects:phraseCompartment.reconfigure(EditorState.phrases.of(value))});},destroy(){compact.removeEventListener('change',render);current?.destroy();}};
}
function balancedTerminalFit(term,fit){
  const balance=()=>{const screen=term.element.querySelector('.xterm-screen');const spare=Math.max(0,parentWidth()-screen.offsetWidth);term.element.style.paddingLeft=(spare/2)+'px';};
  const parentWidth=()=>term.element.parentElement.clientWidth;
  return {fit(){term.element.style.paddingLeft='0px';fit.fit();balance();},proposeDimensions(){term.element.style.paddingLeft='0px';const size=fit.proposeDimensions();balance();return size;}};
}
function terminal(parent,settings={}){
  // The pinned Unicode11 addon requires xterm's proposed Unicode provider API.
  const term=new Terminal({fontFamily:'monospace',fontSize:14,lineHeight:1.15,cursorStyle:'block',cursorBlink:false,scrollback:10000,...settings,allowProposedApi:true});
  const fit=new FitAddon(),search=new SearchAddon(),unicode=new Unicode11Addon();term.loadAddon(fit);term.loadAddon(search);term.loadAddon(unicode);term.unicode.activeVersion='11';term.parser.registerOscHandler(52,()=>true);term.open(parent);
  return {term,fit:balancedTerminalFit(term,fit),search,destroy:()=>term.dispose()};
}
globalThis.CLATDevelopmentWidgets={editor,diff,terminal};
