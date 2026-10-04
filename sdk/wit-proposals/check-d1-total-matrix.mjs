import {copyAuthor} from './author-copy.mjs'
// One row per approved D1 attack. Records executable selectors and a discriminating fault.
import assert from 'node:assert/strict'
import {createHash,randomUUID} from 'node:crypto'
import {cp,mkdir,mkdtemp,readFile,writeFile,rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {spawn} from 'node:child_process'
const here=import.meta.dirname,root=path.resolve(here,'../..'),source=path.join(here,'http-consumer-host'),out=process.argv[2]
assert(out&&path.isAbsolute(out));await mkdir(out)
const rows=[
 ['01-origin','dns_authority/scope.rs','self.inner.fence.check(origin)?;','// deleted origin fence','exact_origin_rejects'],
 ['02-fa-no-manifest','capabilities.rs','let network = caps.network.ok_or(Error::MissingNetwork)?;','let network = caps.network.unwrap_or(Network {protocol:"clat:net-task@0.1.0".into(),origins:vec![]});','capability_network_requires_declaration'],
 ['03-dangerous-combination','capabilities.rs','if !caps.host_tools.is_empty()\n            || !caps.preopens.is_empty()\n            || !config.host_tools.is_empty()\n            || !config.preopens.is_empty()','if false','capability_network_rejects_all_process'],
 ['04-model-egress','capabilities.rs','if self.sampling {','if false {','capability_sampling_label'],
 ['05-full-address-set','dns_authority/addresses.rs','if !is_public(*address) {','if false {','original_provider_shared_pin_vectors'],
 ['06-nat64-six-layouts','dns_authority/nat64.rs','if !is_public(embedded.into()) {','if false {','dns_authority::tests::nat64_'],
 ['07-discovery','dns_authority/nat64.rs','_ => return Err(Failure::InvalidDiscovery),','_ => {},','discovery_malformed_member'],
 ['08-pinned-dial','http_authority/connector.rs','for address in addresses {','for address in addresses.take(0) {','numeric_dial_connects'],
 ['09-foreign-generation','dns_authority/credential.rs','if !Arc::ptr_eq(&scope.inner, &self.scope.inner) {','if false {','foreign_store_and_new_run'],
 ['10-single-consume','dns_authority/credential.rs','.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)','.compare_exchange(self.consumed.load(Ordering::Acquire), true, Ordering::AcqRel, Ordering::Acquire)','wrong_origin_does_not_consume'],
 ['11-control-headers','http_authority/mod.rs','forbidden_header(name.as_str())','false','transport_control_headers_are_closed'],
 ['12-redirect','http_authority/transport/stream.rs','    validate_head(&parts, max)?;','    validate_head(&parts, max)?; if parts.status.is_redirection() { return Err(Failure::Transport); }','redirect_is_returned_without_following'],
 ['13-cancel-chain','http_authority/permission.rs','self.source.check()','Ok(())','core_gate_revocation_closes_http'],
 ['14-task-async-cancel','network_resources/task.rs','pub(crate) fn cancel(&mut self) {\n        self.state = State::Cancelled;','pub(crate) fn cancel(&mut self) {\n        self.state = State::Consumed;','resource_task_get_never_waits'],
 ['15-post-approval-revoke','dns_authority/scope.rs','guard.check(deadline)?;','// deleted authority lifetime','core_dns_ready_and_consumed'],
 ['16-body-caps','http_authority/transport/stream.rs','if data.len() > self.max - body.received {','if false {','chunked_cumulative_body'],
 ['17-no-deadline-renewal','http_authority/transport/stream.rs','connection.pins.restrict_deadline(deadline);','// deleted original deadline','stream_request_ceiling'],
 ['18-pool-bound','dns_authority/resolver.rs','mpsc::sync_channel::<Work>(queue)','mpsc::sync_channel::<Work>(queue + 1)','fixed_workers_and_queue_reject'],
 ['19-end-owner','network_resources/component/invocation.rs','let result = host.owner.close().map_err(super::trap);','let result = Ok(());','invocation_boundary_closes'],
 ['20-secret-summary','http_authority/mod.rs','self.origin.scheme(),\n            self.origin.host(),\n            self.origin.port()', 'self.url.scheme(),\n            self.url.host_str().unwrap(),\n            self.url.path()','summary_never_carries_guest_secrets'],
 ['21-cpu-epoch','network_resources/component/invocation.rs','context.data_mut().owner.check().map_err(super::trap)?;','// deleted epoch authority','invocation_run_revocation'],
]
const run=(args,cwd=root)=>new Promise((resolve,reject)=>{
 const child=spawn('cargo',args,{cwd});let log='';child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);child.on('error',reject);child.on('close',code=>resolve({code,log}))
})
const temp=await mkdtemp(path.join(tmpdir(),'clat-d1-total-')),copy=path.join(temp,'host'),evidence=[]
try{
 await copyAuthor(source,copy,root);await cp(path.join(here,'typed-task'),path.join(temp,'typed-task'),{recursive:true})
 const name='clat-plg4-http-consumer-host',mutant=name+'-matrix-'+randomUUID().replaceAll('-','')
 for(const file of ['Cargo.toml','Cargo.lock']) {
  let text=await readFile(path.join(copy,file),'utf8');text=text.replace(`name = "${name}"`,`name = "${mutant}"`)
  if(file==='Cargo.toml')text=text.replaceAll('path = "../../../crates/core"',`path = ${JSON.stringify(path.join(root,'crates/core'))}`).replace(/^default-run = .*\n/m,'')
  await writeFile(path.join(copy,file),text)
 }
 const selected=process.argv[3]?rows.filter(row=>row[0]===process.argv[3]):rows
 assert(selected.length,'unknown matrix row')
 for(const [id,file,before,after,filter] of selected){
  const destination=path.join(copy,'src',file),original=(await readFile(destination,'utf8')).replaceAll('\r\n','\n')
  assert.equal(original.split(before).length-1,1,`unique guard ${id}`)
  await writeFile(destination,original.replace(before,after))
  const result=await run(['test','--manifest-path',path.join(copy,'Cargo.toml'),'--locked','--offline','--target-dir',path.join(root,'target'),'--lib',filter,'--','--nocapture'])
  await writeFile(path.join(out,id+'.log'),result.log);await writeFile(destination,original)
  assert.equal(result.code,101,`${id} must be red`);assert.match(result.log,/panicked at/);assert.doesNotMatch(result.log,/error\[E\d+\]|could not compile/)
  evidence.push({id,filter,guardSha256:createHash('sha256').update(original).digest('hex'),exit:101});console.log('D1_MATRIX '+id)
 }
 await writeFile(path.join(out,'rust-rows.json'),JSON.stringify({rows:evidence,remainingRows:['22-legacy-linker','23-publication-fence','24-scope-ownership'],all24Complete:false,productionActivationVerified:false},null,2)+'\n')
}finally{await rm(temp,{recursive:true,force:true})}
