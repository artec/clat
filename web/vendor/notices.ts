/* Reproducible offline notices for the locked widget dependency closure. */
import {readFileSync,writeFileSync,readdirSync,existsSync,mkdirSync} from 'node:fs';
import {join} from 'node:path';
export function writeNotices(root:string):void{
  const queue=Object.keys(JSON.parse(readFileSync(join(root,'package.json'),'utf8')).dependencies),seen=new Set<string>(),rows:{name:string;version:string;license:string;files:string[]}[]=[],texts:string[]=[];
  const write=(path:string,text:string)=>{if(!existsSync(path)||readFileSync(path,'utf8')!==text)writeFileSync(path,text);};
  mkdirSync(join(root,'licenses'),{recursive:true});
  while(queue.length){
    const name=queue.shift()!;if(seen.has(name))continue;seen.add(name);
    const directory=join(root,'node_modules',name),pkg=JSON.parse(readFileSync(join(directory,'package.json'),'utf8'));
    queue.push(...Object.keys(pkg.dependencies||{}));
    const files=readdirSync(directory).filter(file=>/^licen[cs]e(?:$|[.\-])/i.test(file));
    if(!files.length)throw new Error('Missing dependency license: '+name);
    const copies=files.map(file=>{const target=name.replace(/^@/,'').replaceAll('/','__')+'-'+file;const text=readFileSync(join(directory,file),'utf8').replace(/\r\n/g,'\n');write(join(root,'licenses',target),text);return {target,text};});
    rows.push({name,version:pkg.version,license:pkg.license||'SEE LICENSE',files:copies.map(file=>file.target)});
    texts.push(name+' '+pkg.version+'\n\n'+copies.map(file=>file.text.trim()).join('\n\n'));
  }
  write(join(root,'licenses','manifest.json'),JSON.stringify(rows,null,2)+'\n');
  write(join(root,'THIRD-PARTY-NOTICES.txt'),texts.join('\n\n'+'='.repeat(72)+'\n\n')+'\n');
}
