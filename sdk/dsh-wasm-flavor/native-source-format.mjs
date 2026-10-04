// ECMAScript leaves native-function source formatting implementation-defined.
// Canonicalize whitespace only for the unforgeable native-body representation.
export function normalizeNativeSource(source) {
  return /^function\s*[^()]*\([^)]*\)\s*\{\s*\[native code\]\s*\}$/.test(source)
    ? source.replace(/\s+/g,' ').trim() : source
}
export function installNativeSourceFormat() {
  const original=Function.prototype.toString
  Object.defineProperty(Function.prototype,'toString',{
    configurable:true,writable:true,
    value:function toString() {return normalizeNativeSource(Reflect.apply(original,this,[]))},
  })
}
