/* One device-owned font/loading authority shared by every project and preview. */
const DEV_TERMINAL_DEFAULTS={fontMode:'system',fontFamily:'',fontFace:'',fontFaceName:'',fontSize:14,fontWeight:400,lineHeight:1.15,letterSpacing:0,cursorStyle:'block',cursorBlink:false,scrollback:10000,version:3};
function readDevTerminalSettings(){
  let stored={};try{stored=JSON.parse(localStorage.getItem('clat.terminal.appearance')||'{}')||{};}catch(_){}
  const value={...DEV_TERMINAL_DEFAULTS,...stored};
  if(![2,3].includes(stored.version)){value.fontMode=stored.fontFamily&&stored.fontFamily!=='CLAT JetBrains Mono'?'custom':'system';}
  if(!['system','custom'].includes(value.fontMode))value.fontMode='system';
  for(const [key,min,max]of [['fontSize',8,40],['fontWeight',100,900],['lineHeight',1,2],['letterSpacing',-1,5],['scrollback',0,100000]])value[key]=Number.isFinite(Number(value[key]))?Math.max(min,Math.min(max,Number(value[key]))):DEV_TERMINAL_DEFAULTS[key];
  for(const key of ['fontFamily','fontFace','fontFaceName'])if(typeof value[key]!=='string')value[key]='';
  if(!['block','bar','underline'].includes(value.cursorStyle))value.cursorStyle='block';value.cursorBlink=value.cursorBlink===true;value.version=3;return value;
}
function devSystemTerminalFont(){const platform=globalThis.__CLAT_NATIVE_FRAME__||navigator.platform;return /mac|iphone|ipad/i.test(platform)?'Menlo':/win/i.test(platform)?'Consolas':'monospace';}
function createTerminalAppearance(){
  const clients=new Set(),fonts=new Map();let epoch=0,current=null,initializing=null;
  async function font(name,fullName){
    if(name==='monospace')return name;
    if(!name.trim()||name.length>200||/[\n\r,;{}]/.test(name))throw new Error(devText('font_unavailable',{font:name}));
    if(!fonts.has(name))fonts.set(name,(async()=>{const alias='CLAT Local '+fonts.size+' '+Date.now();const face=new FontFace(alias,'local('+JSON.stringify(name)+')'+(fullName?', local('+JSON.stringify(fullName)+')':''));await face.load();globalThis.document.fonts.add(face);return alias;})().catch(error=>{fonts.delete(name);throw error;}));
    return fonts.get(name);
  }
  async function resolve(settings){
    const requested=settings.fontMode==='system'?devSystemTerminalFont():(settings.fontFace||settings.fontFamily).trim();
    let family,notice='';
    try{family=await font(requested,settings.fontMode==='custom'?settings.fontFaceName:null);}catch(error){if(settings.fontMode==='custom')throw new Error(devText('font_unavailable',{font:requested}));family='monospace';notice=()=>devText('font_fallback',{font:requested});}
    return {settings,family,effective:settings.fontMode==='custom'&&settings.fontFaceName?settings.fontFaceName:family==='monospace'?family:requested,notice};
  }
  function options(){
    const value=current;const style=globalThis.getComputedStyle(globalThis.document.documentElement);
    const background=style.getPropertyValue('--surface').trim(),foreground=style.getPropertyValue('--text').trim();
    const {fontMode,fontFamily,fontFace,fontFaceName,version,...appearance}=value?.settings||readDevTerminalSettings();
    return {...appearance,fontWeightBold:700,...(fontMode==='custom'&&fontFace?{fontWeight:400,fontWeightBold:400}:{}),fontFamily:value?.family&&value.family!=='monospace'?JSON.stringify(value.family)+', monospace':'monospace',cursorInactiveStyle:'outline',theme:{background,foreground,cursor:foreground,cursorAccent:background,selectionBackground:'#0c416f88'}};
  }
  async function apply(settings=readDevTerminalSettings(),persist=false){
    const request=++epoch,value=await resolve(settings);if(request!==epoch)return current;
    if(persist)localStorage.setItem('clat.terminal.appearance',JSON.stringify(settings));current=value;
    await Promise.all([...clients].map(client=>client(options(),value)));return value;
  }
  async function ready(){
    if(!current){initializing ||= apply().catch(async error=>{const fallback=await apply({...readDevTerminalSettings(),fontMode:'system',fontFace:'',fontFaceName:''});if(fallback)fallback.notice=()=>devText('font_fallback',{font:readDevTerminalSettings().fontFamily||devSystemTerminalFont()});return fallback;}).finally(()=>{initializing=null;});await initializing;if(!current)return ready();}
    return current;
  }
  return {ready,apply,options,refresh:async()=>{if(current)await Promise.all([...clients].map(client=>client(options(),current)));},get current(){return current;},subscribe(client){clients.add(client);if(current)queueMicrotask(()=>{if(clients.has(client))void Promise.resolve(client(options(),current)).catch(()=>{});});return()=>clients.delete(client);}};
}
const terminalAppearance=globalThis.CLATTerminalAppearance||(globalThis.CLATTerminalAppearance=createTerminalAppearance());
function devTerminalOptions(){return terminalAppearance.options();}
async function devLoadTerminalFonts(){return terminalAppearance.ready();}
function saveDevTerminalSettings(value){return terminalAppearance.apply({...value,version:3},true);}
if(!globalThis.CLATDevelopmentThemeObserver){
  const changed=()=>void terminalAppearance.refresh().catch(()=>{}),observer=new MutationObserver(changed);
  observer.observe(globalThis.document.documentElement,{attributes:true,attributeFilter:['data-theme']});globalThis.matchMedia('(prefers-color-scheme: light)').addEventListener('change',changed);globalThis.CLATDevelopmentThemeObserver=observer;
}
