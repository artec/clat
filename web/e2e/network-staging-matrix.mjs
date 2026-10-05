// Ephemeral fixture publications, separate from the PWA's first-release index.
import {cp,readFile,writeFile,rm} from 'node:fs/promises'
import path from 'node:path'
export async function prepareNetworkMatrix({scratch,directory,publication,clat,run}){
 const next=path.join(scratch,'update-package');await cp(directory,next,{recursive:true})
 for(const file of ['clat-plugin.publisher.json','clat-plugin.minisig'])await rm(path.join(next,file),{force:true})
 const file=path.join(next,'clat-plugin.json'),manifest=JSON.parse(await readFile(file,'utf8'))
 manifest.version='0.1.1';manifest.capabilities.network.origins[0].methods.push('GET')
 await writeFile(file,JSON.stringify(manifest)+'\n')
 run('node',['market/scripts/sign-package.mjs','--package',next,'--publisher','artec-fixture','--public-key',path.join(scratch,'publisher.pub'),'--minisign-key',path.join(scratch,'publisher.key')])
 const update=path.join(scratch,'update-publication')
 run('node',['market/scripts/stage-package.mjs','--package',next,'--out',update,'--target','any','--publisher-key-id','fixture-only','--review-url','https://example.com/fixture-review','--source-url','https://github.com/artec/clat','--clat',clat])
 const matrix=path.join(scratch,'matrix-publication');await cp(publication,matrix,{recursive:true})
 await cp(path.join(update,'packages'),path.join(matrix,'packages'),{recursive:true})
 const index=JSON.parse(await readFile(path.join(publication,'index.json'),'utf8'))
 index.packages[0].versions.push(JSON.parse(await readFile(path.join(update,'index.source.proposed.json'),'utf8')).packages[0].versions[0])
 index.market.generatedAtUnix=Math.floor(Date.now()/1000);index.market.expiresAtUnix=index.market.generatedAtUnix+3600
 const out=path.join(matrix,'index.json');await writeFile(out,JSON.stringify(index)+'\n')
 run('minisign',['-S','-s',path.join(scratch,'index.key'),'-m',out,'-x',out+'.minisig','-t',`CLAT plugin index cn.at.pi generated ${index.market.generatedAtUnix}`])
 return matrix
}
