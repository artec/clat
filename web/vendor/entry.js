import {EditorView,basicSetup} from 'codemirror';
import {EditorState,EditorSelection,Compartment} from '@codemirror/state';
import {keymap} from '@codemirror/view';
import {indentWithTab} from '@codemirror/commands';
import {openSearchPanel} from '@codemirror/search';
import {indentUnit} from '@codemirror/language';
import {formatFor,formatList,support} from './languages.js';
import {theme,highlighting,inferIndent,indentGuides} from './editor-theme.js';
import {MergeView,unifiedMergeView,goToNextChunk,goToPreviousChunk} from '@codemirror/merge';
import {Terminal} from '@xterm/xterm';
import {FitAddon} from '@xterm/addon-fit';
import {SearchAddon} from '@xterm/addon-search';
import {Unicode11Addon} from '@xterm/addon-unicode11';
import '@xterm/xterm/css/xterm.css';
const phraseCompartment=new Compartment();
const accessCompartment=new Compartment(),languageCompartment=new Compartment();
function language(path,text=''){return support(formatFor(path,text).id);}
function editor(parent,{text='',path='',readonly=false,state,phrases={},mode,onChange=()=>{},onSave=()=>{},onSelection=()=>{}}={}){
  const access=value=>[EditorState.readOnly.of(value),EditorView.editable.of(!value)];
  const indent=inferIndent(text,path);
  const extensions=[basicSetup,theme,highlighting,indentGuides,indentUnit.of(indent),EditorState.tabSize.of(indent==='\t'?4:indent.length),languageCompartment.of(mode?support(mode):language(path,text)),phraseCompartment.of(EditorState.phrases.of(phrases)),EditorView.contentAttributes.of({'aria-label':path}),keymap.of([{key:'Mod-s',run:()=>{onSave();return true;}},indentWithTab]),accessCompartment.of(access(readonly)),EditorView.updateListener.of(update=>{if(update.docChanged)onChange(update.state.doc.toString());if(update.docChanged||update.selectionSet)onSelection();})];
  const view=new EditorView({state:state||EditorState.create({doc:text,extensions}),parent});
  view.contentDOM.setAttribute('aria-label',path);
  view.dispatch({effects:[phraseCompartment.reconfigure(EditorState.phrases.of(phrases)),accessCompartment.reconfigure(access(readonly)),languageCompartment.reconfigure(mode?support(mode):language(path,text))]});
  return {view,indent:()=>view.state.facet(indentUnit),setLanguage:id=>view.dispatch({effects:languageCompartment.reconfigure(support(id))}),search:()=>openSearchPanel(view),setPhrases:phrases=>view.dispatch({effects:phraseCompartment.reconfigure(EditorState.phrases.of(phrases))}),text:()=>view.state.doc.toString(),state:()=>view.state,focus:()=>view.focus(),destroy:()=>view.destroy(),setReadonly:value=>view.dispatch({effects:accessCompartment.reconfigure(access(value))}),replace:value=>view.dispatch({changes:{from:0,to:view.state.doc.length,insert:value}})};
}
function diff(parent,{left,right,path='',phrases={},mode='auto'}){
  let current,compact=null;const position=()=>{const views=current?.a?[current.a,current.b]:current?[current]:[];return views.map(view=>({top:view.scrollDOM.scrollTop,left:view.scrollDOM.scrollLeft,selection:view.state.selection.toJSON()}));};
  const restore=values=>{const views=current?.a?[current.a,current.b]:current?[current]:[];views.forEach((view,i)=>{const value=values[i]||values[0];if(value){view.scrollDOM.scrollTop=value.top;view.scrollDOM.scrollLeft=value.left;try{view.dispatch({selection:EditorSelection.fromJSON(value.selection)});}catch(_){}}});};
  const render=()=>{const next=mode==='unified'||mode==='auto'&&parent.clientWidth<900;if(next===compact&&current)return;const saved=position();compact=next;current?.destroy();parent.replaceChildren();
    const extensions=[basicSetup,theme,highlighting,language(path,right||left),phraseCompartment.of(EditorState.phrases.of(phrases)),EditorState.readOnly.of(true),EditorView.editable.of(false)];
    if(compact)current=new EditorView({doc:right,extensions:[...extensions,unifiedMergeView({original:left,mergeControls:false,collapseUnchanged:{margin:4,minSize:12}})],parent});
    else{current=new MergeView({a:{doc:left,extensions},b:{doc:right,extensions},parent,collapseUnchanged:{margin:4,minSize:12},diffConfig:{scanLimit:2000}});let syncing=false;for(const [a,b]of [[current.a,current.b],[current.b,current.a]])a.scrollDOM.addEventListener('scroll',()=>{if(syncing)return;syncing=true;b.scrollDOM.scrollTop=a.scrollDOM.scrollTop;requestAnimationFrame(()=>syncing=false);});}
    requestAnimationFrame(()=>restore(saved));
  };render();const observer=new ResizeObserver(render);observer.observe(parent);
  return {position,restore,setMode(value){mode=value;compact=null;render();},navigate(next){const view=current.b||current;(next?goToNextChunk:goToPreviousChunk)(view);view.focus();},setPhrases(value){phrases=value;for(const view of current.a?[current.a,current.b]:[current])view.dispatch({effects:phraseCompartment.reconfigure(EditorState.phrases.of(value))});},destroy(){observer.disconnect();current?.destroy();}};
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
globalThis.CLATDevelopmentWidgets={editor,diff,terminal,formatFor,formats:formatList};
