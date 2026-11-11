/* Remaining percentages only; monetary balances never become percentage rings. */
function projectRailAllowance(allowance){
  if(allowance?.status!=='available')return null;
  const items=(allowance.items||[]).filter(item=>item.kind==='quota'&&item.unit==='%'&&String(item.value).trim()!==''&&Number.isFinite(Number(item.value))&&Number(item.value)>=0&&Number(item.value)<=100);
  if(!items.length)return null;
  return {items,remaining:Math.min(...items.map(item=>Number(item.value))),updated:allowance.updated_at};
}
function createRailAllowance(){
  const button=el('button','shell-button');button.id='nav-allowance';button.type='button';button.hidden=true;
  const ring=CLATRailPopover.progressRing();button.append(ring.element);
  document.getElementById('nav-settings').before(button);
  const popup=el('section');popup.id='rail-allowance-popup';let allowance=null,clock=null;
  const text=(key,args)=>CLATI18n.t('rail.'+key,args);
  function render(){
    popup.replaceChildren(el('strong','',()=>text('allowance')));
    const model=state.workbench?.model?.model;if(model)popup.append(el('p','',model));
    if(!allowance)return;
    for(const item of allowance.items){
      const hours=Number(item.window_hours),window=hours>0?(hours%168===0?text('window_weeks',{count:hours/168}):text('window_hours',{count:hours})):text('remaining');
      CLATRailPopover.fact(popup,window,Number(item.value).toLocaleString(CLATI18n.locale,{maximumFractionDigits:1})+'%');
      const reset=Number(item.reset_at);if(Number.isFinite(reset)&&reset>0&&reset<=8640000000000000){
        CLATRailPopover.fact(popup,text('reset'),new Date(reset).toLocaleString(CLATI18n.locale));
        const minutes=Math.ceil((reset-Date.now())/60000);
        CLATRailPopover.fact(popup,text('reset_in'),minutes>0?text('minutes',{count:minutes}):text('reset_due'));
      }
    }
    if(allowance.updated)CLATRailPopover.fact(popup,text('updated'),new Date(allowance.updated).toLocaleString(CLATI18n.locale));
  }
  CLATI18n.attribute(popup,'aria-label',()=>text('allowance'));
  const card=CLATRailPopover.attach(button,popup,{parent:document.body,onOpen(){render();clock??=setInterval(render,60000);},onClose(){clearInterval(clock);clock=null;}});button.onclick=card.toggle;
  function update(value){
    const next=projectRailAllowance(value?.allowance);
    // A failed refresh cannot keep an old percentage looking current.
    allowance=next;button.hidden=!next;
    if(!next){card.hide();return;}
    ring.set(next.remaining);button.dataset.low=String(next.remaining<=20);button.dataset.critical=String(next.remaining<=5);
    CLATI18n.attribute(button,'aria-label',()=>text('remaining_percent',{percent:next.remaining.toLocaleString(CLATI18n.locale,{maximumFractionDigits:1})}));
    if(!popup.hidden){render();card.position();}
  }
  CLATI18n.onChange(()=>{if(allowance)render();});
  return {update};
}
