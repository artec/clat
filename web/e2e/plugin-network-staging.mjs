// Signed staging only. Test keys and numeric fixture do not change release trust.
import {spawn,spawnSync} from 'node:child_process'
import {cp,mkdtemp,readFile,rm,writeFile} from 'node:fs/promises'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import {gzipSync} from 'node:zlib'
import {prepareNetworkMatrix} from './network-staging-matrix.mjs'
const repo=path.resolve(import.meta.dirname,'../..')
function option(name){const at=process.argv.indexOf(name);if(at<0||!process.argv[at+1])throw new Error(`missing ${name}`);return path.resolve(process.argv[at+1])}
const source=option('--package'),clat=option('--clat'),scratch=await mkdtemp(path.join(os.tmpdir(),'clat-network-staging-'))
function run(command,args){const r=spawnSync(command,args,{cwd:repo,stdio:'inherit'});if(r.error)throw r.error;if(r.status!==0)throw new Error(`${command} failed`)}
let server;let publication;let matrix;
try {
 server=http.createServer(async(request,response)=>{
  try{
   if(request.url==='/anthropic/v1/messages'){
    const chunks=[];for await(const chunk of request)chunks.push(chunk);
    const body=JSON.parse(Buffer.concat(chunks));
    if(request.method!=='POST'||request.headers['x-api-key']!=='private-fixture-key'||body.tools[0].name!=='web_search'){response.writeHead(400);response.end();return}
    response.setHeader('Content-Type','application/json');response.setHeader('Content-Encoding','gzip')
    response.end(gzipSync(JSON.stringify({content:[{type:'web_search_tool_result',content:[{type:'web_search_result',url:'https://example.com/clat',title:'CLAT PLG-2 fixture'}]}]})));return
   }
   let url=request.url,namespace=''
   const selected=url.match(/^\/(matrix|bad-index|bad-signature|tamper)\//)
   if(selected){namespace=selected[1];url=url.slice(namespace.length+1)}
   if(!/^\/(index\.json(?:\.minisig)?|packages\/[A-Za-z0-9._-]+\.clatpkg)$/.test(url)){response.writeHead(404);response.end();return}
   const bytes=await readFile(path.join(selected?matrix:publication,url))
   if((namespace==='bad-index'&&url==='/index.json')||(namespace==='bad-signature'&&url==='/index.json.minisig')||(namespace==='tamper'&&url.startsWith('/packages/')))bytes[bytes.length-1]^=1
   response.setHeader('Content-Length',bytes.length);response.end(bytes)
  }catch{response.writeHead(500);response.end()}
 })
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 const port=server.address().port,origin=`http://typed.invalid:${port}`,base=`http://127.0.0.1:${port}/`
 const directory=path.join(scratch,'package');await cp(source,directory,{recursive:true})
 const manifestPath=path.join(directory,'clat-plugin.json'),manifest=JSON.parse(await readFile(manifestPath,'utf8'))
 manifest.capabilities.network.origins=[{scheme:'http',host:'typed.invalid',port,methods:['POST']}]
 manifest.configSchema.properties.options={type:'object',title:'Provider fixture options'}
 await writeFile(manifestPath,JSON.stringify(manifest)+'\n')
 for(const role of ['publisher','index'])run('minisign',['-G','-W','-p',path.join(scratch,role+'.pub'),'-s',path.join(scratch,role+'.key')])
 run('node',['market/scripts/sign-package.mjs','--package',directory,'--publisher','artec-fixture','--public-key',path.join(scratch,'publisher.pub'),'--minisign-key',path.join(scratch,'publisher.key')])
 publication=path.join(scratch,'publication')
 run('node',['market/scripts/stage-package.mjs','--package',directory,'--out',publication,'--target','any','--publisher-key-id','fixture-only','--review-url','https://example.com/fixture-review','--source-url','https://github.com/artec/clat','--clat',clat])
 const catalog=JSON.parse(await readFile(path.join(publication,'catalog.proposed.json'),'utf8'))
 if(catalog.packages[0].manifestVersion!==2)throw new Error('WASM display catalog must select the signed v2 endpoint')
 const index=JSON.parse(await readFile(path.join(publication,'index.source.proposed.json'),'utf8')),now=Math.floor(Date.now()/1000)
 index.market.generatedAtUnix=now;index.market.expiresAtUnix=now+3600
 const indexPath=path.join(publication,'index.json');await writeFile(indexPath,JSON.stringify(index)+'\n')
 run('minisign',['-S','-s',path.join(scratch,'index.key'),'-m',indexPath,'-x',indexPath+'.minisig','-t',`CLAT plugin index cn.at.pi generated ${now}`])
 matrix=await prepareNetworkMatrix({scratch,directory,publication,clat,run})
 const env={...process.env,CLAT_PLG2_MARKET_URL:base,CLAT_PLG2_MARKET_PUBLIC_KEY:path.join(scratch,'index.pub'),CLAT_PLG4_NUMERIC_FIXTURE:origin,CLAT_PLG4_CATALOG:path.join(publication,'catalog.proposed.json')}
 const matrixChild=spawn('cargo',['test','-p','clat-core','--features','test-support','plg4_signed_network_staging_matrix','--','--ignored','--nocapture'],{cwd:repo,env:{...env,CLAT_PLG4_STAGING_MATRIX_URL:base+'matrix/'},stdio:'inherit'})
 const matrixCode=await new Promise((resolve,reject)=>{matrixChild.once('error',reject);matrixChild.once('exit',resolve)})
 if(matrixCode!==0)throw new Error('signed v2 staging matrix failed')
 const child=spawn('npm',['test','--','tests/plugin-network.spec.js'],{cwd:path.join(repo,'web/e2e'),env,stdio:'inherit'})
 const code=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',resolve)});if(code!==0)throw new Error('WASM signed staging acceptance failed')
}finally{if(server)await new Promise(resolve=>server.close(resolve));await rm(scratch,{recursive:true,force:true})}
