// Web AbortSignal.any iterable semantics. The pinned engine treats the array as a signal.
export function installAbortAny() {
  Object.defineProperty(AbortSignal,'any',{
    configurable:true,writable:true,
    value:function any(signals) {
      // Validate the complete iterable before exposing a result or registering listeners.
      if(signals===null||signals===undefined)throw new TypeError('iterable required')
      const iterator=signals[Symbol.iterator]
      if(typeof iterator!=='function')throw new TypeError('iterable required')
      const inputs=Array.from({[Symbol.iterator]:()=>Reflect.apply(iterator,signals,[])})
      for(const signal of inputs) {
        if(!(signal instanceof AbortSignal))throw new TypeError('AbortSignal required')
      }
      const controller=new AbortController()
      for(const signal of inputs)if(signal.aborted){controller.abort(signal.reason);return controller.signal}
      const listeners=[]
      const abort=signal=>{
        if(controller.signal.aborted)return
        controller.abort(signal.reason)
        for(const [source,listener] of listeners)source.removeEventListener('abort',listener)
        listeners.length=0
      }
      for(const signal of new Set(inputs)) {
        const listener=()=>abort(signal)
        listeners.push([signal,listener]);signal.addEventListener('abort',listener,{once:true})
      }
      return controller.signal
    },
  })
}
