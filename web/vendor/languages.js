/* One offline format registry for editors, diffs, breadcrumbs and file icons. */
import {LanguageDescription,LanguageSupport,StreamLanguage} from '@codemirror/language';
import {javascript} from '@codemirror/lang-javascript';
import {rust} from '@codemirror/lang-rust';
import {json} from '@codemirror/lang-json';
import {html} from '@codemirror/lang-html';
import {css} from '@codemirror/lang-css';
import {markdown} from '@codemirror/lang-markdown';
import {python} from '@codemirror/lang-python';
import {yaml} from '@codemirror/lang-yaml';
import {php} from '@codemirror/lang-php';
import {cpp} from '@codemirror/lang-cpp';
import {java} from '@codemirror/lang-java';
import {go} from '@codemirror/lang-go';
import {vue} from '@codemirror/lang-vue';
import {sass} from '@codemirror/lang-sass';
import {less} from '@codemirror/lang-less';
import {xml} from '@codemirror/lang-xml';
import {sql} from '@codemirror/lang-sql';
import {svelte} from '@replit/codemirror-lang-svelte';
import {shell} from '@codemirror/legacy-modes/mode/shell';
import {toml} from '@codemirror/legacy-modes/mode/toml';
import {csharp,kotlin,scala,dart} from '@codemirror/legacy-modes/mode/clike';
import {swift} from '@codemirror/legacy-modes/mode/swift';
import {ruby} from '@codemirror/legacy-modes/mode/ruby';
import {lua} from '@codemirror/legacy-modes/mode/lua';
import {powerShell} from '@codemirror/legacy-modes/mode/powershell';
import {properties} from '@codemirror/legacy-modes/mode/properties';
import {nginx} from '@codemirror/legacy-modes/mode/nginx';
import {dockerFile} from '@codemirror/legacy-modes/mode/dockerfile';
import {cmake} from '@codemirror/legacy-modes/mode/cmake';
import {diff} from '@codemirror/legacy-modes/mode/diff';
const stream=mode=>()=>new LanguageSupport(StreamLanguage.define(mode));
const rules={
  jsonc:{startState:()=>({comment:false}),token(s,state){if(state.comment){const end=s.string.indexOf('*/',s.pos);if(end<0)s.skipToEnd();else{s.pos=end+2;state.comment=false;}return 'comment';}if(s.match(/\/\/.*/))return 'comment';if(s.match('/*')){state.comment=true;return 'comment';}if(s.match(/"(?:[^"\\]|\\.)*"/))return 'string';if(s.match(/-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/))return 'number';if(s.match(/\b(?:true|false|null)\b/))return 'atom';if(s.match(/[{}\[\]:,]/))return 'punctuation';s.next();return null;}},
  apache:{token(s){if(s.sol()&&s.match(/\s*#.*/))return 'comment';if(s.match(/<\/?[^>]+>/))return 'tag';if(s.match(/"[^"]*"/))return 'string';if(s.sol()&&s.match(/\s*[A-Za-z]+/))return 'keyword';s.next();return null;}},
  ignore:{token(s){if(s.sol()&&s.match(/\s*#.*/))return 'comment';if(s.match(/[!*?\[\]]/))return 'operator';if(s.match(/\\./))return 'string';s.next();return null;}},
  attributes:{token(s){if(s.sol()&&s.match(/\s*#.*/))return 'comment';if(s.match(/\b(?:text|binary|eol|filter|diff|merge|working-tree-encoding)\b/))return 'property';if(s.match(/[=*?!-]/))return 'operator';s.next();return null;}},
  batch:{token(s){if(s.match(/(?:rem\b|::).*/i))return 'comment';if(s.match(/%[^%\s]+%|![^!\s]+!/))return 'variableName';if(s.match(/"[^"]*"/))return 'string';if(s.match(/\b(?:echo|set|if|else|for|do|call|goto|exit|not|exist)\b/i))return 'keyword';s.next();return null;}},
  delimited:sep=>({startState:()=>({quoted:false}),token(s,state){if(s.peek()==='"'){s.next();if(state.quoted&&s.peek()==='"')s.next();else state.quoted=!state.quoted;return 'string';}if(!state.quoted&&s.peek()===sep){s.next();return 'punctuation';}s.next();return state.quoted?'string':null;}}),
};
const formats=[];
function add(id,name,extensions,load,icon='code',names=[]){formats.push({id,name,extensions:extensions.split(' ').filter(Boolean),names,load,icon});}
add('text','Plain text','txt text log',()=>[], 'file');
add('rust','Rust','rs',rust);
add('c','C','c h',cpp);add('cpp','C++','cc cpp cxx c++ hh hpp hxx h++ ino',cpp);
add('csharp','C#','cs csx',stream(csharp));add('java','Java','java',java);
add('kotlin','Kotlin','kt kts',stream(kotlin));add('swift','Swift','swift',stream(swift));
add('go','Go','go',go);add('python','Python','py pyi pyw',python);
add('php','PHP','php php3 php4 php5 phtml',php);add('ruby','Ruby','rb rake gemspec',stream(ruby),'code',['Gemfile','Rakefile']);
add('lua','Lua','lua',stream(lua));add('dart','Dart','dart',stream(dart));add('scala','Scala','scala sc',stream(scala));
add('javascript','JavaScript','js mjs cjs',()=>javascript());
add('jsx','JavaScript JSX','jsx',()=>javascript({jsx:true}));
add('typescript','TypeScript','ts mts cts',()=>javascript({typescript:true}));
add('tsx','TypeScript JSX','tsx',()=>javascript({typescript:true,jsx:true}));
add('html','HTML','html htm xhtml',html);add('css','CSS','css',css);
add('scss','SCSS','scss',sass);add('sass','Sass','sass',()=>sass({indented:true}));add('less','Less','less',less);
add('vue','Vue','vue',vue);add('svelte','Svelte','svelte',svelte);
add('markdown','Markdown','md markdown mdx',()=>markdown({codeLanguages:descriptions}),'document',['README','CHANGELOG','LICENSE']);
add('json','JSON','json ipynb',json,'data',['.babelrc','.prettierrc','.stylelintrc','composer.lock','Pipfile.lock']);
add('jsonc','JSON with comments','jsonc code-workspace',stream(rules.jsonc),'data',['tsconfig.json','jsconfig.json','bun.lock']);
add('yaml','YAML','yaml yml',yaml,'data',['.clang-format','.clang-tidy','.yamllint','yarn.lock']);
add('toml','TOML','toml',stream(toml),'data',['Cargo.lock','uv.lock','poetry.lock','Pipfile']);
add('xml','XML','xml svg xsl xslt xsd plist csproj fsproj props targets',xml,'code');
add('sql','SQL','sql',sql,'data');add('csv','CSV','csv',stream(rules.delimited(',')),'data');add('tsv','TSV','tsv',stream(rules.delimited('\t')),'data');
add('shell','Shell','sh bash zsh fish',stream(shell),'terminal',['.bashrc','.bash_profile','.zshrc','.zprofile','.profile','.env']);
add('powershell','PowerShell','ps1 psm1 psd1',stream(powerShell),'terminal');add('batch','Batch','bat cmd',stream(rules.batch),'terminal');
add('ini','INI / Properties','ini properties cfg',stream(properties),'settings',['.editorconfig','.gitconfig','.gitmodules','.npmrc','.yarnrc']);
add('nginx','Nginx','',stream(nginx),'settings',['nginx.conf']);
add('apache','Apache','htaccess',stream(rules.apache),'settings',['httpd.conf','apache2.conf','.htaccess']);
add('dockerfile','Dockerfile','dockerfile',stream(dockerFile),'settings',['Dockerfile','Containerfile']);
add('makefile','Makefile','mk mak',stream(shell),'settings',['Makefile','GNUmakefile','makefile']);
add('cmake','CMake','cmake',stream(cmake),'settings',['CMakeLists.txt']);
add('ignore','Ignore patterns','',stream(rules.ignore),'settings',['.gitignore','.ignore','.dockerignore','.npmignore','.prettierignore','.eslintignore']);
add('attributes','Git attributes','',stream(rules.attributes),'branch',['.gitattributes']);
add('diff','Diff / Patch','diff patch',stream(diff),'branch');
const aliases={js:'javascript',ts:'typescript',rs:'rust',sh:'shell',bash:'shell',zsh:'shell',csharp:'csharp','c++':'cpp',md:'markdown',yml:'yaml',py:'python',ps1:'powershell'};
const descriptions=formats.filter(f=>f.id!=='text').map(f=>LanguageDescription.of({name:f.name,alias:[f.id,...Object.keys(aliases).filter(k=>aliases[k]===f.id)],extensions:f.extensions,load:()=>Promise.resolve(support(f.id))}));
const cached=new Map();
export function support(id){if(!cached.has(id))cached.set(id,formats.find(f=>f.id===id)?.load()||[]);return cached.get(id);}
export function formatFor(path='',text=''){
  const name=path.split(/[\\/]/).pop(),lower=name.toLowerCase();
  let format=formats.find(f=>f.names.some(n=>n.toLowerCase()===lower));
  if(!format&&/^(dockerfile|containerfile)\./i.test(name))format=formats.find(f=>f.id==='dockerfile');
  if(lower==='yarn.lock'&&!/^\s*__metadata:/m.test(text))format=formats[0];
  if(!format&&/^(?:tsconfig|jsconfig)(?:\.[^.]+)?\.json$/i.test(name))format=formats.find(f=>f.id==='jsonc');
  if(!format&&/^\.env\./i.test(name))format=formats.find(f=>f.id==='shell');
  if(!format)format=formats.find(f=>f.extensions.includes(lower.split('.').pop()));
  if(!format&&text.startsWith('#!')){const first=text.split('\n',1)[0];const id=/python/.test(first)?'python':/ruby/.test(first)?'ruby':/node|deno|bun/.test(first)?'javascript':/pwsh|powershell/.test(first)?'powershell':/\b(?:ba|z|k)?sh\b/.test(first)?'shell':null;format=formats.find(f=>f.id===id);}
  return format||formats[0];
}
export const formatList=formats.map(({id,name,icon})=>({id,name,icon}));
