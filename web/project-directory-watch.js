/* One authenticated Host invalidation stream for the document, not one per project. */
function createProjectDirectoryWatch(refresh){
  let stream=null,retry=0,delay=1000;
  const reload=()=>void refresh().catch(()=>{});
  async function start(){
    if(stream)return;clearTimeout(retry);const owner=new AbortController();stream=owner;
    try{
      const response=await fetch('/api/notifications',{signal:owner.signal});if(!response.ok||!response.body)throw new Error('notification stream unavailable');
      delay=1000;reload();const reader=response.body.getReader(),decoder=new TextDecoder();let buffer='';
      while(!owner.signal.aborted){const value=await reader.read();if(value.done)break;buffer+=decoder.decode(value.value,{stream:true});if(buffer.length>65536)throw new Error('notification frame too large');let end;
        while((end=/\r?\n\r?\n/.exec(buffer))){const frame=buffer.slice(0,end.index).replace(/\r/g,'');buffer=buffer.slice(end.index+end[0].length);const data=frame.split('\n').filter(line=>line.startsWith('data:')).map(line=>line.slice(5).trimStart()).join('\n');if(!data)continue;try{if(JSON.parse(data).kind==='workspace.updated')reload();}catch(_){} }
      }
    }catch(_){/* Reconnect without changing existing project state. */}
    finally{owner.abort();if(stream===owner){stream=null;retry=setTimeout(start,delay);delay=Math.min(delay*2,5000);}}
  }
  window.addEventListener('focus',reload);window.addEventListener('pageshow',()=>{reload();void start();});
  return {start:()=>void start()};
}
