function installDevelopmentSettings(){
  const nav=document.querySelector('.settings-nav'),host=document.querySelector('.settings-content');
  function section(key){const button=devButton(key,()=>selectSettingsCategory(key));button.dataset.settingsCategory=key;nav.append(button);const node=el('section','settings-section');node.dataset.settingsPanel=key;node.hidden=true;node.append(el('p','page-scope',()=>settingsScope(key)),el('h2','',()=>devText(key)));host.append(node);return node;}
  const terminal=section('terminal'),git=section('git'),values=readDevTerminalSettings(),inputs=new Map();
  const preview=el('div','dev-terminal-preview');let widget,appearanceSignature=null;
  const fonts=settingsGroup(terminal,()=>settingsUi('typography')),cursor=settingsGroup(terminal,()=>settingsUi('cursor')),history=settingsGroup(terminal,()=>settingsUi('scrollback'));
  const groups={fontSize:fonts,fontWeight:fonts,lineHeight:fonts,letterSpacing:fonts,cursorStyle:cursor,cursorBlink:cursor,scrollback:history};
  for(const [key,type,range]of [['fontSize','number',[8,40,.5]],['fontWeight','number',[100,900,100]],['lineHeight','number',[1,2,.05]],['letterSpacing','number',[-1,5,.1]],['cursorStyle','select'],['cursorBlink','checkbox'],['scrollback','number',[0,100000,1000]]]){
    const input=el(type==='select'?'select':'input','');
    const label=settingsControlRow(()=>devText(key),input);label.classList.add('dev-setting');
    if(type==='select'){for(const value of ['block','bar','underline']){const option=el('option','',()=>devText('cursor_'+value));option.value=value;input.append(option);}}else input.type=type;
    if(range){[input.min,input.max,input.step]=range;}if(type==='checkbox')input.checked=values[key];else input.value=values[key];
    input.oninput=()=>{if(type==='number'&&!input.checkValidity())return;values[key]=type==='checkbox'?input.checked:type==='number'?Number(input.value):input.value;void saveDevTerminalSettings(values).catch(error=>devReport(fontStatus,error));};inputs.set(key,input);if(type==='checkbox')input.classList.add('settings-switch');groups[key].append(label);
  }
  const fontStatus=el('p','picker-hint'),fontControls=installTerminalFontSettings(fonts,values,fontStatus);
  const actions=el('div','settings-action-bar');terminal.append(actions);actions.append(devButton('reset_defaults',()=>{Object.assign(values,DEV_TERMINAL_DEFAULTS);for(const [key,input]of inputs){if(input.type==='checkbox')input.checked=values[key];else input.value=values[key];}void saveDevTerminalSettings(values).then(()=>syncFonts()).catch(error=>devReport(fontStatus,error));}));
  fonts.after(fontStatus);const previewGroup=el('section','settings-group-block');previewGroup.append(el('h4','',()=>settingsUi('preview')),preview);terminal.append(previewGroup);
  installTerminalLicenses(terminal);
  const language=devLanguageSelect('system'),label=settingsControlRow(()=>devText('commit_language'),language);label.classList.add('settings-card');git.append(label);
  language.onchange=async()=>{try{await rpc('git.settings.set',{language:language.value});developmentGit?.refresh();}catch(error){git.append(el('p','error',error.message));}};
  dom['settings-dialog'].addEventListener('clat:page-enter',()=>{void rpc('git.settings',{}).then(value=>{language.value=value.language;}).catch(()=>{});});
  const sizePreview=()=>{if(!widget)return;const screen=preview.querySelector('.xterm-screen');if(screen&&widget.term.rows)preview.style.setProperty('--terminal-preview-height',Math.ceil(screen.offsetHeight/widget.term.rows*6+26)+'px');};
  const renderPreview=()=>{if(!preview.clientHeight||!preview.clientWidth)return;if(!widget&&terminalAppearance.current){widget=globalThis.CLATDevelopmentWidgets.terminal(preview,devTerminalOptions());widget.term.options.disableStdin=true;widget.term.write('CLAT · abc 0123 · 中文\r\n\x1b[1mBold 粗体\x1b[0m  \x1b[3mItalic\x1b[0m\r\n\ue0b0 \ue0b2 \uf126 \uf120 \uf09b  ✓ →  😀\r\n┌──────┬──────┐\r\n│ Nerd │ Font │\r\n└──────┴──────┘',sizePreview);}widget?.fit.fit();};
  new ResizeObserver(renderPreview).observe(preview);
  terminalAppearance.subscribe((options,value)=>{const signature=JSON.stringify(value.settings),changed=signature!==appearanceSignature;appearanceSignature=signature;Object.assign(values,value.settings);for(const [key,input]of inputs){if(input.type==='checkbox')input.checked=values[key];else input.value=values[key];}if(changed)syncFonts();renderPreview();if(widget){Object.assign(widget.term.options,options);widget.fit.fit();sizePreview();widget.term.refresh(0,widget.term.rows-1);}CLATI18n.bind(fontStatus,'textContent',()=>devText('effective_font',{font:value.effective})+(value.notice?' · '+value.notice():''));});
  void devLoadTerminalFonts().then(renderPreview).catch(error=>devReport(fontStatus,error));
  function syncFonts(){fontControls.sync();const weight=inputs.get('fontWeight');weight.closest('label').hidden=values.fontMode==='custom'&&Boolean(values.fontFace);}

}

function installTerminalFontSettings(host,values,status){
  const mode=el('select'),custom=el('input');mode.dataset.fontMode='true';custom.dataset.fontCustom='true';custom.type='text';custom.value=values.fontFamily;custom.hidden=values.fontMode!=='custom';
  for(const [value,key]of [['system','font_system'],['custom','font_custom']]){const option=el('option','',()=>devText(key,{font:devSystemTerminalFont()}));option.value=value;mode.append(option);}mode.value=values.fontMode;
  const row=el('div','settings-row dev-setting dev-font-setting'),controls=el('div','dev-font-controls');controls.append(mode,custom);row.append(el('strong','settings-row-copy',()=>devText('fontFamily')),controls);host.insertBefore(row,host.querySelector('.dev-setting'));CLATI18n.attribute(mode,'aria-label',()=>devText('fontFamily'));CLATI18n.attribute(custom,'aria-label',()=>devText('font_custom'));
  let request=0;
  async function apply(){const generation=++request,candidate={...values,fontMode:mode.value,fontFamily:custom.value,...(custom.value!==values.fontFamily?{fontFace:'',fontFaceName:''}:{})};custom.hidden=mode.value!=='custom';if(mode.value==='custom'&&!custom.value.trim())return;
    try{const result=await saveDevTerminalSettings(candidate);if(generation!==request||!result)return;Object.assign(values,result.settings);status.classList.remove('error');}
    catch(error){if(generation===request)devReport(status,error);}
  }
  const library=installTerminalFontLibrary(controls,custom,values,status,apply);
  mode.onchange=()=>{library.sync();void apply();};custom.onblur=()=>void apply();custom.onkeydown=event=>{if(event.key==='Enter'){event.preventDefault();void apply();}};
  return {sync(){mode.value=values.fontMode;custom.value=values.fontFamily;custom.hidden=values.fontMode!=='custom';library.sync();}};
}
function installTerminalLicenses(host){
  const details=el('details','dev-licenses'),summary=el('summary','',()=>devText('licenses')),body=el('div',''),text=el('pre',''),status=el('p','picker-hint');let loaded=false,controller;
  const load=async()=>{controller?.abort();controller=new AbortController();try{const response=await fetch('/development-licenses.txt',{signal:controller.signal});if(!response.ok)throw new Error(devText('failed'));text.textContent=await response.text();loaded=true;status.textContent='';}catch(error){if(error.name!=='AbortError')devReport(status,error);}};
  body.append(devButton('copy',()=>void navigator.clipboard.writeText(text.textContent).catch(error=>devReport(status,error))),devButton('refresh',()=>void load()),status,text);details.append(summary,body);host.append(details);
  details.ontoggle=()=>{if(details.open&&!loaded)void load();};dom['settings-dialog'].addEventListener('clat:page-leave',()=>{controller?.abort();details.open=false;});
}

function installTerminalFontLibrary(controls,custom,values,status,apply){
  const faces=el('select'),names=el('datalist'),id='terminal-fonts-'+devId();names.id=id;custom.setAttribute('list',id);controls.append(faces,names);
  CLATI18n.attribute(faces,'aria-label',()=>devText('font_face'));CLATI18n.attribute(custom,'placeholder',()=>devText('font_search'));
  let catalog=[],loading=false,loaded=false;
  function sync(){
    const family=custom.value,rows=catalog.filter(row=>row.family===family);faces.replaceChildren();
    const automatic=el('option','',()=>devText('font_face_auto'));automatic.value='';faces.append(automatic);
    for(const row of rows){const option=el('option','',row.fullName);option.value=row.postscript;faces.append(option);}
    faces.value=values.fontFamily===family?values.fontFace||'':'';const customMode=controls.querySelector('[data-font-mode]').value==='custom';faces.hidden=!customMode||(!rows.length&&!loading);faces.disabled=loading;if(loading){automatic.textContent=devText('font_list_loading');}if(customMode&&!loaded&&!loading)load();
  }
  custom.addEventListener('input',sync);
  custom.addEventListener('change',()=>{sync();void apply();});
  faces.onchange=()=>{const row=catalog.find(row=>row.postscript===faces.value);void saveDevTerminalSettings({...values,fontMode:'custom',fontFamily:custom.value,fontFace:row?.postscript||'',fontFaceName:row?.fullName||''}).catch(error=>devReport(status,error));};
  function load(){
    if(!globalThis.__TAURI__){loaded=true;return;}loading=true;sync();
    globalThis.CLATFontCatalog ||= globalThis.__TAURI__.core.invoke('desktop_fonts').catch(error=>{globalThis.CLATFontCatalog=null;throw error;});
    void globalThis.CLATFontCatalog.then(rows=>{catalog=rows;loaded=true;for(const family of [...new Set(rows.map(row=>row.family))]){const option=el('option');option.value=family;names.append(option);}}).catch(error=>{loaded=true;devReport(status,error);}).finally(()=>{loading=false;sync();});
  }
  sync();return {sync};
}
