/* Read-only comparisons: stable identity includes repository and both revision sources. */
function createGitTabs(ui,{seat,call,openFile,report}){
  let widget=null,visible=null,mode='auto',revision=0;
  const capture=()=>{if(visible&&widget)visible.position=widget.position();};
  function overview(){
    revision++;capture();widget?.destroy();widget=null;visible=null;ui.tabs.replaceChildren();ui.toolbar.replaceChildren();ui.mount.replaceChildren(navigationOverview({title:()=>devText('git_overview'),description:()=>devText(!seat().snapshot?'git_overview_unavailable':seat().snapshot.files.length?'git_overview_changes':'git_overview_clean'),icon:'branch',items:[]}));
    const snapshot=seat().snapshot;if(snapshot)ui.mount.querySelector('.navigation-overview').append(el('p','',()=>[ui.repoName.textContent,snapshot.branch||devText('detached'),devText('changed_count',{count:snapshot.files.length})].join(' · ')));
  }
  function headers(){
    ui.tabs.replaceChildren();for(const tab of seat().tabs.values()){
      const wrap=el('div','dev-file-tab'+(tab===seat().tab?' is-selected':'')),button=el('button','ghost',()=>tab.file.path.split('/').pop());button.type='button';button.onclick=()=>tab.stale?void open(tab.file,tab.fixed):show(tab);button.ondblclick=()=>{tab.fixed=true;headers();};wrap.classList.toggle('git-preview-tab',!tab.fixed);
      const close=navigationIcon(()=>devText('close_file'),'close',()=>{capture();seat().tabs.delete(tab.id);if(seat().tab===tab){seat().tab=[...seat().tabs.values()].at(-1)||null;if(seat().tab)show(seat().tab,false);else overview();}else headers();});wrap.append(button,close);ui.tabs.append(wrap);
    }
  }
  function show(tab,activate=true){
    revision++;capture();widget?.destroy();widget=null;visible=tab;seat().tab=tab;if(activate)ui.page.dataset.detail='true';headers();ui.page.dispatchEvent(new Event('clat:git-selection'));ui.toolbar.replaceChildren();
    const back=navigationIcon(()=>pageText('back_list'),'previous',()=>{delete ui.page.dataset.detail;});back.classList.add('navigation-back');ui.toolbar.append(back,el('span','dev-editor-path',tab.file.path));
    if(!tab.value){ui.mount.replaceChildren(pageEmpty('loading'));return;}
    const value=tab.value,label=x=>['HEAD','index','worktree','empty'].includes(x)?devText(x):x?.slice(0,8)||'';
    ui.toolbar.append(el('small','',()=>label(value.leftLabel)+' ↔ '+label(value.rightLabel)));
    if(!tab.file.commit)ui.toolbar.append(navigationIcon(()=>devText('open_file'),'open',()=>openFile(tab.file.path)));
    if(value.binary||value.unsupported){ui.mount.replaceChildren(el('p','settings-note',()=>devText(value.unsupported==='submodule'?'git_submodule_diff':value.unsupported==='conflicted'?'git_conflict_diff':value.unsupported==='unavailable'?'git_file_unavailable':'binary_diff')));return;}
    ui.toolbar.append(navigationIcon(()=>devText('previous_change'),'previous',()=>widget?.navigate(false)),navigationIcon(()=>devText('next_change'),'next',()=>widget?.navigate(true)));
    const choice=el('select');CLATI18n.attribute(choice,'aria-label',()=>devText('diff_mode'));for(const key of ['auto','split','unified']){const option=el('option','',()=>devText('diff_'+key));option.value=key;choice.append(option);}choice.value=mode;choice.onchange=()=>{mode=choice.value;widget?.setMode(mode);};ui.toolbar.append(choice);
    ui.mount.replaceChildren();widget=globalThis.CLATDevelopmentWidgets.diff(ui.mount,{...value,mode,phrases:devEditorPhrases()});const shown=widget;requestAnimationFrame(()=>{if(widget===shown&&tab.position)widget.restore(tab.position);});
  }
  async function open(file,fixed=false){
    const owner=seat(),id=JSON.stringify([owner.id,file.path,Boolean(file.staged),file.commit||'',file.parent||'']);
    let tab=owner.tabs.get(id);if(tab){tab.fixed||=fixed;if(!tab.stale){show(tab);return;}tab.value=null;}
    if(!tab){capture();for(const [key,prior]of owner.tabs)if(!prior.fixed)owner.tabs.delete(key);tab={id,file,fixed,value:null};owner.tabs.set(id,tab);}show(tab);const version=tab.version=devId();
    try{const value=await call('git.diff',file);if(tab.version!==version)return;tab.value=value;tab.stale=false;if(seat()===owner&&seat().tab===tab)show(tab);}catch(error){if(seat()===owner){report(error);if(owner.tab===tab)ui.mount.replaceChildren(el('p','error',error.message));}}
  }
  async function reload(){
    const owner=seat(),tab=owner.tab;for(const item of owner.tabs.values())if(!item.file.commit)item.stale=true;
    if(!tab||tab.file.commit){if(tab)show(tab,false);else overview();return;}
    const version=tab.version=devId();try{const value=await call('git.diff',tab.file);if(tab.version!==version)return;tab.value=value;tab.stale=false;if(seat()===owner&&owner.tab===tab)show(tab,false);}
    catch(error){if(seat()===owner&&owner.tab===tab&&tab.version===version){report(error);tab.value={unsupported:'unavailable'};show(tab,false);}}
  }

  return {open,overview,capture,reload,get revision(){return revision;},panel(node){revision++;capture();widget?.destroy();widget=null;visible=null;ui.tabs.replaceChildren();ui.toolbar.replaceChildren();ui.mount.replaceChildren(node);ui.page.dataset.detail='true';const back=navigationIcon(()=>pageText('back_list'),'previous',()=>{delete ui.page.dataset.detail;});back.classList.add('navigation-back');ui.toolbar.append(back);},refresh(){if(seat().tab)show(seat().tab,false);else overview();},leave(){capture();},localize(){widget?.setPhrases(devEditorPhrases());}};
}
