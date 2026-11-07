/* A compact link to the core-owned, versioned document and its shared review. */
function decoratePlanToolCard(card, name, argsText) {
  if (name !== 'exit_plan_mode' || card.classList.contains('plan-artifact')) return false;
  let args; try { args=JSON.parse(argsText); } catch (_) { return false; }
  if(typeof args?.plan!=='string'||!args.plan.trim())return false;
  card.classList.add('plan-artifact');card.open=true;
  const body=card.querySelector('.tool-body');
  const parameters=el('details','plan-parameters');parameters.append(el('summary','',()=>shellText('tool_parameters')));
  const raw=body.querySelector('.tool-args');if(raw)parameters.append(raw);
  const open=el('button','ghost',()=>workflowText('view_plan'));open.type='button';open.onclick=()=>void workflowController.plan(args.plan);
  body.prepend(el('p','plan-excerpt',()=>args.plan.slice(0,240)),open,parameters);
  const label=card.querySelector('summary span');CLATI18n.bind(label,'textContent',()=>shellText('proposed_plan'));
  return true;
}

function createPlanReviewCard(payload) {
  const doc=payload.plan_review,card=el('section','question-card plan-review-card');card.dataset.rpcId=payload.rpc_id;
  const actions=el('div','actions'),note=el('p','note');note.setAttribute('role','status');
  card.append(el('div','eyebrow',()=>workflowText('plans')),el('h3','',()=>doc.title),el('p','plan-excerpt',()=>doc.text.slice(0,200)),actions,note);
  const owner=[state.sessionId,state.selectionGeneration,state.stream];
  let execution=planExecution(doc),sending=false;
  for(const [key,intent]of [['approve_execute',null],['approve_only',{action:'approve',plan:doc.id,revision:doc.revision,execute:false}],['revise',{action:'revise',plan:doc.id,revision:doc.revision}]]){
    const button=el('button',key==='approve_execute'?'primary':'ghost',()=>workflowText(key));button.type='button';
    const primary=key==='approve_execute';
    if(primary){
      button.disabled=true;
      void rpc('workflow.details',{}).then(value=>{
        if(!card.isConnected||owner[0]!==state.sessionId||owner[1]!==state.selectionGeneration||owner[2]!==state.stream)return;
        if(value.session!==doc.session || value.review?.id!==doc.id || value.review?.revision!==doc.revision)return;
        const reviewed=value.documents.find(item=>item.id===doc.id&&item.revision===doc.revision);
        if(reviewed){
          execution=planExecution(reviewed);
          CLATI18n.bind(button,'textContent',()=>workflowText(execution.key));
          if(reviewed.continue_goal) button.title=reviewed.continue_goal.objective;
        }
      }).catch(()=>{}).finally(()=>{if(!sending)button.disabled=false;});
    }
    button.onclick=async()=>{
      sending=true;
      for(const node of actions.querySelectorAll('button'))node.disabled=true;
      try{const reply=await sendWorkflowIntent(primary?execution.intent:intent);if(reply!==undefined&&key==='revise'&&card.getClientRects().length){workbenchShell.chat();dom.prompt.focus();}}
      catch(error){sending=false;CLATI18n.bind(note,'textContent',()=>error.message);for(const node of actions.querySelectorAll('button'))node.disabled=false;}
    };actions.append(button);
  }
  const open=el('button','ghost',()=>workflowText('view_plan'));open.type='button';open.onclick=()=>void workflowController.plan(doc.text);actions.append(open);
  return card;
}
