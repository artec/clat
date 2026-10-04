// One isolated-copy boundary for all mutation runners. No shared-tree writes.
import {cp,readFile,writeFile} from 'node:fs/promises'
import path from 'node:path'
export async function copyAuthor(source, copy, repo) {
  await cp(source,copy,{recursive:true,filter:file=>path.basename(file)!=='target'})
  const contract=path.join(copy,'src/network_resources/component/plugin_interface.rs')
  try {
    let binding=await readFile(contract,'utf8')
    binding=binding.replace('"../../dsh-wasm-flavor/plugin-wit"',JSON.stringify(path.join(repo,'sdk/dsh-wasm-flavor/plugin-wit')))
    await writeFile(contract,binding)
  } catch(error) {if(error.code!=='ENOENT')throw error}
  const pins=path.join(copy,'src/dns_authority/tests.rs')
  const pinSource=await readFile(pins,'utf8')
  await writeFile(pins,pinSource.replace('"../../../../dsh-wasm-flavor/pin-cases.json"',JSON.stringify(path.join(repo,'sdk/dsh-wasm-flavor/pin-cases.json'))))
  const file=path.join(copy,'src/distribution/tests.rs')
  let text
  try {text=await readFile(file,'utf8')} catch(error) {if(error.code==='ENOENT')return;throw error}
  text=text.replaceAll('"../../../../../release/minisign.pub"',JSON.stringify(path.join(repo,'release/minisign.pub')))
  await writeFile(file,text)
}
