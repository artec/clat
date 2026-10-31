function installWorkModeMenu(trigger) {
  const popup=el('section','usage-popover work-mode-menu');popup.hidden=true;popup.id='work-mode-menu';
  popup.setAttribute('role','dialog');trigger.setAttribute('aria-haspopup','dialog');trigger.setAttribute('aria-controls',popup.id);
  CLATI18n.attribute(trigger,'aria-label',()=>workflowText('work_mode'));
  let controller;
  async function setMode(mode){
    controller.close();
    try{
      if(mode==='goal'){workflowController.create();return;}
      if(mode==='plan'&&state.runActive&&!confirm(workflowText('stop_and_plan_confirm')))return;
      await sendWorkflowIntent({action:'mode',mode});
    }catch(error){updateRunState(error.message);}
  }
  function render(){
    popup.replaceChildren(el('h3','',()=>workflowText('work_mode')));
    const value=workbenchShell.workflow;
    for(const [mode,key,icon]of [['normal','normal','chat'],['plan',state.runActive?'stop_and_plan':'planning','plan'],['goal','goal_mode','check']]){
      const row=shellButton('',icon,()=>workflowText(key),()=>{if(mode==='goal'&&value?.goal?.goal){controller.close();void workflowController.goal();}else void setMode(mode);},false);
      if(mode==='plan'||value?.plan_mode)disableBuiltin(row,value,'builtin.plan_mode');
      row.setAttribute('aria-pressed',String(mode===(value?.plan_mode?'plan':value?.goal?.armed?'goal':'normal')));popup.append(row);
    }
    if(retainedPlan(value))popup.append(el('p','picker-hint',()=>CLATI18n.msg('extension.retained_plan')));
    popup.append(el('p','picker-hint',()=>workflowText(workflowGoalStatus(value||{}))),shellButton('work-mode-details','tools',()=>workflowText('open_workflow'),()=>{controller.close();workflowController.open();},false));
  }
  controller=attachComposerPopup(trigger,trigger,popup,render,'left');trigger.addEventListener('click',()=>controller.toggle());
  return controller;
}
