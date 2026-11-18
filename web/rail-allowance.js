/* Percentages keep the ring. A wallet balance is at most three characters, with no currency sign. */
const RAIL_MONEY_BUDGET=3;
function railColumns(text){
  let count=0;
  for(const char of String(text))count+='万萬亿億'.includes(char)?2:1;
  return count;
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
function magnitudeUnits(){
  return [{suffix:'k',div:1e3},{suffix:'M',div:1e6},{suffix:'B',div:1e9}];
}
function formatSmall(value){
  const decimal=decimalText(value,1);
  if(decimal&&Number(decimal)<1000&&railColumns(decimal)<=RAIL_MONEY_BUDGET)return decimal;
  const integer=decimalText(value,0);
  if(integer&&Number(integer)<1000&&railColumns(integer)<=RAIL_MONEY_BUDGET)return integer;
  return null;
}
function formatLarge(scaled,suffix){
  const room=RAIL_MONEY_BUDGET-railColumns(suffix);
  if(String(Math.trunc(Math.abs(roundTo(scaled,0)))).length===1){
    const decimal=decimalText(scaled,1);
    if(decimal&&Number(decimal)<10&&railColumns(decimal)<=room)return decimal+suffix;
  }
  const integer=decimalText(scaled,0);
  if(integer&&railColumns(integer)<=room)return integer+suffix;
  return null;
}
function capUnit(unit){
  return '9'.repeat(RAIL_MONEY_BUDGET-railColumns(unit.suffix))+unit.suffix;
}
function compactMagnitude(value){
  const units=magnitudeUnits();
  let fallback=units[0];
  for(let index=0;index<units.length;index++){
    const unit=units[index];
    if(value>=unit.div)fallback=unit;
    const text=formatLarge(value/unit.div,unit.suffix);
    if(!text)continue;
    const numeric=Number(text.slice(0,text.length-unit.suffix.length));
    const next=units[index+1];
    if(!(numeric>0)||(next&&numeric*unit.div>=next.div))continue;
    return text;
  }
  return capUnit(fallback);
}
function railWalletIcon(){
  const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');
  svg.setAttribute('viewBox','0 0 24 24');
  svg.setAttribute('aria-hidden','true');
  const path=document.createElementNS(svg.namespaceURI,'path');
  // The diagonal folded back edge gives the outline a wallet silhouette.
  // Open sides leave the full center clear even for a three-character balance.
  path.setAttribute('d','M5.3 6.3L7.2 3.5H17.2C18.6 3.5 19.4 4.4 19.4 5.7M2.8 9.1V8.7C2.8 7.2 4.1 6.3 5.8 6.3H18.2C20.1 6.3 21.2 7.4 21.2 8.9M2.8 15.1V16.8C2.8 18.6 4.1 19.6 6.1 19.6H17.8C19.8 19.6 21.2 18.5 21.2 16.8V15.1');
  svg.append(path);
  return svg;
}
function compactRailMoney(value,unit,locale){
  const text=String(value??'').trim();
  const amount=Number(text);
  if(text===''||!Number.isFinite(amount)||amount<0)return null;
  if(amount<1000){
    const small=formatSmall(amount);
    if(small)return small;
  }
  return compactMagnitude(amount);
}
function moneyExact(item){
  return `${item.value}${item.unit?' '+item.unit:''}`;
}
// 额度窗口名（owner 2026-11-18）：720 小时呈现为"1 月额度"（OC-Go 月窗，
// 与"1 周额度"同形、始终带数）；周/小时沿用既有形状，0 落回"剩余额度"。
function railWindowLabel(hours,t){
  if(!(hours>0))return t('remaining');
  if(hours%720===0)return t('window_months',{count:hours/720});
  if(hours%168===0)return t('window_weeks',{count:hours/168});
  return t('window_hours',{count:hours});
}
// 重置倒数（owner 2026-11-18）：不要"全是分钟"——天/小时/分钟逐级组合，
// 零单位跳过；不足一小时保留纯分钟。
function railResetCountdown(minutes,t){
  if(minutes<=0)return t('reset_due');
  const days=Math.floor(minutes/1440),hours=Math.floor((minutes%1440)/60),mins=minutes%60;
  if(!days&&!hours)return t('minutes',{count:minutes});
  const parts=[];
  if(days)parts.push(t('days',{count:days}));
  if(hours)parts.push(t('hours',{count:hours}));
  if(mins)parts.push(t('minutes',{count:mins}));
  return parts.join(' ');
}
function projectRailAllowance(allowance){
  if(allowance?.status!=='available')return null;
  const items=(allowance.items||[]).filter(item=>item.kind==='quota'&&item.unit==='%'&&String(item.value).trim()!==''&&Number.isFinite(Number(item.value))&&Number(item.value)>=0&&Number(item.value)<=100);
  if(items.length)return {kind:'quota',items,remaining:Math.min(...items.map(item=>Number(item.value))),updated:allowance.updated_at};
  const money=(allowance.items||[]).filter(item=>item?.kind==='money'&&compactRailMoney(item.value,item.unit,'en'));
  if(!money.length)return null;
  return {kind:'money',items:money,updated:allowance.updated_at};
}
function paintRailAllowance(button,ring,mark,balance,allowance){
  const wallet=allowance.kind==='money';
  ring.element.hidden=wallet;mark.hidden=!wallet;
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
  const balance=el('span','rail-balance');balance.setAttribute('aria-hidden','true');
  const mark=el('span','rail-wallet');mark.hidden=true;mark.append(railWalletIcon(),balance);
  button.append(ring.element,mark);
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
      const window=railWindowLabel(Number(item.window_hours),text);
      CLATRailPopover.fact(popup,window,Number(item.value).toLocaleString(CLATI18n.locale,{maximumFractionDigits:1})+'%');
      const reset=Number(item.reset_at);if(Number.isFinite(reset)&&reset>0&&reset<=8640000000000000){
        CLATRailPopover.fact(popup,text('reset'),new Date(reset).toLocaleString(CLATI18n.locale));
        CLATRailPopover.fact(popup,text('reset_in'),railResetCountdown(Math.ceil((reset-Date.now())/60000),text));
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
    paintRailAllowance(button,ring,mark,balance,next);
    if(!popup.hidden){render();card.position();}
  }
  CLATI18n.onChange(()=>{if(!allowance)return;paintRailAllowance(button,ring,mark,balance,allowance);if(!popup.hidden){render();card.position();}});
  return {update};
}
