/* Percentages keep the ring. A wallet balance is a short label of at most five columns. */
function railColumns(text){
  let count=0;
  for(const char of String(text))count+='万萬亿億'.includes(char)?2:1;
  return count;
}
function moneySymbol(unit){
  if(unit==='CNY')return '\u00A5';
  if(unit==='USD')return '$';
  return '';
}
function roundTo(value,decimals){
  const factor=10**decimals;
  return Math.round(value*factor+Number.EPSILON*Math.sign(value||1))/factor;
}
function decimalText(value,decimals){
  const rounded=roundTo(value,decimals);
  if(!Number.isFinite(rounded)||rounded<0)return null;
  const text=decimals===0?String(Math.round(rounded)):rounded.toFixed(decimals).replace(/0+$/,'').replace(/\.$/,'');
  return /e/i.test(text)?null:text;
}
function magnitudeUnits(locale){
  if(locale==='zh-Hant')return [{suffix:'萬',div:1e4},{suffix:'億',div:1e8}];
  if(locale==='zh-Hans')return [{suffix:'万',div:1e4},{suffix:'亿',div:1e8}];
  return [{suffix:'k',div:1e3},{suffix:'M',div:1e6}];
}
function formatSmall(value,symbol){
  const decimal=decimalText(value,1);
  if(decimal&&Number(decimal)<10000&&railColumns(symbol+decimal)<=5)return symbol+decimal;
  const integer=decimalText(value,0);
  if(integer&&Number(integer)<10000&&railColumns(symbol+integer)<=5)return symbol+integer;
  return null;
}
function formatLarge(scaled,suffix){
  const room=5-railColumns(suffix);
  if(String(Math.trunc(Math.abs(roundTo(scaled,0)))).length===1){
    const decimal=decimalText(scaled,1);
    if(decimal&&Number(decimal)<10&&railColumns(decimal)<=room)return decimal+suffix;
  }
  const integer=decimalText(scaled,0);
  if(integer&&railColumns(integer)<=room)return integer+suffix;
  return null;
}
function capUnit(unit){
  return '9'.repeat(5-railColumns(unit.suffix))+unit.suffix;
}
function compactMagnitude(value,locale){
  const units=magnitudeUnits(locale);
  for(let index=0;index<units.length;index++){
    const unit=units[index],text=formatLarge(value/unit.div,unit.suffix);
    if(!text)continue;
    const numeric=Number(text.slice(0,text.length-unit.suffix.length));
    const next=units[index+1];
    if(!(numeric>0)||(next&&numeric*unit.div>=next.div))continue;
    return text;
  }
  return capUnit(units[units.length-1]);
}
function compactRailMoney(value,unit,locale){
  const text=String(value??'').trim();
  const amount=Number(text);
  if(text===''||!Number.isFinite(amount)||amount<0)return null;
  if(amount<10000){
    const small=formatSmall(amount,moneySymbol(unit));
    if(small)return small;
  }
  return compactMagnitude(amount,locale);
}
function moneyExact(item){
  return `${item.value}${item.unit?' '+item.unit:''}`;
}
function projectRailAllowance(allowance){
  if(allowance?.status!=='available')return null;
  const items=(allowance.items||[]).filter(item=>item.kind==='quota'&&item.unit==='%'&&String(item.value).trim()!==''&&Number.isFinite(Number(item.value))&&Number(item.value)>=0&&Number(item.value)<=100);
  if(items.length)return {kind:'quota',items,remaining:Math.min(...items.map(item=>Number(item.value))),updated:allowance.updated_at};
  const money=(allowance.items||[]).filter(item=>item?.kind==='money'&&compactRailMoney(item.value,item.unit,'en'));
  if(!money.length)return null;
  return {kind:'money',items:money,updated:allowance.updated_at};
}
function paintRailAllowance(button,ring,balance,allowance){
  const wallet=allowance.kind==='money';
  ring.element.hidden=wallet;balance.hidden=!wallet;
  delete button.dataset.low;delete button.dataset.critical;delete button.dataset.empty;
  if(wallet){
    const first=allowance.items[0];
    balance.textContent=compactRailMoney(first.value,first.unit,CLATI18n.locale)||'';
    if(Number(String(first.value).trim())===0)button.dataset.empty='true';
    CLATI18n.attribute(button,'aria-label',()=>CLATI18n.t('rail.wallet_value',{amount:moneyExact(first)}));
    return;
  }
  ring.set(allowance.remaining);
  button.dataset.low=String(allowance.remaining<=20);button.dataset.critical=String(allowance.remaining<=5);
  CLATI18n.attribute(button,'aria-label',()=>CLATI18n.t('rail.remaining_percent',{percent:allowance.remaining.toLocaleString(CLATI18n.locale,{maximumFractionDigits:1})}));
}
function createRailAllowance(){
  const button=el('button','shell-button');button.id='nav-allowance';button.type='button';button.hidden=true;
  const ring=CLATRailPopover.progressRing();
  const balance=el('span','rail-balance');balance.hidden=true;balance.setAttribute('aria-hidden','true');
  button.append(ring.element,balance);
  document.getElementById('nav-settings').before(button);
  const popup=el('section');popup.id='rail-allowance-popup';let allowance=null,clock=null;
  const text=(key,args)=>CLATI18n.t('rail.'+key,args);
  function render(){
    const wallet=allowance?.kind==='money';
    popup.replaceChildren(el('strong','',()=>text(wallet?'wallet':'allowance')));
    const model=state.workbench?.model?.model;if(model)popup.append(el('p','',model));
    if(!allowance)return;
    if(wallet)for(const item of allowance.items)CLATRailPopover.fact(popup,text('wallet'),moneyExact(item));
    else for(const item of allowance.items){
      const hours=Number(item.window_hours),window=hours>0?(hours%168===0?text('window_weeks',{count:hours/168}):text('window_hours',{count:hours})):text('remaining');
      CLATRailPopover.fact(popup,window,Number(item.value).toLocaleString(CLATI18n.locale,{maximumFractionDigits:1})+'%');
      const reset=Number(item.reset_at);if(Number.isFinite(reset)&&reset>0&&reset<=8640000000000000){
        CLATRailPopover.fact(popup,text('reset'),new Date(reset).toLocaleString(CLATI18n.locale));
        const minutes=Math.ceil((reset-Date.now())/60000);
        CLATRailPopover.fact(popup,text('reset_in'),minutes>0?text('minutes',{count:minutes}):text('reset_due'));
      }
    }
    if(allowance.updated)CLATRailPopover.fact(popup,text('updated'),new Date(allowance.updated).toLocaleString(CLATI18n.locale));
    CLATI18n.attribute(popup,'aria-label',()=>text(wallet?'wallet':'allowance'));
  }
  const card=CLATRailPopover.attach(button,popup,{parent:document.body,onOpen(){render();clock??=setInterval(render,60000);},onClose(){clearInterval(clock);clock=null;}});button.onclick=card.toggle;
  function update(value){
    const next=projectRailAllowance(value?.allowance);
    // A failed refresh cannot keep an old reading looking current.
    allowance=next;button.hidden=!next;
    if(!next){card.hide();return;}
    paintRailAllowance(button,ring,balance,next);
    if(!popup.hidden){render();card.position();}
  }
  CLATI18n.onChange(()=>{if(!allowance)return;paintRailAllowance(button,ring,balance,allowance);if(!popup.hidden){render();card.position();}});
  return {update};
}
