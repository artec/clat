const fs=require('node:fs'),vm=require('node:vm');
function fixture(){
 let language='en',failCopy=false;const copies=[],timers=[];
 const el=(tag,cls,text)=>({tag,cls,text,children:[],dataset:{},attributes:{},disabled:false,
  classList:{contains:()=>false},append(...nodes){this.children.push(...nodes);},setAttribute(key,value){this.attributes[key]=value;},
  addEventListener(name,callback){this[name]=callback;},get textContent(){return typeof this.text==='function'?this.text():this.text||'';}});
 const context=vm.createContext({el,navigator:{clipboard:{writeText:async text=>{if(failCopy)throw Error('denied');copies.push(text);}}},setTimeout:fn=>timers.push(fn),
  toolDisplayName:name=>language+':tool:'+name,
  CLATI18n:{msg:key=>language+':'+key,present:(message,fallback='')=>message?language+':'+message.key:fallback,
   bind:(node,key,read)=>{if(key==='textContent')node.text=read;else node[key]=read();},attribute:(node,key,read)=>node.attributes[key]=read(),error:text=>new Error(text)},
 });
 vm.runInContext(fs.readFileSync('web/plugin-market.js','utf8').replace(/\r\n/g,'\n'),context);
 const source=fs.readFileSync('web/composer-ux.js','utf8').replace(/\r\n/g,'\n');
 for(const [name,async] of [['copyMessageText',true],['copyTextButton',false]]){
  const start=source.indexOf((async?'async ':'')+'function '+name+'(');
  vm.runInContext(source.slice(start,source.indexOf('\n}',start)+2),context);
 }
 return{context,copies,timers,language:next=>language=next,failCopy:()=>{failCopy=true;}};
}
const texts=node=>[node.textContent,...node.children.flatMap(child=>texts(child))].join('\n');
const walk=node=>[node,...node.children.flatMap(walk)];
const row=instance=>({id:'test.core',version:'1',runtime:'rust-builtin',scope:'trusted-project',name_presentation:{key:'test.name'},summary_presentation:{key:'test.summary'},instances:[{project:'project',phase:'active',...instance}]});
module.exports={fixture,texts,walk,row};
