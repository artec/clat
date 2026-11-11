/* Shared hover cards for native updates and project-local allowance indicators. */
(() => {
  'use strict';
  if(globalThis.CLATRailPopover)return;
  function installStyle(){
    if(document.getElementById('rail-popover-style'))return;
    const style=document.createElement('style');style.id='rail-popover-style';style.textContent=`
      .rail-popover[hidden]{display:none!important}.rail-popover{position:fixed;z-index:1000;box-sizing:border-box;width:min(320px,calc(100vw - 24px));max-height:calc(100dvh - 24px);overflow:auto;padding:16px;color:var(--text);background:var(--surface-raised,#252525);border:1px solid var(--line-strong);border-radius:12px;box-shadow:0 8px 30px #0004;font:400 var(--text-size-ui,13px)/1.6 var(--font-ui);overflow-wrap:anywhere}
      .rail-popover p{margin:8px 0}.rail-popover .rail-fact{display:flex;justify-content:space-between;gap:16px;margin:8px 0}.rail-fact>span:first-child{color:var(--text-muted)}.rail-fact>span:last-child{text-align:end}.rail-popover button{min-height:32px}.rail-popover button:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
      .global-rail:has(#nav-update:not([hidden])) #nav-allowance{display:none!important}
      #nav-allowance[hidden]{display:none!important}#nav-allowance[data-low=true]{color:var(--warning)}#nav-allowance[data-critical=true]{color:var(--danger)}
      .rail-ring{display:grid;place-items:center;width:22px;height:22px;flex:none}.rail-ring[hidden]{display:none!important}
      .rail-ring>*{grid-area:1/1}.rail-ring svg{width:22px;height:22px;transform:rotate(-90deg)}
      .rail-ring circle{fill:none;stroke:currentColor;stroke-width:1.64;stroke-linecap:butt}
      .rail-ring .ring-track{opacity:.1}.rail-ring-number{font:400 8px var(--font-ui);color:var(--text-soft)}
      .rail-ring[data-spinning=true] svg{animation:rail-ring-spin 1.3s linear infinite}
      @keyframes rail-ring-spin{to{transform:rotate(270deg)}}
      @media(prefers-reduced-motion:reduce){.rail-ring[data-spinning=true] svg{animation:none}}
    `;document.head.append(style);
  }
  function attach(button,popup,{parent=document.body,onOpen=()=>{},onClose=()=>{}}={}){
    installStyle();popup.classList.add('rail-popover');popup.hidden=true;popup.tabIndex=-1;popup.setAttribute('role','dialog');parent.append(popup);
    button.setAttribute('aria-haspopup','dialog');button.setAttribute('aria-expanded','false');
    if(popup.id)button.setAttribute('aria-controls',popup.id);
    let timer=null,pinned=false,dismissed=false;
    const position=()=>{const box=button.getBoundingClientRect();popup.style.left=Math.max(12,Math.min(box.right+10,innerWidth-popup.offsetWidth-12))+'px';popup.style.top=Math.max(12,Math.min(box.top,innerHeight-popup.offsetHeight-12))+'px';};
    const clear=()=>{clearTimeout(timer);timer=null;};
    const hide=(focus=false)=>{clear();pinned=false;dismissed=true;if(!popup.hidden){popup.hidden=true;onClose();}button.setAttribute('aria-expanded','false');if(focus&&button.isConnected)button.focus({preventScroll:true});};
    const show=(pin=false,focus=pin)=>{clear();if(button.hidden||!button.isConnected||!button.getClientRects().length)return;dismissed=false;pinned ||= pin;onOpen();popup.hidden=false;button.setAttribute('aria-expanded','true');position();if(focus)popup.focus({preventScroll:true});};
    const leave=()=>{clear();if(!pinned)timer=setTimeout(()=>{if(!popup.matches(':hover')&&!button.matches(':hover')&&!popup.contains(document.activeElement)&&document.activeElement!==button)hide();},180);};
    button.addEventListener('pointerenter',()=>{dismissed=false;clear();timer=setTimeout(()=>show(false,false),180);});button.addEventListener('pointerleave',leave);
    popup.addEventListener('pointerenter',clear);popup.addEventListener('pointerleave',leave);
    button.addEventListener('focus',()=>{if(!dismissed)show(false,false);});
    for(const node of [button,popup])node.addEventListener('focusout',leave);
    const key=event=>{if(event.key==='Escape'&&!popup.hidden){event.preventDefault();hide(popup.contains(document.activeElement));}};
    const outside=event=>{if(!popup.hidden&&!popup.contains(event.target)&&!button.contains(event.target))hide();};
    document.addEventListener('keydown',key);document.addEventListener('pointerdown',outside);window.addEventListener('resize',position);
    const guard=new MutationObserver(()=>{if(!button.isConnected||button.hidden||!button.getClientRects().length)hide();});const rail=button.closest('.global-rail')||document.getElementById('nav-settings')?.closest('.global-rail');if(rail)guard.observe(rail,{childList:true,subtree:true,attributes:true,attributeFilter:['hidden']});
    const deactivate=()=>hide();window.addEventListener('clat:project-activated',deactivate);
    return {show,hide,position,toggle(){if(!popup.hidden&&pinned)hide();else show(true);},destroy(){hide();guard.disconnect();document.removeEventListener('keydown',key);document.removeEventListener('pointerdown',outside);window.removeEventListener('resize',position);window.removeEventListener('clat:project-activated',deactivate);popup.remove();}};
  }
  function fact(parent,label,value){const row=document.createElement('div');row.className='rail-fact';for(const text of [label,value]){const span=document.createElement('span');span.textContent=text;row.append(span);}parent.append(row);}
  function progressRing(){
    const element=document.createElement('span'),number=document.createElement('span');
    element.className='rail-ring';element.setAttribute('aria-hidden','true');
    number.className='rail-ring-number';
    const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');
    svg.setAttribute('viewBox','0 0 24 24');
    for(const name of ['ring-track','ring-value']){
      const circle=document.createElementNS(svg.namespaceURI,'circle');
      for(const [key,value] of Object.entries({cx:12,cy:12,r:9,class:name}))circle.setAttribute(key,value);
      svg.append(circle);
    }
    element.append(svg,number);
    const circumference=2*Math.PI*9;
    const setArc=value=>svg.lastChild.setAttribute('stroke-dasharray',`${circumference*value/100} ${circumference}`);
    let frame=null,current=null,target=null;
    const draw=value=>{
      current=value;
      setArc(value);
      number.textContent=String(Math.floor(value));
    };
    return {element,set(percent){
      element.dataset.spinning=String(percent===null);
      if(percent===null){
        if(frame!==null)cancelAnimationFrame(frame);
        frame=null;current=null;target=null;
        setArc(25);number.textContent='';return;
      }
      const next=Math.max(0,Math.min(100,percent));
      if(next===target)return;
      if(frame!==null)cancelAnimationFrame(frame);
      target=next;
      if(current===null||matchMedia('(prefers-reduced-motion: reduce)').matches){draw(next);frame=null;return;}
      const from=current,start=performance.now();
      const step=now=>{
        const t=Math.min(1,(now-start)/650),eased=1-(1-t)**3;
        draw(from+(next-from)*eased);
        frame=t<1?requestAnimationFrame(step):null;
      };
      frame=requestAnimationFrame(step);
    }};
  }
  globalThis.CLATRailPopover=Object.freeze({attach,fact,progressRing});
})();
