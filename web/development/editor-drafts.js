/* Draft transport stays serialized per client; lost acknowledgements are reconciled by content. */
function createDevelopmentDrafts({docs,status,activate}){
  let version=0,lane=Promise.resolve();
  async function publish(snapshot,dirty){
    const operationId=devId();
    try{const result=await rpc(dirty?'editor.draft.save':'editor.draft.delete',{operationId,clientId:developmentClient,expectedVersion:version,...snapshot});version=result.version;}
    catch(error){
      const current=await rpc('editor.drafts',{clientId:developmentClient});const saved=current.documents.find(row=>row.path===snapshot.path);
      if(dirty?saved?.text===snapshot.text&&saved?.version===snapshot.version:!saved){version=current.version;return;}
      throw error;
    }
  }
  function queue(doc){
    const snapshot={path:doc.path,text:doc.text,version:doc.version,bom:doc.bom,newline:doc.newline};const dirty=doc.dirty;
    clearTimeout(doc.draftTimer);doc.draftTimer=setTimeout(()=>{lane=lane.catch(()=>{}).then(()=>publish(snapshot,dirty)).catch(error=>devReport(status,error));},250);
  }
  async function recover(others=false){
    try{const data=await rpc(others?'editor.drafts.recover':'editor.drafts',{clientId:developmentClient});if(!others)version=data.version;if(others&&await devChoice(devText('recover_confirm'),['restore','cancel'])!=='restore')return true;
      for(const saved of data.documents){if(docs.has(saved.path))continue;let disk;try{disk=await rpc('editor.open',{path:saved.path});}catch(_){disk={editable:true,text:'',version:'missing'};}
        const doc={...disk,...saved,baseText:disk.text,dirty:saved.text!==disk.text,pinned:true,readonly:!disk.editable,conflict:disk.version!==saved.version,disk};docs.set(doc.path,doc);if(others)queue(doc);
      }if(docs.size)activate([...docs.values()][0]);return true;
    }catch(error){devReport(status,error);return false;}
  }
  return {queue,recover,flush:()=>lane};
}
