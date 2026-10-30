import {writeNotices} from './notices';
import {connectedCanvasFonts} from './canvas-compat';
import {readFileSync,writeFileSync,mkdirSync,existsSync} from 'node:fs';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
const root=import.meta.dir;
const sourceFiles=['entry.js','languages.js','editor-theme.js','notices.ts','canvas-compat.ts','build.ts','package.json','bun.lock'];
function sha(bytes:Uint8Array|string):string{return createHash('sha256').update(bytes).digest('hex');}
function publish(name:string,bytes:Uint8Array|string):void{
  const path=join(root,'dist',name);if(existsSync(path)&&sha(readFileSync(path))===sha(bytes))return;writeFileSync(path,bytes);
}
writeNotices(root);
const result=await Bun.build({entrypoints:[join(root,'entry.js')],plugins:[connectedCanvasFonts()],target:'browser',format:'iife',minify:true,write:false,naming:'entry.[ext]'});
if(!result.success)throw new AggregateError(result.logs,'Development widget build failed');
mkdirSync(join(root,'dist'),{recursive:true});
const files:Record<string,string>={};
for(const output of result.outputs){const bytes=new Uint8Array(await output.arrayBuffer()),name=output.path.split(/[\\/]/).pop()!;publish(name,bytes);files[name]=sha(bytes);}
publish('manifest.json',JSON.stringify({sources:Object.fromEntries(sourceFiles.map(name=>[name,sha(readFileSync(join(root,name)))])),files},null,2)+'\n');
console.log('Offline development widgets are current.');
