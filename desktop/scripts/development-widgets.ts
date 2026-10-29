// Build-time only: distributed Rust binaries embed checked-in, offline widget assets.
import {existsSync,readFileSync} from 'node:fs';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {type Runner,run} from './common';
const sha=(file:string)=>createHash('sha256').update(readFileSync(file)).digest('hex');
export function ensureDevelopmentWidgets(root:string,execute:Runner=run):void{
  const vendor=join(root,'web','vendor');
  // Minimal release-test fixtures and consumers of a prebuilt backend have no widget sources.
  if(!existsSync(join(vendor,'package.json')))return;
  try{
    const manifest=JSON.parse(readFileSync(join(vendor,'dist','manifest.json'),'utf8')) as {sources:Record<string,string>;files:Record<string,string>};
    if(['entry.js','languages.js','editor-theme.js','notices.ts','build.ts','package.json','bun.lock'].every(name=>manifest.sources[name]===sha(join(vendor,name)))&&['entry.js','entry.css'].every(name=>manifest.files[name]===sha(join(vendor,'dist',name))))return;
  }catch{/* A missing or stale build is regenerated before compiling the embedded Host. */}
  execute([process.execPath,'install','--cwd',vendor,'--frozen-lockfile','--ignore-scripts'],{cwd:root});
  execute([process.execPath,'run','--cwd',vendor,'build'],{cwd:root});
}
