// WHATWG URL predicate using the engine's existing URL parser; no Node URL module.
export function installUrlCanParse() {
  if(typeof URL.canParse==='function')return
  Object.defineProperty(URL,'canParse',{
    configurable:true,writable:true,
    value:function canParse(input,base) {
      if(arguments.length===0)throw new TypeError('URL input required')
      const string=value=>{if(typeof value==='symbol')throw new TypeError('URL string required');return String(value)}
      const url=string(input),parent=base===undefined?undefined:string(base)
      try {new URL(url,parent);return true}catch{return false}
    },
  })
}
